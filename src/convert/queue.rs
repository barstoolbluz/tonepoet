//! Conversion queue management

use super::formats::{AudioFormat, ConversionOptions, FileFormat};
use super::pipeline::{ArchiveTrackMetadataOverride, CueSidecarPolicy, PipelineRequest};
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
    /// Audio publication completed, but one or more post-conversion actions
    /// failed. Published audio is retained; the action failures are terminal,
    /// visible warnings rather than conversion-blocking failures.
    CompletedWithActionErrors {
        output_path: PathBuf,
        /// Durable per-album run log, when one was written.
        #[serde(default)]
        log_path: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        errors: Vec<String>,
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

/// Per-track progress state for multi-track sources (SACD, CUE, 7z).
#[derive(Debug, Clone)]
pub struct TrackProgress {
    pub track_label: String,
    pub step_description: String,
    pub progress_fraction: f32,
    pub epoch: u64,
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
    /// Archive password (for 7z files).
    ///
    /// This is process-local execution state. Legacy queue files may still
    /// deserialize this field so they can be migrated, but current serializers
    /// must never emit it.
    #[serde(default, skip_serializing)]
    pub archive_password: Option<String>,
    /// Opaque OS-secret-store reference used to rehydrate an archive password
    /// when a persisted queue is resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_password_ref: Option<String>,
    /// Persisted fact that this item was configured with an archive password.
    /// This remains true even when the secret itself was process-only, so a
    /// resumed item cannot silently attempt extraction without credentials.
    #[serde(default)]
    pub archive_password_required: bool,
    /// Exact Chunk 1 planner settings selected by the UI/CLI.
    ///
    /// Kept on the queue item so persisted queued work can recover without
    /// projecting through the legacy option surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_settings: Option<tonepoet_pipeline::PipelineSettings>,
    /// Queue-time override for CUE sidecar detection. Used when browse queue
    /// expansion has already evaluated and suppressed a sibling CUE as a
    /// metadata artifact, so downstream source detection must not discover it
    /// again as a split-source sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cue_sidecar_override: Option<CueSidecarPolicy>,
    /// Pre-extracted archive preview staging directory transferred from the
    /// Convert source at commit time. The archive materializer reuses this when
    /// it still exists and falls back to extraction when it does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_extracted_staging: Option<PathBuf>,
    /// Compact archive-preview metadata edits transferred at commit time and
    /// mirrored onto any attached pipeline request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archive_metadata_overrides: Vec<ArchiveTrackMetadataOverride>,
    /// New pipeline request (populated during migration; legacy fields
    /// remain until PR 10 finishes CLI/TUI surface).
    #[serde(default)]
    pub pipeline_request: Option<PipelineRequest>,
    /// Per-track progress for multi-track sources. Keyed by track_index.
    /// Transient display state only — not serialized.
    #[serde(skip)]
    pub active_tracks: BTreeMap<u32, TrackProgress>,
    /// Highest epoch cleared per track_index. Transient display guard state.
    #[serde(skip)]
    pub closed_track_epochs: BTreeMap<u32, u64>,
    /// Whether per-track sub-lines are collapsed in the queue display.
    #[serde(skip)]
    pub tracks_collapsed: bool,
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
            archive_password_ref: None,
            archive_password_required: false,
            pipeline_settings: None,
            cue_sidecar_override: None,
            pre_extracted_staging: None,
            archive_metadata_overrides: Vec::new(),
            pipeline_request: None,
            active_tracks: BTreeMap::new(),
            closed_track_epochs: BTreeMap::new(),
            tracks_collapsed: false,
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
            archive_password,
            archive_password_ref: None,
            archive_password_required: false,
            pipeline_settings,
            cue_sidecar_override: None,
            pre_extracted_staging: None,
            archive_metadata_overrides: Vec::new(),
            pipeline_request: None,
            active_tracks: BTreeMap::new(),
            closed_track_epochs: BTreeMap::new(),
            tracks_collapsed: false,
        }
    }

    /// Create a new conversion item with a queue-time CUE sidecar policy override.
    pub fn new_with_cue_sidecar_override(
        input_path: PathBuf,
        input_format: FileFormat,
        options: ConversionOptions,
        cue_sidecar_override: Option<CueSidecarPolicy>,
    ) -> Self {
        let mut item = Self::new(input_path, input_format, options);
        item.cue_sidecar_override = cue_sidecar_override;
        item
    }

    /// Create a new conversion item with a prebuilt orchestrator request attached.
    ///
    /// This is the lossless handoff path for callers that have already built a
    /// complete `PipelineRequest`. It intentionally does not require or copy
    /// `PipelineSettings` onto the legacy queue fields; the request is the exact
    /// executable contract for this item. Callers must validate that the request
    /// item id is non-empty and unique before insertion into a queue.
    pub fn new_with_pipeline_request(
        input_path: PathBuf,
        input_format: FileFormat,
        mut options: ConversionOptions,
        request: PipelineRequest,
        cue_sidecar_override: Option<CueSidecarPolicy>,
    ) -> Self {
        debug_assert!(
            !request.item_id.trim().is_empty(),
            "PipelineRequest item_id must be non-empty before queue insertion"
        );
        options.pipeline_settings = None;
        let mut item = Self::new_with_cue_sidecar_override(
            input_path,
            input_format,
            options,
            cue_sidecar_override,
        );
        item.id = request.item_id.clone();
        item.pipeline_settings = None;
        item.options.pipeline_settings = None;
        item.pre_extracted_staging = request.pre_extracted_staging.clone();
        item.archive_metadata_overrides = request.archive_metadata_overrides.clone();
        item.pipeline_request = Some(request);
        item
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

    /// Create a new conversion item with exact Chunk 1 planner settings and
    /// a queue-time CUE sidecar policy override attached before insertion.
    pub fn new_with_pipeline_settings_and_cue_sidecar_override(
        input_path: PathBuf,
        input_format: FileFormat,
        mut options: ConversionOptions,
        settings: tonepoet_pipeline::PipelineSettings,
        cue_sidecar_override: Option<CueSidecarPolicy>,
    ) -> Self {
        options.pipeline_settings = Some(settings.clone());
        let mut item = Self::new_with_cue_sidecar_override(
            input_path,
            input_format,
            options,
            cue_sidecar_override,
        );
        item.pipeline_settings = Some(settings);
        item
    }

    /// Attach exact Chunk 1 planner settings to an existing queue item.
    pub fn set_pipeline_settings(&mut self, settings: tonepoet_pipeline::PipelineSettings) {
        self.options.pipeline_settings = Some(settings.clone());
        self.pipeline_settings = Some(settings);
        self.pipeline_request = None;
    }

    /// Attach a process-local archive password and, when supplied, the opaque
    /// reference that is safe to persist. The same secret is mirrored into an
    /// attached pipeline request because that request is the executable
    /// contract for ready queue items.
    pub fn set_archive_password(
        &mut self,
        password: Option<String>,
        reference: Option<String>,
    ) {
        self.archive_password_required = password.is_some() || reference.is_some();
        self.archive_password = password.clone();
        self.archive_password_ref = reference;
        if let Some(request) = self.pipeline_request.as_mut() {
            request.source.archive_password = password.map(super::pipeline::SecretString::new);
        }
    }

    /// Rehydrate or migrate archive-password state after queue deserialization.
    ///
    /// Legacy cleartext values are accepted only as migration input and are
    /// immediately moved into the OS secret store under an opaque account
    /// derived from the durable queue-item id. Interrupted retries therefore
    /// overwrite the same account instead of accumulating random orphan entries.
    /// An unavailable reference or backend fails closed: the password is cleared
    /// and the item becomes retryable with a user-visible password error.
    pub fn restore_archive_password_after_load(&mut self) -> Result<bool, String> {
        let nested_legacy = self
            .pipeline_request
            .as_ref()
            .and_then(|request| request.source.archive_password.as_ref())
            .map(|secret| secret.expose().to_string());
        if let (Some(top_level), Some(nested)) =
            (self.archive_password.as_deref(), nested_legacy.as_deref())
        {
            if top_level != nested {
                let message = format!(
                    "Archive password state is ambiguous for resumed queue item '{}': legacy queue fields disagree. Set the archive password again, then retry.",
                    self.input_path.display()
                );
                self.fail_closed_for_unavailable_archive_password(message.clone());
                return Err(message);
            }
        }
        let legacy_password = self.archive_password.clone().or(nested_legacy);

        let (password, migrated) = if let Some(reference) = self.archive_password_ref.as_deref() {
            match crate::secret_store::get(reference) {
                Ok(password) => {
                    if legacy_password
                        .as_deref()
                        .is_some_and(|legacy| legacy != password)
                    {
                        let message = format!(
                            "Archive password state is ambiguous for resumed queue item '{}': the persisted reference and legacy cleartext disagree. Set the archive password again, then retry.",
                            self.input_path.display()
                        );
                        self.fail_closed_for_unavailable_archive_password(message.clone());
                        return Err(message);
                    }
                    (Some(password), false)
                }
                Err(error) => {
                    self.fail_closed_for_unavailable_archive_password(format!(
                        "Archive password unavailable for resumed queue item '{}': {error}. Set the archive password again, then retry.",
                        self.input_path.display()
                    ));
                    return Err(error.to_string());
                }
            }
        } else if let Some(password) = legacy_password {
            let reference = match crate::secret_store::stable_reference("queue-item", &self.id) {
                Ok(reference) => reference,
                Err(error) => {
                    self.fail_closed_for_unavailable_archive_password(format!(
                        "Archive password migration failed for resumed queue item '{}': {error}. Set the archive password again, then retry.",
                        self.input_path.display()
                    ));
                    return Err(error.to_string());
                }
            };
            match crate::secret_store::set(&reference, &password) {
                Ok(()) => {
                    self.archive_password_ref = Some(reference);
                    (Some(password), true)
                }
                Err(error) => {
                    self.fail_closed_for_unavailable_archive_password(format!(
                        "Archive password migration failed for resumed queue item '{}': {error}. Set the archive password again, then retry.",
                        self.input_path.display()
                    ));
                    return Err(error.to_string());
                }
            }
        } else if self.archive_password_required {
            let message = format!(
                "Archive password unavailable for resumed queue item '{}': the password was process-only and was not persisted. Set the archive password again, then retry.",
                self.input_path.display()
            );
            self.fail_closed_for_unavailable_archive_password(message.clone());
            return Err(message);
        } else {
            (None, false)
        };

        self.archive_password_required = password.is_some() || self.archive_password_ref.is_some();
        self.archive_password = password.clone();
        if let Some(request) = self.pipeline_request.as_mut() {
            request.source.archive_password = password.map(super::pipeline::SecretString::new);
        }
        Ok(migrated)
    }

    fn fail_closed_for_unavailable_archive_password(&mut self, message: String) {
        self.archive_password = None;
        if let Some(request) = self.pipeline_request.as_mut() {
            request.source.archive_password = None;
        }
        self.status = ConversionStatus::Failed {
            error: message,
            log_path: None,
        };
    }

    /// Check if the item is in a terminal state
    pub fn is_finished(&self) -> bool {
        matches!(
            self.status,
            ConversionStatus::Completed { .. }
                | ConversionStatus::CompletedWithActionErrors { .. }
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

/// Rehydrate persisted queue secrets and migrate legacy cleartext fields.
/// Returns true when at least one item gained a new reference and the
/// persistence store should be rewritten immediately.
pub(crate) fn restore_archive_passwords_after_load(items: &mut [ConversionItem]) -> bool {
    let mut migrated = false;
    for item in items {
        match item.restore_archive_password_after_load() {
            Ok(item_migrated) => migrated |= item_migrated,
            Err(error) => {
                // The item was changed to a fail-closed retry state and any
                // legacy in-memory secret was cleared. Rewrite the store even
                // when migration itself failed, or the cleartext legacy field
                // would remain on disk indefinitely.
                migrated = true;
                log::warn!(
                    "queue item '{}' could not restore its archive password: {}",
                    item.input_path.display(),
                    error
                );
            }
        }
    }
    migrated
}

fn _status_progress(status: &ConversionStatus) -> f32 {
    match status {
        ConversionStatus::Processing { progress, .. } => *progress,
        ConversionStatus::Completed { .. }
        | ConversionStatus::CompletedWithActionErrors { .. }
        | ConversionStatus::Partial { .. } => 100.0,
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

    /// Add an item to the queue for later configuration.
    ///
    /// This path intentionally preserves legacy `NotConfigured` insertion
    /// semantics: callers may add files/directories before the UI or CLI has
    /// collected full `PipelineSettings`. The full-settings invariant applies
    /// only when an item is actually ready to run with `ConversionStatus::Queued`.
    pub fn add_item(&mut self, path: PathBuf, format: FileFormat, options: ConversionOptions) {
        self.add_item_with_cue_sidecar_override(path, format, options, None);
    }

    /// Add an item to the queue with a CUE sidecar policy override attached
    /// before the item enters the queue.
    pub fn add_item_with_cue_sidecar_override(
        &mut self,
        path: PathBuf,
        format: FileFormat,
        options: ConversionOptions,
        cue_sidecar_override: Option<CueSidecarPolicy>,
    ) {
        let item = ConversionItem::new_with_cue_sidecar_override(
            path,
            format,
            options,
            cue_sidecar_override,
        );
        debug_assert!(
            item.status != ConversionStatus::Queued
                || item.pipeline_settings.is_some()
                || item.pipeline_request.is_some()
                || item.options.pipeline_settings.is_some(),
            "queued ConversionItem must contain full PipelineSettings or a prebuilt PipelineRequest"
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

    /// Add an item to the queue with exact Chunk 1 planner settings and a CUE
    /// sidecar policy override attached before insertion.
    pub fn add_item_with_pipeline_settings_and_cue_sidecar_override(
        &mut self,
        path: PathBuf,
        format: FileFormat,
        options: ConversionOptions,
        settings: tonepoet_pipeline::PipelineSettings,
        cue_sidecar_override: Option<CueSidecarPolicy>,
    ) {
        let item = ConversionItem::new_with_pipeline_settings_and_cue_sidecar_override(
            path,
            format,
            options,
            settings,
            cue_sidecar_override,
        );
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
            current.status = status;

            match &current.status {
                ConversionStatus::Processing { .. } => {
                    if current.started_at.is_none() {
                        current.started_at = Some(Utc::now());
                    }
                }
                ConversionStatus::Completed { .. }
                | ConversionStatus::CompletedWithActionErrors { .. }
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
            .filter(|item| {
                matches!(
                    item.status,
                    ConversionStatus::Completed { .. }
                        | ConversionStatus::CompletedWithActionErrors { .. }
                )
            })
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

    /// Clear completed-success items only and return the IDs actually removed.
    ///
    /// Artifact cleanup must be driven by this returned set, not by a
    /// pre-removal snapshot of broader terminal states. Failed, partial, and
    /// cancelled items are intentionally retryable and must keep any owned
    /// synthetic CUE artifacts until they are explicitly removed or cleared by
    /// a terminal-state operation that actually consumes them.
    pub fn clear_completed(&mut self) -> Vec<String> {
        let is_completed = |item: &ConversionItem| {
            matches!(
                item.status,
                ConversionStatus::Completed { .. }
                    | ConversionStatus::CompletedWithActionErrors { .. }
            )
        };
        self.remove_matching_items(is_completed)
    }

    /// Clear all terminal items (Completed, Failed, Partial, Cancelled)
    /// Move items in terminal states from the active queue to the completed
    /// list. The shared pipeline scheduler updates item status in-place via
    /// `find_item_mut` but does not call `next_item`, so finished items stay
    /// in `self.items`. This method settles them so `completed_items()`,
    /// `failed_items()`, etc. report correctly.
    pub fn settle_finished(&mut self) {
        let mut i = 0;
        while i < self.items.len() {
            if self.items[i].is_finished() {
                let item = self.items.remove(i).unwrap();
                self.completed.push(item);
            } else {
                i += 1;
            }
        }
        if let Some(ref current) = self.current {
            if current.is_finished() {
                self.completed.push(self.current.take().unwrap());
            }
        }
    }

    pub fn clear_finished(&mut self) -> Vec<String> {
        let is_terminal = |item: &ConversionItem| {
            matches!(
                item.status,
                ConversionStatus::Completed { .. }
                    | ConversionStatus::CompletedWithActionErrors { .. }
                    | ConversionStatus::Partial { .. }
                    | ConversionStatus::Failed { .. }
                    | ConversionStatus::Cancelled
            )
        };
        self.remove_matching_items(is_terminal)
    }

    /// Clear all items from the queue and return the IDs actually removed.
    pub fn clear(&mut self) -> Vec<String> {
        let mut removed = Vec::with_capacity(self.total_items());
        removed.extend(self.items.iter().map(|item| item.id.clone()));
        if let Some(current) = self.current.take() {
            removed.push(current.id);
        }
        removed.extend(self.completed.iter().map(|item| item.id.clone()));
        self.items.clear();
        self.completed.clear();
        removed
    }

    pub fn remove_item_by_id(&mut self, item_id: &str) -> bool {
        !self.remove_matching_items(|item| item.id.as_str() == item_id).is_empty()
    }

    pub(crate) fn remove_matching_item_ids<F>(&mut self, should_remove: F) -> Vec<String>
    where
        F: FnMut(&ConversionItem) -> bool,
    {
        self.remove_matching_items(should_remove)
    }

    fn remove_matching_items<F>(&mut self, mut should_remove: F) -> Vec<String>
    where
        F: FnMut(&ConversionItem) -> bool,
    {
        let mut removed = Vec::new();
        self.items.retain(|item| {
            if should_remove(item) {
                removed.push(item.id.clone());
                false
            } else {
                true
            }
        });
        if self
            .current
            .as_ref()
            .map(|item| should_remove(item))
            .unwrap_or(false)
        {
            if let Some(item) = self.current.take() {
                removed.push(item.id);
            }
        }
        self.completed.retain(|item| {
            if should_remove(item) {
                removed.push(item.id.clone());
                false
            } else {
                true
            }
        });
        removed
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
            !(item.selected
                && matches!(
                    item.status,
                    ConversionStatus::Queued | ConversionStatus::Paused
                ))
        });
    }

    /// Retry failed items
    pub fn retry_failed(&mut self) {
        // Failed/cancelled items may still reside in the active deque until a
        // reducer settles them. Normalize first so selected retry semantics do
        // not depend on reducer timing.
        self.settle_finished();
        let mut to_retry = Vec::new();
        let mut retained_completed = Vec::with_capacity(self.completed.len());

        for mut item in self.completed.drain(..) {
            if item.selected && item.can_retry() {
                item.status = ConversionStatus::Queued;
                item.queued_at = Utc::now();
                item.started_at = None;
                item.completed_at = None;
                item.output_path = None;
                item.selected = false;
                to_retry.push(item);
            } else {
                retained_completed.push(item);
            }
        }

        self.completed = retained_completed;
        for item in to_retry {
            self.items.push_back(item);
        }
    }

    /// Retry every retryable failed/cancelled record regardless of selection.
    ///
    /// Bulk counterpart of `retry_failed`: settles finished items first, then
    /// MOVES each retryable record out of completed history back into the
    /// active queue. Flipping statuses in place (the old context-menu path)
    /// stranded Queued rows inside `completed` that the processor never scans
    /// and persistence resurrected on the next session.
    pub fn retry_all_failed(&mut self) -> usize {
        self.settle_finished();
        let mut to_retry = Vec::new();
        let mut retained_completed = Vec::with_capacity(self.completed.len());

        for mut item in self.completed.drain(..) {
            if item.can_retry() {
                item.status = ConversionStatus::Queued;
                item.queued_at = Utc::now();
                item.started_at = None;
                item.completed_at = None;
                item.output_path = None;
                item.selected = false;
                to_retry.push(item);
            } else {
                retained_completed.push(item);
            }
        }

        self.completed = retained_completed;
        let count = to_retry.len();
        for item in to_retry {
            self.items.push_back(item);
        }
        count
    }

    /// Remove selected items and return the removed records' (id, status)
    /// pairs so the manager can make status-aware lifecycle decisions (e.g.
    /// never deleting a synthetic input still being read by an in-flight
    /// worker).
    pub fn remove_selected_records(&mut self) -> Vec<(String, ConversionStatus)> {
        let mut records = Vec::new();
        self.items.retain(|item| {
            if item.selected {
                records.push((item.id.clone(), item.status.clone()));
                false
            } else {
                true
            }
        });
        if self.current.as_ref().map(|item| item.selected).unwrap_or(false) {
            if let Some(item) = self.current.take() {
                records.push((item.id.clone(), item.status.clone()));
            }
        }
        self.completed.retain(|item| {
            if item.selected {
                records.push((item.id.clone(), item.status.clone()));
                false
            } else {
                true
            }
        });
        records
    }

    /// Remove selected items from the queue.
    ///
    /// Selection spans the active queue, the current item, and the completed
    /// collection as rendered by the TUI.  Removing from all three locations is
    /// important for owned temporary inputs such as synthetic album CUE files:
    /// the manager snapshots selected ids across `all_items()` before calling
    /// this method and only cleans lifecycle artifacts for ids that were
    /// actually removed here.
    pub fn remove_selected_item_ids(&mut self) -> Vec<String> {
        self.remove_matching_items(|item| item.selected)
    }

    pub fn remove_selected(&mut self) -> usize {
        self.remove_selected_item_ids().len()
    }
}

impl Default for ConversionQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod cue_sidecar_override_queue_tests {
    use super::*;

    #[test]
    fn conversion_queue_stores_cue_sidecar_override_at_insertion_time() {
        let mut queue = ConversionQueue::new();
        let mut options = ConversionOptions::default();
        options.pipeline_settings = Some(tonepoet_pipeline::PipelineSettings::default());

        queue.add_item_with_cue_sidecar_override(
            PathBuf::from("/tmp/album/01.flac"),
            FileFormat::Audio(AudioFormat::Flac),
            options,
            Some(CueSidecarPolicy::EmbeddedOnly),
        );

        let items = queue.all_items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].cue_sidecar_override, Some(CueSidecarPolicy::EmbeddedOnly));
    }

    #[test]
    fn add_item_allows_not_configured_legacy_options_without_pipeline_settings() {
        let mut queue = ConversionQueue::new();

        queue.add_item(
            PathBuf::from("/tmp/album/01.flac"),
            FileFormat::Audio(AudioFormat::Flac),
            ConversionOptions::default(),
        );

        let items = queue.all_items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, ConversionStatus::NotConfigured);
        assert!(items[0].pipeline_settings.is_none());
        assert!(items[0].options.pipeline_settings.is_none());
        assert!(items[0].pipeline_request.is_none());
    }

    #[test]
    fn same_path_inserted_later_without_artifact_metadata_has_no_stale_override() {
        let path = PathBuf::from("/tmp/album/01.flac");
        let mut options = ConversionOptions::default();
        options.pipeline_settings = Some(tonepoet_pipeline::PipelineSettings::default());

        let first = ConversionItem::new_with_cue_sidecar_override(
            path.clone(),
            FileFormat::Audio(AudioFormat::Flac),
            options.clone(),
            Some(CueSidecarPolicy::EmbeddedOnly),
        );
        assert_eq!(first.cue_sidecar_override, Some(CueSidecarPolicy::EmbeddedOnly));

        let second = ConversionItem::new_with_cue_sidecar_override(
            path,
            FileFormat::Audio(AudioFormat::Flac),
            options,
            None,
        );
        assert_eq!(second.cue_sidecar_override, None);
    }

    #[test]
    fn cue_sidecar_override_round_trips_through_queue_item_serde_when_present() {
        let mut item = ConversionItem::default();
        item.id = "serde-cue-override".to_string();
        item.input_path = PathBuf::from("/tmp/album/01.flac");
        item.cue_sidecar_override = Some(CueSidecarPolicy::EmbeddedOnly);

        let json = serde_json::to_string(&item).expect("serialize item with override");
        assert!(json.contains("cue_sidecar_override"));

        let decoded: ConversionItem = serde_json::from_str(&json).expect("deserialize item");
        assert_eq!(decoded.cue_sidecar_override, Some(CueSidecarPolicy::EmbeddedOnly));
    }

    #[test]
    fn conversion_actions_round_trip_through_queue_item_serde() {
        use crate::convert::pipeline::{
            ActionPipeline, ConversionAction, CreateFolderAction, RunScriptAction,
        };

        let mut item = ConversionItem::default();
        item.id = "serde-actions".to_string();
        item.input_path = PathBuf::from("/tmp/album/01.flac");
        item.options.actions = ActionPipeline {
            pre: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("ready"),
                continue_on_error: false,
            })],
            post: vec![ConversionAction::Runscript(RunScriptAction {
                script: PathBuf::from("/usr/bin/true"),
                args: vec!["literal argument".to_string()],
                timeout_seconds: 30,
                continue_on_error: false,
            })],
        };

        let json = serde_json::to_string(&item).expect("serialize queue item with actions");
        let decoded: ConversionItem = serde_json::from_str(&json).expect("deserialize queue item");
        assert_eq!(decoded.options.actions, item.options.actions);
    }

    #[test]
    fn legacy_queue_item_without_actions_deserializes_with_empty_pipeline() {
        let mut item = ConversionItem::default();
        item.id = "serde-legacy-no-actions".to_string();
        item.input_path = PathBuf::from("/tmp/album/01.flac");

        let mut value = serde_json::to_value(&item).expect("serialize baseline item");
        value
            .get_mut("options")
            .and_then(serde_json::Value::as_object_mut)
            .expect("queue options serialize as object")
            .remove("actions");

        let decoded: ConversionItem = serde_json::from_value(value)
            .expect("deserialize legacy queue item without actions");
        assert!(decoded.options.actions.is_empty());
    }

    #[test]
    fn legacy_queue_item_without_cue_sidecar_override_deserializes_as_none() {
        let mut item = ConversionItem::default();
        item.id = "serde-legacy-no-override".to_string();
        item.input_path = PathBuf::from("/tmp/album/01.flac");

        let mut value = serde_json::to_value(&item).expect("serialize baseline item");
        value
            .as_object_mut()
            .expect("item serializes as object")
            .remove("cue_sidecar_override");

        let decoded: ConversionItem = serde_json::from_value(value).expect("deserialize legacy item");
        assert_eq!(decoded.cue_sidecar_override, None);
    }


    fn item_with_pipeline_request(password: Option<&str>) -> ConversionItem {
        let mut item = ConversionItem::default();
        item.id = "archive-secret-serde".to_string();
        item.input_path = PathBuf::from("/tmp/album.7z");
        item.input_format = FileFormat::Archive;
        item.archive_password = password.map(str::to_string);
        item.archive_password_required = password.is_some();
        item.pipeline_request = Some(
            crate::convert::pipeline::build_pipeline_request_from_settings(
                &item,
                tonepoet_pipeline::PipelineSettings::default(),
            )
            .expect("build request"),
        );
        item
    }

    #[test]
    fn queue_and_nested_pipeline_request_never_serialize_archive_cleartext() {
        let mut item = item_with_pipeline_request(Some("top-and-nested-secret"));
        item.archive_password_ref = Some("archive-password:stable-ref".to_string());

        let value = serde_json::to_value(&item).expect("serialize queue item");
        assert_eq!(
            value.get("archive_password"),
            None,
            "top-level process-local secret must be omitted"
        );
        assert_eq!(
            value
                .pointer("/pipeline_request/source/archive_password"),
            None,
            "nested process-local secret must be omitted"
        );
        assert_eq!(
            value.get("archive_password_ref").and_then(serde_json::Value::as_str),
            Some("archive-password:stable-ref")
        );
        let json = serde_json::to_string(&value).expect("stringify value");
        assert!(!json.contains("top-and-nested-secret"));
    }

    #[test]
    fn legacy_cleartext_queue_secret_migrates_to_reference_and_rehydrates_both_surfaces() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let mut value = serde_json::to_value(item_with_pipeline_request(None))
            .expect("serialize baseline queue item");
        value
            .as_object_mut()
            .expect("queue item object")
            .insert(
                "archive_password".to_string(),
                serde_json::Value::String("legacy-secret".to_string()),
            );
        value
            .pointer_mut("/pipeline_request/source")
            .and_then(serde_json::Value::as_object_mut)
            .expect("pipeline source object")
            .insert(
                "archive_password".to_string(),
                serde_json::Value::String("legacy-secret".to_string()),
            );

        let mut decoded: ConversionItem =
            serde_json::from_value(value).expect("deserialize legacy queue item");
        assert_eq!(decoded.archive_password.as_deref(), Some("legacy-secret"));
        assert_eq!(
            decoded
                .pipeline_request
                .as_ref()
                .and_then(|request| request.source.archive_password.as_ref())
                .map(crate::convert::pipeline::SecretString::expose),
            Some("legacy-secret")
        );

        assert_eq!(decoded.restore_archive_password_after_load(), Ok(true));
        let reference = decoded
            .archive_password_ref
            .as_deref()
            .expect("migration stores reference");
        assert!(crate::secret_store::is_reference(reference));
        assert_eq!(crate::secret_store::get(reference).as_deref(), Ok("legacy-secret"));
        assert_eq!(decoded.archive_password.as_deref(), Some("legacy-secret"));
        assert_eq!(
            decoded
                .pipeline_request
                .as_ref()
                .and_then(|request| request.source.archive_password.as_ref())
                .map(crate::convert::pipeline::SecretString::expose),
            Some("legacy-secret")
        );

        let persisted = serde_json::to_string(&decoded).expect("serialize migrated item");
        assert!(persisted.contains(reference));
        assert!(!persisted.contains("legacy-secret"));
    }

    #[test]
    fn interrupted_legacy_queue_migration_reuses_the_same_opaque_reference() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let mut first = item_with_pipeline_request(Some("legacy-secret"));
        first.id = "durable-queue-item-id".to_string();

        assert_eq!(first.restore_archive_password_after_load(), Ok(true));
        let first_reference = first
            .archive_password_ref
            .clone()
            .expect("first migration reference");
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);

        let mut retry = item_with_pipeline_request(Some("legacy-secret"));
        retry.id = "durable-queue-item-id".to_string();
        assert_eq!(retry.restore_archive_password_after_load(), Ok(true));
        let retry_reference = retry
            .archive_password_ref
            .clone()
            .expect("retry migration reference");

        assert_eq!(retry_reference, first_reference);
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);
        assert_eq!(
            crate::secret_store::get(&retry_reference).expect("stable migrated secret"),
            "legacy-secret"
        );
        assert!(!retry_reference.contains("durable-queue-item-id"));
    }

    #[test]
    fn conflicting_legacy_queue_secret_surfaces_fail_closed() {
        let mut item = item_with_pipeline_request(Some("top-level-secret"));
        item.pipeline_request
            .as_mut()
            .expect("request")
            .source
            .archive_password = Some(
            crate::convert::pipeline::SecretString::new("nested-secret".to_string()),
        );

        let error = item
            .restore_archive_password_after_load()
            .expect_err("disagreeing legacy fields must not choose a password");
        assert_eq!(
            error,
            "Archive password state is ambiguous for resumed queue item '/tmp/album.7z': legacy queue fields disagree. Set the archive password again, then retry."
        );
        assert_eq!(item.archive_password, None);
        assert!(item
            .pipeline_request
            .as_ref()
            .expect("request retained")
            .source
            .archive_password
            .is_none());
        assert!(matches!(item.status, ConversionStatus::Failed { .. }));
    }

    #[test]
    fn unavailable_queue_secret_reference_fails_closed_and_requests_reentry() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let mut item = item_with_pipeline_request(Some("stale-process-secret"));
        item.archive_password_ref = Some("archive-password:missing".to_string());

        assert!(item.restore_archive_password_after_load().is_err());
        assert_eq!(item.archive_password, None);
        assert!(item
            .pipeline_request
            .as_ref()
            .expect("request retained")
            .source
            .archive_password
            .is_none());
        match &item.status {
            ConversionStatus::Failed { error, log_path } => {
                assert_eq!(log_path, &None);
                assert!(error.contains("Archive password unavailable"));
                assert!(error.contains("Set the archive password again, then retry."));
            }
            other => panic!("expected fail-closed status, got {other:?}"),
        }
    }

    #[test]
    fn ephemeral_cli_password_has_no_persisted_secret_or_reference_and_resume_fails_closed() {
        let item = item_with_pipeline_request(Some("cli-only-secret"));
        let value = serde_json::to_value(&item).expect("serialize CLI queue item");

        assert_eq!(value.get("archive_password"), None);
        assert_eq!(value.get("archive_password_ref"), None);
        assert_eq!(value.get("archive_password_required").and_then(serde_json::Value::as_bool), Some(true));
        assert_eq!(value.pointer("/pipeline_request/source/archive_password"), None);
        assert!(!serde_json::to_string(&value)
            .expect("stringify CLI queue item")
            .contains("cli-only-secret"));

        let mut resumed: ConversionItem = serde_json::from_value(value).expect("deserialize persisted CLI item");
        let error = resumed
            .restore_archive_password_after_load()
            .expect_err("process-only password must be re-entered after resume");
        assert_eq!(
            error,
            "Archive password unavailable for resumed queue item '/tmp/album.7z': the password was process-only and was not persisted. Set the archive password again, then retry."
        );
        assert_eq!(resumed.archive_password, None);
        assert!(resumed.archive_password_required);
        assert!(matches!(resumed.status, ConversionStatus::Failed { .. }));
    }

    #[test]
    fn retry_failed_supersedes_completed_history_entry() {
        let mut queue = ConversionQueue::new();

        let mut failed = ConversionItem::default();
        failed.id = "synthetic-album".to_string();
        failed.input_path = PathBuf::from("/tmp/tonepoet-synthetic-cue-albums/process-x/artifact-y/album.cue");
        failed.selected = true;
        failed.status = ConversionStatus::Failed {
            error: "failed once".to_string(),
            log_path: None,
        };
        failed.completed_at = Some(chrono::Utc::now());
        queue.completed.push(failed);

        queue.retry_failed();

        assert!(
            queue.completed.is_empty(),
            "retry must move the failed record out of completed history so it cannot remain retryable with the same item id"
        );
        assert_eq!(queue.items.len(), 1);
        let retry = queue.items.front().expect("retry item");
        assert_eq!(retry.id, "synthetic-album");
        assert_eq!(retry.status, ConversionStatus::Queued);
        assert!(retry.completed_at.is_none());
        assert!(
            !retry.selected,
            "a retried item should not inherit the history-row selection that triggered retry"
        );
    }

    #[test]
    fn retry_all_failed_moves_history_records_back_into_active_queue() {
        let mut queue = ConversionQueue::new();
        let mut failed = ConversionItem::default();
        failed.id = "failed-1".to_string();
        failed.status = ConversionStatus::Failed { error: "boom".to_string(), log_path: None };
        queue.items_mut().push_back(failed);
        queue.settle_finished();
        assert_eq!(queue.queued_items().len(), 0);

        let retried = queue.retry_all_failed();
        assert_eq!(retried, 1);
        let queued = queue.queued_items();
        assert_eq!(queued.len(), 1, "retried record must live in the active queue the processor scans");
        assert_eq!(queued[0].id, "failed-1");
        assert!(
            queue.all_items().into_iter().filter(|item| item.id == "failed-1").count() == 1,
            "no duplicate lifecycle records after bulk retry"
        );
    }

    #[test]
    fn remove_selected_removes_completed_and_finished_current_items() {
        let mut queue = ConversionQueue::new();

        let mut queued = ConversionItem::default();
        queued.id = "queued".to_string();
        queued.input_path = PathBuf::from("/tmp/queued.flac");
        queued.selected = true;
        queue.items.push_back(queued);

        let mut current = ConversionItem::default();
        current.id = "current".to_string();
        current.input_path = PathBuf::from("/tmp/current.flac");
        current.selected = true;
        current.status = ConversionStatus::Failed {
            error: "failed".to_string(),
            log_path: None,
        };
        queue.current = Some(current);

        let mut completed = ConversionItem::default();
        completed.id = "completed".to_string();
        completed.input_path = PathBuf::from("/tmp/completed.flac");
        completed.selected = true;
        completed.status = ConversionStatus::Failed {
            error: "failed".to_string(),
            log_path: None,
        };
        queue.completed.push(completed);

        assert_eq!(queue.remove_selected(), 3);
        assert!(queue.items.is_empty());
        assert!(queue.current.is_none());
        assert!(queue.completed.is_empty());
    }

}
