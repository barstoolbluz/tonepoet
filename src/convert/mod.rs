//! Audio conversion functionality for tonepoet
//!
//! This module provides comprehensive audio format conversion with support for:
//! - Multiple input/output formats (FLAC, WAV, AIFF, WavPack, MP3, AAC, Opus)
//! - Parallel processing with progress tracking
//! - Metadata preservation and ReplayGain calculation
//! - 7z archive extraction and processing

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::RwLock;

pub mod classify;
pub mod cap_fs;
pub mod cue_parser;
pub mod formats;
pub mod queue_expansion;
pub mod split_cue_album;
pub mod source_admission;
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
pub use source_admission::is_direct_queue_source_path;
pub use cue_parser::{parse_cue, parse_cue_file, CueSheet, CueTrack};
pub use queue_expansion::{
    count_audio_files_bounded, cue_sidecar_override_for_commit_path, expand_paths_to_audio,
    expand_paths_to_audio_with_metadata, expand_paths_to_audio_with_metadata_limited,
    expand_paths_to_audio_with_metadata_using_grouping_decisions,
    split_cue_album_grouping_key_for_queue, QueueExpansionLimitedError,
    QueueExpansionResult, QueueSplitCueAlbumGroupingDecisions,
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

    #[error("Synthetic CUE artifact ownership registration failed for {artifact:?}: {reason}")]
    SyntheticCueArtifactOwnershipFailed { artifact: PathBuf, reason: String },

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
    /// Transient synthetic CUE inputs currently owned by queued conversion
    /// items. The key is the queue item id; values are removed when the item is
    /// terminal, removed from the queue, or when the manager drops.
    synthetic_cue_artifacts: Arc<Mutex<HashMap<String, PathBuf>>>,
    /// Synthetic CUE inputs accepted by the manager while queue ownership is
    /// being resolved asynchronously. This lets reducer callers transfer
    /// responsibility without blocking on the queue lock or destructively
    /// cleaning artifacts whose item-id ownership has not yet been observed.
    pending_synthetic_cue_artifacts: Arc<Mutex<HashSet<PathBuf>>>,
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


#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntheticCueArtifactRegistration {
    /// Ownership was resolved against a real queue snapshot in this call. The
    /// caller may clean artifacts not listed in `claimed`.
    Registered { claimed: HashSet<PathBuf> },
    /// The reducer-safe path accepted the artifacts into manager-owned pending
    /// storage and scheduled asynchronous ownership resolution. The caller must
    /// not clean any of the supplied paths.
    Deferred { paths: HashSet<PathBuf> },
    /// Ownership could not be resolved because manager bookkeeping was
    /// unavailable. The caller must preserve every supplied artifact. If the
    /// manager had already observed concrete queue item ids under a queue
    /// guard, `item_ids` names the exact queue records whose admission must be
    /// rolled back without synthetic-artifact cleanup.
    Failed {
        paths: HashSet<PathBuf>,
        item_ids: HashSet<String>,
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitBatchOutcome {
    pub enqueued: usize,
    pub skipped: usize,
    pub errors: usize,
    pub previously_converted: usize,
    pub last_error: Option<String>,
}

impl CommitBatchOutcome {
    fn success() -> Self {
        Self {
            enqueued: 0,
            skipped: 0,
            errors: 0,
            previously_converted: 0,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitBatchRollbackResult {
    pub attempted_item_ids: Vec<String>,
    pub removed_item_ids: Vec<String>,
    pub failed_item_ids: Vec<String>,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitBatchCueArtifactTransaction {
    pub outcome: CommitBatchOutcome,
    /// Queue item ids admitted by this transaction and still present after a
    /// successful commit. Empty after rollback.
    pub admitted_item_ids: Vec<String>,
    /// Synthetic CUE artifacts whose ownership was recorded in the manager
    /// registry before the queue write guard was released and that remain
    /// manager-owned after the transaction returns.
    pub artifacts_transferred_to_manager: HashSet<PathBuf>,
    /// Synthetic CUE artifacts that were caller-owned at transaction start and
    /// were deterministically cleaned by the transaction because the matching
    /// queue item had already completed.
    pub artifacts_cleaned_after_completed_skip: HashSet<PathBuf>,
    /// Synthetic CUE artifacts from the caller's source state that were not
    /// transferred to manager ownership by this transaction.
    pub artifacts_remaining_caller_owned: HashSet<PathBuf>,
    /// Input paths skipped because they were already represented in the queue.
    pub skipped_items: Vec<PathBuf>,
    /// Rollback result when admission failed after queue mutation began.
    pub rollback: Option<CommitBatchRollbackResult>,
}

impl CommitBatchCueArtifactTransaction {
    pub fn failed_without_queue_mutation(
        mut outcome: CommitBatchOutcome,
        caller_owned: HashSet<PathBuf>,
    ) -> Self {
        outcome.enqueued = 0;
        Self {
            outcome,
            admitted_item_ids: Vec::new(),
            artifacts_transferred_to_manager: HashSet::new(),
            artifacts_cleaned_after_completed_skip: HashSet::new(),
            artifacts_remaining_caller_owned: caller_owned,
            skipped_items: Vec::new(),
            rollback: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticCueRollback {
    pub removed: Vec<String>,
    pub deferred: HashSet<String>,
}

impl SyntheticCueRollback {
    pub fn total(&self) -> usize {
        self.removed.len().saturating_add(self.deferred.len())
    }
}

fn recover_mutex_lock<'a, T>(mutex: &'a Mutex<T>, name: &str) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            log::error!("recovering poisoned {name} mutex; preserving synthetic CUE artifacts");
            poisoned.into_inner()
        }
    }
}


fn lock_synthetic_cue_artifact_registry<'a>(
    mutex: &'a Mutex<HashMap<String, PathBuf>>,
) -> Result<MutexGuard<'a, HashMap<String, PathBuf>>, String> {
    match mutex.lock() {
        Ok(guard) => Ok(guard),
        Err(_) => {
            let error = "synthetic CUE artifact registry is poisoned; preserving artifacts until restart or manual cleanup".to_string();
            log::error!("{error}");
            Err(error)
        }
    }
}

fn synthetic_cue_artifact_pairs_from_queue(
    queue: &ConversionQueue,
    paths: &HashSet<PathBuf>,
) -> Vec<(String, PathBuf, ConversionStatus)> {
    let mut pairs = Vec::new();
    for item in queue.all_items() {
        for path in paths {
            if same_path_for_queue(&item.input_path, path) {
                pairs.push((item.id.clone(), path.clone(), item.status.clone()));
            }
        }
    }
    pairs
}

fn register_synthetic_cue_artifact_pairs(
    synthetic_cue_artifacts: &Arc<Mutex<HashMap<String, PathBuf>>>,
    pairs: Vec<(String, PathBuf, ConversionStatus)>,
) -> Result<HashSet<PathBuf>, String> {
    let mut claimed = HashSet::new();
    let mut artifacts = lock_synthetic_cue_artifact_registry(synthetic_cue_artifacts.as_ref())?;

    for (item_id, path, status) in pairs {
        // Completed items are not retryable. If asynchronous ownership
        // resolution races with completion, preserve the eager-completion
        // cleanup contract instead of registering a now-stale owner.
        if matches!(
            status,
            ConversionStatus::Completed { .. }
                | ConversionStatus::CompletedWithActionErrors { .. }
        ) {
            crate::convert::queue_expansion::cleanup_synthetic_cue_artifact(&path);
            claimed.insert(path);
            continue;
        }
        artifacts.insert(item_id, path.clone());
        claimed.insert(path);
    }

    Ok(claimed)
}

fn register_synthetic_cue_artifact_pairs_from_locked_queue(
    synthetic_cue_artifacts: &Arc<Mutex<HashMap<String, PathBuf>>>,
    queue: &ConversionQueue,
    paths: &HashSet<PathBuf>,
) -> Result<HashSet<PathBuf>, String> {
    let pairs = synthetic_cue_artifact_pairs_from_queue(queue, paths);
    register_synthetic_cue_artifact_pairs(synthetic_cue_artifacts, pairs)
}

fn cleanup_unclaimed_synthetic_cue_artifacts(paths: &HashSet<PathBuf>, claimed: &HashSet<PathBuf>) {
    for artifact in paths.difference(claimed) {
        crate::convert::queue_expansion::cleanup_synthetic_cue_artifact(artifact);
    }
}

fn conversion_item_has_full_pipeline_handoff(item: &ConversionItem) -> bool {
    item.pipeline_request.is_some()
        || item.pipeline_settings.is_some()
        || item.options.pipeline_settings.is_some()
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
        crate::convert::queue_expansion::scavenge_stale_synthetic_cue_album_artifacts();

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
            synthetic_cue_artifacts: Arc::new(Mutex::new(HashMap::new())),
            pending_synthetic_cue_artifacts: Arc::new(Mutex::new(HashSet::new())),
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
        if !dir.is_dir() {
            return Err(ConversionError::ValidationError(format!(
                "not a directory: {}",
                dir.display()
            )));
        }

        let expansion = crate::convert::queue_expansion::expand_paths_to_audio_with_metadata(&[
            dir.to_path_buf(),
        ]);
        if let Some(message) = expansion.first_error() {
            if expansion.paths.is_empty() {
                return Err(ConversionError::ValidationError(message.to_string()));
            }
            log::warn!("directory queue expansion warning: {message}");
        }

        let synthetic_cue_artifacts = expansion.synthetic_cue_artifacts.clone();
        let mut detected = Vec::with_capacity(expansion.paths.len());
        for file in &expansion.paths {
            match FormatDetector::detect(file) {
                Ok(format) => detected.push((file.clone(), format)),
                Err(err) => {
                    crate::convert::queue_expansion::cleanup_synthetic_cue_artifacts(&synthetic_cue_artifacts);
                    return Err(err);
                }
            }
        }

        let mut claimed = std::collections::HashSet::new();
        let mut claimed_by_item: Vec<(String, PathBuf)> = Vec::new();
        let mut admitted_ids: Vec<String> = Vec::new();
        let mut ownership_error = None;
        let mut queue = self.queue.write().await;
        for (file, format) in detected {
            queue.add_item(file.clone(), format, options.clone());
            let Some(item_id) = queue.items_mut().back().map(|item| item.id.clone()) else {
                continue;
            };
            admitted_ids.push(item_id.clone());
            if synthetic_cue_artifacts.iter().any(|artifact| same_path_for_queue(&file, artifact)) {
                if let Some(artifact) = synthetic_cue_artifacts
                    .iter()
                    .find(|artifact| same_path_for_queue(&file, artifact))
                    .cloned()
                {
                    if let Err(error) = self.register_synthetic_cue_artifact(&item_id, &artifact) {
                        let rollback_ids: HashSet<String> = admitted_ids.iter().cloned().collect();
                        queue
                            .items_mut()
                            .retain(|item| !rollback_ids.contains(item.id.as_str()));
                        ownership_error = Some(ConversionError::SyntheticCueArtifactOwnershipFailed {
                            artifact: artifact.clone(),
                            reason: error,
                        });
                        break;
                    }
                    claimed.insert(artifact.clone());
                    claimed_by_item.push((item_id, artifact));
                }
            }
        }
        drop(queue);

        if let Some(error) = ownership_error {
            // Directory admission is transactional: a registry failure rolls
            // back every item admitted by this call, not just the item whose
            // synthetic artifact failed registration. Artifacts that had
            // already transferred into the manager are cleaned through the
            // manager lifecycle after their rolled-back item IDs are removed;
            // artifacts that never transferred remain caller-owned and are
            // cleaned below.
            self.cleanup_rolled_back_synthetic_cue_artifacts(&claimed_by_item);
            for artifact in synthetic_cue_artifacts.difference(&claimed) {
                crate::convert::queue_expansion::cleanup_synthetic_cue_artifact(artifact);
            }
            return Err(error);
        }

        for artifact in synthetic_cue_artifacts.difference(&claimed) {
            crate::convert::queue_expansion::cleanup_synthetic_cue_artifact(artifact);
        }
        Ok(())
    }

    /// Transactionally admit a Convert-screen batch and register any staged
    /// synthetic album-CUE artifacts as part of the same lifecycle transition.
    ///
    /// This is the handoff contract for reviewed Convert-screen work. The
    /// returned transaction is authoritative: callers must use its exact
    /// admitted item ids and artifact ownership sets instead of reconstructing
    /// ownership later by scanning paths. If any ownership registration fails
    /// after queue mutation begins, every item admitted by this transaction is
    /// rolled back before the method returns, and the exact caller-owned
    /// artifact set is reported for deterministic cleanup/retention.
    pub fn commit_batch_with_cue_artifacts<F>(
        &self,
        batch: &[PathBuf],
        cue_artifact_audio: &HashSet<PathBuf>,
        source_synthetic_cue_artifacts: &HashSet<PathBuf>,
        options: &ConversionOptions,
        mut configure_admitted_item: F,
    ) -> CommitBatchCueArtifactTransaction
    where
        F: FnMut(&mut ConversionItem),
    {
        let mut outcome = CommitBatchOutcome::success();
        if batch.is_empty() {
            outcome.errors = 1;
            outcome.last_error = Some("nothing to commit".to_string());
            return CommitBatchCueArtifactTransaction::failed_without_queue_mutation(
                outcome,
                source_synthetic_cue_artifacts.clone(),
            );
        }

        let mut detected = Vec::with_capacity(batch.len());
        for file in batch {
            match FormatDetector::detect(file) {
                Ok(format) => detected.push((file.clone(), format)),
                Err(err) => {
                    outcome.errors += 1;
                    outcome.last_error = Some(format!(
                        "skipped {}: {}",
                        file.display(),
                        err
                    ));
                }
            }
        }
        if detected.is_empty() {
            return CommitBatchCueArtifactTransaction::failed_without_queue_mutation(
                outcome,
                source_synthetic_cue_artifacts.clone(),
            );
        }

        let mut queue = match self.queue.try_write() {
            Ok(queue) => queue,
            Err(_) => {
                outcome.errors = batch.len().max(1);
                outcome.last_error = Some("Queue is busy, try again".to_string());
                return CommitBatchCueArtifactTransaction::failed_without_queue_mutation(
                    outcome,
                    source_synthetic_cue_artifacts.clone(),
                );
            }
        };

        let mut admitted_item_ids = Vec::new();
        let mut transferred = HashSet::new();
        let mut transferred_by_item: Vec<(String, PathBuf)> = Vec::new();
        let mut caller_owned_completed_artifacts_to_cleanup: Vec<PathBuf> = Vec::new();
        let mut cleaned_after_completed_skip: HashSet<PathBuf> = HashSet::new();
        let mut skipped_items = Vec::new();
        let mut ownership_error: Option<(PathBuf, String)> = None;
        let mut configuration_error: Option<String> = None;

        for (file, format) in detected {
            if let Some((existing_id, existing_status)) = queue
                .all_items()
                .into_iter()
                .find(|item| same_path_for_queue(&item.input_path, &file))
                .map(|item| (item.id.clone(), item.status.clone()))
            {
                outcome.skipped += 1;
                skipped_items.push(file.clone());
                if matches!(
                    existing_status,
                    ConversionStatus::Completed { .. }
                        | ConversionStatus::CompletedWithActionErrors { .. }
                ) {
                    outcome.previously_converted += 1;
                }
                if let Some(artifact) = source_synthetic_cue_artifacts
                    .iter()
                    .find(|artifact| same_path_for_queue(&file, artifact))
                    .cloned()
                {
                    if matches!(
                        existing_status,
                        ConversionStatus::Completed { .. }
                            | ConversionStatus::CompletedWithActionErrors { .. }
                    ) {
                        caller_owned_completed_artifacts_to_cleanup.push(artifact.clone());
                        cleaned_after_completed_skip.insert(artifact);
                    } else if let Err(error) = self.register_synthetic_cue_artifact(&existing_id, &artifact) {
                        ownership_error = Some((artifact, error));
                        outcome.errors += 1;
                        outcome.last_error = Some(
                            "synthetic CUE artifact ownership registration failed".to_string(),
                        );
                        break;
                    } else {
                        transferred.insert(artifact);
                    }
                }
                continue;
            }

            let cue_sidecar_override = cue_artifact_audio
                .iter()
                .any(|path| same_path_for_queue(path, &file))
                .then_some(crate::convert::pipeline::CueSidecarPolicy::EmbeddedOnly);
            let mut item = ConversionItem::new_with_cue_sidecar_override(
                file.clone(),
                format,
                options.clone(),
                cue_sidecar_override,
            );
            // Finish all executable request configuration before the item is
            // marked queued or published to the shared queue. This keeps a
            // processing worker from ever observing a runnable item that lacks
            // selected-track, disc-stream, archive, naming, companion, action,
            // or source-option projection.
            configure_admitted_item(&mut item);
            if !conversion_item_has_full_pipeline_handoff(&item) {
                configuration_error = Some(format!(
                    "conversion item configuration failed for {}: queued items require full PipelineSettings or a prebuilt PipelineRequest",
                    file.display()
                ));
                outcome.errors += 1;
                outcome.last_error = Some("conversion item configuration failed".to_string());
                break;
            }
            item.status = ConversionStatus::Queued;
            let item_id = item.id.clone();
            queue.items_mut().push_back(item);
            admitted_item_ids.push(item_id.clone());
            outcome.enqueued += 1;

            if let Some(artifact) = source_synthetic_cue_artifacts
                .iter()
                .find(|artifact| same_path_for_queue(&file, artifact))
                .cloned()
            {
                if let Err(error) = self.register_synthetic_cue_artifact(&item_id, &artifact) {
                    ownership_error = Some((artifact, error));
                    outcome.errors += 1;
                    outcome.last_error = Some(
                        "synthetic CUE artifact ownership registration failed".to_string(),
                    );
                    break;
                }
                transferred.insert(artifact.clone());
                transferred_by_item.push((item_id, artifact));
            }
        }

        if let Some(reason) = configuration_error {
            let rollback_ids: HashSet<String> = admitted_item_ids.iter().cloned().collect();
            queue
                .items_mut()
                .retain(|item| !rollback_ids.contains(item.id.as_str()));
            let remaining_ids = queue
                .all_items()
                .into_iter()
                .filter(|item| rollback_ids.contains(item.id.as_str()))
                .map(|item| item.id.clone())
                .collect::<HashSet<_>>();
            let failed_item_ids = admitted_item_ids
                .iter()
                .filter(|id| remaining_ids.contains(id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let removed_item_ids = admitted_item_ids
                .iter()
                .filter(|id| !remaining_ids.contains(id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            drop(queue);

            for artifact in &caller_owned_completed_artifacts_to_cleanup {
                crate::convert::queue_expansion::cleanup_synthetic_cue_artifact(artifact);
            }
            // Rollback restores ownership to the caller. Release transferred
            // siblings from the manager registry without deleting them: the
            // retained source batch still references them and must remain
            // self-consistent for retry.
            self.release_rolled_back_synthetic_cue_artifacts_to_caller(&transferred_by_item);

            let mut caller_owned = source_synthetic_cue_artifacts.clone();
            for artifact in &cleaned_after_completed_skip {
                caller_owned.remove(artifact);
            }
            outcome.enqueued = 0;
            outcome.last_error = Some(reason);

            return CommitBatchCueArtifactTransaction {
                outcome,
                admitted_item_ids: Vec::new(),
                artifacts_transferred_to_manager: HashSet::new(),
                artifacts_cleaned_after_completed_skip: cleaned_after_completed_skip,
                artifacts_remaining_caller_owned: caller_owned,
                skipped_items,
                rollback: Some(CommitBatchRollbackResult {
                    attempted_item_ids: admitted_item_ids,
                    removed_item_ids,
                    failed_item_ids,
                    completed: remaining_ids.is_empty(),
                }),
            };
        }

        if let Some((failed_artifact, reason)) = ownership_error {
            let rollback_ids: HashSet<String> = admitted_item_ids.iter().cloned().collect();
            queue
                .items_mut()
                .retain(|item| !rollback_ids.contains(item.id.as_str()));
            let remaining_ids = queue
                .all_items()
                .into_iter()
                .filter(|item| rollback_ids.contains(item.id.as_str()))
                .map(|item| item.id.clone())
                .collect::<HashSet<_>>();
            let failed_item_ids = admitted_item_ids
                .iter()
                .filter(|id| remaining_ids.contains(id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let removed_item_ids = admitted_item_ids
                .iter()
                .filter(|id| !remaining_ids.contains(id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            drop(queue);

            for artifact in &caller_owned_completed_artifacts_to_cleanup {
                crate::convert::queue_expansion::cleanup_synthetic_cue_artifact(artifact);
            }
            // Ownership failure is still a retryable Convert-screen rollback.
            // Release any earlier successful registrations from manager
            // ownership, but keep the artifact files themselves because the
            // retained source batch still references them.
            self.release_rolled_back_synthetic_cue_artifacts_to_caller(&transferred_by_item);

            let mut caller_owned = source_synthetic_cue_artifacts.clone();
            for artifact in &cleaned_after_completed_skip {
                caller_owned.remove(artifact);
            }
            outcome.enqueued = 0;
            outcome.last_error = Some(format!(
                "synthetic CUE artifact ownership registration failed for {}: {}",
                failed_artifact.display(), reason
            ));

            return CommitBatchCueArtifactTransaction {
                outcome,
                admitted_item_ids: Vec::new(),
                artifacts_transferred_to_manager: HashSet::new(),
                artifacts_cleaned_after_completed_skip: cleaned_after_completed_skip,
                artifacts_remaining_caller_owned: caller_owned,
                skipped_items,
                rollback: Some(CommitBatchRollbackResult {
                    attempted_item_ids: admitted_item_ids,
                    removed_item_ids,
                    failed_item_ids,
                    completed: remaining_ids.is_empty(),
                }),
            };
        }
        drop(queue);

        for artifact in &caller_owned_completed_artifacts_to_cleanup {
            crate::convert::queue_expansion::cleanup_synthetic_cue_artifact(artifact);
        }

        let mut accounted_artifacts = transferred.clone();
        accounted_artifacts.extend(cleaned_after_completed_skip.iter().cloned());
        let artifacts_remaining_caller_owned = source_synthetic_cue_artifacts
            .difference(&accounted_artifacts)
            .cloned()
            .collect::<HashSet<_>>();

        CommitBatchCueArtifactTransaction {
            outcome,
            admitted_item_ids,
            artifacts_transferred_to_manager: transferred,
            artifacts_cleaned_after_completed_skip: cleaned_after_completed_skip,
            artifacts_remaining_caller_owned,
            skipped_items,
            rollback: None,
        }
    }

    /// Scan a directory for queueable conversion inputs.
    ///
    /// Keep directory admission as a thin compatibility adapter over the
    /// canonical conversion-domain queue expansion planner. This avoids a
    /// second CUE/queue implementation inside `convert::mod` and keeps CLI,
    /// TUI, and manager directory scans on one set of semantics.
    #[allow(dead_code)]
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
        if !conversion_item_has_full_pipeline_handoff(&item) {
            return Err(ConversionError::ConversionFailed(
                "ready queue insertion requires exact PipelineSettings or a prebuilt PipelineRequest".to_string(),
            ));
        }

        // Use try_write() instead of blocking_write() to avoid panic in async context.
        let mut queue = self.queue.try_write().map_err(|_| {
            ConversionError::ConversionFailed("Queue is busy, try again".to_string())
        })?;

        let id = item.id.clone();
        queue.items_mut().push_back(item);
        if crate::convert::queue_expansion::is_synthetic_cue_album_artifact(&file) {
            if let Err(error) = self.register_synthetic_cue_artifact(&id, &file) {
                queue.items_mut().retain(|queued| queued.id.as_str() != id.as_str());
                return Err(ConversionError::SyntheticCueArtifactOwnershipFailed {
                    artifact: file.clone(),
                    reason: error,
                });
            }
        }

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
        if crate::convert::queue_expansion::is_synthetic_cue_album_artifact(&file) {
            if let Err(error) = self.register_synthetic_cue_artifact(&id, &file) {
                queue.items_mut().retain(|queued| queued.id.as_str() != id.as_str());
                return Err(ConversionError::SyntheticCueArtifactOwnershipFailed {
                    artifact: file.clone(),
                    reason: error,
                });
            }
        }

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

    /// Clear queue blocking - for UI compatibility.
    ///
    /// Lifecycle cleanup is performed only after the queue write lock is
    /// acquired and the queue mutation actually removes every item. If the
    /// queue is busy, live synthetic inputs are preserved.
    pub fn clear_queue(&mut self) {
        self.try_resolve_pending_synthetic_cue_artifacts();
        let cleared_processing = if let Ok(mut queue) = self.queue.try_write() {
            let mut processing = HashSet::new();
            let mut processing_synthetic_inputs = HashSet::new();
            for item in queue.all_items() {
                if matches!(item.status, ConversionStatus::Processing { .. }) {
                    processing.insert(item.id.clone());
                    if crate::convert::queue_expansion::is_synthetic_cue_album_artifact(&item.input_path) {
                        processing_synthetic_inputs.insert(item.input_path.clone());
                    }
                }
            }
            queue.clear();
            Some((processing, processing_synthetic_inputs))
        } else {
            None
        };
        if let Some((processing, processing_synthetic_inputs)) = cleared_processing {
            // In-flight items keep their artifacts until the worker's terminal
            // status arrives (deferred cleanup in `update_item_status`).
            self.cleanup_all_synthetic_cue_artifacts_except_with_processing_inputs(
                &processing,
                Some(&processing_synthetic_inputs),
            );
        }
    }

    pub fn register_synthetic_cue_artifact(&self, item_id: &str, path: &Path) -> Result<(), String> {
        if !crate::convert::queue_expansion::is_synthetic_cue_album_artifact(path) {
            return Ok(());
        }
        let mut artifacts = lock_synthetic_cue_artifact_registry(self.synthetic_cue_artifacts.as_ref())?;
        artifacts.insert(item_id.to_string(), path.to_path_buf());
        Ok(())
    }

    pub fn synthetic_cue_artifact_paths_owned_by_manager(
        &self,
        paths: &HashSet<PathBuf>,
    ) -> Result<HashSet<PathBuf>, String> {
        if paths.is_empty() {
            return Ok(HashSet::new());
        }
        let artifacts = lock_synthetic_cue_artifact_registry(self.synthetic_cue_artifacts.as_ref())?;
        Ok(artifacts
            .values()
            .filter(|owned| paths.iter().any(|path| same_path_for_queue(owned, path)))
            .cloned()
            .collect())
    }

    /// Accept synthetic CUE artifacts into the manager's non-deleting pending
    /// lifecycle state when item-id ownership cannot be proven after queue
    /// admission. This is intentionally conservative: the manager keeps the
    /// paths out of SourceState so source cleanup cannot delete a live queue
    /// input, but it also avoids immediate filesystem deletion until ownership
    /// can be resolved or the manager is dropped.
    pub fn quarantine_synthetic_cue_artifacts_without_cleanup(
        &self,
        paths: &HashSet<PathBuf>,
    ) -> usize {
        if paths.is_empty() {
            return 0;
        }
        self.remember_pending_synthetic_cue_artifacts(paths);
        paths.len()
    }

    fn remember_pending_synthetic_cue_artifacts(&self, paths: &HashSet<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let mut pending = recover_mutex_lock(
            self.pending_synthetic_cue_artifacts.as_ref(),
            "pending_synthetic_cue_artifacts",
        );
        pending.extend(paths.iter().cloned());
    }

    fn schedule_pending_synthetic_cue_artifact_registration(&self, paths: HashSet<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        self.remember_pending_synthetic_cue_artifacts(&paths);

        let queue = Arc::clone(&self.queue);
        let synthetic_cue_artifacts = Arc::clone(&self.synthetic_cue_artifacts);
        let pending_synthetic_cue_artifacts = Arc::clone(&self.pending_synthetic_cue_artifacts);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let (active_paths, claimed) = {
                    let queue = queue.read().await;
                    let active_paths = {
                        let pending = recover_mutex_lock(
                            pending_synthetic_cue_artifacts.as_ref(),
                            "pending_synthetic_cue_artifacts",
                        );
                        paths
                            .iter()
                            .filter(|path| pending.contains(*path))
                            .cloned()
                            .collect::<HashSet<_>>()
                    };
                    if active_paths.is_empty() {
                        return;
                    }
                    let claimed = match register_synthetic_cue_artifact_pairs_from_locked_queue(
                        &synthetic_cue_artifacts,
                        &queue,
                        &active_paths,
                    ) {
                        Ok(claimed) => claimed,
                        Err(error) => {
                            log::error!(
                                "deferred synthetic CUE artifact ownership registration failed: {error}; preserving pending artifacts"
                            );
                            return;
                        }
                    };
                    (active_paths, claimed)
                };
                {
                    let mut pending = recover_mutex_lock(
                        pending_synthetic_cue_artifacts.as_ref(),
                        "pending_synthetic_cue_artifacts",
                    );
                    for path in &active_paths {
                        pending.remove(path);
                    }
                }
                cleanup_unclaimed_synthetic_cue_artifacts(&active_paths, &claimed);
            });
        } else {
            std::thread::spawn(move || {
                let (active_paths, claimed) = {
                    let queue = queue.blocking_read();
                    let active_paths = {
                        let pending = recover_mutex_lock(
                            pending_synthetic_cue_artifacts.as_ref(),
                            "pending_synthetic_cue_artifacts",
                        );
                        paths
                            .iter()
                            .filter(|path| pending.contains(*path))
                            .cloned()
                            .collect::<HashSet<_>>()
                    };
                    if active_paths.is_empty() {
                        return;
                    }
                    let claimed = match register_synthetic_cue_artifact_pairs_from_locked_queue(
                        &synthetic_cue_artifacts,
                        &queue,
                        &active_paths,
                    ) {
                        Ok(claimed) => claimed,
                        Err(error) => {
                            log::error!(
                                "deferred synthetic CUE artifact ownership registration failed: {error}; preserving pending artifacts"
                            );
                            return;
                        }
                    };
                    (active_paths, claimed)
                };
                {
                    let mut pending = recover_mutex_lock(
                        pending_synthetic_cue_artifacts.as_ref(),
                        "pending_synthetic_cue_artifacts",
                    );
                    for path in &active_paths {
                        pending.remove(path);
                    }
                }
                cleanup_unclaimed_synthetic_cue_artifacts(&active_paths, &claimed);
            });
        }
    }

    fn try_resolve_pending_synthetic_cue_artifacts(&self) {
        let pending_paths = {
            let pending = recover_mutex_lock(
                self.pending_synthetic_cue_artifacts.as_ref(),
                "pending_synthetic_cue_artifacts",
            );
            if pending.is_empty() {
                return;
            }
            pending.iter().cloned().collect::<HashSet<_>>()
        };

        let claimed = {
            let Ok(queue) = self.queue.try_read() else {
                return;
            };
            match register_synthetic_cue_artifact_pairs_from_locked_queue(
                &self.synthetic_cue_artifacts,
                &queue,
                &pending_paths,
            ) {
                Ok(claimed) => claimed,
                Err(error) => {
                    log::error!(
                        "pending synthetic CUE artifact ownership registration failed: {error}; preserving pending artifacts"
                    );
                    return;
                }
            }
        };

        {
            let mut pending = recover_mutex_lock(
                self.pending_synthetic_cue_artifacts.as_ref(),
                "pending_synthetic_cue_artifacts",
            );
            for path in &pending_paths {
                pending.remove(path);
            }
        }
        cleanup_unclaimed_synthetic_cue_artifacts(&pending_paths, &claimed);
    }

    /// Resolve synthetic album-CUE ownership without blocking the caller.  If
    /// the queue lock is available, this registers matching item ids from a
    /// real queue snapshot and returns the claimed paths.  If the queue is
    /// contended, the manager accepts the paths into pending ownership and
    /// spawns an async resolver that awaits the queue read lock off the reducer.
    pub fn register_synthetic_cue_artifacts_for_current_queue_nonblocking(
        &self,
        paths: &HashSet<PathBuf>,
    ) -> SyntheticCueArtifactRegistration {
        if paths.is_empty() {
            return SyntheticCueArtifactRegistration::Registered {
                claimed: HashSet::new(),
            };
        }

        self.try_resolve_pending_synthetic_cue_artifacts();

        match self.queue.try_read() {
            Ok(queue) => {
                let pairs = synthetic_cue_artifact_pairs_from_queue(&queue, paths);
                let item_ids = pairs.iter().map(|(id, _, _)| id.clone()).collect::<HashSet<_>>();
                match register_synthetic_cue_artifact_pairs(
                    &self.synthetic_cue_artifacts,
                    pairs,
                ) {
                    Ok(claimed) => SyntheticCueArtifactRegistration::Registered { claimed },
                    Err(error) => SyntheticCueArtifactRegistration::Failed {
                        paths: paths.clone(),
                        item_ids,
                        error,
                    },
                }
            }
            Err(_) => {
                let deferred = paths.clone();
                self.schedule_pending_synthetic_cue_artifact_registration(deferred.clone());
                SyntheticCueArtifactRegistration::Deferred { paths: deferred }
            }
        }
    }

    /// Resolve synthetic album-CUE ownership by awaiting a real queue snapshot.
    /// This is appropriate for CLI/background code; reducer callers must use
    /// the nonblocking variant above.
    pub async fn register_synthetic_cue_artifacts_for_current_queue_await(
        &self,
        paths: &HashSet<PathBuf>,
    ) -> Result<HashSet<PathBuf>, String> {
        if paths.is_empty() {
            return Ok(HashSet::new());
        }
        let queue = self.queue.read().await;
        register_synthetic_cue_artifact_pairs_from_locked_queue(
            &self.synthetic_cue_artifacts,
            &queue,
            paths,
        )
    }

    fn cleanup_synthetic_cue_artifact_for_item_id(&self, item_id: &str) {
        self.try_resolve_pending_synthetic_cue_artifacts();
        let path = {
            let Ok(mut artifacts) =
                lock_synthetic_cue_artifact_registry(self.synthetic_cue_artifacts.as_ref())
            else {
                return;
            };
            let mut pending = recover_mutex_lock(
                self.pending_synthetic_cue_artifacts.as_ref(),
                "pending_synthetic_cue_artifacts",
            );
            let path = artifacts.remove(item_id);
            if let Some(ref path) = path {
                pending.remove(path);
            }
            path
        };
        if let Some(path) = path {
            crate::convert::queue_expansion::cleanup_synthetic_cue_artifact(&path);
        }
    }

    /// Clean synthetic artifacts for queue items that were admitted and then
    /// rolled back in the same manager transaction. The caller supplies the
    /// exact `(item_id, path)` pairs that this transaction registered or tried
    /// to register, so cleanup does not depend on re-reading lifecycle state
    /// that may have become poisoned after a partial registration.
    ///
    /// This helper is intentionally narrower than ordinary per-item cleanup:
    /// use it only after the corresponding queue records have already been
    /// removed under the queue write guard. In that state, deleting these exact
    /// paths is safe even if the artifact registry can no longer be inspected.
    fn cleanup_rolled_back_synthetic_cue_artifacts(&self, artifacts_to_clean: &[(String, PathBuf)]) {
        if artifacts_to_clean.is_empty() {
            return;
        }

        if let Ok(mut artifacts) =
            lock_synthetic_cue_artifact_registry(self.synthetic_cue_artifacts.as_ref())
        {
            for (item_id, _) in artifacts_to_clean {
                artifacts.remove(item_id);
            }
        } else {
            log::error!(
                "synthetic CUE artifact registry unavailable during rollback cleanup; \
                 deleting exact rolled-back artifacts from transaction-owned paths"
            );
        }

        let mut pending = recover_mutex_lock(
            self.pending_synthetic_cue_artifacts.as_ref(),
            "pending_synthetic_cue_artifacts",
        );
        for (_, path) in artifacts_to_clean {
            pending.remove(path);
        }
        drop(pending);

        for (_, path) in artifacts_to_clean {
            crate::convert::queue_expansion::cleanup_synthetic_cue_artifact(path);
        }
    }

    /// Release rolled-back synthetic artifacts back to the caller without
    /// deleting the filesystem paths. Convert-screen batch retries still hold
    /// source records that reference these files, so rollback must remove
    /// stale manager ownership while preserving the caller-owned artifacts.
    fn release_rolled_back_synthetic_cue_artifacts_to_caller(
        &self,
        artifacts_to_release: &[(String, PathBuf)],
    ) {
        if artifacts_to_release.is_empty() {
            return;
        }

        if let Ok(mut artifacts) =
            lock_synthetic_cue_artifact_registry(self.synthetic_cue_artifacts.as_ref())
        {
            for (item_id, _) in artifacts_to_release {
                artifacts.remove(item_id);
            }
        } else {
            log::error!(
                "synthetic CUE artifact registry unavailable during rollback release; \
                 preserving exact rolled-back artifact paths for caller retry"
            );
        }

        let mut pending = recover_mutex_lock(
            self.pending_synthetic_cue_artifacts.as_ref(),
            "pending_synthetic_cue_artifacts",
        );
        for (_, path) in artifacts_to_release {
            pending.remove(path);
        }
    }

    fn cleanup_synthetic_cue_artifacts_for_item_ids<I>(&self, item_ids: I)
    where
        I: IntoIterator<Item = String>,
    {
        for item_id in item_ids {
            self.cleanup_synthetic_cue_artifact_for_item_id(&item_id);
        }
    }

    pub fn cleanup_all_synthetic_cue_artifacts(&self) {
        self.cleanup_all_synthetic_cue_artifacts_except(&HashSet::new());
    }

    /// Clean all owned synthetic artifacts EXCEPT those registered to the
    /// given item ids. In-flight (Processing) items keep their registry
    /// entries: the worker is still reading the artifact, and the deferred
    /// terminal-status path in `update_item_status` cleans it once the worker
    /// reports done (manager drop and the TTL scavenger are the backstops).
    fn cleanup_all_synthetic_cue_artifacts_except(&self, keep_item_ids: &HashSet<String>) {
        let processing_inputs = self.processing_synthetic_cue_inputs_snapshot();
        self.cleanup_all_synthetic_cue_artifacts_except_with_processing_inputs(
            keep_item_ids,
            processing_inputs.as_ref(),
        );
    }

    fn cleanup_all_synthetic_cue_artifacts_except_with_processing_inputs(
        &self,
        keep_item_ids: &HashSet<String>,
        processing_inputs: Option<&HashSet<PathBuf>>,
    ) {
        let Ok(mut artifacts) =
            lock_synthetic_cue_artifact_registry(self.synthetic_cue_artifacts.as_ref())
        else {
            // A poisoned registry is an integrity failure in lifecycle
            // bookkeeping. Prefer leaking temporary artifacts over deleting
            // inputs whose ownership cannot be trusted.
            return;
        };
        let paths = {
            let mut pending = recover_mutex_lock(
                self.pending_synthetic_cue_artifacts.as_ref(),
                "pending_synthetic_cue_artifacts",
            );
            let mut kept = HashMap::new();
            let mut paths: HashSet<PathBuf> = HashSet::new();
            for (item_id, path) in artifacts.drain() {
                if keep_item_ids.contains(&item_id) {
                    pending.remove(&path);
                    kept.insert(item_id, path);
                } else {
                    paths.insert(path);
                }
            }
            *artifacts = kept;

            // Pending artifacts do not yet have item-id ownership.  A clear-all
            // during the deferred-registration window must therefore match them
            // by the Processing queue input path and preserve them until worker
            // terminal status or manager drop can clean them safely.  If the
            // queue snapshot is contended, fail closed by preserving all pending
            // paths rather than deleting an input a worker may be reading.
            let mut retained_pending = HashSet::new();
            for path in pending.drain() {
                let keep_pending = processing_inputs
                    .as_ref()
                    .map(|inputs| inputs.iter().any(|input| same_path_for_queue(input, &path)))
                    .unwrap_or(true);
                if keep_pending {
                    retained_pending.insert(path);
                } else {
                    paths.insert(path);
                }
            }
            *pending = retained_pending;
            paths
        };
        for path in paths {
            crate::convert::queue_expansion::cleanup_synthetic_cue_artifact(&path);
        }
    }

    fn processing_synthetic_cue_inputs_snapshot(&self) -> Option<HashSet<PathBuf>> {
        let queue = self.queue.try_read().ok()?;
        Some(
            queue
                .all_items()
                .into_iter()
                .filter(|item| matches!(item.status, ConversionStatus::Processing { .. }))
                .filter(|item| crate::convert::queue_expansion::is_synthetic_cue_album_artifact(&item.input_path))
                .map(|item| item.input_path.clone())
                .collect(),
        )
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
        self.try_resolve_pending_synthetic_cue_artifacts();
        let mut updated = false;

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
                updated = true;
            }
        }

        let terminal = matches!(
            status,
            ConversionStatus::Completed { .. }
                | ConversionStatus::CompletedWithActionErrors { .. }
                | ConversionStatus::Failed { .. }
                | ConversionStatus::Partial { .. }
                | ConversionStatus::Cancelled
        );
        if updated
            && matches!(
                status,
                ConversionStatus::Completed { .. } | ConversionStatus::CompletedWithActionErrors { .. }
            )
        {
            self.cleanup_synthetic_cue_artifact_for_item_id(id);
        } else if !updated && terminal {
            // The worker finished but the queue write lock was contended (a
            // completed item's cleanup would otherwise be silently lost) or the
            // item was removed/cleared while Processing (its artifact was
            // deliberately preserved for the in-flight worker). Verify against
            // a read snapshot before touching artifacts: an item that is live
            // and non-terminal (e.g. retried under the same id) keeps its input.
            if let Ok(queue) = self.queue.try_read() {
                let safe = match queue.all_items().into_iter().find(|item| item.id == id) {
                    None => true,
                    Some(item) => matches!(
                        item.status,
                        ConversionStatus::Completed { .. }
                            | ConversionStatus::CompletedWithActionErrors { .. }
                    ),
                };
                drop(queue);
                if safe {
                    self.cleanup_synthetic_cue_artifact_for_item_id(id);
                }
            }
            // Read-lock miss: leave the artifact; manager drop and the TTL
            // scavenger are the backstops.
        }
        updated
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
        // Cancel all queued items.  Cancelled items are retryable, so their
        // transient synthetic CUE inputs must stay alive until the item is
        // removed from the queue or retried to completion.
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
        self.try_resolve_pending_synthetic_cue_artifacts();
        let removed_records = if let Ok(mut queue) = self.queue.try_write() {
            queue.remove_selected_records()
        } else {
            Vec::new()
        };
        let removed = removed_records.len();
        // Never delete a synthetic input a worker is still reading: Processing
        // items keep their registry entries, and the worker's terminal status
        // message triggers the deferred cleanup in `update_item_status` (manager
        // drop and the TTL scavenger are the backstops).
        let cleanable: Vec<String> = removed_records
            .into_iter()
            .filter(|(_, status)| !matches!(status, ConversionStatus::Processing { .. }))
            .map(|(id, _)| id)
            .collect();
        self.cleanup_synthetic_cue_artifacts_for_item_ids(cleanable);
        removed
    }

    /// Remove a queue item by id without touching synthetic-artifact lifecycle state.
    ///
    /// This is used only when queue admission must be rolled back because
    /// artifact ownership registration failed before the item became safely
    /// manager-owned. The artifact remains on disk for the caller/reporting
    /// path rather than being deleted based on incomplete bookkeeping.
    pub fn discard_item_without_synthetic_artifact_cleanup(&self, item_id: &str) -> bool {
        if let Ok(mut queue) = self.queue.try_write() {
            queue.remove_item_by_id(item_id)
        } else {
            false
        }
    }

    /// Remove exact just-admitted queue item ids without touching synthetic-artifact
    /// cleanup. If the reducer cannot acquire the queue write lock immediately,
    /// schedule the rollback on a blocking/async worker so the queue cannot be
    /// left permanently committed without artifact ownership. The caller keeps
    /// source-side artifact ownership until this rollback has either completed or
    /// the user retries the commit.
    pub fn discard_items_without_synthetic_artifact_cleanup_for_item_ids_or_defer(
        &self,
        item_ids: HashSet<String>,
    ) -> SyntheticCueRollback {
        if item_ids.is_empty() {
            return SyntheticCueRollback { removed: Vec::new(), deferred: HashSet::new() };
        }

        if let Ok(mut queue) = self.queue.try_write() {
            let removed = queue.remove_matching_item_ids(|item| item_ids.contains(&item.id));
            return SyntheticCueRollback { removed, deferred: HashSet::new() };
        }

        let queue = Arc::clone(&self.queue);
        let deferred = item_ids.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut queue = queue.write().await;
                let removed = queue.remove_matching_item_ids(|item| item_ids.contains(&item.id));
                if !removed.is_empty() {
                    log::warn!(
                        "deferred rollback removed {} queue item(s) after synthetic CUE artifact ownership registration failed",
                        removed.len()
                    );
                }
            });
        } else {
            std::thread::spawn(move || {
                let mut queue = queue.blocking_write();
                let removed = queue.remove_matching_item_ids(|item| item_ids.contains(&item.id));
                if !removed.is_empty() {
                    log::warn!(
                        "deferred rollback removed {} queue item(s) after synthetic CUE artifact ownership registration failed",
                        removed.len()
                    );
                }
            });
        }

        SyntheticCueRollback { removed: Vec::new(), deferred }
    }

    /// Remove just-admitted items without touching synthetic-artifact cleanup.
    ///
    /// This is used when a later admission step cannot complete ownership
    /// registration. The caller still owns the source artifacts and must report
    /// the failed commit rather than presenting a successful queue admission.
    pub fn item_ids_for_paths(&self, paths: &[PathBuf]) -> HashSet<String> {
        if paths.is_empty() {
            return HashSet::new();
        }
        if let Ok(queue) = self.queue.try_read() {
            queue
                .all_items()
                .into_iter()
                .filter(|item| {
                    paths
                        .iter()
                        .any(|path| same_path_for_queue(&item.input_path, path))
                })
                .map(|item| item.id.clone())
                .collect()
        } else {
            HashSet::new()
        }
    }

    pub fn discard_items_without_synthetic_artifact_cleanup_for_paths_except(
        &self,
        paths: &[PathBuf],
        preserve_item_ids: &HashSet<String>,
    ) -> Vec<String> {
        if paths.is_empty() {
            return Vec::new();
        }
        if let Ok(mut queue) = self.queue.try_write() {
            queue.remove_matching_item_ids(|item| {
                !preserve_item_ids.contains(&item.id)
                    && paths
                        .iter()
                        .any(|path| same_path_for_queue(&item.input_path, path))
            })
        } else {
            Vec::new()
        }
    }

    pub fn discard_items_without_synthetic_artifact_cleanup_for_paths(
        &self,
        paths: &[PathBuf],
    ) -> Vec<String> {
        self.discard_items_without_synthetic_artifact_cleanup_for_paths_except(
            paths,
            &HashSet::new(),
        )
    }

    /// Get all items as a cloned vector for UI display
    pub fn get_items_clone(&self) -> Vec<ConversionItem> {
        if let Ok(queue) = self.queue.try_read() {
            queue.all_items().iter().map(|&item| item.clone()).collect()
        } else {
            Vec::new()
        }
    }

    /// Clear completed-success items from queue.
    ///
    /// Cleanup is based on the IDs actually removed by the queue while the
    /// write lock is held. Retryable Failed/Partial/Cancelled entries are not
    /// removed by this operation and therefore keep owned synthetic inputs.
    pub fn clear_completed(&mut self) {
        self.try_resolve_pending_synthetic_cue_artifacts();
        let removed_item_ids = if let Ok(mut queue) = self.queue.try_write() {
            queue.clear_completed()
        } else {
            Vec::new()
        };
        self.cleanup_synthetic_cue_artifacts_for_item_ids(removed_item_ids);
    }

    pub fn clear_finished(&mut self) {
        self.try_resolve_pending_synthetic_cue_artifacts();
        let removed_item_ids = if let Ok(mut queue) = self.queue.try_write() {
            queue.clear_finished()
        } else {
            Vec::new()
        };
        self.cleanup_synthetic_cue_artifacts_for_item_ids(removed_item_ids);
    }

    pub fn clear_all(&mut self) {
        self.try_resolve_pending_synthetic_cue_artifacts();
        let cleared_processing = if let Ok(mut queue) = self.queue.try_write() {
            let mut processing = HashSet::new();
            let mut processing_synthetic_inputs = HashSet::new();
            for item in queue.all_items() {
                if matches!(item.status, ConversionStatus::Processing { .. }) {
                    processing.insert(item.id.clone());
                    if crate::convert::queue_expansion::is_synthetic_cue_album_artifact(&item.input_path) {
                        processing_synthetic_inputs.insert(item.input_path.clone());
                    }
                }
            }
            queue.clear();
            Some((processing, processing_synthetic_inputs))
        } else {
            None
        };
        if let Some((processing, processing_synthetic_inputs)) = cleared_processing {
            // In-flight items keep their artifacts until the worker's terminal
            // status arrives (deferred cleanup in `update_item_status`).
            self.cleanup_all_synthetic_cue_artifacts_except_with_processing_inputs(
                &processing,
                Some(&processing_synthetic_inputs),
            );
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
                    if crate::convert::queue_expansion::is_synthetic_cue_album_artifact(&item.input_path) {
                        return false;
                    }
                    // Save: NotConfigured, Queued, Paused, Completed, Failed
                    // Don't save: Processing, Cancelled, transient synthetic CUE inputs
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

impl Drop for ConversionManager {
    fn drop(&mut self) {
        self.cleanup_all_synthetic_cue_artifacts();
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
    use super::per_track_epoch_tests::{assert_path_exists, synthetic_artifact_for, test_manager_with_item};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
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

    pub(super) fn pipeline_request_for(path: &Path, item_id: &str) -> crate::convert::pipeline::PipelineRequest {
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
    fn synthetic_cue_artifact_registration_defers_without_blocking_under_contention() {
        let temp = TempDir::new("synthetic-claim-contention");
        let artifact = temp.path.join("album.cue");
        fs::write(&artifact, b"FILE \"a.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n")
            .expect("write synthetic cue");

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let manager = ConversionManager::new(ConversionConfig::default());
            let mut paths = std::collections::HashSet::new();
            paths.insert(artifact.clone());

            let mut guard = manager.queue.write().await;
            guard.add_item(
                artifact.clone(),
                FileFormat::Audio(AudioFormat::Flac),
                ConversionOptions::default(),
            );

            let started = std::time::Instant::now();
            let registration = manager
                .register_synthetic_cue_artifacts_for_current_queue_nonblocking(&paths);
            assert!(
                started.elapsed() < std::time::Duration::from_millis(50),
                "reducer-safe registration must not spin or sleep while the queue write lock is held"
            );
            assert!(
                matches!(registration, SyntheticCueArtifactRegistration::Deferred { .. }),
                "contended registration must be explicitly deferred, not falsely reported as claimed"
            );
            assert!(
                manager
                    .synthetic_cue_artifacts
                    .lock()
                    .expect("registry lock")
                    .is_empty(),
                "deferred registration must not claim item-id ownership until it reads the queue"
            );
            assert!(artifact.exists(), "deferred registration must preserve the artifact");

            drop(guard);
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if manager
                        .synthetic_cue_artifacts
                        .lock()
                        .expect("registry lock")
                        .values()
                        .any(|path| path == &artifact)
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("deferred registration resolves after queue lock release");
        });
    }

    #[test]
    fn synthetic_cue_artifact_registry_poison_returns_error_and_preserves_artifact() {
        let (_artifact_dir, artifact) = synthetic_artifact_for("synthetic-item", "registry-poison");

        let manager = ConversionManager::new(ConversionConfig::default());
        let registry = Arc::clone(&manager.synthetic_cue_artifacts);
        let poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = registry.lock().expect("registry lock before poison");
            panic!("intentional synthetic artifact registry poison");
        }));
        assert!(poison.is_err());

        let mut paths = std::collections::HashSet::new();
        paths.insert(artifact.clone());
        let registration = manager
            .register_synthetic_cue_artifacts_for_current_queue_nonblocking(&paths);

        match registration {
            SyntheticCueArtifactRegistration::Failed { paths: failed_paths, item_ids: _, error } => {
                assert!(failed_paths.contains(&artifact));
                assert!(error.contains("poisoned"), "unexpected error: {error}");
            }
            other => panic!("poisoned registry must return explicit failure, got {other:?}"),
        }
        let single = manager.register_synthetic_cue_artifact("synthetic-item", &artifact);
        match single {
            Err(error) => assert!(error.contains("poisoned"), "unexpected error: {error}"),
            Ok(()) => panic!("single-artifact registration must report poisoned registry failure"),
        }
        match manager.synthetic_cue_artifact_paths_owned_by_manager(&paths) {
            Err(error) => assert!(error.contains("poisoned"), "unexpected error: {error}"),
            Ok(owned) => panic!(
                "ownership inspection must report poisoned registry failure, not collapse to {:?}",
                owned
            ),
        }
        assert!(
            artifact.exists(),
            "registry failure must preserve synthetic CUE artifacts rather than cleaning them"
        );
    }

    #[test]
    fn ready_queue_synthetic_admission_reports_registry_failure_without_deleting_artifact() {
        let (artifact_dir, artifact) = synthetic_artifact_for("poisoned-ready", "ready-poison");
        let mut manager = ConversionManager::new(ConversionConfig::default());
        let registry = Arc::clone(&manager.synthetic_cue_artifacts);
        let poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = registry.lock().expect("registry lock before poison");
            panic!("intentional synthetic artifact registry poison");
        }));
        assert!(poison.is_err());

        let request = pipeline_request_for(&artifact, "poisoned-ready");
        let err = manager
            .add_file_ready_for_processing_with_pipeline_request(
                artifact.clone(),
                ConversionOptions::default(),
                request,
                None,
            )
            .expect_err("poisoned registry must reject ready synthetic admission");

        match err {
            ConversionError::SyntheticCueArtifactOwnershipFailed { artifact: failed, reason } => {
                assert_eq!(failed, artifact);
                assert!(reason.contains("poisoned"), "unexpected error: {reason}");
            }
            other => panic!("expected typed synthetic ownership failure, got {other:?}"),
        }
        assert_path_exists(
            &artifact,
            "ownership failure must preserve the caller-owned synthetic artifact",
        );
        let queue = manager.queue.try_read().expect("queue read lock");
        assert_eq!(queue.total_items(), 0, "failed ownership registration must roll back queue admission");
        drop(queue);
        let _ = fs::remove_dir_all(artifact_dir);
    }

    #[test]
    fn add_directory_registry_failure_cleans_unclaimed_synthetic_artifacts() {
        let temp = TempDir::new("add-directory-registry-failure");
        let album = temp.path.join("album");
        fs::create_dir_all(&album).expect("album dir");
        fs::write(album.join("add_directory_leak_a.flac"), b"placeholder a").expect("audio a");
        fs::write(album.join("add_directory_leak_b.flac"), b"placeholder b").expect("audio b");
        fs::write(
            album.join("add_directory_leak_a.cue"),
            "TITLE \"Add Directory Leak Check\"\nFILE \"add_directory_leak_a.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"A1\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"A2\"\n    INDEX 01 00:30:00\n",
        )
        .expect("cue a");
        fs::write(
            album.join("add_directory_leak_b.cue"),
            "TITLE \"Add Directory Leak Check\"\nFILE \"add_directory_leak_b.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"B1\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"B2\"\n    INDEX 01 00:30:00\n",
        )
        .expect("cue b");

        let mut manager = ConversionManager::new(ConversionConfig::default());
        let registry = Arc::clone(&manager.synthetic_cue_artifacts);
        let poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = registry.lock().expect("registry lock before poison");
            panic!("intentional synthetic artifact registry poison");
        }));
        assert!(poison.is_err());

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let err = rt
            .block_on(manager.add_directory(&album, ConversionOptions::default()))
            .expect_err("poisoned registry must fail directory admission");
        match err {
            ConversionError::SyntheticCueArtifactOwnershipFailed { artifact, reason } => {
                assert!(artifact.ends_with("album.cue"));
                assert!(reason.contains("poisoned"), "unexpected error: {reason}");
            }
            other => panic!("expected synthetic ownership failure, got {other:?}"),
        }

        let queue = manager.queue.try_read().expect("queue read lock after failed add_directory");
        assert_eq!(
            queue.total_items(),
            0,
            "add_directory ownership failure must roll back every item admitted by the call"
        );
        drop(queue);

        let root = std::env::temp_dir().join("tonepoet-synthetic-cue-albums");
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.file_name().and_then(|name| name.to_str()) == Some("album.cue") {
                    let text = fs::read_to_string(&path).unwrap_or_default();
                    assert!(
                        !text.contains("add_directory_leak_a.flac")
                            && !text.contains("add_directory_leak_b.flac"),
                        "directory admission failure must not abandon caller-owned synthetic artifact {}",
                        path.display()
                    );
                }
            }
        }
    }

    #[test]
    fn rollback_cleanup_deletes_previously_claimed_artifact_after_registry_poison() {
        let (_artifact_dir, artifact) = synthetic_artifact_for("claimed-before-poison", "rollback-poison");
        let manager = ConversionManager::new(ConversionConfig::default());
        let item_id = "claimed-before-poison".to_string();

        manager
            .register_synthetic_cue_artifact(&item_id, &artifact)
            .expect("first synthetic artifact registration succeeds before poison");
        assert_path_exists(&artifact, "registered artifact before rollback");

        let registry = Arc::clone(&manager.synthetic_cue_artifacts);
        let poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = registry.lock().expect("registry lock before poison");
            panic!("intentional synthetic artifact registry poison after first registration");
        }));
        assert!(poison.is_err());

        manager.cleanup_rolled_back_synthetic_cue_artifacts(&[(item_id, artifact.clone())]);
        assert!(
            !artifact.exists(),
            "rollback must delete exact artifacts registered by a failed transaction even after registry poisoning: {}",
            artifact.display()
        );
    }

    #[test]
    fn synthetic_artifact_registration_holds_queue_guard_until_registry_insert() {
        let manager = ConversionManager::new(ConversionConfig::default());
        let item_id = "synthetic-atomic-item".to_string();
        let (_artifact_dir, artifact) = synthetic_artifact_for(&item_id, "atomic-register");
        {
            let mut queue = manager.queue.try_write().expect("queue write lock");
            let mut item = ConversionItem::default();
            item.id = item_id.clone();
            item.input_path = artifact.clone();
            item.status = ConversionStatus::Processing {
                progress: 1.0,
                message: Some("processing".to_string()),
                file_progress: None,
                phase: Some(ConversionPhase::Converting),
                phase_progress: Some(1.0),
            };
            queue.items_mut().push_back(item);
        }

        let registry_guard = manager
            .synthetic_cue_artifacts
            .lock()
            .expect("hold registry lock to pause registration after queue read");
        let queue = Arc::clone(&manager.queue);
        let registry = Arc::clone(&manager.synthetic_cue_artifacts);
        let mut paths = std::collections::HashSet::new();
        paths.insert(artifact.clone());
        let (guard_acquired_tx, guard_acquired_rx) = std::sync::mpsc::channel();
        let registration_thread = std::thread::spawn(move || {
            let queue_guard = queue.blocking_read();
            guard_acquired_tx
                .send(())
                .expect("signal that registration holds the queue read guard");
            register_synthetic_cue_artifact_pairs_from_locked_queue(&registry, &queue_guard, &paths)
        });

        guard_acquired_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("registration thread must signal after acquiring the queue read guard");
        assert!(
            !manager.update_item_status(
                &item_id,
                ConversionStatus::Completed {
                    output_path: PathBuf::from("/tmp/out.flac"),
                    log_path: None,
                },
                100.0,
            ),
            "terminal queue mutation must not pass while registration holds a queue read guard"
        );
        assert_path_exists(
            &artifact,
            "artifact must not be cleaned before ownership registration completes",
        );

        drop(registry_guard);
        let claimed = registration_thread
            .join()
            .expect("registration thread joins")
            .expect("registration succeeds");
        assert!(claimed.contains(&artifact));

        assert!(manager.update_item_status(
            &item_id,
            ConversionStatus::Completed {
                output_path: PathBuf::from("/tmp/out.flac"),
                log_path: None,
            },
            100.0,
        ));
        assert!(
            !artifact.exists(),
            "completion after atomic registration must clean the owned artifact"
        );
    }

    #[test]
    fn remove_selected_cleans_only_ids_returned_by_successful_queue_mutation() {
        let (mut manager, item_id) = test_manager_with_item();
        let (artifact_dir, artifact) = synthetic_artifact_for(&item_id, "remove-selected-returned-ids");
        manager
            .register_synthetic_cue_artifact(&item_id, &artifact)
            .expect("synthetic artifact registration");
        {
            let mut queue = manager.queue.try_write().expect("queue write lock");
            let item = queue.find_item_mut(&item_id).expect("item exists");
            item.selected = true;
            // Terminal status: in-flight (Processing) items deliberately keep
            // their artifacts on removal (see remove_selected_preserves_...).
            item.status = ConversionStatus::Completed {
                output_path: PathBuf::from("/tmp/out.flac"),
                log_path: None,
            };
        }

        let removed = manager.remove_selected();
        assert_eq!(removed, 1, "selected item must be removed by the queue mutation");
        assert!(
            !artifact_dir.exists(),
            "manager cleanup must follow the item ids returned by remove_selected_item_ids"
        );
    }

    #[test]
    fn synthetic_artifact_cleanup_paths_use_one_lock_order() {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/convert/mod.rs"),
        )
        .expect("read convert manager source");

        let per_item_start = source
            .find("fn cleanup_synthetic_cue_artifact_for_item_id")
            .expect("per-item cleanup helper exists");
        let per_item_end = source[per_item_start..]
            .find("fn cleanup_synthetic_cue_artifacts_for_item_ids")
            .map(|offset| per_item_start + offset)
            .expect("next cleanup helper exists");
        let per_item = &source[per_item_start..per_item_end];
        let per_item_registry = per_item
            .find("lock_synthetic_cue_artifact_registry")
            .expect("per-item cleanup locks registry");
        let per_item_pending = per_item
            .find("self.pending_synthetic_cue_artifacts")
            .expect("per-item cleanup locks pending set");
        assert!(
            per_item_registry < per_item_pending,
            "per-item cleanup must lock the registry before the pending set"
        );

        let global_start = source
            .find("pub fn cleanup_all_synthetic_cue_artifacts")
            .expect("global cleanup helper exists");
        let global_end = source[global_start..]
            .find("fn record_closed_track_epoch")
            .map(|offset| global_start + offset)
            .expect("next manager helper exists");
        let global = &source[global_start..global_end];
        let global_registry = global
            .find("lock_synthetic_cue_artifact_registry")
            .expect("global cleanup locks registry");
        let global_pending = global
            .find("pending_synthetic_cue_artifacts")
            .expect("global cleanup locks pending set");
        assert!(
            global_registry < global_pending,
            "global cleanup must lock the registry before the pending set"
        );
    }

    #[test]
    fn exact_item_id_rollback_does_not_remove_preexisting_same_path_items() {
        let manager = ConversionManager::new(ConversionConfig::default());
        let (_artifact_dir, artifact) = synthetic_artifact_for("rollback-exact", "same-path");
        let preexisting_id = "preexisting-same-path".to_string();
        let just_admitted_id = "just-admitted-same-path".to_string();
        {
            let mut queue = manager.queue.try_write().expect("queue write lock");
            let mut preexisting = ConversionItem::default();
            preexisting.id = preexisting_id.clone();
            preexisting.input_path = artifact.clone();
            preexisting.status = ConversionStatus::Queued;
            queue.items_mut().push_back(preexisting);

            let mut just_admitted = ConversionItem::default();
            just_admitted.id = just_admitted_id.clone();
            just_admitted.input_path = artifact.clone();
            just_admitted.status = ConversionStatus::Queued;
            queue.items_mut().push_back(just_admitted);
        }

        let rollback = manager
            .discard_items_without_synthetic_artifact_cleanup_for_item_ids_or_defer(
                [just_admitted_id.clone()].into_iter().collect(),
            );
        assert_eq!(rollback.removed, vec![just_admitted_id.clone()]);
        assert!(rollback.deferred.is_empty());
        let queue = manager.queue.try_read().expect("queue read lock");
        let ids = queue
            .all_items()
            .into_iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![preexisting_id.as_str()]);
    }

    #[test]
    fn exact_item_id_rollback_defers_when_queue_write_lock_is_busy() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let manager = ConversionManager::new(ConversionConfig::default());
            let item_id = "deferred-rollback-item".to_string();
            {
                let mut queue = manager.queue.write().await;
                let mut item = ConversionItem::default();
                item.id = item_id.clone();
                item.status = ConversionStatus::Queued;
                queue.items_mut().push_back(item);
            }

            let guard = manager.queue.write().await;
            let rollback = manager
                .discard_items_without_synthetic_artifact_cleanup_for_item_ids_or_defer(
                    [item_id.clone()].into_iter().collect(),
                );
            assert!(rollback.removed.is_empty());
            assert!(rollback.deferred.contains(&item_id));
            drop(guard);

            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    let still_present = {
                        let queue = manager.queue.read().await;
                        queue
                            .all_items()
                            .into_iter()
                            .any(|item| item.id.as_str() == item_id.as_str())
                    };
                    if !still_present {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("deferred exact-id rollback completes after queue lock release");
        });
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
            err.contains("ready queue insertion requires exact PipelineSettings or a prebuilt PipelineRequest"),
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
    use super::bluray_queue_admission_tests::pipeline_request_for;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    pub(super) fn test_manager_with_item() -> (ConversionManager, String) {
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


    pub(super) fn synthetic_artifact_for(item_id: &str, name: &str) -> (PathBuf, PathBuf) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let artifact_dir = std::env::temp_dir()
            .join("tonepoet-synthetic-cue-albums")
            .join(format!("process-{}-{name}-{nanos}", std::process::id()))
            .join(format!("artifact-test-{item_id}"));
        let artifact = artifact_dir.join("album.cue");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(
            &artifact,
            b"FILE \"a.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .expect("artifact file");
        assert!(crate::convert::queue_expansion::is_synthetic_cue_album_artifact(&artifact));
        (artifact_dir, artifact)
    }

    pub(super) fn assert_path_exists(path: &Path, context: &str) {
        assert!(path.exists(), "{context}: {}", path.display());
    }

    fn poison_synthetic_artifact_registry(manager: &ConversionManager) {
        let registry = Arc::clone(&manager.synthetic_cue_artifacts);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = registry.lock().expect("registry lock before poisoning");
            panic!("intentional synthetic artifact registry poison for regression test");
        }));
    }

    fn queued_synthetic_item(manager: &ConversionManager, item_id: &str, artifact: &Path, status: ConversionStatus) {
        let mut item = ConversionItem::default();
        item.id = item_id.to_string();
        item.input_path = artifact.to_path_buf();
        item.status = status;
        manager
            .queue
            .try_write()
            .expect("queue write lock")
            .items_mut()
            .push_back(item);
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
    fn completed_item_status_removes_owned_synthetic_cue_artifact() {
        let (manager, item_id) = test_manager_with_item();
        let (artifact_dir, artifact) = synthetic_artifact_for(&item_id, "completed-cleanup");

        manager.register_synthetic_cue_artifact(&item_id, &artifact).expect("synthetic artifact registration");
        assert!(manager.update_item_status(
            &item_id,
            ConversionStatus::Completed {
                output_path: PathBuf::from("/tmp/out.flac"),
                log_path: None,
            },
            100.0,
        ));

        assert!(
            !artifact_dir.exists(),
            "completed terminal state must remove the owned synthetic CUE artifact directory"
        );
    }

    #[test]
    fn clear_completed_preserves_retryable_failed_synthetic_artifact() {
        let (mut manager, item_id) = test_manager_with_item();
        let (_artifact_dir, artifact) = synthetic_artifact_for(&item_id, "failed-clear-completed");

        manager.register_synthetic_cue_artifact(&item_id, &artifact).expect("synthetic artifact registration");
        assert!(manager.update_item_status(
            &item_id,
            ConversionStatus::Failed {
                error: "synthetic failure".to_string(),
                log_path: None,
            },
            100.0,
        ));

        manager.clear_completed();

        assert_path_exists(
            &artifact,
            "clear_completed must not delete artifacts for retryable failed items",
        );
        let queue = manager.queue.try_read().expect("queue read lock");
        let failed = queue
            .all_items()
            .into_iter()
            .find(|item| item.id == item_id)
            .expect("failed retryable item must remain after clear_completed");
        assert!(
            failed.can_retry(),
            "failed item remaining after clear_completed must still be retryable"
        );
    }

    #[test]
    fn clear_queue_preserves_artifacts_when_queue_write_lock_is_busy() {
        let (mut manager, item_id) = test_manager_with_item();
        let (_artifact_dir, artifact) = synthetic_artifact_for(&item_id, "clear-queue-busy");
        manager.register_synthetic_cue_artifact(&item_id, &artifact).expect("synthetic artifact registration");

        let queue = Arc::clone(&manager.queue);
        let guard = queue.try_write().expect("hold queue write lock");
        manager.clear_queue();
        assert_path_exists(
            &artifact,
            "clear_queue must preserve live artifacts when it cannot mutate the queue",
        );
        drop(guard);

        let queue = manager.queue.try_read().expect("queue read lock");
        assert!(
            queue.all_items().into_iter().any(|item| item.id == item_id),
            "clear_queue must not remove items when the write lock was unavailable"
        );
    }

    #[test]
    fn clear_all_preserves_artifacts_when_queue_write_lock_is_busy() {
        let (mut manager, item_id) = test_manager_with_item();
        let (_artifact_dir, artifact) = synthetic_artifact_for(&item_id, "clear-all-busy");
        manager.register_synthetic_cue_artifact(&item_id, &artifact).expect("synthetic artifact registration");

        let queue = Arc::clone(&manager.queue);
        let guard = queue.try_write().expect("hold queue write lock");
        manager.clear_all();
        assert_path_exists(
            &artifact,
            "clear_all must preserve live artifacts when it cannot mutate the queue",
        );
        drop(guard);

        let queue = manager.queue.try_read().expect("queue read lock");
        assert!(
            queue.all_items().into_iter().any(|item| item.id == item_id),
            "clear_all must not remove items when the write lock was unavailable"
        );
    }

    #[test]
    fn failed_synthetic_cue_artifact_survives_retry_and_is_cleaned_on_remove() {
        let (mut manager, item_id) = test_manager_with_item();
        let (artifact_dir, artifact) = synthetic_artifact_for(&item_id, "failed-retry-remove");
        {
            let mut queue = manager.queue.try_write().expect("queue write lock");
            queue.find_item_mut(&item_id).expect("queued item").input_path = artifact.clone();
        }

        manager.register_synthetic_cue_artifact(&item_id, &artifact).expect("synthetic artifact registration");
        assert!(manager.update_item_status(
            &item_id,
            ConversionStatus::Failed {
                error: "synthetic failure".to_string(),
                log_path: None,
            },
            100.0,
        ));
        assert_path_exists(&artifact, "failed retryable item must retain synthetic artifact");

        {
            let mut queue = manager.queue.try_write().expect("queue write lock");
            let failed = queue.find_item_mut(&item_id).expect("failed item exists");
            failed.selected = true;
            queue.settle_finished();
            queue.retry_failed();
            for item in queue.all_items_mut() {
                if item.id == item_id {
                    item.selected = true;
                }
            }
        }

        {
            let queue = manager.queue.try_read().expect("queue read lock");
            let matching = queue
                .all_items()
                .into_iter()
                .filter(|item| item.id == item_id)
                .collect::<Vec<_>>();
            assert_eq!(
                matching.len(),
                1,
                "retry must supersede the failed history entry instead of leaving duplicate lifecycle records"
            );
            let retry = matching
                .into_iter()
                .find(|item| item.status == ConversionStatus::Queued)
                .expect("retry requeues the failed synthetic item");
            assert_path_exists(
                &retry.input_path,
                "retry must point at an existing synthetic album.cue",
            );
        }

        let removed = manager.remove_selected();
        assert!(removed >= 1, "selected retry/failed entries must be removable");
        assert!(
            !artifact_dir.exists(),
            "explicit removal must clean the owned synthetic CUE artifact directory"
        );
    }

    #[test]
    fn remove_selected_preserves_artifact_of_processing_item_until_worker_terminal() {
        let (mut manager, item_id) = test_manager_with_item();
        let (artifact_dir, artifact) = synthetic_artifact_for(&item_id, "remove-processing-preserve");
        manager.register_synthetic_cue_artifact(&item_id, &artifact).expect("registration");
        {
            let mut queue = manager.queue.try_write().expect("queue write lock");
            let item = queue.find_item_mut(&item_id).expect("item");
            item.selected = true;
            // status is already Processing from the fixture
        }

        let removed = manager.remove_selected();
        assert_eq!(removed, 1);
        assert_path_exists(
            &artifact,
            "removing an in-flight item must not delete the input its worker is reading",
        );

        // The worker's terminal status closes the loop: item is absent from the
        // queue, so the deferred branch cleans the artifact.
        let updated = manager.update_item_status(
            &item_id,
            ConversionStatus::Completed {
                output_path: PathBuf::from("/tmp/out.flac"),
                log_path: None,
            },
            100.0,
        );
        assert!(!updated, "removed item is no longer in the queue");
        assert!(
            !artifact_dir.exists(),
            "worker-terminal status must clean the deferred artifact"
        );
    }

    #[test]
    fn clear_all_preserves_processing_artifacts_and_cleans_the_rest() {
        let (mut manager, item_id) = test_manager_with_item();
        let (processing_dir, processing_artifact) =
            synthetic_artifact_for(&item_id, "clear-processing-preserve");
        manager
            .register_synthetic_cue_artifact(&item_id, &processing_artifact)
            .expect("registration");

        let (queued_dir, queued_artifact) = synthetic_artifact_for("queued-item", "clear-queued-clean");
        queued_synthetic_item(&manager, "queued-item", &queued_artifact, ConversionStatus::Queued);
        manager
            .register_synthetic_cue_artifact("queued-item", &queued_artifact)
            .expect("registration");

        manager.clear_all();
        assert_path_exists(
            &processing_artifact,
            "clear must not delete the in-flight worker's input",
        );
        assert!(
            !queued_dir.exists(),
            "clear must clean artifacts of non-processing items"
        );
        let _ = std::fs::remove_dir_all(processing_dir);
    }

    #[test]
    fn completed_cleanup_lost_to_queue_contention_recovers_on_read_verified_retry() {
        let (manager, item_id) = test_manager_with_item();
        let (artifact_dir, artifact) = synthetic_artifact_for(&item_id, "contention-recover");
        manager.register_synthetic_cue_artifact(&item_id, &artifact).expect("registration");
        {
            let mut queue = manager.queue.try_write().expect("queue write lock");
            let item = queue.find_item_mut(&item_id).expect("item");
            item.status = ConversionStatus::Completed {
                output_path: PathBuf::from("/tmp/out.flac"),
                log_path: None,
            };
        }

        // Simulate the reducer's completion message arriving while another
        // writer holds the queue lock: updated == false, but the read-verified
        // deferred branch must still clean the completed item's artifact.
        let guard = manager.queue.try_write().expect("hold write lock");
        let updated_under_contention = {
            // update_item_status takes &self; the held guard forces try_write to fail
            manager.update_item_status(
                &item_id,
                ConversionStatus::Completed {
                    output_path: PathBuf::from("/tmp/out.flac"),
                    log_path: None,
                },
                100.0,
            )
        };
        assert!(!updated_under_contention);
        assert_path_exists(&artifact, "read lock also contended: artifact preserved");
        drop(guard);

        let updated = manager.update_item_status(
            &item_id,
            ConversionStatus::Completed {
                output_path: PathBuf::from("/tmp/out.flac"),
                log_path: None,
            },
            100.0,
        );
        let _ = updated;
        assert!(
            !artifact_dir.exists(),
            "uncontended terminal delivery must clean the completed item's artifact"
        );
    }

    #[test]
    fn remove_selected_completed_item_cleans_owned_synthetic_cue_artifact() {
        let (mut manager, item_id) = test_manager_with_item();
        let (artifact_dir, artifact) = synthetic_artifact_for(&item_id, "completed-remove");

        manager.register_synthetic_cue_artifact(&item_id, &artifact).expect("synthetic artifact registration");
        assert!(manager.update_item_status(
            &item_id,
            ConversionStatus::Failed {
                error: "synthetic failure".to_string(),
                log_path: None,
            },
            100.0,
        ));
        assert_path_exists(&artifact, "failed item must retain synthetic artifact before removal");

        {
            let mut queue = manager.queue.try_write().expect("queue write lock");
            let failed = queue.find_item_mut(&item_id).expect("failed item exists");
            failed.selected = true;
            queue.settle_finished();
        }

        let removed = manager.remove_selected();
        assert_eq!(removed, 1, "selected completed/failed item must be removed");
        assert!(
            !artifact_dir.exists(),
            "removing a completed/failed item must clean its owned synthetic artifact"
        );
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
    fn commit_batch_completed_skip_cleans_artifact_without_reporting_manager_transfer() {
        let manager = ConversionManager::new(ConversionConfig::default());
        let (_artifact_dir, artifact) = synthetic_artifact_for("completed-skip", "completed-skip");
        queued_synthetic_item(
            &manager,
            "completed-skip-existing",
            &artifact,
            ConversionStatus::Completed {
                output_path: PathBuf::from("/tmp/out.flac"),
                log_path: None,
            },
        );

        let source_artifacts = [artifact.clone()].into_iter().collect::<HashSet<_>>();
        let transaction = manager.commit_batch_with_cue_artifacts(
            &[artifact.clone()],
            &HashSet::new(),
            &source_artifacts,
            &ConversionOptions::default(),
            |_| panic!("skipped completed item must not be configured as a new admission"),
        );

        assert_eq!(transaction.outcome.enqueued, 0);
        assert_eq!(transaction.outcome.skipped, 1);
        assert_eq!(transaction.outcome.previously_converted, 1);
        assert!(transaction.artifacts_transferred_to_manager.is_empty());
        assert!(transaction.artifacts_remaining_caller_owned.is_empty());
        assert!(transaction.artifacts_cleaned_after_completed_skip.contains(&artifact));
        assert!(
            !artifact.exists(),
            "completed duplicate synthetic artifact should be deleted, not reported as manager-owned"
        );
    }

    #[test]
    fn commit_batch_zero_new_item_registry_failure_has_complete_empty_rollback_and_real_error() {
        let manager = ConversionManager::new(ConversionConfig::default());
        let (_artifact_dir, artifact) = synthetic_artifact_for("existing-registry-fail", "existing-fail");
        queued_synthetic_item(
            &manager,
            "existing-registry-fail-item",
            &artifact,
            ConversionStatus::Queued,
        );
        poison_synthetic_artifact_registry(&manager);

        let source_artifacts = [artifact.clone()].into_iter().collect::<HashSet<_>>();
        let transaction = manager.commit_batch_with_cue_artifacts(
            &[artifact.clone()],
            &HashSet::new(),
            &source_artifacts,
            &ConversionOptions::default(),
            |_| panic!("existing queue item should not be configured as a new admission"),
        );

        assert_eq!(transaction.outcome.enqueued, 0);
        assert_eq!(transaction.outcome.errors, 1);
        assert!(
            transaction
                .outcome
                .last_error
                .as_deref()
                .unwrap_or_default()
                .contains("synthetic CUE artifact ownership registration failed"),
            "the user-facing error should describe the ownership failure, not rollback bookkeeping"
        );
        let rollback = transaction
            .rollback
            .as_ref()
            .expect("ownership failure should return rollback metadata even when no new item was admitted");
        assert!(rollback.attempted_item_ids.is_empty());
        assert!(rollback.removed_item_ids.is_empty());
        assert!(rollback.failed_item_ids.is_empty());
        assert!(rollback.completed, "empty rollback is complete because no queue mutation needed rollback");
        assert!(transaction.artifacts_remaining_caller_owned.contains(&artifact));
        assert_path_exists(&artifact, "ownership failure must preserve caller-owned synthetic artifact");
    }

    #[test]
    fn commit_batch_registry_failure_releases_transferred_artifacts_for_retry() {
        let manager = ConversionManager::new(ConversionConfig::default());
        let (_first_dir, first_artifact) = synthetic_artifact_for("commit-first", "commit-first");
        let (_second_dir, second_artifact) = synthetic_artifact_for("commit-second", "commit-second");
        let source_artifacts = [first_artifact.clone(), second_artifact.clone()]
            .into_iter()
            .collect::<HashSet<_>>();
        let registry = Arc::clone(&manager.synthetic_cue_artifacts);

        let transaction = manager.commit_batch_with_cue_artifacts(
            &[first_artifact.clone(), second_artifact.clone()],
            &HashSet::new(),
            &source_artifacts,
            &ConversionOptions::default(),
            |item| {
                item.pipeline_request = Some(pipeline_request_for(&item.input_path, &item.id));
                if same_path_for_queue(&item.input_path, &second_artifact) {
                    let registry = Arc::clone(&registry);
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _guard = registry.lock().expect("registry lock before poisoning");
                        panic!("intentional poison before second synthetic registration");
                    }));
                }
            },
        );

        assert_eq!(transaction.outcome.enqueued, 0);
        assert_eq!(transaction.outcome.errors, 1);
        let rollback = transaction.rollback.as_ref().expect("rollback result");
        assert_eq!(rollback.attempted_item_ids.len(), 2);
        assert_eq!(rollback.removed_item_ids.len(), 2);
        assert!(rollback.failed_item_ids.is_empty());
        assert!(rollback.completed);
        assert!(transaction.artifacts_transferred_to_manager.is_empty());
        assert!(transaction.artifacts_remaining_caller_owned.contains(&first_artifact));
        assert!(transaction.artifacts_remaining_caller_owned.contains(&second_artifact));
        assert_path_exists(
            &first_artifact,
            "artifact successfully transferred earlier in the failed transaction must be preserved for retry"
        );
        assert_path_exists(
            &second_artifact,
            "artifact whose registration failed remains caller-owned for explicit cleanup/retention"
        );
        let queue = manager.queue.try_read().expect("queue read lock");
        assert!(queue.all_items().is_empty(), "failed transaction must not leave partial queue side effects");
    }

    #[test]
    fn commit_batch_behaviorally_configures_item_before_it_is_runnable() {
        let manager = ConversionManager::new(ConversionConfig::default());
        let (_artifact_dir, artifact) = synthetic_artifact_for("configured-before-runnable", "configured");
        let source_artifacts = [artifact.clone()].into_iter().collect::<HashSet<_>>();

        let transaction = manager.commit_batch_with_cue_artifacts(
            &[artifact.clone()],
            &HashSet::new(),
            &source_artifacts,
            &ConversionOptions::default(),
            |item| {
                assert!(
                    !matches!(item.status, ConversionStatus::Queued),
                    "configuration closure must run before the item is marked runnable"
                );
                let request = pipeline_request_for(&item.input_path, &item.id);
                item.pipeline_request = Some(request);
                item.archive_password = Some("configured-before-queue".to_string());
            },
        );

        assert_eq!(transaction.outcome.enqueued, 1);
        let admitted_id = transaction
            .admitted_item_ids
            .first()
            .expect("transaction should report exact admitted id")
            .clone();
        let queue = manager.queue.try_read().expect("queue read lock");
        let item = queue
            .all_items()
            .into_iter()
            .find(|item| item.id == admitted_id)
            .expect("admitted item is present");
        assert_eq!(item.archive_password.as_deref(), Some("configured-before-queue"));
        let request = item
            .pipeline_request
            .as_ref()
            .expect("transaction should publish a fully configured PipelineRequest");
        assert_eq!(request.item_id, admitted_id);
        assert_eq!(request.container, artifact);
        assert!(matches!(item.status, ConversionStatus::Queued));
        assert!(transaction.artifacts_transferred_to_manager.contains(&artifact));
        assert!(transaction.artifacts_remaining_caller_owned.is_empty());
    }

    #[test]
    fn commit_batch_rejects_unconfigured_runnable_item_without_queue_mutation() {
        let manager = ConversionManager::new(ConversionConfig::default());
        let (_artifact_dir, artifact) = synthetic_artifact_for("unconfigured-transaction", "unconfigured");
        let source_artifacts = [artifact.clone()].into_iter().collect::<HashSet<_>>();

        let transaction = manager.commit_batch_with_cue_artifacts(
            &[artifact.clone()],
            &HashSet::new(),
            &source_artifacts,
            &ConversionOptions::default(),
            |item| {
                assert!(
                    !matches!(item.status, ConversionStatus::Queued),
                    "configuration closure still runs before the item is runnable"
                );
                item.archive_password = Some("not-a-pipeline-handoff".to_string());
            },
        );

        assert_eq!(transaction.outcome.enqueued, 0);
        assert_eq!(transaction.outcome.errors, 1);
        assert!(
            transaction
                .outcome
                .last_error
                .as_deref()
                .unwrap_or_default()
                .contains("queued items require full PipelineSettings or a prebuilt PipelineRequest")
        );
        assert!(transaction.admitted_item_ids.is_empty());
        assert!(transaction.artifacts_transferred_to_manager.is_empty());
        assert!(transaction.artifacts_remaining_caller_owned.contains(&artifact));
        let rollback = transaction.rollback.as_ref().expect("configuration failure should report rollback state");
        assert!(rollback.attempted_item_ids.is_empty());
        assert!(rollback.completed);
        let queue = manager.queue.try_read().expect("queue read lock");
        assert!(queue.all_items().is_empty(), "invalid runnable item must not be inserted");
        assert_path_exists(&artifact, "unconfigured transaction preserves caller-owned artifact");
    }

    #[test]
    fn commit_batch_returns_busy_without_blocking_or_mutating_queue() {
        let manager = ConversionManager::new(ConversionConfig::default());
        let (_artifact_dir, artifact) = synthetic_artifact_for("busy-transaction", "busy");
        let source_artifacts = [artifact.clone()].into_iter().collect::<HashSet<_>>();
        let guard = manager.queue.try_write().expect("hold queue write lock");

        let transaction = manager.commit_batch_with_cue_artifacts(
            &[artifact.clone()],
            &HashSet::new(),
            &source_artifacts,
            &ConversionOptions::default(),
            |_| panic!("busy queue must not invoke item configuration"),
        );
        drop(guard);

        assert_eq!(transaction.outcome.enqueued, 0);
        assert_eq!(transaction.outcome.errors, 1);
        assert!(transaction.admitted_item_ids.is_empty());
        assert!(transaction.rollback.is_none());
        assert!(transaction.artifacts_remaining_caller_owned.contains(&artifact));
        assert_path_exists(&artifact, "busy queue transaction must preserve caller-owned artifact");
        let queue = manager.queue.try_read().expect("queue read lock");
        assert!(queue.all_items().is_empty(), "busy transaction must not mutate the queue");
    }

    #[test]
    fn commit_batch_configures_item_before_marking_runnable_or_publishing() {
        let source = include_str!("mod.rs");
        let start = source
            .find("pub fn commit_batch_with_cue_artifacts")
            .expect("commit transaction helper should exist");
        let end = source[start..]
            .find("/// Scan a directory for queueable conversion inputs.")
            .map(|offset| start + offset)
            .expect("commit transaction helper should precede directory scan");
        let body = &source[start..end];

        let configure = body
            .find("configure_admitted_item(&mut item);")
            .expect("admitted item must be configured inside the transaction");
        let validate = body
            .find("conversion_item_has_full_pipeline_handoff(&item)")
            .expect("transaction must validate the complete pipeline handoff before publishing");
        let mark_queued = body
            .find("item.status = ConversionStatus::Queued;")
            .expect("admitted item should be marked queued only after configuration");
        let publish = body
            .find("queue.items_mut().push_back(item);")
            .expect("admitted item should be published to the queue");

        assert!(
            configure < validate && validate < mark_queued && mark_queued < publish,
            "PipelineRequest/source projection and handoff validation must complete before an item is runnable and visible to workers"
        );
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
