//! Audio conversion processor backed by one shared pipeline scheduler.
//!
//! Queue items materialize first. Each materialized track then enters the same
//! worker pool as every other track from every other item. Album-level stages
//! run only after the scheduler has observed completion of all tracks for that
//! album.

use super::{
    ConversionError, ConversionItem, ConversionPhase, ConversionQueue, ConversionResult,
    ConversionStatus,
};
use log::info;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
use std::sync::OnceLock;
use tokio::sync::{broadcast, mpsc, Semaphore};
use tokio_util::sync::CancellationToken;
use tonepoet_pipeline::PipelineSettings;

use crate::config::DEFAULT_SCRATCH_MEMORY_LIMIT_PERCENT;

use crate::convert::formats::naming_template_with_disc_subfolder;

use crate::convert::pipeline::{
    boxed_work, build_pipeline_request as raw_build_pipeline_request, detect_source_kind,
    build_pipeline_request_from_settings as raw_build_pipeline_request_from_settings,
    prepare_independent_single_file_album_batch_for_dispatch,
    encode_realized_track_for_scheduler_with_tool_limits_and_version_cache,
    encode_track_for_scheduler_with_tool_limits_and_version_cache,
    finish_pipeline_album_for_scheduler_with_tool_limits, map_album_outcome,
    prepare_pipeline_item_for_scheduler,
    realize_track_for_scheduler_with_tool_limits_and_version_cache,
    run_pipeline_item_with_tool_paths_and_tool_limits, scheduled_track_outputs_have_scratch_scoped_storage_exhaustion_for_retry,
    scheduled_worker_failure_output,
    AlbumBatchTrackContext, AlbumCompletionTracker, BatchResolvedAlbumIdentity,
    AlbumOutcome, AlbumReadiness, BroadcastReporter, CompanionCopyPolicy, MetadataTextOverride, PipelineReport, PipelineRequest, PoolLimits,
    RealToolRunner, ScheduledAlbum, ScheduledMaterialization,
    ScheduledRealizedTrack, ScheduledTrackOutput, SchedulerMetrics, SchedulerMetricsSnapshot,
    ScratchStagingConfig, SharedWorkerPool, SourceKind, StageOutcome, ToolBinary, ToolConcurrencyLimits,
    TrackMetadata, TrackOutcome, TrackSourceRef, TrySubmitError, WorkKind, WorkUnit,
    source_text_tag_key_from_extra,
};
use crate::convert::pipeline::stages::{
    disk_staging_parent_for, independent_single_file_album_batch_lifecycle_key,
    pipeline_report_requests_scratch_disk_retry, plan_album_dir_from_dispatch_metadata,
    prepare_independent_single_file_album_batch_for_completion_order_dispatch,
    prepare_verified_single_file_album_batch_completion_order_fallback,
};
use crate::convert::pipeline::materializer_single::read_track_metadata_with_warnings;
#[cfg(test)]
use crate::convert::pipeline::stages::{
    set_post_materialization_stage_fault_hook_for_test, set_publish_fault_hook_for_test,
};
#[cfg(test)]
use crate::convert::pipeline::{
    AlbumMetadata, AlbumPlan, CueSidecarPolicy, DvdaGroupSelection,
    ExtractionProvenance, FailurePolicy, LogPolicy, NamingCollisionPolicy, NamingPolicy,
    OverwritePolicy, PipelineStage, PlannedMetadataSatisfaction, PlannedTrackOutput, PreparedSource, PreparedTrack, PublishedAlbum,
    PublishPolicy, RedactedPipelineRequest, scheduled_album_for_test,
    SourceAudioDescriptor,
    SourceAudioCoding, SourceOptions, StagePolicy, StageRequirement, StagingDir,
    TrackArtifact, TrackId, TrackRecord, TrackSelection,
};

/// Scratch-staging policy for direct single-item API calls.
///
/// Keeping this as an options struct avoids repeatedly bolting scratch-policy
/// fields onto public helper signatures as the staging policy evolves.
#[derive(Debug, Clone)]
pub struct ScratchStagingPolicy {
    pub directory: Option<PathBuf>,
    pub memory_limit_percent: u8,
}

impl ScratchStagingPolicy {
    #[must_use]
    pub fn new(directory: Option<PathBuf>, memory_limit_percent: u8) -> Self {
        Self {
            directory,
            memory_limit_percent,
        }
    }

    #[must_use]
    pub fn disabled() -> Self {
        Self {
            directory: None,
            memory_limit_percent: DEFAULT_SCRATCH_MEMORY_LIMIT_PERCENT,
        }
    }

    #[must_use]
    pub fn with_default_memory_limit(directory: Option<PathBuf>) -> Self {
        Self::new(directory, DEFAULT_SCRATCH_MEMORY_LIMIT_PERCENT)
    }
}

/// Configuration for the conversion processor.
pub struct ProcessorConfig {
    /// Number of shared workers. The default is supplied by the UI/config layer.
    pub worker_count: usize,
    /// Paths to external tools.
    pub tool_paths: HashMap<String, PathBuf>,
    /// Default destination directory for converted output.
    pub default_destination_directory: Option<PathBuf>,
    /// Local scratch directory for extraction.
    pub scratch_directory: Option<PathBuf>,
    /// Maximum percentage of total RAM scratch staging may reserve (0-90).
    pub scratch_memory_limit_percent: u8,
}

/// Handles the actual conversion of audio files.
pub struct ConversionProcessor {
    config: ProcessorConfig,
    progress_tx: Option<broadcast::Sender<ProgressUpdate>>,
    lifecycle_tx: Option<mpsc::UnboundedSender<LifecycleEvent>>,
    pool_limits: PoolLimits,
    scheduler_metrics: Arc<SchedulerMetrics>,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
    /// External cancellation token from the TUI. When triggered, the
    /// scheduler cancels all in-flight workers and kills child processes.
    external_cancel: Option<CancellationToken>,
}

/// Progress update from a worker.
///
/// When `track_index` is `Some(idx)`, the update describes one concurrent
/// track inside a multi-track source (SACD ISO, CUE+image, 7z archive).
/// The TUI routes these to per-track sub-lines below the parent item row.
/// When `track_index` is `None`, the update applies to the item itself.
#[derive(Debug, Clone)]
pub struct ProgressUpdate {
    pub item_id: String,
    pub track_index: Option<u32>,
    pub track_epoch: Option<u64>,
    pub progress: f32,
    pub status: ConversionStatus,
}

#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    ClearTrack {
        item_id: String,
        track_index: u32,
        track_epoch: u64,
    },
    ItemTerminal {
        item_id: String,
        progress: f32,
        status: ConversionStatus,
    },
}

/// Helper to send phase progress updates.
#[allow(dead_code)]
async fn send_phase_update(
    tx: &broadcast::Sender<ProgressUpdate>,
    item_id: &str,
    phase: ConversionPhase,
    phase_progress: f32,
    message: Option<String>,
    file_progress: Option<(u32, u32)>,
) {
    let overall_progress = phase.calculate_overall_progress(phase_progress);

    let _ = tx.send(ProgressUpdate {
        item_id: item_id.to_string(),
        track_index: None,
        track_epoch: None,
        progress: overall_progress,
        status: ConversionStatus::Processing {
            progress: overall_progress,
            message,
            file_progress,
            phase: Some(phase),
            phase_progress: Some(phase_progress),
        },
    });
}

fn terminal_progress_for_status(
    status: &ConversionStatus,
    last_known_progress: Option<f32>,
) -> f32 {
    match status {
        ConversionStatus::Processing { progress, .. } => *progress,
        ConversionStatus::Completed { .. }
        | ConversionStatus::CompletedWithActionErrors { .. }
        | ConversionStatus::Partial { .. } => 100.0,
        ConversionStatus::Failed { .. } | ConversionStatus::Cancelled => {
            last_known_progress.unwrap_or(0.0).clamp(0.0, 100.0)
        }
        ConversionStatus::Queued
        | ConversionStatus::Paused
        | ConversionStatus::Interrupted
        | ConversionStatus::NotConfigured => 0.0,
    }
}


#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct IndependentSingleFileBatchKey {
    source_grouping_root_key: String,
    provisional_album_output_dir_key: String,
    target_format_key: String,
    naming_key: String,
    lifecycle_policy_key: String,
}

#[derive(Debug, Clone)]
struct IndependentSingleFileBatchCandidate {
    item_index: usize,
    request: PipelineRequest,
    source_kind: SourceKind,
    create_disc_subfolders: bool,
}


fn companion_policy_from_item(item: &ConversionItem) -> CompanionCopyPolicy {
    CompanionCopyPolicy {
        extensions: item.options.effective_companion_extensions(),
        folders: item.options.effective_companion_folders(),
        exclude_files: item.options.effective_companion_exclude_files(),
    }
}

fn apply_companion_policy_from_item(request: &mut PipelineRequest, item: &ConversionItem) {
    request.companion = companion_policy_from_item(item);
}

/// Enforce conversion-option invariants at the production request boundary.
///
/// The request builders live behind the pipeline module and may be reached by
/// TUI, direct processor, and settings-driven call sites. The processor must not
/// assume those builders have already projected every legacy `ConversionOptions`
/// field into `PipelineRequest`. In particular, `create_disc_subfolders` affects
/// the final publish path through `PipelineRequest::naming.template`, so apply it
/// idempotently to the concrete template returned by the raw builder before the
/// request can enter batching, scratch staging, or scheduler dispatch.
fn apply_conversion_options_request_contract(
    request: &mut PipelineRequest,
    item: &ConversionItem,
) {
    request.actions = item.options.actions.clone();
    if item.options.create_disc_subfolders {
        request.naming.template = naming_template_with_disc_subfolder(
            std::mem::take(&mut request.naming.template),
            true,
        );
    }
    if let Some(value) = item
        .options
        .album_artist_override
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        request.metadata_overrides.album_artist = MetadataTextOverride::Set(value.to_string());
    }
}

pub(crate) fn build_pipeline_request(
    item: &ConversionItem,
) -> ConversionResult<PipelineRequest> {
    let mut request = raw_build_pipeline_request(item)?;
    apply_conversion_options_request_contract(&mut request, item);
    Ok(request)
}

fn build_pipeline_request_from_settings(
    item: &ConversionItem,
    settings: PipelineSettings,
) -> ConversionResult<PipelineRequest> {
    let mut request = raw_build_pipeline_request_from_settings(item, settings)?;
    apply_conversion_options_request_contract(&mut request, item);
    Ok(request)
}

fn apply_scratch_staging_from_run(
    request: &mut PipelineRequest,
    scratch_staging: &Option<ScratchStagingConfig>,
) {
    request.scratch_staging = scratch_staging.clone();
}

fn scratch_staging_config_for_run(
    scratch_directory: Option<PathBuf>,
    scratch_memory_limit_percent: u8,
) -> Option<ScratchStagingConfig> {
    scratch_directory
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| ScratchStagingConfig::new(path, scratch_memory_limit_percent))
}

/// Attach the conversion-log album-batch contract at the real queue dispatch
/// boundary, before `build_initial_work()` submits independent single-file jobs
/// to the shared scheduler. This is the production call site that turns a
/// folder of per-file `ConversionItem`s into one fragment-backed album batch;
/// tests that call the lower-level helper directly do not exercise this path.
fn prepare_album_batches_for_queued_independent_single_file_jobs(items: &mut [ConversionItem]) {
    let mut groups: BTreeMap<IndependentSingleFileBatchKey, Vec<IndependentSingleFileBatchCandidate>> =
        BTreeMap::new();
    let mut identity_groups: BTreeMap<IndependentSingleFileBatchKey, Vec<IndependentSingleFileBatchCandidate>> =
        BTreeMap::new();

    for (item_index, item) in items.iter().enumerate() {
        let request = match build_pipeline_request(item) {
            Ok(mut request) => {
                apply_companion_policy_from_item(&mut request, item);
                request
            },
            Err(err) => {
                log::warn!(
                    "skipping album-batch preparation for {} because request construction failed: {err}",
                    item.id
                );
                continue;
            }
        };

        if request.album_batch.is_some() {
            continue;
        }

        let source_kind = match detect_source_kind(&request) {
            Ok(kind @ (SourceKind::SingleFile | SourceKind::CueImage)) => kind,
            Ok(_) => continue,
            Err(_) => continue,
        };

        let source_grouping_root = source_grouping_root_for_dispatch_request(&request);
        let album_output_dir = provisional_album_output_dir_for_dispatch_request(&request, &source_grouping_root);
        let key = IndependentSingleFileBatchKey {
            source_grouping_root_key: normalized_path_key(&source_grouping_root),
            provisional_album_output_dir_key: normalized_path_key(&album_output_dir),
            // Group by FOLDER-level identity only. Settings/lifecycle
            // fingerprints deliberately stay OUT of the key: heterogeneous
            // items in one album folder must land in ONE group so the
            // mismatch detector below can suppress ordered logging for the
            // whole folder — separate singleton batches over the same album
            // dir would interleave two conversion.log authorities.
            target_format_key: format!(
                "{:?}|{:?}|{:?}",
                request.settings.target_format,
                request.container_extension,
                request.container_ffmpeg_flags,
            ),
            naming_key: format!(
                "{}|{:?}|{}|{:?}|windows_portable={}",
                request.naming.template,
                request.naming.folder_template,
                request.naming.per_album_subdir,
                request.naming.collision_policy,
                request.naming.windows_portable,
            ),
            // Constant: lifecycle policy participates in the MISMATCH
            // detector (suppression), never in group identity (see above).
            lifecycle_policy_key: String::new(),
        };
        if let Err(error) = independent_single_file_album_batch_lifecycle_key(&request) {
            log::error!(
                "skipping album-batch preparation for {} because its lifecycle policy could not be serialized: {error}",
                request.container.display()
            );
            continue;
        }
        let candidate = IndependentSingleFileBatchCandidate {
            item_index,
            request,
            source_kind,
            create_disc_subfolders: item.options.create_disc_subfolders,
        };
        if candidate.create_disc_subfolders {
            identity_groups.entry(key.clone()).or_default().push(candidate.clone());
        }
        if matches!(candidate.source_kind, SourceKind::SingleFile)
            || (candidate.create_disc_subfolders && matches!(candidate.source_kind, SourceKind::CueImage))
        {
            groups.entry(key).or_default().push(candidate);
        }
    }

    let resolved_identity_by_key: BTreeMap<IndependentSingleFileBatchKey, BatchResolvedAlbumIdentity> = identity_groups
        .iter()
        .filter_map(|(key, group)| resolve_batch_album_identity(group).map(|identity| (key.clone(), identity)))
        .collect();

    for (key, group) in identity_groups.iter() {
        if group.len() <= 1 {
            continue;
        }
        let Some(identity) = resolved_identity_by_key.get(key).cloned() else {
            continue;
        };
        for candidate in group {
            if matches!(candidate.source_kind, SourceKind::SingleFile) {
                continue;
            }
            if let Some(item) = items.get_mut(candidate.item_index) {
                let mut request = candidate.request.clone();
                request.batch_resolved_identity = Some(identity.clone());
                item.pipeline_request = Some(request);
            }
        }
    }

    for (key, group) in groups {
        if group.len() <= 1 {
            continue;
        }

        let source_grouping_root = source_grouping_root_for_dispatch_request(&group[0].request);
        let album_output_dir = provisional_album_output_dir_for_dispatch_request(&group[0].request, &source_grouping_root);

        if let Some(mismatched) = group.iter().find(|candidate| {
            conversion_settings_fingerprint_key(&candidate.request.settings)
                != conversion_settings_fingerprint_key(&group[0].request.settings)
                || independent_single_file_album_batch_lifecycle_key(&candidate.request).ok()
                    != independent_single_file_album_batch_lifecycle_key(&group[0].request).ok()
        }) {
            log::error!(
                "independent single-file album batch at {} cannot enable ordered fragment logging because {} has conversion settings or action/lifecycle policy that differ from the rest of the batch; suppressing legacy conversion.log append for this batch instead of mixing incompatible participants",
                source_grouping_root.display(),
                mismatched.request.container.display()
            );
            prepare_completion_order_album_batch(
                items,
                &group,
                resolved_identity_by_key.get(&key),
                &album_output_dir,
                &source_grouping_root,
                "conversion settings or action/lifecycle policy differ",
            );
            continue;
        }

        let mut prepared = Vec::with_capacity(group.len());
        let mut unavailable_order_reason = None;
        for candidate in group.iter() {
            let source_probe = batch_identity_probe_for_request(&candidate.request, candidate.source_kind);
            let disc_number = candidate
                .request
                .album_batch_track
                .as_ref()
                .and_then(|track| track.disc_number)
                .or(source_probe.disc_number)
                .or_else(|| disc_number_from_dispatch_path(&candidate.request.container));

            let order_key = if let Some(track) = candidate.request.album_batch_track.as_ref() {
                SchedulerTrackOrderKey::Numeric {
                    disc_number: track.disc_number.unwrap_or(0),
                    track_number: track.track_number,
                }
            } else {
                match source_probe.scheduler_track_number.clone() {
                    Some(DispatchTrackNumber::Numeric(track_number)) => {
                        SchedulerTrackOrderKey::Numeric {
                            disc_number: disc_number.unwrap_or(0),
                            track_number,
                        }
                    }
                    Some(DispatchTrackNumber::Vinyl(vinyl)) => {
                        SchedulerTrackOrderKey::Vinyl(vinyl)
                    }
                    Some(DispatchTrackNumber::Unorderable) => {
                        unavailable_order_reason = Some(format!(
                            "{} has explicit TRACKNUMBER metadata that is neither a valid numeric ordinal nor a valid vinyl side/position value",
                            candidate.request.container.display()
                        ));
                        break;
                    }
                    None => {
                        let track_number = source_probe
                            .track_number
                            .or_else(|| strict_track_number_from_dispatch_path(&candidate.request.container));
                        let Some(track_number) = track_number.filter(|value| *value > 0) else {
                            let reason = if filename_contains_non_prefix_digits(&candidate.request.container) {
                                format!(
                                    "{} has no unambiguous TRACKNUMBER metadata and contains only non-prefix filename digits",
                                    candidate.request.container.display()
                                )
                            } else {
                                format!(
                                    "{} has no unambiguous TRACKNUMBER metadata or strict filename track prefix",
                                    candidate.request.container.display()
                                )
                            };
                            unavailable_order_reason = Some(reason);
                            break;
                        };
                        SchedulerTrackOrderKey::Numeric {
                            disc_number: disc_number.unwrap_or(0),
                            track_number,
                        }
                    }
                }
            };

            prepared.push((
                order_key,
                normalized_path_key(&candidate.request.container),
                candidate.item_index,
                candidate.request.clone(),
            ));
        }

        if let Some(reason) = unavailable_order_reason {
            prepare_completion_order_album_batch(
                items,
                &group,
                resolved_identity_by_key.get(&key),
                &album_output_dir,
                &source_grouping_root,
                &reason,
            );
            continue;
        }

        let first_uses_vinyl = prepared
            .first()
            .is_some_and(|(order, _, _, _)| matches!(order, SchedulerTrackOrderKey::Vinyl(_)));
        if let Some((_, _, _, request)) = prepared.iter().find(|(order, _, _, _)| {
            matches!(order, SchedulerTrackOrderKey::Vinyl(_)) != first_uses_vinyl
        }) {
            prepare_completion_order_album_batch(
                items,
                &group,
                resolved_identity_by_key.get(&key),
                &album_output_dir,
                &source_grouping_root,
                &format!(
                    "{} prevents one consistent numeric-or-vinyl TRACKNUMBER ordering scheme for the batch",
                    request.container.display()
                ),
            );
            continue;
        }

        let mut seen_track_identities: BTreeMap<SchedulerTrackOrderKey, PathBuf> = BTreeMap::new();
        let mut duplicate_track_identity = None;
        for (order_key, _path_key, _item_index, request) in prepared.iter() {
            if let Some(first_path) = seen_track_identities.insert(order_key.clone(), request.container.clone()) {
                duplicate_track_identity = Some((order_key.clone(), first_path, request.container.clone()));
                break;
            }
        }
        if let Some((order_key, first_path, duplicate_path)) = duplicate_track_identity {
            prepare_completion_order_album_batch(
                items,
                &group,
                resolved_identity_by_key.get(&key),
                &album_output_dir,
                &source_grouping_root,
                &format!(
                    "duplicate scheduler track identity {order_key:?} for {} and {}",
                    first_path.display(),
                    duplicate_path.display()
                ),
            );
            continue;
        }

        prepared.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
        });

        let mut requests = Vec::with_capacity(prepared.len());
        let mut item_indices = Vec::with_capacity(prepared.len());

        for (ordinal_index, (order_key, _path_key, item_index, mut request)) in prepared.into_iter().enumerate() {
            if let Some(identity) = resolved_identity_by_key.get(&key) {
                request.batch_resolved_identity = Some(identity.clone());
            }
            if request.album_batch_track.is_none() {
                let source_ordinal = u32::try_from(ordinal_index + 1).unwrap_or(u32::MAX);
                let (disc_number, coordination_track_number) = match order_key {
                    SchedulerTrackOrderKey::Numeric { disc_number, track_number } => {
                        ((disc_number > 0).then_some(disc_number), track_number)
                    }
                    SchedulerTrackOrderKey::Vinyl(_) => {
                        // The sorted ordinal is only a durable batch coordination
                        // identity. It represents the already-proven vinyl order
                        // for fragment machinery; it is never promoted to the
                        // source TRACKNUMBER or used to normalize A1/B2 metadata.
                        (None, source_ordinal)
                    }
                };
                request.album_batch_track = Some(AlbumBatchTrackContext::new(
                    source_ordinal,
                    disc_number,
                    coordination_track_number,
                ));
            }
            request.suppress_incremental_conversion_log_append = false;
            item_indices.push(item_index);
            requests.push(request);
        }

        let planner_resolved_album_output_dir =
            planner_resolved_album_output_dir_for_dispatch(&requests);
        let dispatch_album_output_dir = planner_resolved_album_output_dir
            .clone()
            .unwrap_or_else(|| album_output_dir.clone());

        match prepare_independent_single_file_album_batch_for_dispatch(
            requests,
            dispatch_album_output_dir.clone(),
            source_grouping_root.clone(),
        ) {
            Ok(dispatch) => {
                for (item_index, mut request) in item_indices.into_iter().zip(dispatch.requests.into_iter()) {
                    if planner_resolved_album_output_dir.is_some() {
                        if let Some(batch) = request.album_batch.take() {
                            request.album_batch = Some(
                                batch.with_planner_resolved_album_output_dir(
                                    dispatch_album_output_dir.clone(),
                                ),
                            );
                        }
                    }
                    if let Some(item) = items.get_mut(item_index) {
                        item.pipeline_request = Some(request);
                    }
                }
            }
            Err(err) => {
                log::error!(
                    "independent single-file album batch at {} could not enable ordered fragment logging: {err}; falling back to structural completion-order publication",
                    source_grouping_root.display()
                );
                prepare_completion_order_album_batch(
                    items,
                    &group,
                    resolved_identity_by_key.get(&key),
                    &album_output_dir,
                    &source_grouping_root,
                    &format!("ordered dispatch preparation failed: {err}"),
                );
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VinylTrackOrderKey {
    side: String,
    position: u32,
}

impl Ord for VinylTrackOrderKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Spreadsheet-style alphabetic ordering without a fixed side ceiling:
        // A..Z, AA..AZ, BA..BZ, ... . Comparing length first is equivalent to
        // a bijective base-26 ordinal for uppercase ASCII designators, while
        // avoiding integer overflow for long-but-valid prefixes.
        self.side
            .len()
            .cmp(&other.side.len())
            .then_with(|| self.side.cmp(&other.side))
            .then_with(|| self.position.cmp(&other.position))
    }
}

impl PartialOrd for VinylTrackOrderKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DispatchTrackNumber {
    Numeric(u32),
    Vinyl(VinylTrackOrderKey),
    Unorderable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DispatchTrackOrderContext {
    disc_number: Option<u32>,
    track_number: DispatchTrackNumber,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum SchedulerTrackOrderKey {
    Numeric { disc_number: u32, track_number: u32 },
    Vinyl(VinylTrackOrderKey),
}

#[derive(Debug, Clone, Default)]
struct BatchIdentityProbe {
    path_key: String,
    album: Option<String>,
    album_artist: Option<String>,
    artist: Option<String>,
    date: Option<String>,
    disc_number: Option<u32>,
    total_discs: Option<u32>,
    track_number: Option<u32>,
    scheduler_track_number: Option<DispatchTrackNumber>,
}

fn resolve_batch_album_identity(
    group: &[IndependentSingleFileBatchCandidate],
) -> Option<BatchResolvedAlbumIdentity> {
    if group.len() <= 1 || group.iter().any(|candidate| !candidate.create_disc_subfolders) {
        return None;
    }

    let probes: Vec<BatchIdentityProbe> = group
        .iter()
        .map(|candidate| batch_identity_probe_for_request(&candidate.request, candidate.source_kind))
        .collect();

    let override_album_artist = group.iter().find_map(|candidate| match &candidate.request.metadata_overrides.album_artist {
        MetadataTextOverride::Set(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
        _ => None,
    });

    resolve_batch_album_identity_from_probes(probes, override_album_artist)
}

fn resolve_batch_album_identity_from_probes(
    mut probes: Vec<BatchIdentityProbe>,
    override_album_artist: Option<String>,
) -> Option<BatchResolvedAlbumIdentity> {
    if probes.len() <= 1 {
        return None;
    }

    for probe in &mut probes {
        // Enforce the lookup key normalization here rather than trusting every
        // probe constructor: disc_number_for_path() normalizes its argument,
        // so stored keys must match or path-derived disc numbers vanish.
        probe.path_key = probe.path_key.replace('\\', "/").to_ascii_lowercase();
        if probe.disc_number.is_none() {
            probe.disc_number = disc_number_from_dispatch_path(Path::new(&probe.path_key));
        }
    }

    let disc_numbers: BTreeSet<u32> = probes
        .iter()
        .filter_map(|probe| probe.disc_number)
        .filter(|disc| *disc > 0)
        .collect();
    let max_total_discs = probes
        .iter()
        .filter_map(|probe| probe.total_discs)
        .max()
        .or_else(|| disc_numbers.iter().next_back().copied());
    let has_disc_evidence = disc_numbers.len() > 1 || max_total_discs.is_some_and(|total| total > 1);
    if !has_disc_evidence {
        return None;
    }

    let mut normalized_album_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for album in probes.iter().filter_map(|probe| probe.album.as_deref()) {
        let normalized = normalize_album_identity_key(album);
        if !normalized.is_empty() {
            normalized_album_values
                .entry(normalized)
                .or_default()
                .push(album.trim().to_string());
        }
    }
    if normalized_album_values.len() > 1 {
        return None;
    }

    let resolved_album = normalized_album_values
        .values()
        .next()
        .and_then(|values| canonical_album_from_batch_variants(values, &disc_numbers, max_total_discs));

    let date_values: BTreeSet<String> = probes
        .iter()
        .filter_map(|probe| probe.date.as_deref())
        .map(normalize_date_for_batch_identity)
        .filter(|value| !value.is_empty())
        .collect();
    if date_values.len() > 1 {
        return None;
    }
    let resolved_date = date_values.iter().next().cloned();

    let resolved_album_artist = override_album_artist.or_else(|| {
        majority_text_value(probes.iter().filter_map(|probe| {
            probe
                .album_artist
                .as_deref()
                .or(probe.artist.as_deref())
        }))
    });

    let source_disc_numbers: BTreeMap<String, u32> = probes
        .iter()
        .filter_map(|probe| {
            probe
                .disc_number
                .filter(|disc| *disc > 0)
                .map(|disc| (probe.path_key.clone(), disc))
        })
        .collect();

    let identity = BatchResolvedAlbumIdentity {
        album: resolved_album,
        album_artist: resolved_album_artist,
        date: resolved_date,
        total_discs: max_total_discs.filter(|total| *total > 1),
        source_disc_numbers,
    };

    if identity.is_empty() {
        None
    } else {
        Some(identity)
    }
}

fn batch_identity_probe_for_request(req: &PipelineRequest, source_kind: SourceKind) -> BatchIdentityProbe {
    let mut probe = match source_kind {
        SourceKind::SingleFile => single_file_batch_identity_probe(&req.container).unwrap_or_default(),
        SourceKind::CueImage => cue_batch_identity_probe(&req.container).unwrap_or_default(),
        _ => BatchIdentityProbe::default(),
    };
    probe.path_key = normalized_path_key(&req.container);
    if let Some(context) = track_order_context_from_dispatch_metadata(&req.container) {
        probe.disc_number = probe.disc_number.or(context.disc_number);
        match context.track_number {
            DispatchTrackNumber::Numeric(track) => {
                probe.track_number = probe.track_number.or(Some(track));
                probe.scheduler_track_number = Some(DispatchTrackNumber::Numeric(track));
            }
            DispatchTrackNumber::Vinyl(vinyl) => {
                // A raw lexical TRACKNUMBER is stronger scheduler evidence than a
                // library-derived numeric suffix. Preserve Vinyl/Unorderable so the
                // batch cannot silently fall back to filename digits and invent a
                // canonical ordering.
                probe.scheduler_track_number = Some(DispatchTrackNumber::Vinyl(vinyl));
            }
            DispatchTrackNumber::Unorderable => {
                probe.scheduler_track_number = Some(DispatchTrackNumber::Unorderable);
            }
        }
    }
    probe.disc_number = probe
        .disc_number
        .or_else(|| disc_number_from_dispatch_path(&req.container));
    if probe.scheduler_track_number.is_none() {
        if let Some(track) = probe.track_number {
            probe.scheduler_track_number = Some(DispatchTrackNumber::Numeric(track));
        }
    }
    probe.track_number = probe
        .track_number
        .or_else(|| strict_track_number_from_dispatch_path(&req.container));
    probe
}

fn dispatch_track_metadata_for_output_planning(
    req: &PipelineRequest,
    source_kind: SourceKind,
) -> Option<TrackMetadata> {
    match source_kind {
        // A failed tag read yields no planning evidence (None -> the batch
        // stays provisional); the materializer owns the authoritative
        // fail-closed read.
        SourceKind::SingleFile => {
            if let Some(source) = req.source.sidecar_cue_track_metadata.as_ref() {
                let (cue_metadata, _album) =
                    crate::convert::pipeline::materializer_cue::metadata_for_transferred_sidecar_cue_track(source)
                        .ok()?;
                let base = crate::convert::pipeline::materializer_single::read_track_metadata_with_warnings_and_viability(&req.container)
                    .ok()
                    .filter(|(_metadata, _warnings, _recovered, viable)| *viable)
                    .map(|(metadata, _warnings, _recovered, _viable)| metadata)
                    .unwrap_or_default();
                Some(crate::convert::pipeline::materializer_single::merge_sidecar_cue_track_metadata(
                    base,
                    cue_metadata,
                ))
            } else {
                read_track_metadata_with_warnings(&req.container)
                    .ok()
                    .map(|(metadata, _warnings, _recovered_by_fallback)| metadata)
            }
        }
        SourceKind::CueImage => {
            let cue = crate::convert::pipeline::dispatch_metadata_sheet_for_sidecar_cue(
                &req.container,
            )
            .or_else(|| crate::convert::cue_parser::parse_cue_file(&req.container).ok())?;
            let first = cue.tracks.first();
            let mut extra = BTreeMap::new();
            if let Some(album) = cue.title.as_ref().filter(|value| !value.trim().is_empty()) {
                extra.insert("album".to_string(), album.clone());
            }
            if let Some(catalog) = cue.catalog.as_ref().filter(|value| !value.trim().is_empty()) {
                extra.insert("catalog".to_string(), catalog.clone());
            }
            Some(TrackMetadata {
                title: first.and_then(|track| track.title.clone()),
                artist: first
                    .and_then(|track| track.performer.clone())
                    .or_else(|| cue.performer.clone()),
                album_artist: cue.performer.clone(),
                date: cue.date.clone(),
                track_number: first.map(|track| track.number),
                disc_number: req
                    .album_batch_track
                    .as_ref()
                    .and_then(|track| track.disc_number)
                    .or_else(|| disc_number_from_dispatch_path(&req.container)),
                extra,
                ..TrackMetadata::default()
            })
        }
        _ => None,
    }
}

/// Return an authoritative batch album directory only when every request can
/// be planned from queue-time metadata through the canonical output planner
/// and every result agrees. Any uncertainty deliberately leaves the batch
/// provisional; the pipeline will then refuse unsafe early pre-action
/// execution rather than racing a guessed destination against rerun recovery.
fn planner_resolved_album_output_dir_for_dispatch(
    requests: &[PipelineRequest],
) -> Option<PathBuf> {
    let mut resolved: Option<PathBuf> = None;
    for req in requests {
        let source_kind = detect_source_kind(req).ok()?;
        let metadata = dispatch_track_metadata_for_output_planning(req, source_kind)?;
        let album_dir = plan_album_dir_from_dispatch_metadata(req, source_kind, metadata).ok()?;
        match resolved.as_ref() {
            Some(existing) if normalized_path_key(existing) != normalized_path_key(&album_dir) => {
                return None;
            }
            Some(_) => {}
            None => resolved = Some(album_dir),
        }
    }
    resolved
}

fn single_file_batch_identity_probe(path: &Path) -> Option<BatchIdentityProbe> {
    // Use the same Lofty-backed metadata reader as the single-file materializer
    // instead of maintaining a parallel FLAC-only tag parser in the dispatcher.
    // The dispatcher only uses these fields as conservative organizational
    // evidence; written metadata still comes from the materialized source and
    // explicit request overrides.
    let (metadata, _warnings, _recovered_by_fallback) =
        read_track_metadata_with_warnings(path).ok()?;
    batch_identity_probe_from_track_metadata(&metadata)
}

fn batch_identity_probe_from_track_metadata(metadata: &TrackMetadata) -> Option<BatchIdentityProbe> {
    let album = metadata.extra.get("album").cloned();
    let total_discs = metadata
        .extra
        .get("disctotal")
        .and_then(|value| parse_metadata_ordinal(value));
    let scheduler_track_number = source_track_number_value(metadata)
        .map(parse_dispatch_track_number)
        .or_else(|| metadata.track_number.map(DispatchTrackNumber::Numeric));

    let probe = BatchIdentityProbe {
        path_key: String::new(),
        album,
        album_artist: metadata.album_artist.clone(),
        artist: metadata.artist.clone(),
        date: metadata.date.clone(),
        disc_number: metadata.disc_number,
        total_discs,
        track_number: metadata.track_number,
        scheduler_track_number,
    };

    if probe.album.is_none()
        && probe.album_artist.is_none()
        && probe.artist.is_none()
        && probe.date.is_none()
        && probe.disc_number.is_none()
        && probe.total_discs.is_none()
        && probe.track_number.is_none()
        && probe.scheduler_track_number.is_none()
    {
        None
    } else {
        Some(probe)
    }
}

fn cue_batch_identity_probe(path: &Path) -> Option<BatchIdentityProbe> {
    // Same freshness precedence as the CUE materializer: the metadata editor
    // writes corrections to the referenced image's embedded CUESHEET, and the
    // identity that names the album folder must match what conversion emits.
    let cue = crate::convert::pipeline::dispatch_metadata_sheet_for_sidecar_cue(path)
        .or_else(|| crate::convert::cue_parser::parse_cue_file(path).ok())?;
    Some(BatchIdentityProbe {
        album: cue.title,
        album_artist: cue.performer,
        artist: None,
        date: cue.date,
        disc_number: disc_number_from_dispatch_path(path),
        total_discs: None,
        track_number: cue.tracks.first().map(|track| track.number),
        scheduler_track_number: cue
            .tracks
            .first()
            .map(|track| DispatchTrackNumber::Numeric(track.number)),
        path_key: String::new(),
    })
}

fn majority_text_value<'a>(values: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut counts: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut total = 0usize;
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        total += 1;
        let key = normalize_text_identity_key(trimmed);
        let entry = counts.entry(key).or_insert_with(|| (0, trimmed.to_string()));
        entry.0 += 1;
    }
    counts
        .into_values()
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)))
        .and_then(|(count, value)| if count * 2 > total { Some(value) } else { None })
}

fn canonical_album_from_batch_variants(
    values: &[String],
    disc_numbers: &BTreeSet<u32>,
    max_total_discs: Option<u32>,
) -> Option<String> {
    let mut unique: BTreeSet<String> = values
        .iter()
        .map(|value| strip_album_disc_designator(value).trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    if unique.is_empty() {
        return None;
    }
    if unique.len() == 1 {
        return unique.pop_first();
    }
    let values = unique.into_iter().collect::<Vec<_>>();
    merge_trailing_numeric_catalog_variants(&values, disc_numbers, max_total_discs)
}

fn merge_trailing_numeric_catalog_variants(
    values: &[String],
    disc_numbers: &BTreeSet<u32>,
    max_total_discs: Option<u32>,
) -> Option<String> {
    if !catalog_variant_disc_evidence_is_strict(values.len(), disc_numbers, max_total_discs) {
        return None;
    }

    let mut parts = Vec::new();
    for value in values {
        let (prefix, digits, closing) = trailing_catalog_number_parts(value)?;
        parts.push((prefix, digits, closing));
    }
    let (prefix, first_digits, closing) = parts.first()?.clone();
    if parts.iter().any(|(p, _, c)| p != &prefix || *c != closing) {
        return None;
    }
    let numbers: Vec<u32> = parts
        .iter()
        .filter_map(|(_, digits, _)| digits.parse::<u32>().ok())
        .collect();
    if numbers.len() != parts.len() {
        return None;
    }
    let min = *numbers.iter().min()?;
    let max = *numbers.iter().max()?;
    if max.saturating_sub(min) + 1 != numbers.len() as u32 {
        return None;
    }
    let start = first_digits;
    let end = max.to_string();
    let common = start
        .chars()
        .zip(end.chars())
        .take_while(|(left, right)| left == right)
        .count();
    let rendered = if min == max {
        format!("{prefix}{start}")
    } else {
        let end_suffix = end.get(common..).unwrap_or(&end);
        format!("{prefix}{start}-{end_suffix}")
    };
    Some(if closing { format!("{rendered})") } else { rendered })
}

fn catalog_variant_disc_evidence_is_strict(
    variant_count: usize,
    disc_numbers: &BTreeSet<u32>,
    max_total_discs: Option<u32>,
) -> bool {
    if variant_count < 2 || disc_numbers.len() != variant_count {
        return false;
    }
    match max_total_discs.and_then(|total| usize::try_from(total).ok()) {
        Some(total) => total == variant_count,
        None => true,
    }
}

fn normalize_album_identity_key(value: &str) -> String {
    normalize_text_identity_key(&strip_album_catalog_variance(&strip_album_disc_designator(value)))
}

fn strip_album_disc_designator(value: &str) -> String {
    let mut out = value.trim().to_string();
    loop {
        let Some(stripped) = strip_trailing_parenthesized_disc_designator(&out)
            .or_else(|| strip_trailing_dash_disc_designator(&out))
        else {
            break;
        };
        if stripped == out {
            break;
        }
        out = stripped;
    }
    out.trim().to_string()
}

fn strip_trailing_parenthesized_disc_designator(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let close = trimmed.chars().last()?;
    if close != ')' && close != ']' {
        return None;
    }
    let open = if close == ')' { '(' } else { '[' };
    let open_idx = trimmed.rfind(open)?;
    let inner = &trimmed[open_idx + 1..trimmed.len() - 1];
    if is_disc_designator_text(inner) {
        Some(trimmed[..open_idx].trim_end().to_string())
    } else {
        None
    }
}

fn strip_trailing_dash_disc_designator(value: &str) -> Option<String> {
    let trimmed = value.trim();
    for sep in [" - ", " — ", " – "] {
        if let Some(idx) = trimmed.rfind(sep) {
            let tail = &trimmed[idx + sep.len()..];
            if is_disc_designator_text(tail) {
                return Some(trimmed[..idx].trim_end().to_string());
            }
        }
    }
    None
}

fn is_disc_designator_text(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    for prefix in ["disc", "disk", "cd"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            return rest.trim().chars().next().is_some_and(|ch| ch.is_ascii_digit());
        }
    }
    false
}

fn strip_album_catalog_variance(value: &str) -> String {
    let Some((prefix, _digits, closing)) = trailing_catalog_number_parts(value) else {
        return value.trim().to_string();
    };
    let mut out = prefix.trim_end().to_string();
    if closing {
        out.push(')');
    }
    out
}

fn trailing_catalog_number_parts(value: &str) -> Option<(String, String, bool)> {
    let trimmed = value.trim();
    let closing = trimmed.ends_with(')');
    let core = if closing {
        &trimmed[..trimmed.len().saturating_sub(1)]
    } else {
        trimmed
    };
    let digit_start = core
        .char_indices()
        .rev()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(0);
    if digit_start == core.len() || core[digit_start..].len() < 2 {
        return None;
    }
    let prefix = &core[..digit_start];
    if !catalog_prefix_has_catalog_evidence(prefix, closing) {
        return None;
    }
    Some((prefix.to_string(), core[digit_start..].to_string(), closing))
}

fn catalog_prefix_has_catalog_evidence(prefix: &str, closing_parenthetical: bool) -> bool {
    let has_parenthetical_catalog_context = closing_parenthetical && prefix.rfind('(').is_some();
    let has_catalog_token = prefix
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'))
        .any(catalog_token_has_letter_and_digit);
    has_parenthetical_catalog_context && has_catalog_token
}

fn catalog_token_has_letter_and_digit(token: &str) -> bool {
    let token = token.trim_matches(|ch: char| ch == '-' || ch == '_');
    token.len() >= 3
        && token.chars().any(|ch| ch.is_ascii_alphabetic())
        && token.chars().any(|ch| ch.is_ascii_digit())
}

fn normalize_text_identity_key(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| ch.to_lowercase())
        .map(|ch| if ch.is_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_date_for_batch_identity(value: &str) -> String {
    value
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
}

fn conversion_settings_fingerprint_key(settings: &PipelineSettings) -> String {
    tonepoet_pipeline::fingerprint::settings_fingerprint(settings).to_string()
}

fn source_grouping_root_for_dispatch_request(req: &PipelineRequest) -> PathBuf {
    let parent = req
        .container
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| req.container.clone());

    if is_disc_directory(&parent) {
        parent.parent().map(Path::to_path_buf).unwrap_or(parent)
    } else {
        parent
    }
}

fn provisional_album_output_dir_for_dispatch_request(req: &PipelineRequest, source_grouping_root: &Path) -> PathBuf {
    if !req.naming.per_album_subdir {
        return req.output_root.clone();
    }

    // This is only a dispatch-time grouping/fallback directory. It must not be
    // treated as the authoritative album output directory when folder templates
    // or tag metadata can change the planner's final album path. The
    // AlbumBatchContext created from it is marked provisional; fragment staging
    // and identity bind to the directory returned by plan_outputs() once a
    // track has been materialized/planned.
    let component = source_grouping_root
        .file_name()
        .and_then(|value| value.to_str())
        .map(sanitize_album_batch_component)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Album".to_string());
    req.output_root.join(component)
}

fn is_disc_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|name| {
            let lower = name.trim().to_ascii_lowercase();
            matches!(lower.as_str(), "disc" | "disk" | "cd")
                || disc_number_from_directory_name(name).is_some()
        })
        .unwrap_or(false)
}

fn disc_number_from_dispatch_path(path: &Path) -> Option<u32> {
    let parent = path.parent()?;
    let name = parent.file_name()?.to_str()?;
    disc_number_from_directory_name(name)
}

fn disc_number_from_directory_name(name: &str) -> Option<u32> {
    let lower = name.trim().to_ascii_lowercase();
    for prefix in ["disc", "disk", "cd"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let rest = rest.trim_start();
            let rest = rest
                .strip_prefix('-')
                .or_else(|| rest.strip_prefix('_'))
                .or_else(|| rest.strip_prefix('.'))
                .unwrap_or(rest)
                .trim_start();
            let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            if !digits.is_empty() {
                if let Ok(number) = digits.parse::<u32>() {
                    if number > 0 {
                        return Some(number);
                    }
                }
            }
        }
    }
    None
}

fn prepare_completion_order_album_batch(
    items: &mut [ConversionItem],
    group: &[IndependentSingleFileBatchCandidate],
    resolved_identity: Option<&BatchResolvedAlbumIdentity>,
    provisional_album_output_dir: &Path,
    source_grouping_root: &Path,
    reason: &str,
) {
    let mut ordered = group.to_vec();
    ordered.sort_by(|left, right| {
        normalized_path_key(&left.request.container)
            .cmp(&normalized_path_key(&right.request.container))
            .then_with(|| left.item_index.cmp(&right.item_index))
    });

    let mut requests = Vec::with_capacity(ordered.len());
    let mut item_indices = Vec::with_capacity(ordered.len());
    for (index, candidate) in ordered.iter().enumerate() {
        let ordinal = match u32::try_from(index + 1) {
            Ok(ordinal) => ordinal,
            Err(_) => {
                log::error!(
                    "independent single-file album batch at {} contains more participants than the durable u32 coordination identity can represent; failing closed",
                    source_grouping_root.display()
                );
                mark_queued_album_batch_as_ordering_unavailable(items, group);
                return;
            }
        };
        let mut request = candidate.request.clone();
        if let Some(identity) = resolved_identity {
            request.batch_resolved_identity = Some(identity.clone());
        }
        // These unique ordinals are coordination identities and may be rendered
        // as filename numbers only for untagged completion-order tracks. They
        // are never promoted to TRACKNUMBER metadata or conversion-log ordering.
        request.album_batch_track = Some(AlbumBatchTrackContext::new(ordinal, None, ordinal));
        request.suppress_incremental_conversion_log_append = false;
        item_indices.push(candidate.item_index);
        requests.push(request);
    }

    let planner_resolved_album_output_dir = planner_resolved_album_output_dir_for_dispatch(&requests);
    let dispatch_album_output_dir = planner_resolved_album_output_dir
        .clone()
        .unwrap_or_else(|| provisional_album_output_dir.to_path_buf());

    let primary_requests = requests.clone();
    let dispatch = match prepare_independent_single_file_album_batch_for_completion_order_dispatch(
        primary_requests,
        dispatch_album_output_dir.clone(),
        source_grouping_root.to_path_buf(),
    ) {
        Ok(dispatch) => dispatch,
        Err(primary_error) => {
            log::error!(
                "independent single-file album batch at {} could not complete full completion-order dispatch after {reason}: {primary_error}; attempting verified structural publish fallback",
                source_grouping_root.display()
            );
            match prepare_verified_single_file_album_batch_completion_order_fallback(
                requests,
                dispatch_album_output_dir.clone(),
                source_grouping_root.to_path_buf(),
            ) {
                Ok(dispatch) => dispatch,
                Err(fallback_error) => {
                    log::error!(
                        "independent single-file album batch at {} could not establish even verified structural publication after {reason}: {fallback_error}; suppressing incremental append",
                        source_grouping_root.display()
                    );
                    mark_queued_album_batch_as_ordering_unavailable(items, group);
                    return;
                }
            }
        }
    };

    log::warn!(
        "independent single-file album batch at {} cannot prove canonical track ordering ({reason}); publishing through one structural album batch and logging in completion order",
        source_grouping_root.display()
    );
    for (item_index, mut request) in item_indices.into_iter().zip(dispatch.requests.into_iter()) {
        if planner_resolved_album_output_dir.is_some() {
            if let Some(batch) = request.album_batch.take() {
                request.album_batch = Some(
                    batch.with_planner_resolved_album_output_dir(dispatch_album_output_dir.clone()),
                );
            }
        }
        if let Some(item) = items.get_mut(item_index) {
            item.pipeline_request = Some(request);
        }
    }
}

fn mark_queued_album_batch_as_ordering_unavailable(
    items: &mut [ConversionItem],
    group: &[IndependentSingleFileBatchCandidate],
) {
    for candidate in group {
        if let Some(item) = items.get_mut(candidate.item_index) {
            let mut request = candidate.request.clone();
            request.album_batch = None;
            request.album_batch_track = None;
            request.expected_album_track_count = None;
            request.suppress_incremental_conversion_log_append = true;
            item.pipeline_request = Some(request);
        }
    }
}

fn parse_vinyl_track_order(value: &str) -> Option<VinylTrackOrderKey> {
    let value = value.trim();
    let side_len = value
        .as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_alphabetic())
        .count();
    if side_len == 0 || side_len == value.len() {
        return None;
    }
    let side = &value[..side_len];
    let position = &value[side_len..];
    if position.is_empty() || !position.as_bytes().iter().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let position = position
        .parse::<u32>()
        .ok()
        .filter(|position| *position > 0)?;
    Some(VinylTrackOrderKey {
        side: side.to_ascii_uppercase(),
        position,
    })
}

fn parse_dispatch_track_number(value: &str) -> DispatchTrackNumber {
    if let Some(vinyl) = parse_vinyl_track_order(value) {
        return DispatchTrackNumber::Vinyl(vinyl);
    }

    // Preserve the scheduler's pre-existing numeric TRACKNUMBER semantics for
    // ordinary numeric metadata (including forms such as `7/12`). Vinyl is an
    // adjacent extension, not a reason to tighten or redirect the numeric fast
    // path. Alphabetic malformed values still fail this leading-numeric parse
    // and therefore remain explicitly unorderable.
    if let Some(track) = parse_metadata_ordinal(value) {
        return DispatchTrackNumber::Numeric(track);
    }
    DispatchTrackNumber::Unorderable
}

fn merge_dispatch_track_number(
    current: Option<DispatchTrackNumber>,
    candidate: Option<DispatchTrackNumber>,
) -> Option<DispatchTrackNumber> {
    match candidate {
        // Match the old numeric-reader behavior when duplicate TRACKNUMBER
        // fields exist: a later malformed field must not erase earlier valid
        // evidence, but a malformed field must remain visible when it is the
        // only evidence so filename digits cannot silently manufacture order.
        Some(DispatchTrackNumber::Unorderable) => {
            current.or(Some(DispatchTrackNumber::Unorderable))
        }
        Some(valid) => Some(valid),
        None => current,
    }
}

fn source_track_number_value(metadata: &TrackMetadata) -> Option<&str> {
    metadata.extra.iter().find_map(|(marker_key, marker_value)| {
        let source_key = source_text_tag_key_from_extra(&metadata.extra, marker_key, marker_value)?;
        (source_key.eq_ignore_ascii_case("TRACKNUMBER") || source_key.eq_ignore_ascii_case("TRACK"))
            .then_some(marker_value.as_str())
    })
}

#[cfg(test)]
fn numeric_track_context(
    context: DispatchTrackOrderContext,
) -> Option<(Option<u32>, u32)> {
    match context.track_number {
        DispatchTrackNumber::Numeric(track) => Some((context.disc_number, track)),
        DispatchTrackNumber::Vinyl(_) | DispatchTrackNumber::Unorderable => None,
    }
}

pub(crate) fn strict_track_number_from_dispatch_path(path: &Path) -> Option<u32> {
    let stem = path.file_stem()?.to_str()?.trim();
    let mut digits = String::new();
    let mut chars = stem.char_indices().peekable();
    while let Some((_idx, ch)) = chars.peek().copied() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            chars.next();
        } else {
            break;
        }
    }
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    let rest = &stem[digits.len()..];
    if !has_strict_track_prefix_separator(rest) {
        return None;
    }
    digits.parse::<u32>().ok().filter(|value| *value > 0)
}

pub(crate) fn has_strict_track_prefix_separator(rest: &str) -> bool {
    if rest.is_empty() {
        return true;
    }
    let trimmed = rest.trim_start();
    matches!(trimmed.chars().next(), Some('-' | '_' | '.'))
}

fn track_order_context_from_dispatch_metadata(path: &Path) -> Option<DispatchTrackOrderContext> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());

    match extension.as_deref() {
        Some("flac") => flac_vorbis_comment_track_order_context(path)
            .or_else(|| id3v2_track_order_context(path))
            .or_else(|| apev2_track_order_context(path)),
        Some("mp3") | Some("aif") | Some("aiff") => id3v2_track_order_context(path)
            .or_else(|| id3v1_track_context(path).map(|(disc_number, track)| DispatchTrackOrderContext {
                disc_number,
                track_number: DispatchTrackNumber::Numeric(track),
            }))
            .or_else(|| apev2_track_order_context(path)),
        Some("ape") | Some("wv") | Some("mpc") | Some("mp+") => apev2_track_order_context(path)
            .or_else(|| id3v2_track_order_context(path))
            .or_else(|| id3v1_track_context(path).map(|(disc_number, track)| DispatchTrackOrderContext {
                disc_number,
                track_number: DispatchTrackNumber::Numeric(track),
            })),
        _ => id3v2_track_order_context(path)
            .or_else(|| id3v1_track_context(path).map(|(disc_number, track)| DispatchTrackOrderContext {
                disc_number,
                track_number: DispatchTrackNumber::Numeric(track),
            }))
            .or_else(|| apev2_track_order_context(path)),
    }
}

fn flac_vorbis_comment_track_order_context(path: &Path) -> Option<DispatchTrackOrderContext> {
    let mut file = fs::File::open(path).ok()?;
    read_flac_vorbis_comment_track_order_context(&mut file)
}

fn read_flac_vorbis_comment_track_order_context<R: Read + Seek>(
    reader: &mut R,
) -> Option<DispatchTrackOrderContext> {
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic).ok()?;
    if &magic != b"fLaC" {
        return None;
    }

    loop {
        let mut header = [0u8; 4];
        reader.read_exact(&mut header).ok()?;
        let is_last = header[0] & 0x80 != 0;
        let block_type = header[0] & 0x7f;
        let length = ((usize::from(header[1])) << 16)
            | ((usize::from(header[2])) << 8)
            | usize::from(header[3]);

        if block_type == 4 {
            let mut block = vec![0u8; length];
            reader.read_exact(&mut block).ok()?;
            return parse_vorbis_comment_track_order_context(&block);
        }

        reader.seek(SeekFrom::Current(i64::try_from(length).ok()?)).ok()?;
        if is_last {
            break;
        }
    }
    None
}

#[cfg(test)]
fn read_flac_vorbis_comment_track_context<R: Read + Seek>(reader: &mut R) -> Option<(Option<u32>, u32)> {
    numeric_track_context(read_flac_vorbis_comment_track_order_context(reader)?)
}

#[cfg(test)]
fn parse_flac_vorbis_comment_track_context(bytes: &[u8]) -> Option<(Option<u32>, u32)> {
    numeric_track_context(parse_flac_vorbis_comment_track_order_context(bytes)?)
}

#[cfg(test)]
fn parse_flac_vorbis_comment_track_order_context(bytes: &[u8]) -> Option<DispatchTrackOrderContext> {
    if bytes.len() < 4 || &bytes[..4] != b"fLaC" {
        return None;
    }

    let mut offset = 4usize;
    while offset.checked_add(4)? <= bytes.len() {
        let header = bytes[offset];
        let is_last = header & 0x80 != 0;
        let block_type = header & 0x7f;
        let length = ((usize::from(bytes[offset + 1])) << 16)
            | ((usize::from(bytes[offset + 2])) << 8)
            | usize::from(bytes[offset + 3]);
        offset = offset.checked_add(4)?;
        let end = offset.checked_add(length)?;
        if end > bytes.len() {
            return None;
        }
        if block_type == 4 {
            return parse_vorbis_comment_track_order_context(&bytes[offset..end]);
        }
        offset = end;
        if is_last {
            break;
        }
    }
    None
}

fn parse_vorbis_comment_track_order_context(block: &[u8]) -> Option<DispatchTrackOrderContext> {
    let mut offset = 0usize;
    let vendor_len = read_le_u32(block, &mut offset)? as usize;
    offset = offset.checked_add(vendor_len)?;
    if offset > block.len() {
        return None;
    }
    let comment_count = read_le_u32(block, &mut offset)? as usize;
    let mut track_number = None;
    let mut disc_number = None;
    for _ in 0..comment_count {
        let len = read_le_u32(block, &mut offset)? as usize;
        let end = offset.checked_add(len)?;
        if end > block.len() {
            return None;
        }
        let comment = std::str::from_utf8(&block[offset..end]).ok()?;
        offset = end;
        let Some((key, value)) = comment.split_once('=') else {
            continue;
        };
        match key.trim().to_ascii_uppercase().as_str() {
            "TRACKNUMBER" | "TRACK" => {
                track_number = merge_dispatch_track_number(
                    track_number,
                    Some(parse_dispatch_track_number(value)),
                );
            }
            "DISCNUMBER" | "DISC" => {
                disc_number = parse_metadata_ordinal(value).or(disc_number);
            }
            _ => {}
        }
    }
    track_number.map(|track_number| DispatchTrackOrderContext {
        disc_number,
        track_number,
    })
}

fn id3v2_track_order_context(path: &Path) -> Option<DispatchTrackOrderContext> {
    let mut file = fs::File::open(path).ok()?;
    let mut header = [0u8; 10];
    file.read_exact(&mut header).ok()?;
    if &header[..3] != b"ID3" {
        return None;
    }
    let major = header[3];
    if !(2..=4).contains(&major) {
        return None;
    }
    let tag_size = read_synchsafe_u32(&header[6..10])? as usize;
    if tag_size == 0 || tag_size > 16 * 1024 * 1024 {
        return None;
    }
    let mut body = vec![0u8; tag_size];
    file.read_exact(&mut body).ok()?;
    parse_id3v2_track_order_context(major, header[5], &body)
}

#[cfg(test)]
fn parse_id3v2_track_context(major: u8, flags: u8, body: &[u8]) -> Option<(Option<u32>, u32)> {
    numeric_track_context(parse_id3v2_track_order_context(major, flags, body)?)
}

fn parse_id3v2_track_order_context(
    major: u8,
    flags: u8,
    body: &[u8],
) -> Option<DispatchTrackOrderContext> {
    let deunsynchronized;
    let body = if id3v2_tag_uses_unsynchronization(flags) {
        deunsynchronized = id3v2_deunsynchronize(body);
        deunsynchronized.as_slice()
    } else {
        body
    };
    let mut offset = id3v2_frame_start_offset(major, flags, body).unwrap_or(0);
    let mut track_number = None;
    let mut disc_number = None;

    while offset < body.len() {
        if major == 2 {
            let frame_header_end = offset.checked_add(6)?;
            if frame_header_end > body.len() {
                break;
            }
            let id = &body[offset..offset + 3];
            if id.iter().all(|byte| *byte == 0) {
                break;
            }
            let size = ((usize::from(body[offset + 3])) << 16)
                | ((usize::from(body[offset + 4])) << 8)
                | usize::from(body[offset + 5]);
            offset = frame_header_end;
            let end = offset.checked_add(size)?;
            if end > body.len() {
                break;
            }
            match id {
                b"TRK" => {
                    track_number = merge_dispatch_track_number(
                        track_number,
                        parse_id3_text_track_number(&body[offset..end]),
                    );
                }
                b"TPA" => disc_number = parse_id3_text_ordinal(&body[offset..end]).or(disc_number),
                _ => {}
            }
            offset = end;
        } else {
            let frame_header_end = offset.checked_add(10)?;
            if frame_header_end > body.len() {
                break;
            }
            let id = &body[offset..offset + 4];
            if id.iter().all(|byte| *byte == 0) {
                break;
            }
            let size = if major == 4 {
                read_synchsafe_u32(&body[offset + 4..offset + 8])? as usize
            } else {
                u32::from_be_bytes(body[offset + 4..offset + 8].try_into().ok()?) as usize
            };
            offset = frame_header_end;
            let end = offset.checked_add(size)?;
            if end > body.len() {
                break;
            }
            match id {
                b"TRCK" => {
                    track_number = merge_dispatch_track_number(
                        track_number,
                        parse_id3_text_track_number(&body[offset..end]),
                    );
                }
                b"TPOS" => disc_number = parse_id3_text_ordinal(&body[offset..end]).or(disc_number),
                _ => {}
            }
            offset = end;
        }
    }

    track_number.map(|track_number| DispatchTrackOrderContext {
        disc_number,
        track_number,
    })
}

fn id3v2_tag_uses_unsynchronization(flags: u8) -> bool {
    flags & 0x80 != 0
}

fn id3v2_deunsynchronize(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len());
    let mut index = 0;
    while index < data.len() {
        let byte = data[index];
        output.push(byte);
        if byte == 0xff && data.get(index + 1) == Some(&0x00) {
            index += 2;
        } else {
            index += 1;
        }
    }
    output
}

fn id3v2_frame_start_offset(major: u8, flags: u8, body: &[u8]) -> Option<usize> {
    if flags & 0x40 == 0 {
        return Some(0);
    }
    if major == 4 {
        let size = read_synchsafe_u32(body.get(0..4)?)? as usize;
        return size.checked_add(4).filter(|offset| *offset <= body.len());
    }
    if major == 3 {
        let size = u32::from_be_bytes(body.get(0..4)?.try_into().ok()?) as usize;
        return size.checked_add(4).filter(|offset| *offset <= body.len());
    }
    Some(0)
}

fn parse_id3_text_track_number(frame: &[u8]) -> Option<DispatchTrackNumber> {
    let value = decode_id3_text_frame(frame)?;
    Some(parse_dispatch_track_number(value.trim_matches(char::from(0))))
}

fn parse_id3_text_ordinal(frame: &[u8]) -> Option<u32> {
    let value = decode_id3_text_frame(frame)?;
    parse_metadata_ordinal(value.trim_matches(char::from(0)))
}

fn decode_id3_text_frame(frame: &[u8]) -> Option<String> {
    let (&encoding, payload) = frame.split_first()?;
    match encoding {
        0 => Some(payload.iter().map(|byte| char::from(*byte)).collect()),
        3 => String::from_utf8(payload.to_vec()).ok(),
        1 | 2 => decode_utf16_id3_text(encoding, payload),
        _ => None,
    }
}

fn decode_utf16_id3_text(encoding: u8, payload: &[u8]) -> Option<String> {
    let (little_endian, bytes) = if encoding == 1 && payload.len() >= 2 {
        match &payload[..2] {
            [0xff, 0xfe] => (true, &payload[2..]),
            [0xfe, 0xff] => (false, &payload[2..]),
            _ => (false, payload),
        }
    } else {
        (false, payload)
    };
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        let unit = if little_endian {
            u16::from_le_bytes([chunk[0], chunk[1]])
        } else {
            u16::from_be_bytes([chunk[0], chunk[1]])
        };
        units.push(unit);
    }
    String::from_utf16(&units).ok()
}

fn id3v1_track_context(path: &Path) -> Option<(Option<u32>, u32)> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len < 128 {
        return None;
    }
    file.seek(SeekFrom::End(-128)).ok()?;
    let mut tag = [0u8; 128];
    file.read_exact(&mut tag).ok()?;
    parse_id3v1_track_context(&tag)
}

fn parse_id3v1_track_context(tag: &[u8]) -> Option<(Option<u32>, u32)> {
    if tag.len() != 128 || &tag[..3] != b"TAG" {
        return None;
    }
    // ID3v1.1 stores the track number in the comment terminator slot:
    // bytes 97..127 are the comment field, byte 125 must be zero, and
    // byte 126 stores a nonzero track ordinal. ID3v1 has no disc field.
    let track = *tag.get(126)?;
    if *tag.get(125)? != 0 || track == 0 {
        return None;
    }
    Some((None, u32::from(track)))
}

fn apev2_track_order_context(path: &Path) -> Option<DispatchTrackOrderContext> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let footer_offset = apev2_footer_offset(&mut file, len)?;
    file.seek(SeekFrom::Start(footer_offset)).ok()?;
    let mut footer = [0u8; 32];
    file.read_exact(&mut footer).ok()?;
    let tag_size = u32::from_le_bytes(footer[12..16].try_into().ok()?) as u64;
    let item_count = u32::from_le_bytes(footer[16..20].try_into().ok()?) as usize;
    if tag_size < 32 || tag_size > len || tag_size > 16 * 1024 * 1024 {
        return None;
    }
    let tag_start = footer_offset.checked_add(32)?.checked_sub(tag_size)?;
    let item_end = footer_offset;
    if item_end < tag_start {
        return None;
    }
    let mut item_start = tag_start;
    if item_end.checked_sub(item_start)? >= 32 {
        file.seek(SeekFrom::Start(item_start)).ok()?;
        let mut maybe_header = [0u8; 8];
        file.read_exact(&mut maybe_header).ok()?;
        if &maybe_header == b"APETAGEX" {
            item_start = item_start.checked_add(32)?;
        }
    }
    let item_bytes_len = usize::try_from(item_end.checked_sub(item_start)?).ok()?;
    let mut items = vec![0u8; item_bytes_len];
    file.seek(SeekFrom::Start(item_start)).ok()?;
    file.read_exact(&mut items).ok()?;
    parse_apev2_track_order_context(&items, item_count)
}

fn apev2_footer_offset(file: &mut fs::File, len: u64) -> Option<u64> {
    if len < 32 {
        return None;
    }
    if file_has_signature_at(file, len - 32, b"APETAGEX") {
        return Some(len - 32);
    }
    if len >= 160 && file_has_signature_at(file, len - 160, b"APETAGEX") && file_has_signature_at(file, len - 128, b"TAG") {
        return Some(len - 160);
    }
    None
}

fn file_has_signature_at(file: &mut fs::File, offset: u64, signature: &[u8]) -> bool {
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return false;
    }
    let mut buf = vec![0u8; signature.len()];
    file.read_exact(&mut buf).is_ok() && buf == signature
}

#[cfg(test)]
fn parse_apev2_track_context(items: &[u8], item_count: usize) -> Option<(Option<u32>, u32)> {
    numeric_track_context(parse_apev2_track_order_context(items, item_count)?)
}

fn parse_apev2_track_order_context(
    items: &[u8],
    item_count: usize,
) -> Option<DispatchTrackOrderContext> {
    let mut offset = 0usize;
    let mut track_number = None;
    let mut disc_number = None;
    for _ in 0..item_count {
        let header_end = offset.checked_add(8)?;
        if header_end > items.len() {
            return None;
        }
        let value_size = u32::from_le_bytes(items[offset..offset + 4].try_into().ok()?) as usize;
        offset = header_end;
        let key_start = offset;
        while offset < items.len() && items[offset] != 0 {
            offset += 1;
        }
        if offset >= items.len() {
            return None;
        }
        let key = std::str::from_utf8(&items[key_start..offset]).ok()?;
        offset += 1;
        let value_end = offset.checked_add(value_size)?;
        if value_end > items.len() {
            return None;
        }
        let value = std::str::from_utf8(&items[offset..value_end]).ok()?;
        match key.trim().to_ascii_uppercase().as_str() {
            "TRACK" | "TRACKNUMBER" => {
                track_number = merge_dispatch_track_number(
                    track_number,
                    Some(parse_dispatch_track_number(value)),
                );
            }
            "DISC" | "DISCNUMBER" => {
                disc_number = parse_metadata_ordinal(value).or(disc_number);
            }
            _ => {}
        }
        offset = value_end;
    }
    track_number.map(|track_number| DispatchTrackOrderContext {
        disc_number,
        track_number,
    })
}

fn read_synchsafe_u32(bytes: &[u8]) -> Option<u32> {
    if bytes.len() != 4 || bytes.iter().any(|byte| byte & 0x80 != 0) {
        return None;
    }
    Some(
        (u32::from(bytes[0]) << 21)
            | (u32::from(bytes[1]) << 14)
            | (u32::from(bytes[2]) << 7)
            | u32::from(bytes[3]),
    )
}

fn read_le_u32(bytes: &[u8], offset: &mut usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let slice: [u8; 4] = bytes.get(*offset..end)?.try_into().ok()?;
    *offset = end;
    Some(u32::from_le_bytes(slice))
}

fn parse_metadata_ordinal(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    let digits: String = trimmed
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u32>().ok().filter(|value| *value > 0)
}

fn filename_contains_non_prefix_digits(path: &Path) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| {
            let trimmed = stem.trim();
            strict_track_number_from_dispatch_path(path).is_none()
                && trimmed.chars().any(|ch| ch.is_ascii_digit())
        })
        .unwrap_or(false)
}

fn sanitize_album_batch_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();
    let trimmed = sanitized.trim().to_string();
    if trimmed.is_empty() || matches!(trimmed.as_str(), "." | "..") {
        "Album".to_string()
    } else {
        trimmed
    }
}

fn normalized_path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn effective_worker_count_for_tool_limits(
    configured_worker_count: usize,
    tool_concurrency_limits: &ToolConcurrencyLimits,
) -> usize {
    configured_worker_count
        .max(1)
        .max(tool_concurrency_limits.max_tool_concurrency())
}

impl ConversionProcessor {
    /// Create a new processor.
    pub fn new(config: ProcessorConfig) -> Self {
        Self {
            config,
            progress_tx: None,
            lifecycle_tx: None,
            pool_limits: PoolLimits::default(),
            scheduler_metrics: Arc::new(SchedulerMetrics::default()),
            tool_concurrency_limits: Arc::new(ToolConcurrencyLimits::from_available_parallelism()),
            external_cancel: None,
        }
    }

    /// Set progress channel.
    pub fn set_progress_channel(&mut self, tx: broadcast::Sender<ProgressUpdate>) {
        self.progress_tx = Some(tx);
    }

    /// Set lifecycle channel.
    pub fn set_lifecycle_channel(&mut self, tx: mpsc::UnboundedSender<LifecycleEvent>) {
        self.lifecycle_tx = Some(tx);
    }

    /// Set an external cancellation token from the TUI. When triggered,
    /// the scheduler will cancel all workers and kill child processes.
    pub fn set_cancel_token(&mut self, token: CancellationToken) {
        self.external_cancel = Some(token);
    }

    /// Set shared scheduler queue limits. Defaults to the legacy unbounded queue.
    pub fn set_pool_limits(&mut self, limits: PoolLimits) {
        self.pool_limits = limits;
    }

    /// Return a lock-free metrics handle that remains valid while a conversion run is active.
    pub fn scheduler_metrics(&self) -> Arc<SchedulerMetrics> {
        self.scheduler_metrics.clone()
    }

    /// Read a point-in-time snapshot of scheduler metrics.
    pub fn scheduler_metrics_snapshot(&self) -> SchedulerMetricsSnapshot {
        self.scheduler_metrics.snapshot()
    }

    /// Process the conversion queue.
    pub async fn process_queue(&mut self, queue: &mut ConversionQueue) -> ConversionResult<()> {
        let queue_arc = std::sync::Arc::new(tokio::sync::RwLock::new(std::mem::take(queue)));
        let result = self.process_queue_with_progress_arc(queue_arc.clone(), None).await;
        *queue = std::mem::take(&mut *queue_arc.write().await);
        result
    }

    /// Process the conversion queue with fine-grained locking for UI updates.
    pub async fn process_queue_with_progress(
        &mut self,
        queue: std::sync::Arc<tokio::sync::RwLock<ConversionQueue>>,
        progress_rx: Option<broadcast::Receiver<ProgressUpdate>>,
    ) -> ConversionResult<()> {
        self.process_queue_with_progress_arc(queue, progress_rx).await
    }

    async fn process_queue_with_progress_arc(
        &mut self,
        queue: std::sync::Arc<tokio::sync::RwLock<ConversionQueue>>,
        progress_rx: Option<broadcast::Receiver<ProgressUpdate>>,
    ) -> ConversionResult<()> {
        info!("ConversionProcessor::process_queue starting shared pipeline scheduler");

        let progress_tx = if let Some(tx) = &self.progress_tx {
            tx.clone()
        } else {
            let (tx, _rx) = broadcast::channel::<ProgressUpdate>(100);
            self.progress_tx = Some(tx.clone());
            tx
        };

        let mut queued_items: Vec<_> = {
            let q = queue.read().await;
            if let Err(err) = q.validate_full_settings_handoff() {
                return Err(ConversionError::ValidationError(err));
            }
            q.queued_items().into_iter().cloned().collect()
        };
        if queued_items.is_empty() {
            return Ok(());
        }

        for item in queued_items.iter_mut() {
            if item.options.output_dir.is_none() {
                if let Some(ref default_dest) = self.config.default_destination_directory {
                    item.options.output_dir = Some(default_dest.clone());
                }
            }
            let mut q = queue.write().await;
            if let Some(queue_item) = q.find_item_mut(&item.id) {
                queue_item.active_tracks.clear();
                queue_item.closed_track_epochs.clear();
                queue_item.status = ConversionStatus::Processing {
                    progress: 0.0,
                    message: Some(format!("Starting conversion to {}", item.output_format)),
                    file_progress: None,
                    phase: Some(ConversionPhase::Extracting),
                    phase_progress: Some(0.0),
                };
            }
        }

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut queued_items);

        self.scheduler_metrics.reset();
        let effective_worker_count = effective_worker_count_for_tool_limits(
            self.config.worker_count,
            self.tool_concurrency_limits.as_ref(),
        );
        let outcomes = run_queue_with_shared_orchestrator(
            queued_items,
            progress_tx.clone(),
            self.lifecycle_tx.clone(),
            progress_rx,
            self.config.tool_paths.clone(),
            effective_worker_count,
            self.pool_limits,
            self.scheduler_metrics.clone(),
            self.tool_concurrency_limits.clone(),
            self.config.scratch_directory.clone(),
            self.config.scratch_memory_limit_percent,
            self.external_cancel.take(),
        )
        .await;

        for (item_id, final_status, last_progress) in outcomes {
            let progress = terminal_progress_for_status(&final_status, last_progress);
            let _ = progress_tx.send(ProgressUpdate {
                item_id: item_id.clone(),
                track_index: None,
                track_epoch: None,
                progress,
                status: final_status.clone(),
            });
            let mut q = queue.write().await;
            if let Some(queue_item) = q.find_item_mut(&item_id) {
                queue_item.active_tracks.clear();
                queue_item.closed_track_epochs.clear();
                queue_item.status = final_status;
                queue_item.completed_at = Some(chrono::Utc::now());
            }
        }

        // Move finished items from the active queue to the completed list so
        // completed_items() / failed_items() report correctly. The shared
        // scheduler sets status via find_item_mut but doesn't call next_item.
        {
            let mut q = queue.write().await;
            q.settle_finished();
        }

        Ok(())
    }
}

struct PendingAlbum {
    album: Option<ScheduledAlbum>,
    outputs: Vec<ScheduledTrackOutput>,
    finished: usize,
    expected: usize,
    next_source_track: usize,
    job_cancel: CancellationToken,
    cancel_requested: bool,
}

enum QueueWorkOutput {
    Materialized {
        item_id: String,
        result: ScheduledMaterialization,
    },
    Realized {
        job_id: String,
        track: ScheduledRealizedTrack,
    },
    Encoded {
        job_id: String,
        output: ScheduledTrackOutput,
    },
    PostProcessed {
        item_id: String,
        status: ConversionStatus,
    },
    #[cfg(test)]
    SyntheticEncoded {
        job_id: String,
        #[allow(dead_code)]
        track_index: usize,
    },
}

#[cfg(test)]
struct SyntheticFanout {
    job_id: String,
    _item_id: String,
    remaining: usize,
    next_index: usize,
}

struct DeferredSubmission {
    unit: WorkUnit<QueueWorkOutput>,
    counted: bool,
}

struct SubmissionPump {
    initial_items: VecDeque<ConversionItem>,
    album_fanout: VecDeque<String>,
    realized_encodes: VecDeque<(String, ScheduledRealizedTrack, CancellationToken)>,
    album_postprocess: VecDeque<(ScheduledAlbum, Vec<ScheduledTrackOutput>)>,
    #[cfg(test)]
    synthetic_fanout: VecDeque<SyntheticFanout>,
    deferred_unit: Option<DeferredSubmission>,
    retry_after_busy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PumpFlushProgress {
    Progress,
    Blocked,
    Empty,
}

impl SubmissionPump {
    fn new(items: Vec<ConversionItem>) -> Self {
        Self {
            initial_items: items.into_iter().collect(),
            album_fanout: VecDeque::new(),
            realized_encodes: VecDeque::new(),
            album_postprocess: VecDeque::new(),
            #[cfg(test)]
            synthetic_fanout: VecDeque::new(),
            deferred_unit: None,
            retry_after_busy: false,
        }
    }

    fn enqueue_album_fanout(&mut self, job_id: String) {
        self.album_fanout.push_back(job_id);
    }

    fn enqueue_realized_encode(
        &mut self,
        job_id: String,
        realized: ScheduledRealizedTrack,
        job_cancel: CancellationToken,
    ) {
        self.realized_encodes.push_back((job_id, realized, job_cancel));
    }

    fn enqueue_album_postprocess(
        &mut self,
        album: ScheduledAlbum,
        outputs: Vec<ScheduledTrackOutput>,
    ) {
        self.album_postprocess.push_back((album, outputs));
    }

    #[cfg(test)]
    fn enqueue_synthetic_album_fanout(
        &mut self,
        job_id: String,
        item_id: String,
        tracks: usize,
        metrics: &SchedulerMetrics,
    ) {
        if tracks > 0 {
            metrics.record_jobs_queued(tracks as u64);
            self.synthetic_fanout.push_back(SyntheticFanout {
                job_id,
                _item_id: item_id,
                remaining: tracks,
                next_index: 0,
            });
        }
    }

    fn should_retry_after_busy(&self) -> bool {
        self.retry_after_busy
    }

    fn try_submit_unit(
        &mut self,
        pool: &SharedWorkerPool<QueueWorkOutput>,
        unit: WorkUnit<QueueWorkOutput>,
        counted: bool,
    ) -> bool {
        let submitted = if counted {
            pool.try_submit_counted(unit)
        } else {
            pool.try_submit(unit)
        };

        match submitted {
            Ok(()) => true,
            Err(TrySubmitError::QueueBusy(unit)) => {
                if !counted {
                    pool.metrics().record_jobs_queued(1);
                }
                self.retry_after_busy = true;
                self.deferred_unit = Some(DeferredSubmission { unit, counted: true });
                false
            }
            Err(TrySubmitError::QueueFull { unit, .. }) => {
                if !counted {
                    pool.metrics().record_jobs_queued(1);
                }
                self.retry_after_busy = false;
                self.deferred_unit = Some(DeferredSubmission { unit, counted: true });
                false
            }
        }
    }

    fn flush_album_fanout_once<F>(
        &mut self,
        pool: &SharedWorkerPool<QueueWorkOutput>,
        mut next_source_work: F,
    ) -> PumpFlushProgress
    where
        F: FnMut(&str) -> Option<WorkUnit<QueueWorkOutput>>,
    {
        let Some(job_id) = self.album_fanout.pop_front() else {
            return PumpFlushProgress::Empty;
        };

        let Some(unit) = next_source_work(&job_id) else {
            return PumpFlushProgress::Progress;
        };

        self.album_fanout.push_back(job_id);
        if self.try_submit_unit(pool, unit, true) {
            PumpFlushProgress::Progress
        } else {
            PumpFlushProgress::Blocked
        }
    }

    #[cfg(test)]
    fn flush_synthetic_fanout_once(
        &mut self,
        pool: &SharedWorkerPool<QueueWorkOutput>,
    ) -> PumpFlushProgress {
        let Some(mut fanout) = self.synthetic_fanout.pop_front() else {
            return PumpFlushProgress::Empty;
        };

        if fanout.remaining == 0 {
            return PumpFlushProgress::Progress;
        }

        let track_index = fanout.next_index;
        fanout.next_index += 1;
        fanout.remaining -= 1;
        let job_id = fanout.job_id.clone();
        let unit_id = format!("synthetic-encode-{track_index:04}");
        let unit = synthetic_queue_work_unit_for_processor_test(&job_id, unit_id, track_index);
        if fanout.remaining > 0 {
            self.synthetic_fanout.push_back(fanout);
        }

        if self.try_submit_unit(pool, unit, true) {
            PumpFlushProgress::Progress
        } else {
            PumpFlushProgress::Blocked
        }
    }

    fn flush(
        &mut self,
        pool: &SharedWorkerPool<QueueWorkOutput>,
        pending_albums: &mut BTreeMap<String, PendingAlbum>,
        terminal: &mut BTreeMap<String, ConversionStatus>,
        job_to_item: &mut BTreeMap<String, String>,
        tool_paths: &HashMap<String, PathBuf>,
        version_cache: Arc<Mutex<HashMap<ToolBinary, String>>>,
        progress_tx: &broadcast::Sender<ProgressUpdate>,
        lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
        _cancel: &CancellationToken,
        worker_count: usize,
        tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
        scratch_staging: Option<ScratchStagingConfig>,
    ) {
        self.retry_after_busy = false;
        loop {
            if let Some(deferred) = self.deferred_unit.take() {
                if !self.try_submit_unit(pool, deferred.unit, deferred.counted) {
                    return;
                }
                continue;
            }

            if let Some((album, outputs)) = self.album_postprocess.pop_front() {
                let unit = build_album_postprocess_work(
                    album,
                    outputs,
                    tool_paths,
                    version_cache.clone(),
                    progress_tx,
                    lifecycle_tx,
                    tool_concurrency_limits.clone(),
                );
                if !self.try_submit_unit(pool, unit, true) {
                    return;
                }
                continue;
            }

            if let Some((job_id, realized, job_cancel)) = self.realized_encodes.pop_front() {
                let unit = build_realized_encode_work(
                    job_id,
                    realized,
                    tool_paths,
                    version_cache.clone(),
                    progress_tx,
                    lifecycle_tx,
                    job_cancel,
                    tool_concurrency_limits.clone(),
                );
                if !self.try_submit_unit(pool, unit, true) {
                    return;
                }
                continue;
            }

            match self.flush_album_fanout_once(pool, |job_id| {
                next_album_source_work(
                    pending_albums,
                    job_id,
                    tool_paths,
                    version_cache.clone(),
                    progress_tx,
                    lifecycle_tx,
                    tool_concurrency_limits.clone(),
                )
            }) {
                PumpFlushProgress::Progress => continue,
                PumpFlushProgress::Blocked => return,
                PumpFlushProgress::Empty => {}
            }

            #[cfg(test)]
            match self.flush_synthetic_fanout_once(pool) {
                PumpFlushProgress::Progress => continue,
                PumpFlushProgress::Blocked => return,
                PumpFlushProgress::Empty => {}
            }

            if let Some(item) = self.initial_items.pop_front() {
                if let Some(unit) = build_initial_work(
                    item,
                    pool,
                    terminal,
                    job_to_item,
                    tool_paths,
                    version_cache.clone(),
                    progress_tx,
                    lifecycle_tx,
                    worker_count,
                    tool_concurrency_limits.clone(),
                    scratch_staging.clone(),
                ) {
                    if !self.try_submit_unit(pool, unit, false) {
                        return;
                    }
                }
                continue;
            }

            break;
        }
    }

    fn record_backlog(
        &self,
        metrics: &SchedulerMetrics,
        pending_albums: &BTreeMap<String, PendingAlbum>,
    ) {
        metrics.record_submission_backlog_depth(self.submission_backlog_depth(pending_albums));
    }

    fn submission_backlog_depth(&self, pending_albums: &BTreeMap<String, PendingAlbum>) -> usize {
        self.initial_items
            .len()
            .saturating_add(usize::from(self.deferred_unit.is_some()))
            .saturating_add(self.realized_encodes.len())
            .saturating_add(self.album_postprocess.len())
            .saturating_add(self.pending_album_source_units(pending_albums))
            .saturating_add(self.synthetic_fanout_backlog_depth())
    }

    #[cfg(test)]
    fn synthetic_fanout_backlog_depth(&self) -> usize {
        self.synthetic_fanout.iter().map(|fanout| fanout.remaining).sum()
    }

    #[cfg(not(test))]
    fn synthetic_fanout_backlog_depth(&self) -> usize {
        0
    }

    fn pending_album_source_units(&self, pending_albums: &BTreeMap<String, PendingAlbum>) -> usize {
        self.album_fanout
            .iter()
            .filter_map(|job_id| pending_albums.get(job_id))
            .map(|pending| {
                let Some(album) = pending.album.as_ref() else {
                    return 0;
                };
                album
                    .source
                    .tracks
                    .iter()
                    .skip(pending.next_source_track)
                    .filter(|track| album.planned_final_path(&track.id).is_some())
                    .count()
            })
            .sum()
    }

    #[cfg(test)]
    fn pending_work_units(&self) -> usize {
        usize::from(self.deferred_unit.is_some())
    }
}

fn source_warning_count(source: Option<&crate::convert::pipeline::PreparedSource>) -> u32 {
    source.map_or(0, |source| {
        source.tracks.iter().fold(0_u32, |total, track| {
            total.saturating_add(u32::try_from(track.warnings.len()).unwrap_or(u32::MAX))
        })
    })
}

fn record_terminal_status(
    pool: &SharedWorkerPool<QueueWorkOutput>,
    status: &ConversionStatus,
) {
    match status {
        ConversionStatus::Completed { .. }
        | ConversionStatus::CompletedWithActionErrors { .. }
        | ConversionStatus::Partial { .. } => {
            pool.metrics().record_job_completed();
        }
        ConversionStatus::Failed { .. } | ConversionStatus::Cancelled => {
            pool.metrics().record_job_failed();
        }
        ConversionStatus::Queued
        | ConversionStatus::Paused
        | ConversionStatus::Interrupted
        | ConversionStatus::Processing { .. }
        | ConversionStatus::NotConfigured => {}
    }
}

async fn run_queue_with_shared_orchestrator(
    queued_items: Vec<ConversionItem>,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    lifecycle_tx: Option<mpsc::UnboundedSender<LifecycleEvent>>,
    mut progress_rx: Option<broadcast::Receiver<ProgressUpdate>>,
    tool_paths: HashMap<String, PathBuf>,
    worker_count: usize,
    pool_limits: PoolLimits,
    metrics: Arc<SchedulerMetrics>,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
    scratch_directory: Option<PathBuf>,
    scratch_memory_limit_percent: u8,
    external_cancel: Option<CancellationToken>,
) -> Vec<(String, ConversionStatus, Option<f32>)> {
    let cancel = external_cancel.unwrap_or_else(CancellationToken::new);
    let pool = SharedWorkerPool::<QueueWorkOutput>::new_with_limits_and_metrics(
        Some(worker_count.max(1)),
        cancel.clone(),
        pool_limits,
        metrics,
    );
    let total_items = queued_items.len();
    let scratch_staging = scratch_staging_config_for_run(
        scratch_directory,
        scratch_memory_limit_percent,
    );
    let mut submissions = SubmissionPump::new(queued_items);
    let version_cache = Arc::new(Mutex::new(HashMap::new()));
    let mut run = pool.start();
    let mut pending_albums: BTreeMap<String, PendingAlbum> = BTreeMap::new();
    let mut tracker = AlbumCompletionTracker::default();
    let mut terminal: BTreeMap<String, ConversionStatus> = BTreeMap::new();
    let mut job_to_item: BTreeMap<String, String> = BTreeMap::new();
    let mut last_progress_by_item: HashMap<String, f32> = HashMap::new();
    submissions.record_backlog(pool.metrics(), &pending_albums);

    while terminal.len() < total_items {
        submissions.flush(
            &pool,
            &mut pending_albums,
            &mut terminal,
            &mut job_to_item,
            &tool_paths,
            version_cache.clone(),
            &progress_tx,
            lifecycle_tx.as_ref(),
            &cancel,
            worker_count.max(1),
            tool_concurrency_limits.clone(),
            scratch_staging.clone(),
        );
        submissions.record_backlog(pool.metrics(), &pending_albums);
        if terminal.len() >= total_items {
            break;
        }
        tokio::select! {
            _ = tokio::task::yield_now(), if submissions.should_retry_after_busy() => {},
            Ok(update) = async {
                if let Some(ref mut rx) = progress_rx {
                    rx.recv().await
                } else {
                    std::future::pending::<Result<ProgressUpdate, broadcast::error::RecvError>>().await
                }
            }, if progress_rx.is_some() => {
                last_progress_by_item.insert(update.item_id.clone(), update.progress);
            }
            Some(result) = run.results.recv() => {
                match result.outcome {
                    Ok(QueueWorkOutput::Materialized { item_id, result }) => {
                        match result {
                            ScheduledMaterialization::Finished(report) => {
                                let warning_count = source_warning_count(report.source.as_ref());
                                let status = map_album_outcome(
                                    &report.outcome,
                                    report.published.as_ref(),
                                    report.durable_log.as_deref(),
                                    warning_count,
                                );
                                if terminal.insert(item_id, status.clone()).is_none() {
                                    record_terminal_status(&pool, &status);
                                }
                            }
                            ScheduledMaterialization::Ready(album) => {
                                let job_id = album.req.job_id.clone();
                                let expected = album
                                    .source
                                    .tracks
                                    .iter()
                                    .filter(|track| album.planned_final_path(&track.id).is_some())
                                    .count();
                                let staged_tracks_ready_for_encode = album
                                    .source
                                    .tracks
                                    .iter()
                                    .filter(|track| album.planned_final_path(&track.id).is_some())
                                    .filter(|track| matches!(&track.source_ref, TrackSourceRef::StagedFile(_)))
                                    .count();
                                pool.metrics()
                                    .record_tracks_materialized(staged_tracks_ready_for_encode as u64);
                                tracker.register_album(job_id.clone(), expected, album.allow_partial());
                                pool.metrics().record_jobs_queued(expected as u64);
                                if expected == 0 {
                                    pool.metrics().record_jobs_queued(1);
                                    submissions.enqueue_album_postprocess(album, Vec::new());
                                    submissions.record_backlog(pool.metrics(), &pending_albums);
                                } else {
                                    let job_cancel = cancel.child_token();
                                    pending_albums.insert(job_id.clone(), PendingAlbum {
                                        album: Some(album),
                                        outputs: Vec::with_capacity(expected),
                                        finished: 0,
                                        expected,
                                        next_source_track: 0,
                                        job_cancel,
                                        cancel_requested: false,
                                    });
                                    submissions.enqueue_album_fanout(job_id.clone());
                                    submissions.record_backlog(pool.metrics(), &pending_albums);
                                }
                            }
                        }
                    }
                    Ok(QueueWorkOutput::Realized { job_id, track }) => {
                        if pending_albums.contains_key(&job_id) {
                            pool.metrics().record_tracks_materialized(1);
                            let job_cancel = pending_albums
                                .get(&job_id)
                                .map(|pending| pending.job_cancel.clone())
                                .unwrap_or_else(|| cancel.child_token());
                            pool.metrics().record_jobs_queued(1);
                            submissions.enqueue_realized_encode(job_id, track, job_cancel);
                            submissions.record_backlog(pool.metrics(), &pending_albums);
                        }
                    }
                    Ok(QueueWorkOutput::Encoded { job_id, output }) => {
                        if !output.ok {
                            log::warn!("track encode failed: job={job_id} track={} outcome={:?}", output.index, output.record.outcome);
                        }
                        pool.metrics().record_track_encode_output(output.ok);
                        let readiness = tracker.mark_track_finished(&job_id, output.ok);
                        let mut submit_postprocess: Option<(ScheduledAlbum, Vec<ScheduledTrackOutput>)> = None;

                        if let Some(pending) = pending_albums.get_mut(&job_id) {
                            pending.finished += 1;
                            pending.outputs.push(output);

                            match readiness {
                                AlbumReadiness::Waiting { .. } | AlbumReadiness::ReadyForPostProcess => {}
                                AlbumReadiness::Failed { .. } => {
                                    // Do not remove the pending album on the first failure.
                                    // Fail-fast policy cancels not-yet-started and in-flight units,
                                    // but each expected track must still report a deterministic
                                    // terminal ScheduledTrackOutput before album post-processing,
                                    // durable logging, and queue accounting run.
                                    if !pending.cancel_requested {
                                        pending.job_cancel.cancel();
                                        pending.cancel_requested = true;
                                    }
                                }
                            }

                            if pending.finished >= pending.expected {
                                if let Some(mut pending) = pending_albums.remove(&job_id) {
                                    pending.outputs.sort_by_key(|output| output.index);
                                    if let Some(album) = pending.album.take() {
                                        submit_postprocess = Some((album, pending.outputs));
                                    }
                                }
                            }
                        } else {
                            log::error!(
                                "encoded track output arrived after album accounting closed: job={} track_index={}",
                                job_id,
                                output.index
                            );
                        }

                        if let Some((album, outputs)) = submit_postprocess {
                            pool.metrics().record_jobs_queued(1);
                            submissions.enqueue_album_postprocess(album, outputs);
                            submissions.record_backlog(pool.metrics(), &pending_albums);
                        }
                    }
                    Ok(QueueWorkOutput::PostProcessed { item_id, status }) => {
                        if terminal.insert(item_id, status.clone()).is_none() {
                            record_terminal_status(&pool, &status);
                        }
                    }
                    #[cfg(test)]
                    Ok(QueueWorkOutput::SyntheticEncoded { job_id: _, track_index: _ }) => {
                        // Test-only processor harness output. Production workers never emit this variant.
                    }
                    Err(err) => {
                        let item_id = job_to_item
                            .get(&result.job_id)
                            .cloned()
                            .unwrap_or_else(|| result.job_id.clone());
                        log::error!(
                            "shared scheduler work unit failed: job={} unit={} kind={:?}: {err}",
                            result.job_id,
                            result.unit_id,
                            result.kind
                        );
                        if !terminal.contains_key(&item_id) {
                            pool.metrics().record_job_failed();
                            terminal.insert(item_id, ConversionStatus::Failed {
                                error: format!("scheduler work unit failed: {err}"),
                                log_path: None,
                            });
                        }
                    }
                }
            }
            else => break,
        }
    }

    submissions.record_backlog(pool.metrics(), &pending_albums);
    pool.metrics().record_submission_backlog_depth(0);
    cancel.cancel();
    run.shutdown().await;

    terminal
        .into_iter()
        .map(|(item_id, status)| {
            let last = last_progress_by_item.get(&item_id).copied();
            (item_id, status, last)
        })
        .collect()
}

fn build_initial_work(
    mut item: ConversionItem,
    pool: &SharedWorkerPool<QueueWorkOutput>,
    terminal: &mut BTreeMap<String, ConversionStatus>,
    job_to_item: &mut BTreeMap<String, String>,
    tool_paths: &HashMap<String, PathBuf>,
    version_cache: Arc<Mutex<HashMap<ToolBinary, String>>>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
    worker_count: usize,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
    scratch_staging: Option<ScratchStagingConfig>,
) -> Option<WorkUnit<QueueWorkOutput>> {
    let item_id = item.id.clone();
    if !item.input_path.exists() {
        pool.metrics().record_job_failed();
        terminal.insert(
            item_id.clone(),
            ConversionStatus::Failed {
                error: format!("Source file not found: {}", item.input_path.display()),
                log_path: None,
            },
        );
        return None;
    }

    if let Err(error) = item.resolve_archive_password_for_execution() {
        pool.metrics().record_job_failed();
        terminal.insert(
            item_id.clone(),
            ConversionStatus::Failed {
                error,
                log_path: None,
            },
        );
        return None;
    }

    let request = match build_pipeline_request(&item) {
        Ok(mut req) => {
            apply_companion_policy_from_item(&mut req, &item);
            apply_scratch_staging_from_run(&mut req, &scratch_staging);
            req.worker_count = Some(worker_count.max(1));
            req
        }
        Err(err) => {
            pool.metrics().record_job_failed();
            terminal.insert(
                item_id.clone(),
                ConversionStatus::Failed {
                    error: err.to_string(),
                    log_path: None,
                },
            );
            return None;
        }
    };

    job_to_item.insert(request.job_id.clone(), item_id.clone());

    let source_kind = detect_source_kind(&request).ok();
    if matches!(source_kind, Some(SourceKind::SingleFile)) {
        return Some(build_single_file_work(
            request,
            tool_paths,
            version_cache.clone(),
            progress_tx,
            lifecycle_tx,
            tool_concurrency_limits,
        ));
    }

    let Some(source_kind) = source_kind else {
        pool.metrics().record_job_failed();
        terminal.insert(
            item_id.clone(),
            ConversionStatus::Failed {
                error: format!(
                    "Unsupported conversion source: {}. Regular audio folders must be expanded into supported audio files before queue processing.",
                    request.container.display()
                ),
                log_path: None,
            },
        );
        return None;
    };

    let materialize_kind = match source_kind {
        SourceKind::Archive => WorkKind::ArchiveExtract,
        SourceKind::CueImage => WorkKind::MaterializeItem,
        SourceKind::SacdIso => WorkKind::MaterializeItem,
        SourceKind::DvdAudio => WorkKind::MaterializeItem,
        SourceKind::DvdVideo => WorkKind::MaterializeItem,
        SourceKind::BluRay => WorkKind::MaterializeItem,
        SourceKind::SingleFile => unreachable!("single files are submitted as immediate work units"),
    };
    let unit_prefix = match source_kind {
        SourceKind::Archive => "archive-extract",
        SourceKind::CueImage => "cue-materialize",
        SourceKind::SacdIso => "sacd-materialize",
        SourceKind::DvdAudio => "dvda-materialize",
        SourceKind::DvdVideo => "dvdv-materialize",
        SourceKind::BluRay => "bluray-materialize",
        SourceKind::SingleFile => unreachable!("single files are submitted as immediate work units"),
    };
    let submit_tool_paths = tool_paths.clone();
    let submit_version_cache = version_cache.clone();
    let submit_progress_tx = progress_tx.clone();
    let submit_item_id = item_id.clone();
    Some(WorkUnit {
        job_id: request.job_id.clone(),
        unit_id: format!("{unit_prefix}:{submit_item_id}"),
        kind: materialize_kind,
        task: boxed_work(move |worker_cancel| async move {
            let runner = RealToolRunner::with_version_cache(submit_tool_paths.clone(), submit_version_cache.clone());
            let reporter = BroadcastReporter::new(submit_progress_tx, None, submit_item_id.clone(), None);
            let result = prepare_pipeline_item_for_scheduler(
                request,
                &runner,
                &reporter,
                &worker_cancel,
                &submit_tool_paths,
            )
            .await;
            Ok(QueueWorkOutput::Materialized {
                item_id: submit_item_id,
                result,
            })
        }),
    })
}

fn build_single_file_work(
    request: PipelineRequest,
    tool_paths: &HashMap<String, PathBuf>,
    version_cache: Arc<Mutex<HashMap<ToolBinary, String>>>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    _lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
) -> WorkUnit<QueueWorkOutput> {
    let item_id = request.item_id.clone();
    let job_id = request.job_id.clone();
    let tool_paths = tool_paths.clone();
    let version_cache = version_cache.clone();
    let progress_tx = progress_tx.clone();
    let tool_concurrency_limits = tool_concurrency_limits.clone();
    WorkUnit {
        job_id,
        unit_id: format!("single-file:{item_id}"),
        kind: WorkKind::SingleFile,
        task: boxed_work(move |worker_cancel| async move {
            let runner = RealToolRunner::with_version_cache(tool_paths.clone(), version_cache.clone());
            let reporter = BroadcastReporter::new(progress_tx, None, item_id.clone(), None);
            let report = run_pipeline_item_with_tool_paths_and_tool_limits(
                request,
                &runner,
                &reporter,
                &worker_cancel,
                &tool_paths,
                Some(tool_concurrency_limits),
            )
            .await;
            let warning_count = source_warning_count(report.source.as_ref());
            let status = map_album_outcome(
                &report.outcome,
                report.published.as_ref(),
                report.durable_log.as_deref(),
                warning_count,
            );
            Ok(QueueWorkOutput::PostProcessed { item_id, status })
        }),
    }
}

fn next_album_source_work(
    pending_albums: &mut BTreeMap<String, PendingAlbum>,
    job_id: &str,
    tool_paths: &HashMap<String, PathBuf>,
    version_cache: Arc<Mutex<HashMap<ToolBinary, String>>>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
) -> Option<WorkUnit<QueueWorkOutput>> {
    let pending = pending_albums.get_mut(job_id)?;
    loop {
        let track_index = pending.next_source_track;
        let album = pending.album.as_ref()?;
        if track_index >= album.source.tracks.len() {
            return None;
        }

        let track = album.source.tracks[track_index].clone();
        pending.next_source_track += 1;
        let Some(final_path) = album.planned_final_path(&track.id) else {
            log::error!("missing planned final path for {}", track.id.source_ordinal);
            continue;
        };

        let convert_root = album.convert_root();
        let _ = std::fs::create_dir_all(&convert_root);
        return Some(match &track.source_ref {
            TrackSourceRef::StagedFile(_) => {
                let kind = if album.source.kind == SourceKind::SingleFile {
                    WorkKind::SingleFile
                } else {
                    WorkKind::EncodeTrack { track_id: track.id.clone() }
                };
                build_staged_encode_work(
                    album,
                    track_index,
                    track,
                    final_path,
                    convert_root,
                    kind,
                    tool_paths,
                    version_cache.clone(),
                    progress_tx,
                    lifecycle_tx,
                    pending.job_cancel.clone(),
                    tool_concurrency_limits.clone(),
                )
            }
            TrackSourceRef::CueSegmentCarrier { .. }
            | TrackSourceRef::ImageSegment { .. }
            | TrackSourceRef::SacdTrack { .. }
            | TrackSourceRef::DvdaTrack { .. }
            | TrackSourceRef::DvdVideoTrack { .. }
            | TrackSourceRef::BluRayTrack { .. } => {
                build_realize_work(
                    album,
                    track_index,
                    track,
                    final_path,
                    convert_root,
                    tool_paths,
                    version_cache.clone(),
                    progress_tx,
                    lifecycle_tx,
                    pending.job_cancel.clone(),
                    tool_concurrency_limits.clone(),
                )
            }
        });
    }
}

fn build_realize_work(
    album: &ScheduledAlbum,
    track_index: usize,
    track: crate::convert::pipeline::PreparedTrack,
    final_path: PathBuf,
    convert_root: PathBuf,
    tool_paths: &HashMap<String, PathBuf>,
    version_cache: Arc<Mutex<HashMap<ToolBinary, String>>>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
    job_cancel: CancellationToken,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
) -> WorkUnit<QueueWorkOutput> {
    let req = album.req.clone();
    let staging_root = album.staging.root.clone();
    let staging_job = album.staging.job_id.clone();
    let tool_paths = tool_paths.clone();
    let version_cache = version_cache.clone();
    let progress_tx = progress_tx.clone();
    let lifecycle_tx = lifecycle_tx.cloned();
    let tool_concurrency_limits = tool_concurrency_limits.clone();
    let job_id = album.req.job_id.clone();
    let item_id = album.req.item_id.clone();
    let track_id = track.id.clone();
    let kind = match &track.source_ref {
        TrackSourceRef::CueSegmentCarrier { .. }
        | TrackSourceRef::ImageSegment { .. } => WorkKind::CueSplitTrack { track_id: track_id.clone() },
        TrackSourceRef::SacdTrack { .. } => WorkKind::SacdExtractTrack { track_id: track_id.clone() },
        TrackSourceRef::DvdaTrack { .. } => WorkKind::MaterializeItem, // Phase 3: DvdaExtractTrack
        TrackSourceRef::DvdVideoTrack { .. } => WorkKind::MaterializeItem,
        TrackSourceRef::BluRayTrack { .. } => WorkKind::MaterializeItem,
        TrackSourceRef::StagedFile(_) => WorkKind::EncodeTrack { track_id: track_id.clone() },
    };
    WorkUnit {
        job_id: job_id.clone(),
        unit_id: format!("realize-track:{:04}", track_id.source_ordinal),
        kind,
        task: boxed_work(move |_worker_cancel| async move {
            let reporter = BroadcastReporter::new(progress_tx, lifecycle_tx, item_id, Some(track_index as u32));
            let _guard = reporter.track_lifecycle_guard();
            if job_cancel.is_cancelled() {
                let output = scheduled_worker_failure_output(
                    track_index,
                    &track,
                    None,
                    Some(final_path.clone()),
                    "album cancelled before track realization".to_string(),
                );
                return Ok(QueueWorkOutput::Encoded { job_id, output });
            }
            match realize_track_for_scheduler_with_tool_limits_and_version_cache(
                track_index,
                track,
                final_path,
                req,
                staging_root,
                staging_job,
                convert_root,
                tool_paths,
                Some(tool_concurrency_limits),
                Some(version_cache),
                &reporter,
                job_cancel,
            )
            .await
            {
                Ok(track) => Ok(QueueWorkOutput::Realized { job_id, track }),
                Err(output) => {
                    log::warn!("track realization failed: {:?}", output.record.outcome);
                    Ok(QueueWorkOutput::Encoded { job_id, output })
                }
            }
        }),
    }
}

fn build_staged_encode_work(
    album: &ScheduledAlbum,
    track_index: usize,
    track: crate::convert::pipeline::PreparedTrack,
    final_path: PathBuf,
    convert_root: PathBuf,
    kind: WorkKind,
    tool_paths: &HashMap<String, PathBuf>,
    version_cache: Arc<Mutex<HashMap<ToolBinary, String>>>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
    job_cancel: CancellationToken,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
) -> WorkUnit<QueueWorkOutput> {
    let req = album.req.clone();
    let staging_root = album.staging.root.clone();
    let staging_job = album.staging.job_id.clone();
    let tool_paths = tool_paths.clone();
    let version_cache = version_cache.clone();
    let progress_tx = progress_tx.clone();
    let lifecycle_tx = lifecycle_tx.cloned();
    let tool_concurrency_limits = tool_concurrency_limits.clone();
    let job_id = album.req.job_id.clone();
    let item_id = album.req.item_id.clone();
    let track_id = track.id.clone();
    WorkUnit {
        job_id: job_id.clone(),
        unit_id: format!("encode-track:{:04}", track_id.source_ordinal),
        kind,
        task: boxed_work(move |_worker_cancel| async move {
            let reporter = BroadcastReporter::new(progress_tx, lifecycle_tx, item_id, Some(track_index as u32));
            let _guard = reporter.track_lifecycle_guard();
            if job_cancel.is_cancelled() {
                let output = scheduled_worker_failure_output(
                    track_index,
                    &track,
                    None,
                    Some(final_path.clone()),
                    "album cancelled before staged encode".to_string(),
                );
                return Ok(QueueWorkOutput::Encoded { job_id, output });
            }
            let output = match encode_track_for_scheduler_with_tool_limits_and_version_cache(
                track_index,
                track.clone(),
                final_path.clone(),
                req,
                staging_root,
                staging_job,
                convert_root,
                tool_paths,
                Some(tool_concurrency_limits),
                Some(version_cache),
                &reporter,
                job_cancel,
            )
            .await
            {
                Ok(output) => output,
                Err(err) => {
                    log::warn!("staged encode worker failed for track {}: {err}", track_index);
                    scheduled_worker_failure_output(
                        track_index,
                        &track,
                        None,
                        Some(final_path),
                        format!("encode worker failed: {err}"),
                    )
                }
            };
            Ok(QueueWorkOutput::Encoded { job_id, output })
        }),
    }
}

fn build_realized_encode_work(
    job_id: String,
    realized: ScheduledRealizedTrack,
    tool_paths: &HashMap<String, PathBuf>,
    version_cache: Arc<Mutex<HashMap<ToolBinary, String>>>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
    job_cancel: CancellationToken,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
) -> WorkUnit<QueueWorkOutput> {
    let tool_paths = tool_paths.clone();
    let version_cache = version_cache.clone();
    let progress_tx = progress_tx.clone();
    let lifecycle_tx = lifecycle_tx.cloned();
    let tool_concurrency_limits = tool_concurrency_limits.clone();
    let item_id = realized.req.item_id.clone();
    let track_id = realized.track.id.clone();
    let unit_index = realized.index;
    let failure_track = realized.track.clone();
    let failure_realized_input = Some(realized.realized_path.clone());
    let failure_final_path = Some(realized.final_path.clone());
    WorkUnit {
        job_id: job_id.clone(),
        unit_id: format!("encode-realized-track:{:04}", track_id.source_ordinal),
        kind: WorkKind::EncodeTrack { track_id },
        task: boxed_work(move |_worker_cancel| async move {
            let reporter = BroadcastReporter::new(progress_tx, lifecycle_tx, item_id, Some(unit_index as u32));
            let _guard = reporter.track_lifecycle_guard();
            if job_cancel.is_cancelled() {
                let output = scheduled_worker_failure_output(
                    unit_index,
                    &failure_track,
                    failure_realized_input.clone(),
                    failure_final_path.clone(),
                    "album cancelled before realized encode".to_string(),
                );
                return Ok(QueueWorkOutput::Encoded { job_id, output });
            }
            let output = match encode_realized_track_for_scheduler_with_tool_limits_and_version_cache(
                realized,
                tool_paths,
                Some(tool_concurrency_limits),
                Some(version_cache),
                &reporter,
            )
            .await
            {
                Ok(output) => output,
                Err(err) => {
                    log::warn!("realized encode worker failed for track {}: {err}", unit_index);
                    scheduled_worker_failure_output(
                        unit_index,
                        &failure_track,
                        failure_realized_input.clone(),
                        failure_final_path.clone(),
                        format!("realized encode worker failed: {err}"),
                    )
                }
            };
            Ok(QueueWorkOutput::Encoded { job_id, output })
        }),
    }
}

fn build_album_postprocess_work(
    album: ScheduledAlbum,
    outputs: Vec<ScheduledTrackOutput>,
    tool_paths: &HashMap<String, PathBuf>,
    version_cache: Arc<Mutex<HashMap<ToolBinary, String>>>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    _lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
) -> WorkUnit<QueueWorkOutput> {
    let item_id = album.req.item_id.clone();
    let job_id = album.req.job_id.clone();
    let unit_id = format!("album-postprocess:{item_id}");
    let tool_paths = tool_paths.clone();
    let version_cache = version_cache.clone();
    let progress_tx = progress_tx.clone();
    let tool_concurrency_limits = tool_concurrency_limits.clone();
    WorkUnit {
        job_id,
        unit_id,
        kind: WorkKind::AlbumPostProcess,
        task: boxed_work(move |worker_cancel| async move {
            Ok(run_album_postprocess_work(
                album,
                outputs,
                tool_paths,
                version_cache,
                progress_tx,
                tool_concurrency_limits,
                worker_cancel,
            )
            .await)
        }),
    }
}


fn scratch_track_retry_original_error(outputs: &[ScheduledTrackOutput]) -> String {
    outputs
        .iter()
        .find_map(|output| match &output.record.outcome {
            TrackOutcome::Err(error) | TrackOutcome::Blocked(error) => Some(error.clone()),
            TrackOutcome::Ok => None,
        })
        .unwrap_or_else(|| "scratch-scoped track storage exhaustion".to_string())
}

fn scratch_postprocess_retry_original_error(report: &PipelineReport) -> String {
    fn stage_error(stages: &[crate::convert::pipeline::StageRecord]) -> Option<String> {
        stages.iter().rev().find_map(|stage| match &stage.outcome {
            StageOutcome::Failed(error) => Some(format!("{:?}: {}", stage.stage, error)),
            StageOutcome::Ok
            | StageOutcome::NotRequested
            | StageOutcome::Skipped
            | StageOutcome::SkippedWithReason(_) => None,
        })
    }

    fn track_error(records: &[crate::convert::pipeline::TrackRecord]) -> Option<String> {
        records.iter().rev().find_map(|record| match &record.outcome {
            TrackOutcome::Err(error) | TrackOutcome::Blocked(error) => Some(error.clone()),
            TrackOutcome::Ok => None,
        })
    }

    match &report.outcome {
        AlbumOutcome::Complete { stages, .. } => stage_error(stages),
        AlbumOutcome::Partial { failed, stages, .. }
        | AlbumOutcome::Blocked { failed, stages, .. } => {
            stage_error(stages).or_else(|| track_error(failed))
        }
    }
    .unwrap_or_else(|| "scratch-scoped postprocess storage exhaustion".to_string())
}

async fn run_album_postprocess_work(
    album: ScheduledAlbum,
    outputs: Vec<ScheduledTrackOutput>,
    tool_paths: HashMap<String, PathBuf>,
    version_cache: Arc<Mutex<HashMap<ToolBinary, String>>>,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
    worker_cancel: CancellationToken,
) -> QueueWorkOutput {
    let item_id = album.req.item_id.clone();
    let runner = RealToolRunner::with_version_cache(tool_paths.clone(), version_cache.clone());
    let reporter = BroadcastReporter::new(progress_tx, None, item_id.clone(), None);
    let scratch_retry_context = if album.staging.is_scratch_staging() {
        Some((
            album.req.clone(),
            album.staging.root.clone(),
            album.req.output_root.clone(),
        ))
    } else {
        None
    };

    if let Some((mut retry_req, staging_root, output_root)) = scratch_retry_context.clone() {
        if !worker_cancel.is_cancelled()
            && scheduled_track_outputs_have_scratch_scoped_storage_exhaustion_for_retry(
                &outputs,
                &staging_root,
                &output_root,
            )
        {
            let original_error = scratch_track_retry_original_error(&outputs);
            retry_req.scratch_staging = None;
            log::warn!(
                "scratch retrying on disk: job_id={}, item_id={}, disk_staging_path={}, original_error={}",
                retry_req.job_id,
                item_id,
                disk_staging_parent_for(&retry_req).display(),
                original_error
            );
            drop(outputs);
            drop(album);
            let report = retry_scratch_backed_item_once_on_disk_for_scheduler(
                retry_req,
                &runner,
                &reporter,
                &worker_cancel,
                &tool_paths,
                Some(tool_concurrency_limits),
            )
            .await;
            let warning_count = source_warning_count(report.source.as_ref());
            let status = map_album_outcome(
                &report.outcome,
                report.published.as_ref(),
                report.durable_log.as_deref(),
                warning_count,
            );
            return QueueWorkOutput::PostProcessed { item_id, status };
        }
    }

    let report = finish_pipeline_album_for_scheduler_with_tool_limits(
        album,
        outputs,
        &runner,
        &reporter,
        &worker_cancel,
        Some(tool_concurrency_limits.clone()),
    )
    .await;
    let report = if let Some((mut retry_req, _staging_root, _output_root)) = scratch_retry_context {
        if !worker_cancel.is_cancelled() && pipeline_report_requests_scratch_disk_retry(&report) {
            let original_error = report
                .scratch_retry_intent
                .as_ref()
                .map(|intent| intent.original_error.clone())
                .unwrap_or_else(|| scratch_postprocess_retry_original_error(&report));
            retry_req.scratch_staging = None;
            log::warn!(
                "scratch retrying on disk: job_id={}, item_id={}, disk_staging_path={}, original_error={}",
                retry_req.job_id,
                item_id,
                disk_staging_parent_for(&retry_req).display(),
                original_error
            );
            retry_scratch_backed_item_once_on_disk_for_scheduler(
                retry_req,
                &runner,
                &reporter,
                &worker_cancel,
                &tool_paths,
                Some(tool_concurrency_limits),
            )
            .await
        } else {
            report
        }
    } else {
        report
    };
    let warning_count = source_warning_count(report.source.as_ref());
    let status = map_album_outcome(
        &report.outcome,
        report.published.as_ref(),
        report.durable_log.as_deref(),
        warning_count,
    );
    QueueWorkOutput::PostProcessed { item_id, status }
}

async fn retry_scratch_backed_item_once_on_disk_for_scheduler(
    retry_req: PipelineRequest,
    runner: &dyn crate::convert::pipeline::ToolRunner,
    reporter: &dyn crate::convert::pipeline::PipelineReporter,
    cancel: &CancellationToken,
    tool_paths: &HashMap<String, PathBuf>,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
) -> crate::convert::pipeline::PipelineReport {
    #[cfg(test)]
    if let Some(report) = call_scheduler_disk_retry_hook_for_test(&retry_req) {
        return report;
    }

    run_pipeline_item_with_tool_paths_and_tool_limits(
        retry_req,
        runner,
        reporter,
        cancel,
        tool_paths,
        tool_concurrency_limits,
    )
    .await
}

#[cfg(test)]
type SchedulerDiskRetryHook = dyn Fn(&PipelineRequest) -> Option<crate::convert::pipeline::PipelineReport>
    + Send
    + Sync
    + 'static;

#[cfg(test)]
static SCHEDULER_DISK_RETRY_HOOKS: OnceLock<Mutex<Vec<(u64, Box<SchedulerDiskRetryHook>)>>> =
    OnceLock::new();
#[cfg(test)]
static SCHEDULER_DISK_RETRY_HOOK_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
fn scheduler_disk_retry_hooks() -> &'static Mutex<Vec<(u64, Box<SchedulerDiskRetryHook>)>> {
    SCHEDULER_DISK_RETRY_HOOKS.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(test)]
fn call_scheduler_disk_retry_hook_for_test(
    req: &PipelineRequest,
) -> Option<crate::convert::pipeline::PipelineReport> {
    let guard = scheduler_disk_retry_hooks()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.iter().find_map(|(_, hook)| hook(req))
}

#[cfg(test)]
struct SchedulerDiskRetryHookGuard {
    id: u64,
}

#[cfg(test)]
impl Drop for SchedulerDiskRetryHookGuard {
    fn drop(&mut self) {
        let mut guard = scheduler_disk_retry_hooks()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.retain(|(id, _)| *id != self.id);
    }
}

#[cfg(test)]
fn set_scheduler_disk_retry_hook_for_test(
    hook: Box<SchedulerDiskRetryHook>,
) -> SchedulerDiskRetryHookGuard {
    let id = SCHEDULER_DISK_RETRY_HOOK_ID.fetch_add(1, Ordering::Relaxed);
    let mut guard = scheduler_disk_retry_hooks()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.push((id, hook));
    SchedulerDiskRetryHookGuard { id }
}

/// Run one conversion item through the shared scheduler graph.
///
/// This helper is intentionally the same path as queue processing: materialized
/// multi-track jobs feed track realization, encode, and post-processing work
/// back into `SharedWorkerPool`. The serial `run_pipeline_item_with_tool_paths`
/// function remains in `stages.rs` only as a unit-test/compatibility fallback.
async fn run_single_item_with_shared_scheduler(
    item: ConversionItem,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    tool_paths: HashMap<String, PathBuf>,
    worker_count: usize,
    scratch_staging: ScratchStagingPolicy,
) -> ConversionResult<(String, ConversionStatus)> {
    let item_id = item.id.clone();
    let tool_concurrency_limits = Arc::new(ToolConcurrencyLimits::from_available_parallelism());
    let effective_worker_count = effective_worker_count_for_tool_limits(
        worker_count,
        tool_concurrency_limits.as_ref(),
    );
    let mut outcomes = run_queue_with_shared_orchestrator(
        vec![item],
        progress_tx,
        None,
        None,
        tool_paths,
        effective_worker_count,
        PoolLimits::default(),
        Arc::new(SchedulerMetrics::default()),
        tool_concurrency_limits,
        scratch_staging.directory,
        scratch_staging.memory_limit_percent,
        None,
    )
    .await;

    let (_, status, _) = outcomes
        .pop()
        .unwrap_or_else(|| (item_id.clone(), ConversionStatus::Failed {
            error: "shared scheduler produced no terminal result".to_string(),
            log_path: None,
        }, None));
    Ok((item_id, status))
}

/// Construct a single `ConversionItem` request from an already unified
/// `PipelineSettings` value and then run it through the same shared scheduler
/// graph as queue processing. UI and CLI entry points that expose Chunk 1
/// planner settings should call this API instead of relying on legacy
/// `ConversionOptions` projection.
pub async fn process_item_with_pipeline_settings(
    item: ConversionItem,
    settings: PipelineSettings,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    tool_paths: HashMap<String, PathBuf>,
    file_semaphore: Arc<Semaphore>,
    worker_count: usize,
    scratch_directory: Option<PathBuf>,
) -> ConversionResult<(String, ConversionStatus)> {
    process_item_with_pipeline_settings_and_scratch_policy(
        item,
        settings,
        progress_tx,
        tool_paths,
        file_semaphore,
        worker_count,
        ScratchStagingPolicy::with_default_memory_limit(scratch_directory),
    )
    .await
}

/// Policy-aware variant of `process_item_with_pipeline_settings`. New call sites
/// should use this when they have the configured scratch memory limit available.
pub async fn process_item_with_pipeline_settings_and_scratch_policy(
    mut item: ConversionItem,
    settings: PipelineSettings,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    tool_paths: HashMap<String, PathBuf>,
    _file_semaphore: Arc<Semaphore>,
    worker_count: usize,
    scratch_staging: ScratchStagingPolicy,
) -> ConversionResult<(String, ConversionStatus)> {
    if !item.input_path.exists() {
        let error_msg = format!("Source file not found: {}", item.input_path.display());
        log::error!("{}", error_msg);
        return Ok((
            item.id.clone(),
            ConversionStatus::Failed {
                error: error_msg,
                log_path: None,
            },
        ));
    }

    item.options.pipeline_settings = Some(settings.clone());
    item.pipeline_settings = Some(settings.clone());
    let mut request = build_pipeline_request_from_settings(&item, settings)?;
    apply_companion_policy_from_item(&mut request, &item);
    request.worker_count = Some(worker_count.max(1));
    item.pipeline_request = Some(request);
    process_item_with_scratch_policy(
        item,
        progress_tx,
        tool_paths,
        _file_semaphore,
        worker_count,
        scratch_staging,
    )
    .await
}

#[cfg(test)]
fn synthetic_queue_work_unit_for_processor_test(
    job_id: &str,
    unit_id: String,
    track_index: usize,
) -> WorkUnit<QueueWorkOutput> {
    let job_id = job_id.to_string();
    let output_job_id = job_id.clone();
    WorkUnit {
        job_id,
        unit_id,
        kind: WorkKind::EncodeTrack {
            track_id: crate::convert::pipeline::TrackId {
                source_ordinal: track_index as u32,
                disc_number: None,
                track_number: track_index as u32 + 1,
            },
        },
        task: boxed_work(move |_cancel| async move {
            Ok(QueueWorkOutput::SyntheticEncoded {
                job_id: output_job_id,
                track_index,
            })
        }),
    }
}

/// Process a single conversion item. This direct path now delegates to the
/// shared scheduler graph, so SACD/CUE/archive requests get the same
/// materialization -> encode -> album-postprocess behavior as queued batches.
pub async fn process_item(
    item: ConversionItem,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    tool_paths: HashMap<String, PathBuf>,
    file_semaphore: Arc<Semaphore>,
    worker_count: usize,
    scratch_directory: Option<PathBuf>,
) -> ConversionResult<(String, ConversionStatus)> {
    process_item_with_scratch_policy(
        item,
        progress_tx,
        tool_paths,
        file_semaphore,
        worker_count,
        ScratchStagingPolicy::with_default_memory_limit(scratch_directory),
    )
    .await
}

/// Policy-aware variant of `process_item`. This is the direct single-item path
/// that preserves configured scratch memory limits instead of silently using
/// `DEFAULT_SCRATCH_MEMORY_LIMIT_PERCENT`.
pub async fn process_item_with_scratch_policy(
    item: ConversionItem,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    tool_paths: HashMap<String, PathBuf>,
    _file_semaphore: Arc<Semaphore>,
    worker_count: usize,
    scratch_staging: ScratchStagingPolicy,
) -> ConversionResult<(String, ConversionStatus)> {
    if !item.input_path.exists() {
        let error_msg = format!("Source file not found: {}", item.input_path.display());
        log::error!("{}", error_msg);
        return Ok((
            item.id.clone(),
            ConversionStatus::Failed {
                error: error_msg,
                log_path: None,
            },
        ));
    }

    let _ = progress_tx.send(ProgressUpdate {
        item_id: item.id.clone(),
        track_index: None,
        track_epoch: None,
        progress: 0.0,
        status: ConversionStatus::Processing {
            progress: 0.0,
            message: Some(format!("Starting conversion to {}", item.output_format)),
            file_progress: None,
            phase: None,
            phase_progress: None,
        },
    });

    run_single_item_with_shared_scheduler(
        item,
        progress_tx,
        tool_paths,
        worker_count,
        scratch_staging,
    )
    .await
}

#[cfg(test)]
mod tests {

    #[test]
    fn album_batch_component_preserves_dot_runs_and_guards_only_navigation_tokens() {
        assert_eq!(
            sanitize_album_batch_component("...And Then There Were Three..."),
            "...And Then There Were Three..."
        );
        assert_eq!(sanitize_album_batch_component("."), "Album");
        assert_eq!(sanitize_album_batch_component(".."), "Album");
        assert_eq!(sanitize_album_batch_component(" .hidden. "), ".hidden.");
    }

    #[test]
    fn unsupported_source_does_not_fall_through_to_generic_materialization() {
        let source = include_str!("processor.rs");
        // Only scan runtime code: the assertions below would otherwise match
        // their own literals inside this test module.
        let runtime = source.split("\nmod tests {").next().unwrap_or(source);
        let build_work_start = runtime
            .find("fn build_work_unit_for_item")
            .or_else(|| runtime.find("let source_kind = detect_source_kind(&request).ok();"))
            .expect("processor work-unit source-kind branch should exist");
        let tail = &runtime[build_work_start..];

        assert!(
            tail.contains("let Some(source_kind) = source_kind else"),
            "unknown source kinds must fail explicitly before materialization"
        );
        assert!(
            tail.contains("Regular audio folders must be expanded into supported audio files before queue processing"),
            "unsupported-source error should explain the folder-expansion contract"
        );
        assert!(
            !tail.contains("None => WorkKind::MaterializeItem"),
            "unknown source kinds must not fall through to generic materialization"
        );
    }

    use super::*;
    use crate::convert::pipeline::DvdaDownmixPolicy;

    struct CapturingTestLogger;

    static TEST_LOGGER: CapturingTestLogger = CapturingTestLogger;
    static TEST_LOG_LINES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

    impl log::Log for CapturingTestLogger {
        fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
            true
        }

        fn log(&self, record: &log::Record<'_>) {
            if !self.enabled(record.metadata()) {
                return;
            }
            let mut guard = TEST_LOG_LINES
                .get_or_init(|| Mutex::new(Vec::new()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.push(format!("{}:{}", record.level(), record.args()));
        }

        fn flush(&self) {}
    }

    fn install_test_logger() {
        let _ = log::set_logger(&TEST_LOGGER);
        log::set_max_level(log::LevelFilter::Trace);
        let _ = TEST_LOG_LINES.get_or_init(|| Mutex::new(Vec::new()));
    }

    fn test_log_cursor() -> usize {
        TEST_LOG_LINES
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    fn captured_test_logs_since(cursor: usize) -> Vec<String> {
        TEST_LOG_LINES
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .skip(cursor)
            .cloned()
            .collect()
    }

    fn captured_test_logs_since_for_item(cursor: usize, item_id: &str) -> Vec<String> {
        captured_test_logs_since(cursor)
            .into_iter()
            .filter(|line| line.contains(&format!("item_id={item_id}")))
            .collect()
    }

    fn synthetic_queue_work_unit(job_id: &str, unit_id: String) -> WorkUnit<QueueWorkOutput> {
        let item_id = unit_id.clone();
        WorkUnit {
            job_id: job_id.to_string(),
            unit_id,
            kind: WorkKind::EncodeTrack {
                track_id: crate::convert::pipeline::TrackId {
                    source_ordinal: 0,
                    disc_number: None,
                    track_number: 1,
                },
            },
            task: boxed_work(move |_cancel| async move {
                let _ = item_id;
                Err("test unit should remain queued".to_string())
            }),
        }
    }

    fn blocking_synthetic_processor_unit(
        job_id: &str,
        unit_id: &str,
        track_index: usize,
        started_tx: tokio::sync::oneshot::Sender<()>,
        release_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> WorkUnit<QueueWorkOutput> {
        let job_id = job_id.to_string();
        let output_job_id = job_id.clone();
        WorkUnit {
            job_id,
            unit_id: unit_id.to_string(),
            kind: WorkKind::EncodeTrack {
                track_id: crate::convert::pipeline::TrackId {
                    source_ordinal: track_index as u32,
                    disc_number: None,
                    track_number: track_index as u32 + 1,
                },
            },
            task: boxed_work(move |_cancel| async move {
                let _ = started_tx.send(());
                let _ = release_rx.await;
                Ok(QueueWorkOutput::SyntheticEncoded {
                    job_id: output_job_id,
                    track_index,
                })
            }),
        }
    }

    #[allow(dead_code)]
    async fn expect_processor_scheduler_event(
        events_rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::convert::pipeline::SchedulerTestEvent>,
        expected: crate::convert::pipeline::SchedulerTestEvent,
    ) {
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), events_rx.recv())
            .await
            .expect("scheduler test event arrives before timeout")
            .expect("scheduler test event sender remains live");
        assert_eq!(event, expected);
    }

    fn pipeline_request_for_processor_limit_test(root: &std::path::Path) -> PipelineRequest {
        PipelineRequest {
            actions: crate::convert::pipeline::ActionPipeline::default(),
            job_id: "processor-limit-job".to_string(),
            item_id: "processor-limit-item".to_string(),
            container: root.join("input.flac"),
            source: SourceOptions {
                archive_password: None,
                sacd_area: None,
                dvda_group: None,
                dvda_group_selection: DvdaGroupSelection::Default,
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
                sidecar_cue_track_metadata: None,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection: TrackSelection::All,
            },
            settings: PipelineSettings::default(),
            worker_count: Some(1),
            scratch_staging: None,
            merge: false,
            output_root: root.join("out"),
            naming: NamingPolicy {
                template: "%NN% - %TITLE%".to_string(),
                folder_template: None,
                per_album_subdir: true,
                collision_policy: NamingCollisionPolicy::Fail,
            windows_portable: false,
            },
            publish: PublishPolicy {
                overwrite: OverwritePolicy::FailIfExists,
                same_filesystem_required: false,
                write_manifest: false,
            },
            log: LogPolicy {
                root: root.join("logs"),
                write_for_blocked: true,
                write_json_log: true,
                write_conversion_log: true,
            },
            stages: StagePolicy {
                metadata: StageRequirement::Disabled,
                replaygain: StageRequirement::Disabled,
                features: StageRequirement::Disabled,
                generate_cue: false,
            },
            failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
            album_batch: None,
            album_batch_track: None,
            pre_extracted_staging: None,
            archive_metadata_overrides: Vec::new(),
            metadata_overrides: Default::default(),
            batch_resolved_identity: None,
            suppress_incremental_conversion_log_append: false,
            expected_album_track_count: None,
            container_extension: None,
            container_ffmpeg_flags: Vec::new(),
            companion: CompanionCopyPolicy::default(),
        }
    }

    fn conversion_item_with_pipeline_request(id: &str, request: PipelineRequest) -> ConversionItem {
        let mut item = ConversionItem::default();
        item.id = id.to_string();
        item.input_path = request.container.clone();
        item.pipeline_request = Some(request);
        item
    }

    #[test]
    fn production_request_boundary_applies_disc_subfolder_template_to_raw_builder_output() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut request = pipeline_request_for_processor_limit_test(temp.path());
        request.naming.template = "%ARTIST%/%TRACK% - %TITLE%".to_string();

        let mut item =
            conversion_item_with_pipeline_request("disc-boundary-item", request.clone());
        item.options.create_disc_subfolders = true;

        apply_conversion_options_request_contract(&mut request, &item);

        assert_eq!(
            request.naming.template,
            "%DISC_FOLDER%/%ARTIST%/%TRACK% - %TITLE%",
            "processor request construction must project create_disc_subfolders into the concrete PipelineRequest naming template returned by the raw builder"
        );

        apply_conversion_options_request_contract(&mut request, &item);

        assert_eq!(
            request.naming.template,
            "%DISC_FOLDER%/%ARTIST%/%TRACK% - %TITLE%",
            "the production request-boundary projection must be idempotent"
        );
    }

    #[test]
    fn production_request_boundary_preserves_explicit_disc_folder_token() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut request = pipeline_request_for_processor_limit_test(temp.path());
        request.naming.template = "%ALBUM%/%DISC_FOLDER%/%TRACK% - %TITLE%".to_string();

        let mut item =
            conversion_item_with_pipeline_request("explicit-disc-token-item", request.clone());
        item.options.create_disc_subfolders = true;

        apply_conversion_options_request_contract(&mut request, &item);

        assert_eq!(
            request.naming.template,
            "%ALBUM%/%DISC_FOLDER%/%TRACK% - %TITLE%",
            "explicit user/request templates containing the disc-folder token must not be double-prefixed"
        );
    }

    #[test]
    fn production_request_boundary_does_not_remove_manual_disc_folder_template_when_toggle_off() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut request = pipeline_request_for_processor_limit_test(temp.path());
        request.naming.template = "%DISC_FOLDER%/%TRACK% - %TITLE%".to_string();

        let mut item =
            conversion_item_with_pipeline_request("manual-disc-token-item", request.clone());
        item.options.create_disc_subfolders = false;

        apply_conversion_options_request_contract(&mut request, &item);

        assert_eq!(
            request.naming.template,
            "%DISC_FOLDER%/%TRACK% - %TITLE%",
            "the contract must not strip a manually authored disc-folder token when the UI toggle is off"
        );
    }

    #[tokio::test]
    async fn scratch_queued_track_enospc_retries_disk_before_terminal_failure_publication() {
        let temp = tempfile::tempdir().expect("temp dir");
        let scratch_root = temp.path().join("scratch");
        let scratch_parent = scratch_root.join(".tonepoet-staging");
        let output_root = temp.path().join("out");
        let log_root = temp.path().join("logs");
        std::fs::create_dir_all(&scratch_parent).expect("scratch parent");
        std::fs::create_dir_all(&output_root).expect("output root");
        std::fs::create_dir_all(&log_root).expect("log root");
        let container = temp.path().join("input.flac");
        std::fs::write(&container, b"synthetic input").expect("container");

        let scratch_config = ScratchStagingConfig::with_fixed_memory_and_filesystem_for_test(
            scratch_root.clone(),
            50,
            1024 * 1024 * 1024,
            1024 * 1024 * 1024,
            1024 * 1024 * 1024,
            1024 * 1024 * 1024,
        );
        scratch_config
            .ensure_usable(&scratch_parent)
            .expect("scratch usable");
        let reservation = scratch_config.try_reserve(4096).expect("scratch reservation");
        assert_eq!(scratch_config.active_reserved_bytes_for_test(), 4096);

        let mut req = pipeline_request_for_processor_limit_test(temp.path());
        req.job_id = "scratch-retry-job".to_string();
        req.item_id = "scratch-retry-item".to_string();
        req.container = container.clone();
        req.output_root = output_root.clone();
        req.log.root = log_root.clone();
        req.scratch_staging = Some(scratch_config.clone());

        let staging_root = scratch_parent.join("scratch-retry-job-scratch-retry-item");
        let converted_root = staging_root.join("converted");
        let realized_root = staging_root.join("realized");
        std::fs::create_dir_all(&converted_root).expect("converted root");
        std::fs::create_dir_all(&realized_root).expect("realized root");
        let staging = StagingDir::new_with_scratch_reservation(
            staging_root.clone(),
            req.job_id.clone(),
            reservation,
        );

        let track_id = TrackId {
            source_ordinal: 0,
            disc_number: None,
            track_number: 1,
        };
        let realized_path = realized_root.join("01.wav");
        let failed_staged_path = converted_root.join("01.flac");
        let source = PreparedSource {
            container: container.clone(),
            kind: SourceKind::SingleFile,
            tracks: vec![PreparedTrack {
                id: track_id.clone(),
                source_ref: TrackSourceRef::StagedFile(realized_path.clone()),
                metadata: TrackMetadata {
                    title: Some("Scratch Retry".to_string()),
                    track_number: Some(1),
                    ..TrackMetadata::default()
                },
                expected_samples: Some(44_100),
                sample_rate: Some(44_100),
                source_audio: SourceAudioDescriptor::from_scalar(
                    Some(44_100),
                    Some(16),
                    Some(SourceAudioCoding::Pcm),
                ),
                bit_depth: Some(16),
                warnings: Vec::new(),
            }],
            album_metadata: AlbumMetadata {
                album: Some("Scratch Retry Album".to_string()),
                total_tracks: 1,
                ..AlbumMetadata::default()
            },
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SingleFile,
                source_sha256: None,
                tool_versions: BTreeMap::new(),
                extracted_at: chrono::Utc::now(),
            },
        };
        let disk_album_dir = output_root.join("disk-attempt-complete");
        let plan = AlbumPlan {
            album_dir: disk_album_dir.clone(),
            album_dirs: Vec::new(),
            entries: vec![PlannedTrackOutput {
                track_id: track_id.clone(),
                final_path: disk_album_dir.join("01.flac"),
            }],
        };
        let album = scheduled_album_for_test(
            req.clone(),
            req.item_id.clone(),
            staging,
            source,
            plan,
            Vec::new(),
            &scratch_parent,
        );
        let scratch_failed_output = ScheduledTrackOutput {
            index: 0,
            record: TrackRecord {
                track_id,
                outcome: TrackOutcome::Err(format!(
                    "No space left on device while writing {}",
                    failed_staged_path.display()
                )),
                source_ref: TrackSourceRef::StagedFile(realized_path.clone()),
                realized_input: Some(realized_path),
                output_file: Some(failed_staged_path),
                commands: Vec::new(),
                bytes_in: None,
                bytes_out: None,
                duration: None,
                dsd_dst_stats: None,
                verified_output_bit_depth: None,
            },
            artifact: None,
            ok: false,
            metadata_satisfaction: crate::convert::pipeline::PlannedMetadataSatisfaction::none(),
        };

        let retry_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hook_retry_count = retry_count.clone();
        let hook_scratch_config = scratch_config.clone();
        let hook_staging_root = staging_root.clone();
        let hook_disk_album_dir = disk_album_dir.clone();
        let hook_log_root = log_root.clone();
        let _hook_guard = set_scheduler_disk_retry_hook_for_test(Box::new(move |disk_req| {
            if disk_req.item_id != "scratch-retry-item" {
                return None;
            }
            hook_retry_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert!(
                disk_req.scratch_staging.is_none(),
                "disk retry request must disable scratch staging"
            );
            assert!(
                !hook_staging_root.exists(),
                "scratch staging root must be dropped before disk retry starts"
            );
            assert_eq!(
                hook_scratch_config.active_reserved_bytes_for_test(),
                0,
                "scratch reservation must be released before disk retry starts"
            );
            let pre_retry_fragment_count = std::fs::read_dir(&hook_log_root)
                .expect("log root exists before disk retry")
                .count();
            assert_eq!(
                pre_retry_fragment_count, 0,
                "scratch failure artifacts must not be published before the disk retry starts"
            );
            Some(crate::convert::pipeline::PipelineReport {
                request: RedactedPipelineRequest::from(disk_req),
                source: None,
                plan: None,
                artifacts: None,
                published: Some(PublishedAlbum {
                    album_dir: hook_disk_album_dir.clone(),
                    entries: Vec::new(),
                    manifest_path: None,
                    batch_completion: None,
                }),
                outcome: AlbumOutcome::Complete {
                    tracks: Vec::new(),
                    stages: Vec::new(),
                },
                durable_log: None,
                scratch_retry_intent: None,
                settings_fingerprint: Some(tonepoet_pipeline::fingerprint::settings_fingerprint(
                    &disk_req.settings,
                )),
                manifest_path: None,
                action_reports: Vec::new(),
            })
        }));

        let (progress_tx, _progress_rx) = broadcast::channel(8);
        let result = run_album_postprocess_work(
            album,
            vec![scratch_failed_output],
            HashMap::new(),
            Arc::new(Mutex::new(HashMap::new())),
            progress_tx,
            Arc::new(ToolConcurrencyLimits::from_available_parallelism()),
            CancellationToken::new(),
        )
        .await;

        match result {
            QueueWorkOutput::PostProcessed { item_id, status } => {
                assert_eq!(item_id, "scratch-retry-item");
                match status {
                    ConversionStatus::Completed { output_path, log_path, .. } => {
                        assert_eq!(output_path, disk_album_dir);
                        assert!(log_path.is_none(), "disk retry hook returned no durable log");
                    }
                    other => panic!("terminal status should come from disk retry attempt, got {other:?}"),
                }
            }
            _ => panic!("album postprocess should produce one terminal postprocess result"),
        }

        assert_eq!(
            retry_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "scratch ENOSPC should trigger exactly one disk retry"
        );
        assert!(
            !staging_root.exists(),
            "scratch staging should stay cleaned after retry"
        );
        assert_eq!(
            scratch_config.active_reserved_bytes_for_test(),
            0,
            "scratch reservation should remain released after retry"
        );
        let terminal_fragment_count = std::fs::read_dir(&log_root)
            .expect("log root exists")
            .count();
        assert_eq!(
            terminal_fragment_count, 0,
            "scratch first attempt must not publish terminal failure fragments before disk retry"
        );
    }



    #[tokio::test]
    async fn scratch_post_materialization_stage_enospc_retries_disk_before_terminal_failure_publication() {
        for retry_stage in [
            PipelineStage::Merge,
            PipelineStage::Metadata,
            PipelineStage::ReplayGain,
            PipelineStage::Features,
        ] {
            let temp = tempfile::tempdir().expect("temp dir");
            let scratch_root = temp.path().join("scratch");
            let scratch_parent = scratch_root.join(".tonepoet-staging");
            let output_root = temp.path().join("out");
            let log_root = temp.path().join("logs");
            std::fs::create_dir_all(&scratch_parent).expect("scratch parent");
            std::fs::create_dir_all(&output_root).expect("output root");
            std::fs::create_dir_all(&log_root).expect("log root");
            let container = temp.path().join("input.flac");
            std::fs::write(&container, b"synthetic input").expect("container");

            let scratch_config = ScratchStagingConfig::with_fixed_memory_and_filesystem_for_test(
                scratch_root.clone(),
                50,
                1024 * 1024 * 1024,
                1024 * 1024 * 1024,
                1024 * 1024 * 1024,
                1024 * 1024 * 1024,
            );
            scratch_config
                .ensure_usable(&scratch_parent)
                .expect("scratch usable");
            let reservation = scratch_config.try_reserve(8192).expect("scratch reservation");
            assert_eq!(scratch_config.active_reserved_bytes_for_test(), 8192);

            let item_id = format!("scratch-postprocess-{retry_stage:?}");
            let job_id = format!("scratch-postprocess-job-{retry_stage:?}");
            install_test_logger();
            let log_cursor = test_log_cursor();
            let mut req = pipeline_request_for_processor_limit_test(temp.path());
            req.job_id = job_id.clone();
            req.item_id = item_id.clone();
            req.container = container.clone();
            req.output_root = output_root.clone();
            req.log.root = log_root.clone();
            req.scratch_staging = Some(scratch_config.clone());
            req.merge = retry_stage == PipelineStage::Merge;
            req.stages = StagePolicy {
                metadata: if retry_stage == PipelineStage::Metadata {
                    StageRequirement::Enabled
                } else {
                    StageRequirement::Disabled
                },
                replaygain: if retry_stage == PipelineStage::ReplayGain {
                    StageRequirement::Enabled
                } else {
                    StageRequirement::Disabled
                },
                features: if retry_stage == PipelineStage::Features {
                    StageRequirement::Enabled
                } else {
                    StageRequirement::Disabled
                },
                generate_cue: false,
            };

            let staging_root = scratch_parent.join(format!("{job_id}-{item_id}"));
            let converted_root = staging_root.join("converted");
            let realized_root = staging_root.join("realized");
            std::fs::create_dir_all(&converted_root).expect("converted root");
            std::fs::create_dir_all(&realized_root).expect("realized root");
            let staging = StagingDir::new_with_scratch_reservation(
                staging_root.clone(),
                req.job_id.clone(),
                reservation,
            );

            let track_count = if retry_stage == PipelineStage::Merge { 2 } else { 1 };
            let mut tracks = Vec::new();
            let mut plan_entries = Vec::new();
            let mut outputs = Vec::new();
            let album_dir = output_root.join(format!("disk-attempt-complete-{retry_stage:?}"));

            for index in 0..track_count {
                let track_id = TrackId {
                    source_ordinal: index as u32,
                    disc_number: None,
                    track_number: index as u32 + 1,
                };
                let realized_path = realized_root.join(format!("{:02}.wav", index + 1));
                let staged_path = converted_root.join(format!("{:02}.flac", index + 1));
                let final_path = album_dir.join(format!("{:02}.flac", index + 1));
                std::fs::write(&realized_path, b"realized audio").expect("realized track");
                std::fs::write(&staged_path, b"encoded audio").expect("staged track");
                let metadata_required = if retry_stage == PipelineStage::Metadata {
                    PlannedMetadataSatisfaction {
                        authoritative_tags_applied: true,
                        ..PlannedMetadataSatisfaction::none()
                    }
                } else {
                    PlannedMetadataSatisfaction::none()
                };

                tracks.push(PreparedTrack {
                    id: track_id.clone(),
                    source_ref: TrackSourceRef::StagedFile(realized_path.clone()),
                    metadata: TrackMetadata {
                        title: Some(format!("Scratch Retry {}", index + 1)),
                        track_number: Some(index as u32 + 1),
                        ..TrackMetadata::default()
                    },
                    expected_samples: Some(44_100),
                    sample_rate: Some(44_100),
                    source_audio: SourceAudioDescriptor::from_scalar(
                        Some(44_100),
                        Some(16),
                        Some(SourceAudioCoding::Pcm),
                    ),
                    bit_depth: Some(16),
                    warnings: Vec::new(),
                });
                plan_entries.push(PlannedTrackOutput {
                    track_id: track_id.clone(),
                    final_path: final_path.clone(),
                });
                outputs.push(ScheduledTrackOutput {
                    index,
                    record: TrackRecord {
                        track_id: track_id.clone(),
                        outcome: TrackOutcome::Ok,
                        source_ref: TrackSourceRef::StagedFile(realized_path.clone()),
                        realized_input: Some(realized_path.clone()),
                        output_file: Some(staged_path.clone()),
                        commands: Vec::new(),
                        bytes_in: Some(1024),
                        bytes_out: Some(1024),
                        duration: None,
                        dsd_dst_stats: None,
                        verified_output_bit_depth: None,
                    },
                    artifact: Some(TrackArtifact {
                    reference_evidence: None,
                        track_id,
                        staged_path,
                        final_path,
                        samples: Some(44_100),
                        metadata_satisfaction: PlannedMetadataSatisfaction::none(),
                        metadata_required,
                        planned_command_hash: None,
                    }),
                    ok: true,
                    metadata_satisfaction: PlannedMetadataSatisfaction::none(),
                });
            }

            let source = PreparedSource {
                container: container.clone(),
                kind: SourceKind::SingleFile,
                tracks,
                album_metadata: AlbumMetadata {
                    album: Some("Scratch Retry Album".to_string()),
                    total_tracks: track_count as u32,
                    ..AlbumMetadata::default()
                },
                provenance: ExtractionProvenance {
                    source_kind: SourceKind::SingleFile,
                    source_sha256: None,
                    tool_versions: BTreeMap::new(),
                    extracted_at: chrono::Utc::now(),
                },
            };
            let plan = AlbumPlan {
                album_dir: album_dir.clone(),
                album_dirs: Vec::new(),
                entries: plan_entries,
            };
            let album = scheduled_album_for_test(
                req.clone(),
                req.item_id.clone(),
                staging,
                source,
                plan,
                Vec::new(),
                &scratch_parent,
            );

            let expected_stage = retry_stage;
            let expected_item_id = item_id.clone();
            let expected_staging_root = staging_root.clone();
            let _stage_fault_guard = set_post_materialization_stage_fault_hook_for_test(Box::new(
                move |stage, hook_req, observed_path| {
                    if stage != expected_stage || hook_req.item_id != expected_item_id {
                        return None;
                    }
                    if let Some(path) = observed_path {
                        assert!(
                            path.starts_with(&expected_staging_root),
                            "fault hook should observe scratch staging path, got {}",
                            path.display()
                        );
                    }
                    Some(format!(
                        "injected {stage:?} ENOSPC while writing {}: No space left on device",
                        expected_staging_root.join("postprocess-stage.tmp").display()
                    ))
                },
            ));

            let retry_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let hook_retry_count = retry_count.clone();
            let hook_scratch_config = scratch_config.clone();
            let hook_staging_root = staging_root.clone();
            let hook_album_dir = album_dir.clone();
            let hook_log_root = log_root.clone();
            let hook_item_id = item_id.clone();
            let _retry_hook_guard = set_scheduler_disk_retry_hook_for_test(Box::new(move |disk_req| {
                if disk_req.item_id != hook_item_id {
                    return None;
                }
                hook_retry_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                assert!(
                    disk_req.scratch_staging.is_none(),
                    "disk retry request must disable scratch staging"
                );
                assert!(
                    !hook_staging_root.exists(),
                    "scratch staging root must be dropped before disk retry starts"
                );
                assert_eq!(
                    hook_scratch_config.active_reserved_bytes_for_test(),
                    0,
                    "scratch reservation must be released before disk retry starts"
                );
                let pre_retry_fragment_count = std::fs::read_dir(&hook_log_root)
                    .expect("log root exists before disk retry")
                    .count();
                assert_eq!(
                    pre_retry_fragment_count,
                    0,
                    "scratch postprocess failure must not publish terminal fragments before disk retry"
                );
                Some(crate::convert::pipeline::PipelineReport {
                    request: RedactedPipelineRequest::from(disk_req),
                    source: None,
                    plan: None,
                    artifacts: None,
                    published: Some(PublishedAlbum {
                        album_dir: hook_album_dir.clone(),
                        entries: Vec::new(),
                        manifest_path: None,
                        batch_completion: None,
                    }),
                    outcome: AlbumOutcome::Complete {
                        tracks: Vec::new(),
                        stages: Vec::new(),
                    },
                    durable_log: None,
                    scratch_retry_intent: None,
                    settings_fingerprint: Some(tonepoet_pipeline::fingerprint::settings_fingerprint(
                        &disk_req.settings,
                    )),
                    manifest_path: None,
                    action_reports: Vec::new(),
                })
            }));

            let (progress_tx, _progress_rx) = broadcast::channel(8);
            let result = run_album_postprocess_work(
                album,
                outputs,
                HashMap::new(),
                Arc::new(Mutex::new(HashMap::new())),
                progress_tx,
                Arc::new(ToolConcurrencyLimits::from_available_parallelism()),
                CancellationToken::new(),
            )
            .await;

            match result {
                QueueWorkOutput::PostProcessed { item_id: actual_item_id, status } => {
                    assert_eq!(actual_item_id, item_id);
                    match status {
                        ConversionStatus::Completed { output_path, log_path, .. } => {
                            assert_eq!(output_path, album_dir);
                            assert!(log_path.is_none(), "disk retry hook returned no durable log");
                        }
                        other => panic!(
                            "terminal status for {retry_stage:?} should come from disk retry attempt, got {other:?}"
                        ),
                    }
                }
                _ => panic!("album postprocess should produce one terminal postprocess result"),
            }

            assert_eq!(
                retry_count.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "{retry_stage:?} ENOSPC should trigger exactly one disk retry"
            );
            let expected_disk_staging_path = output_root.join(".tonepoet-staging");
            let retry_logs = captured_test_logs_since_for_item(log_cursor, &item_id);
            assert!(
                retry_logs.iter().any(|line| {
                    line.contains("WARN:scratch retrying on disk")
                        && line.contains(&format!("job_id={job_id}"))
                        && line.contains(&format!("item_id={item_id}"))
                        && line.contains(&format!(
                            "disk_staging_path={}",
                            expected_disk_staging_path.display()
                        ))
                        && line.contains("original_error=")
                        && line.contains(&format!("{retry_stage:?}"))
                        && line.contains("No space left on device")
                }),
                "scratch retry log for {retry_stage:?} must include job_id, item_id, disk staging path, and original error; logs were {retry_logs:?}"
            );
            assert!(
                retry_logs.iter().any(|line| {
                    line.contains("WARN:scratch failure eligible for disk retry; deferring terminal failure publication")
                        && line.contains(&format!("job_id={job_id}"))
                        && line.contains(&format!("item_id={item_id}"))
                        && line.contains("original_error=")
                        && line.contains(&format!("{retry_stage:?}"))
                        && line.contains("No space left on device")
                }),
                "inner retry-intent log for {retry_stage:?} must distinguish eligibility/deferral from the actual retry; logs were {retry_logs:?}"
            );
            assert!(
                !staging_root.exists(),
                "scratch staging should stay cleaned after retry"
            );
            assert_eq!(
                scratch_config.active_reserved_bytes_for_test(),
                0,
                "scratch reservation should remain released after retry"
            );
            let terminal_fragment_count = std::fs::read_dir(&log_root)
                .expect("log root exists")
                .count();
            assert_eq!(
                terminal_fragment_count,
                0,
                "scratch postprocess attempt must not publish terminal failure fragments before disk retry"
            );
        }
    }

    #[tokio::test]
    async fn scratch_output_publish_enospc_does_not_retry_on_disk_and_publishes_terminal_failure() {
        install_test_logger();
        let temp = tempfile::tempdir().expect("temp dir");
        let scratch_root = temp.path().join("scratch");
        let scratch_parent = scratch_root.join(".tonepoet-staging");
        let output_root = temp.path().join("out");
        let log_root = temp.path().join("logs");
        std::fs::create_dir_all(&scratch_parent).expect("scratch parent");
        std::fs::create_dir_all(&output_root).expect("output root");
        std::fs::create_dir_all(&log_root).expect("log root");
        let container = temp.path().join("input.flac");
        std::fs::write(&container, b"synthetic input").expect("container");

        let scratch_config = ScratchStagingConfig::with_fixed_memory_and_filesystem_for_test(
            scratch_root.clone(),
            50,
            1024 * 1024 * 1024,
            1024 * 1024 * 1024,
            1024 * 1024 * 1024,
            1024 * 1024 * 1024,
        );
        scratch_config
            .ensure_usable(&scratch_parent)
            .expect("scratch usable");
        let reservation = scratch_config.try_reserve(4096).expect("scratch reservation");
        assert_eq!(scratch_config.active_reserved_bytes_for_test(), 4096);

        let mut req = pipeline_request_for_processor_limit_test(temp.path());
        req.job_id = "scratch-output-publish-job".to_string();
        req.item_id = "scratch-output-publish-item".to_string();
        let log_cursor = test_log_cursor();
        req.container = container.clone();
        req.output_root = output_root.clone();
        req.log.root = log_root.clone();
        req.scratch_staging = Some(scratch_config.clone());
        req.stages = StagePolicy {
            metadata: StageRequirement::Disabled,
            replaygain: StageRequirement::Disabled,
            features: StageRequirement::Disabled,
            generate_cue: false,
        };

        let staging_root = scratch_parent.join(format!("{}-{}", req.job_id, req.item_id));
        let converted_root = staging_root.join("converted");
        let realized_root = staging_root.join("realized");
        std::fs::create_dir_all(&converted_root).expect("converted root");
        std::fs::create_dir_all(&realized_root).expect("realized root");
        let staging = StagingDir::new_with_scratch_reservation(
            staging_root.clone(),
            req.job_id.clone(),
            reservation,
        );

        let track_id = TrackId {
            source_ordinal: 0,
            disc_number: None,
            track_number: 1,
        };
        let realized_path = realized_root.join("01.wav");
        let staged_path = converted_root.join("01.flac");
        let album_dir = output_root.join("output-capacity-failure");
        let final_path = album_dir.join("01.flac");
        std::fs::write(&realized_path, b"realized audio").expect("realized track");
        std::fs::write(&staged_path, b"encoded audio").expect("staged track");

        let source = PreparedSource {
            container: container.clone(),
            kind: SourceKind::SingleFile,
            tracks: vec![PreparedTrack {
                id: track_id.clone(),
                source_ref: TrackSourceRef::StagedFile(realized_path.clone()),
                metadata: TrackMetadata {
                    title: Some("Output Capacity".to_string()),
                    track_number: Some(1),
                    ..TrackMetadata::default()
                },
                expected_samples: Some(44_100),
                sample_rate: Some(44_100),
                source_audio: SourceAudioDescriptor::from_scalar(
                    Some(44_100),
                    Some(16),
                    Some(SourceAudioCoding::Pcm),
                ),
                bit_depth: Some(16),
                warnings: Vec::new(),
            }],
            album_metadata: AlbumMetadata {
                album: Some("Output Capacity Album".to_string()),
                total_tracks: 1,
                ..AlbumMetadata::default()
            },
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SingleFile,
                source_sha256: None,
                tool_versions: BTreeMap::new(),
                extracted_at: chrono::Utc::now(),
            },
        };
        let plan = AlbumPlan {
            album_dir: album_dir.clone(),
            album_dirs: Vec::new(),
            entries: vec![PlannedTrackOutput {
                track_id: track_id.clone(),
                final_path: final_path.clone(),
            }],
        };
        let album = scheduled_album_for_test(
            req.clone(),
            req.item_id.clone(),
            staging,
            source,
            plan,
            Vec::new(),
            &scratch_parent,
        );
        let outputs = vec![ScheduledTrackOutput {
            index: 0,
            record: TrackRecord {
                track_id: track_id.clone(),
                outcome: TrackOutcome::Ok,
                source_ref: TrackSourceRef::StagedFile(realized_path.clone()),
                realized_input: Some(realized_path),
                output_file: Some(staged_path.clone()),
                commands: Vec::new(),
                bytes_in: Some(1024),
                bytes_out: Some(1024),
                duration: None,
                dsd_dst_stats: None,
                verified_output_bit_depth: None,
            },
            artifact: Some(TrackArtifact {
                    reference_evidence: None,
                track_id,
                staged_path,
                final_path,
                samples: Some(44_100),
                metadata_satisfaction: PlannedMetadataSatisfaction::none(),
                metadata_required: PlannedMetadataSatisfaction::none(),
                planned_command_hash: None,
            }),
            ok: true,
            metadata_satisfaction: PlannedMetadataSatisfaction::none(),
        }];

        let hook_album_dir = album_dir.clone();
        let hook_output_root = output_root.clone();
        let _publish_fault_guard = set_publish_fault_hook_for_test(Box::new(move |_staging, plan| {
            if plan.album_dir != hook_album_dir {
                return None;
            }
            Some(format!(
                "No space left on device while publishing final output under {}",
                hook_output_root.display()
            ))
        }));

        let retry_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hook_retry_count = retry_count.clone();
        let _retry_hook_guard = set_scheduler_disk_retry_hook_for_test(Box::new(move |disk_req| {
            if disk_req.item_id != "scratch-output-publish-item" {
                return None;
            }
            hook_retry_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(crate::convert::pipeline::PipelineReport {
                request: RedactedPipelineRequest::from(disk_req),
                source: None,
                plan: None,
                artifacts: None,
                published: Some(PublishedAlbum {
                    album_dir: disk_req.output_root.join("unexpected-disk-retry"),
                    entries: Vec::new(),
                    manifest_path: None,
                    batch_completion: None,
                }),
                outcome: AlbumOutcome::Complete {
                    tracks: Vec::new(),
                    stages: Vec::new(),
                },
                durable_log: None,
                scratch_retry_intent: None,
                settings_fingerprint: Some(tonepoet_pipeline::fingerprint::settings_fingerprint(
                    &disk_req.settings,
                )),
                manifest_path: None,
                action_reports: Vec::new(),
            })
        }));

        let (progress_tx, _progress_rx) = broadcast::channel(8);
        let result = run_album_postprocess_work(
            album,
            outputs,
            HashMap::new(),
            Arc::new(Mutex::new(HashMap::new())),
            progress_tx,
            Arc::new(ToolConcurrencyLimits::from_available_parallelism()),
            CancellationToken::new(),
        )
        .await;

        match result {
            QueueWorkOutput::PostProcessed { item_id, status } => {
                assert_eq!(item_id, "scratch-output-publish-item");
                match status {
                    ConversionStatus::Failed { error, log_path } => {
                        assert!(
                            error.contains("Publish") && error.contains("No space left on device"),
                            "terminal failure should remain a publish/output capacity failure, got {error}"
                        );
                        assert!(
                            error.contains(&output_root.to_string_lossy().to_string()),
                            "publish failure should name the output filesystem path, got {error}"
                        );
                        let log_path = log_path.expect("blocked publish failure should write a durable log");
                        assert!(log_path.exists(), "durable terminal failure log should exist");
                    }
                    other => panic!("output ENOSPC must fail terminally without disk retry, got {other:?}"),
                }
            }
            _ => panic!("album postprocess should produce one terminal postprocess result"),
        }

        assert_eq!(
            retry_count.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "publish/output ENOSPC must not trigger scratch disk retry"
        );
        assert!(
            !staging_root.exists(),
            "scratch staging should still be cleaned after terminal output publish failure"
        );
        assert_eq!(
            scratch_config.active_reserved_bytes_for_test(),
            0,
            "scratch reservation should be released after terminal output publish failure"
        );
        let logs = captured_test_logs_since_for_item(log_cursor, "scratch-output-publish-item");
        assert!(
            !logs.iter().any(|line| line.contains("scratch retrying on disk")),
            "output publish exhaustion must not be logged as a scratch retry; logs were {logs:?}"
        );
    }

    fn processor_dispatch_request_for_path(
        root: &std::path::Path,
        item_id: &str,
        job_id: &str,
        container: std::path::PathBuf,
    ) -> PipelineRequest {
        let mut req = pipeline_request_for_processor_limit_test(root);
        req.item_id = item_id.to_string();
        req.job_id = job_id.to_string();
        req.container = container;
        req.output_root = root.join("out");
        req.log.root = root.join("logs");
        req.album_batch = None;
        req.album_batch_track = None;
        req.suppress_incremental_conversion_log_append = false;
        req.expected_album_track_count = None;
        req
    }

    #[test]
    fn queued_folder_dispatch_attaches_fragment_batch_before_scheduler_enqueue() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let track_01 = album_root.join("01 - Speak To Me.flac");
        let track_02 = album_root.join("02 - Breathe.flac");
        std::fs::write(&track_01, b"not real audio; dispatch test only").expect("track 1 file");
        std::fs::write(&track_02, b"not real audio; dispatch test only").expect("track 2 file");

        // Intentionally pass request/order as 02, then 01. Production dispatch
        // must not preserve filesystem/request enumeration order; it should
        // parse canonical track identity, sort, and then attach a shared
        // fragment-batch contract before build_initial_work() submits jobs.
        let req_02 = processor_dispatch_request_for_path(
            temp.path(),
            "item-02",
            "job-02",
            track_02.clone(),
        );
        let req_01 = processor_dispatch_request_for_path(
            temp.path(),
            "item-01",
            "job-01",
            track_01.clone(),
        );
        let mut items = vec![
            conversion_item_with_pipeline_request("item-02", req_02),
            conversion_item_with_pipeline_request("item-01", req_01),
        ];

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let prepared_02 = items[0]
            .pipeline_request
            .as_ref()
            .expect("item 02 has prepared request");
        let prepared_01 = items[1]
            .pipeline_request
            .as_ref()
            .expect("item 01 has prepared request");
        let batch_02 = prepared_02
            .album_batch
            .as_ref()
            .expect("track 02 enters fragment-backed batch");
        let batch_01 = prepared_01
            .album_batch
            .as_ref()
            .expect("track 01 enters fragment-backed batch");
        assert_eq!(batch_01.conversion_log_batch_id, batch_02.conversion_log_batch_id);
        assert_eq!(batch_01.expected_track_count, 2);
        assert_eq!(batch_02.expected_track_count, 2);
        assert!(!batch_01.uses_completion_order());
        assert!(!batch_02.uses_completion_order());

        let order_01 = prepared_01
            .album_batch_track
            .as_ref()
            .expect("track 01 receives dispatcher ordering");
        let order_02 = prepared_02
            .album_batch_track
            .as_ref()
            .expect("track 02 receives dispatcher ordering");
        assert_eq!(order_01.track_number, 1);
        assert_eq!(order_02.track_number, 2);
        assert!(order_01.source_ordinal < order_02.source_ordinal);
    }


    #[test]
    fn queued_folder_dispatch_uses_structural_completion_order_when_conversion_settings_differ() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let track_01 = album_root.join("01 - First.flac");
        let track_02 = album_root.join("02 - Second.flac");
        std::fs::write(&track_01, b"not real audio; dispatch test only").expect("track 1 file");
        std::fs::write(&track_02, b"not real audio; dispatch test only").expect("track 2 file");

        let req_01 = processor_dispatch_request_for_path(
            temp.path(),
            "item-01",
            "job-01",
            track_01,
        );
        let mut req_02 = processor_dispatch_request_for_path(
            temp.path(),
            "item-02",
            "job-02",
            track_02,
        );
        req_02.settings.force_encode = !req_02.settings.force_encode;
        assert_ne!(
            conversion_settings_fingerprint_key(&req_01.settings),
            conversion_settings_fingerprint_key(&req_02.settings),
            "test setup must exercise a real settings-fingerprint mismatch"
        );

        let mut items = vec![
            conversion_item_with_pipeline_request("item-01", req_01),
            conversion_item_with_pipeline_request("item-02", req_02),
        ];

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let mut coordination_ids = BTreeSet::new();
        let batches = items
            .iter()
            .map(|item| {
                let request = item
                    .pipeline_request
                    .as_ref()
                    .expect("heterogeneous item keeps explicit request");
                let coordination = request
                    .album_batch_track
                    .as_ref()
                    .expect("settings mismatch retains coordination identity");
                assert!(coordination_ids.insert(coordination.source_ordinal));
                assert!(!request.suppress_incremental_conversion_log_append);
                let batch = request
                    .album_batch
                    .as_ref()
                    .expect("heterogeneous settings retain one structural publish batch");
                assert!(batch.uses_completion_order());
                batch
            })
            .collect::<Vec<_>>();
        assert_eq!(coordination_ids.len(), 2);
        assert_eq!(
            batches[0].conversion_log_batch_id,
            batches[1].conversion_log_batch_id
        );
        assert_eq!(batches[0].expected_track_count, 2);
    }

    #[test]
    fn ordering_unprovable_batch_stays_structural_with_conversion_logs_enabled() {
        // §1.5 leg (a): the logs-ON regression pin. Before round 9 this
        // shape survived only via the legacy standalone-log incremental
        // escape; membership must now be structural regardless of logs.
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let track_a = album_root.join("Speak To Me.flac");
        let track_b = album_root.join("Breathe.flac");
        std::fs::write(&track_a, b"not real audio; dispatch test only").expect("track a");
        std::fs::write(&track_b, b"not real audio; dispatch test only").expect("track b");

        let req_a = processor_dispatch_request_for_path(temp.path(), "item-a", "job-a", track_a);
        let req_b = processor_dispatch_request_for_path(temp.path(), "item-b", "job-b", track_b);
        assert!(
            req_a.log.write_conversion_log,
            "leg (a) must exercise the logs-ON configuration"
        );

        let mut items = vec![
            conversion_item_with_pipeline_request("item-a", req_a),
            conversion_item_with_pipeline_request("item-b", req_b),
        ];
        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let batches = items
            .iter()
            .map(|item| {
                let request = item.pipeline_request.as_ref().expect("request retained");
                assert!(!request.suppress_incremental_conversion_log_append);
                let batch = request
                    .album_batch
                    .as_ref()
                    .expect("ordering-unprovable batch keeps structural membership with logs on");
                assert!(batch.uses_completion_order());
                batch
            })
            .collect::<Vec<_>>();
        assert_eq!(
            batches[0].conversion_log_batch_id,
            batches[1].conversion_log_batch_id
        );
        assert_eq!(batches[0].expected_track_count, 2);
    }

    #[test]
    fn completion_order_dispatch_validation_error_retains_structural_membership() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let track_a = album_root.join("First.flac");
        let track_b = album_root.join("Second.flac");
        std::fs::write(&track_a, b"not real audio; dispatch test only").expect("track a");
        std::fs::write(&track_b, b"not real audio; dispatch test only").expect("track b");

        let mut req_a =
            processor_dispatch_request_for_path(temp.path(), "item-a", "job-a", track_a);
        let mut req_b =
            processor_dispatch_request_for_path(temp.path(), "item-b", "job-b", track_b);
        // Force the primary completion-order helper through validate_request()'s
        // InvalidTemplate branch. The scheduler safety net must preserve the
        // already-verified same-album publish contract instead of converting
        // these siblings into colliding singleton publishes.
        req_a.naming.template.clear();
        req_b.naming.template.clear();

        let mut items = vec![
            conversion_item_with_pipeline_request("item-a", req_a),
            conversion_item_with_pipeline_request("item-b", req_b),
        ];
        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let mut coordination_ids = BTreeSet::new();
        let batches = items
            .iter()
            .map(|item| {
                let request = item.pipeline_request.as_ref().expect("request retained");
                assert!(!request.suppress_incremental_conversion_log_append);
                let coordination = request
                    .album_batch_track
                    .as_ref()
                    .expect("dispatch validation error retains coordination identity");
                assert!(coordination_ids.insert(coordination.source_ordinal));
                let batch = request
                    .album_batch
                    .as_ref()
                    .expect("dispatch validation error must retain structural membership");
                assert!(batch.uses_completion_order());
                batch
            })
            .collect::<Vec<_>>();
        assert_eq!(coordination_ids.len(), 2);
        assert_eq!(
            batches[0].conversion_log_batch_id,
            batches[1].conversion_log_batch_id
        );
        assert_eq!(batches[0].expected_track_count, 2);
    }

    #[test]
    fn vinyl_track_order_parser_is_generic_case_insensitive_and_numeric_within_side() {
        let mut values = [
            "E2", "D2", "C2", "B2", "A10", "A9", "A2", "A1", "b1", "c1", "d1", "e1",
        ]
        .into_iter()
        .map(|value| (parse_vinyl_track_order(value).expect("valid vinyl order"), value))
        .collect::<Vec<_>>();
        values.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            values.into_iter().map(|(_, raw)| raw).collect::<Vec<_>>(),
            vec!["A1", "A2", "A9", "A10", "b1", "B2", "c1", "C2", "d1", "D2", "e1", "E2"]
        );
        assert_eq!(parse_vinyl_track_order("a1"), parse_vinyl_track_order("A1"));
        assert_eq!(parse_vinyl_track_order("c2"), parse_vinyl_track_order("C2"));
        assert_eq!(parse_vinyl_track_order("e3"), parse_vinyl_track_order("E3"));

        let z = parse_vinyl_track_order("Z1").expect("Z side");
        let aa = parse_vinyl_track_order("AA1").expect("AA side");
        let ab = parse_vinyl_track_order("AB1").expect("AB side");
        assert!(z < aa && aa < ab, "multi-letter sides remain monotonic after Z");

        for rejected in ["", "A", "1", "A0", "A1/2", "A 1", "A-1", "1A"] {
            assert_eq!(parse_vinyl_track_order(rejected), None, "{rejected}");
        }
        assert_eq!(parse_dispatch_track_number("7/12"), DispatchTrackNumber::Numeric(7));
        assert_eq!(parse_dispatch_track_number("7 of 12"), DispatchTrackNumber::Numeric(7));
        assert_eq!(parse_dispatch_track_number("B2/5"), DispatchTrackNumber::Unorderable);
    }

    #[test]
    fn source_vinyl_tracknumber_is_scheduler_evidence_without_metadata_mutation() {
        let mut metadata = TrackMetadata {
            track_number: Some(2),
            ..TrackMetadata::default()
        };
        crate::convert::pipeline::insert_source_text_tag(
            &mut metadata.extra,
            "TRACKNUMBER",
            "C2",
        );
        let before = metadata.extra.clone();

        let probe = batch_identity_probe_from_track_metadata(&metadata)
            .expect("vinyl source metadata produces a batch probe");

        assert_eq!(
            probe.scheduler_track_number,
            Some(DispatchTrackNumber::Vinyl(VinylTrackOrderKey {
                side: "C".to_string(),
                position: 2,
            }))
        );
        assert_eq!(metadata.track_number, Some(2));
        assert_eq!(metadata.extra, before, "scheduler probing must not rewrite source tags");
    }

    #[test]
    fn vinyl_lettered_tracknumber_tags_dispatch_in_proven_side_position_order() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let input = [
            ("E2", "item-e2"),
            ("C2", "item-c2"),
            ("A10", "item-a10"),
            ("B2", "item-b2"),
            ("D2", "item-d2"),
            ("D1", "item-d1"),
            ("A2", "item-a2"),
            ("E1", "item-e1"),
            ("C1", "item-c1"),
            ("B1", "item-b1"),
            ("A1", "item-a1"),
        ];
        let expected_order = ["A1", "A2", "A10", "B1", "B2", "C1", "C2", "D1", "D2", "E1", "E2"];
        let mut items = Vec::new();
        for (raw_track_number, item_id) in input {
            let path = album_root.join(format!("{item_id}.flac"));
            std::fs::write(
                &path,
                fake_flac_with_vorbis_comments(&[
                    ("ALBUM", "Vinyl Album"),
                    ("TRACKNUMBER", raw_track_number),
                ]),
            )
            .expect("vinyl FLAC fixture");
            let request = processor_dispatch_request_for_path(
                temp.path(),
                item_id,
                &format!("job-{item_id}"),
                path,
            );
            items.push(conversion_item_with_pipeline_request(item_id, request));
        }

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let mut by_ordinal = BTreeMap::new();
        let mut batch_id = None;
        for item in &items {
            let request = item.pipeline_request.as_ref().expect("request retained");
            assert!(!request.suppress_incremental_conversion_log_append);
            let batch = request
                .album_batch
                .as_ref()
                .expect("vinyl-numbered siblings share an album batch");
            assert!(
                !batch.uses_completion_order(),
                "fully parseable vinyl numbering must use proven scheduler order"
            );
            assert_eq!(batch.expected_track_count, expected_order.len());
            if let Some(existing) = batch_id.as_ref() {
                assert_eq!(existing, &batch.conversion_log_batch_id);
            } else {
                batch_id = Some(batch.conversion_log_batch_id.clone());
            }
            let track = request
                .album_batch_track
                .as_ref()
                .expect("vinyl track receives internal coordination identity");
            assert_eq!(track.source_ordinal, track.track_number);
            assert!(track.disc_number.is_none());
            let raw = item.id.strip_prefix("item-").expect("item id").to_ascii_uppercase();
            by_ordinal.insert(track.source_ordinal, raw);
        }
        assert!(batch_id.is_some());
        assert_eq!(
            by_ordinal.into_values().collect::<Vec<_>>(),
            expected_order.iter().map(|value| value.to_ascii_uppercase()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn malformed_explicit_vinyl_tracknumber_uses_structural_fallback_not_filename_guessing() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let first = album_root.join("01 - First.wv");
        let second = album_root.join("02 - Second.wv");
        std::fs::write(&first, fake_apev2_with_items(&[("Album", "Vinyl Album"), ("Track", "A1")])).expect("first fixture");
        std::fs::write(&second, fake_apev2_with_items(&[("Album", "Vinyl Album"), ("Track", "B2/5")])).expect("second fixture");

        let mut items = vec![
            conversion_item_with_pipeline_request(
                "item-a",
                processor_dispatch_request_for_path(temp.path(), "item-a", "job-a", first),
            ),
            conversion_item_with_pipeline_request(
                "item-b",
                processor_dispatch_request_for_path(temp.path(), "item-b", "job-b", second),
            ),
        ];
        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let batches = items
            .iter()
            .map(|item| {
                let request = item.pipeline_request.as_ref().expect("request retained");
                assert!(!request.suppress_incremental_conversion_log_append);
                let batch = request.album_batch.as_ref().expect("shared structural batch");
                assert!(batch.uses_completion_order());
                batch.conversion_log_batch_id.clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(batches[0], batches[1]);
    }

    #[test]
    fn mixed_numeric_and_vinyl_tracknumbers_preserve_album_via_structural_completion_order() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let vinyl = album_root.join("vinyl.wv");
        let numeric = album_root.join("numeric.wv");
        std::fs::write(&vinyl, fake_apev2_with_items(&[("Album", "Vinyl Album"), ("Track", "A1")]))
            .expect("vinyl fixture");
        std::fs::write(&numeric, fake_apev2_with_items(&[("Album", "Vinyl Album"), ("Track", "2")]))
            .expect("numeric fixture");

        let mut items = vec![
            conversion_item_with_pipeline_request(
                "item-vinyl",
                processor_dispatch_request_for_path(
                    temp.path(),
                    "item-vinyl",
                    "job-vinyl",
                    vinyl,
                ),
            ),
            conversion_item_with_pipeline_request(
                "item-numeric",
                processor_dispatch_request_for_path(
                    temp.path(),
                    "item-numeric",
                    "job-numeric",
                    numeric,
                ),
            ),
        ];
        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let mut batch_id = None;
        let mut coordination_ids = BTreeSet::new();
        for item in &items {
            let request = item.pipeline_request.as_ref().expect("request retained");
            assert!(!request.suppress_incremental_conversion_log_append);
            let batch = request
                .album_batch
                .as_ref()
                .expect("mixed numbering remains one structural album batch");
            assert!(batch.uses_completion_order());
            if let Some(existing) = batch_id.as_ref() {
                assert_eq!(existing, &batch.conversion_log_batch_id);
            } else {
                batch_id = Some(batch.conversion_log_batch_id.clone());
            }
            let coordination = request
                .album_batch_track
                .as_ref()
                .expect("structural fallback retains distinct coordination identities");
            assert!(coordination_ids.insert(coordination.source_ordinal));
        }
        assert_eq!(coordination_ids.len(), 2);
    }

    #[test]
    fn queued_folder_dispatch_splits_tracks_with_different_action_pipelines() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let track_01 = album_root.join("01 - First.flac");
        let track_02 = album_root.join("02 - Second.flac");
        std::fs::write(&track_01, b"not real audio; dispatch test only").expect("track 1 file");
        std::fs::write(&track_02, b"not real audio; dispatch test only").expect("track 2 file");

        let req_01 = processor_dispatch_request_for_path(
            temp.path(),
            "item-01",
            "job-01",
            track_01,
        );
        let mut req_02 = processor_dispatch_request_for_path(
            temp.path(),
            "item-02",
            "job-02",
            track_02,
        );
        let destructive_pipeline = crate::convert::pipeline::ActionPipeline {
            pre: Vec::new(),
            post: vec![crate::convert::pipeline::ConversionAction::CreateFolder(
                crate::convert::pipeline::CreateFolderAction {
                    path: PathBuf::from("post-action"),
                    continue_on_error: false,
                },
            )],
        };
        req_02.actions = destructive_pipeline.clone();

        let mut first = conversion_item_with_pipeline_request("item-01", req_01);
        first.options.actions = crate::convert::pipeline::ActionPipeline::default();
        let mut second = conversion_item_with_pipeline_request("item-02", req_02);
        second.options.actions = destructive_pipeline.clone();
        let mut items = vec![first, second];

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let first_request = items[0]
            .pipeline_request
            .as_ref()
            .expect("first request remains available");
        let second_request = items[1]
            .pipeline_request
            .as_ref()
            .expect("second request remains available");
        assert_ne!(
            independent_single_file_album_batch_lifecycle_key(first_request).unwrap(),
            independent_single_file_album_batch_lifecycle_key(second_request).unwrap(),
            "test setup must carry distinct canonical lifecycle contracts"
        );
        let first_batch = first_request
            .album_batch
            .as_ref()
            .expect("first request retains structural batch membership");
        let second_batch = second_request
            .album_batch
            .as_ref()
            .expect("second request retains structural batch membership");
        assert!(first_batch.uses_completion_order());
        assert!(second_batch.uses_completion_order());
        assert_eq!(
            first_batch.conversion_log_batch_id,
            second_batch.conversion_log_batch_id
        );
        assert_eq!(second_request.actions, destructive_pipeline);
    }

    #[test]
    fn dispatch_track_number_parser_accepts_only_strict_track_prefixes() {
        assert_eq!(strict_track_number_from_dispatch_path(std::path::Path::new("01 - Speak To Me.flac")), Some(1));
        assert_eq!(strict_track_number_from_dispatch_path(std::path::Path::new("1. Breathe.flac")), Some(1));
        assert_eq!(strict_track_number_from_dispatch_path(std::path::Path::new("02_Time.flac")), Some(2));

        assert_eq!(strict_track_number_from_dispatch_path(std::path::Path::new("Speak To Me.flac")), None);
        assert_eq!(strict_track_number_from_dispatch_path(std::path::Path::new("Symphony No. 5.flac")), None);
        assert_eq!(strict_track_number_from_dispatch_path(std::path::Path::new("Take 2.flac")), None);
        assert_eq!(strict_track_number_from_dispatch_path(std::path::Path::new("2024 Remaster.flac")), None);
    }

    fn fake_flac_with_vorbis_comments(comments: &[(&str, &str)]) -> Vec<u8> {
        let mut block = Vec::new();
        let vendor = b"tonepoet-test";
        block.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        block.extend_from_slice(vendor);
        block.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for (key, value) in comments {
            let comment = format!("{key}={value}");
            block.extend_from_slice(&(comment.len() as u32).to_le_bytes());
            block.extend_from_slice(comment.as_bytes());
        }

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"fLaC");
        bytes.push(0x84);
        bytes.push(((block.len() >> 16) & 0xff) as u8);
        bytes.push(((block.len() >> 8) & 0xff) as u8);
        bytes.push((block.len() & 0xff) as u8);
        bytes.extend_from_slice(&block);
        bytes
    }

    fn synchsafe_bytes(size: usize) -> [u8; 4] {
        [
            ((size >> 21) & 0x7f) as u8,
            ((size >> 14) & 0x7f) as u8,
            ((size >> 7) & 0x7f) as u8,
            (size & 0x7f) as u8,
        ]
    }

    fn fake_id3v23_with_text_frames(frames: &[(&str, &str)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (id, value) in frames {
            let mut payload = Vec::new();
            payload.push(3); // UTF-8 text frame
            payload.extend_from_slice(value.as_bytes());
            body.extend_from_slice(id.as_bytes());
            body.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            body.extend_from_slice(&[0, 0]);
            body.extend_from_slice(&payload);
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"ID3");
        bytes.extend_from_slice(&[3, 0, 0]);
        bytes.extend_from_slice(&synchsafe_bytes(body.len()));
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(b"fake audio payload");
        bytes
    }

    fn id3v2_unsynchronize_for_test(data: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(data.len());
        for byte in data {
            output.push(*byte);
            if *byte == 0xff {
                output.push(0x00);
            }
        }
        output
    }

    fn fake_unsynchronized_id3v23_with_utf16_track(track: &str) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.push(1); // UTF-16 with BOM
        payload.extend_from_slice(&[0xff, 0xfe]);
        for unit in track.encode_utf16() {
            payload.extend_from_slice(&unit.to_le_bytes());
        }

        let mut body = Vec::new();
        body.extend_from_slice(b"TRCK");
        body.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&payload);
        let body = id3v2_unsynchronize_for_test(&body);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"ID3");
        bytes.extend_from_slice(&[3, 0, 0x80]);
        bytes.extend_from_slice(&synchsafe_bytes(body.len()));
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(b"fake audio payload");
        bytes
    }

    fn fake_id3v1_with_track(track: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"fake audio payload");
        let mut tag = [0u8; 128];
        tag[..3].copy_from_slice(b"TAG");
        tag[3..8].copy_from_slice(b"Title");
        tag[33..39].copy_from_slice(b"Artist");
        tag[63..68].copy_from_slice(b"Album");
        tag[93..97].copy_from_slice(b"2026");
        tag[125] = 0;
        tag[126] = track;
        tag[127] = 255;
        bytes.extend_from_slice(&tag);
        bytes
    }

    fn fake_apev2_with_items(items: &[(&str, &str)]) -> Vec<u8> {
        let mut item_bytes = Vec::new();
        for (key, value) in items {
            item_bytes.extend_from_slice(&(value.as_bytes().len() as u32).to_le_bytes());
            item_bytes.extend_from_slice(&0u32.to_le_bytes());
            item_bytes.extend_from_slice(key.as_bytes());
            item_bytes.push(0);
            item_bytes.extend_from_slice(value.as_bytes());
        }
        let tag_size = item_bytes.len() + 32;
        let mut footer = Vec::new();
        footer.extend_from_slice(b"APETAGEX");
        footer.extend_from_slice(&2000u32.to_le_bytes());
        footer.extend_from_slice(&(tag_size as u32).to_le_bytes());
        footer.extend_from_slice(&(items.len() as u32).to_le_bytes());
        footer.extend_from_slice(&0u32.to_le_bytes());
        footer.extend_from_slice(&0u64.to_le_bytes());

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"fake audio payload");
        bytes.extend_from_slice(&item_bytes);
        bytes.extend_from_slice(&footer);
        bytes
    }

    #[test]
    fn dispatch_reads_flac_tracknumber_metadata_for_unnumbered_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let track_a = album_root.join("Speak To Me.flac");
        let track_b = album_root.join("Breathe.flac");
        std::fs::write(&track_a, fake_flac_with_vorbis_comments(&[("TRACKNUMBER", "1/2")]))
            .expect("track a flac");
        std::fs::write(&track_b, fake_flac_with_vorbis_comments(&[("TRACKNUMBER", "2/2")]))
            .expect("track b flac");

        let req_a = processor_dispatch_request_for_path(
            temp.path(),
            "item-a",
            "job-a",
            track_a,
        );
        let req_b = processor_dispatch_request_for_path(
            temp.path(),
            "item-b",
            "job-b",
            track_b,
        );
        let mut items = vec![
            conversion_item_with_pipeline_request("item-b", req_b),
            conversion_item_with_pipeline_request("item-a", req_a),
        ];

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let prepared_b = items[0]
            .pipeline_request
            .as_ref()
            .expect("track b prepared request");
        let prepared_a = items[1]
            .pipeline_request
            .as_ref()
            .expect("track a prepared request");
        assert_eq!(
            prepared_a.album_batch.as_ref().map(|batch| batch.conversion_log_batch_id.clone()),
            prepared_b.album_batch.as_ref().map(|batch| batch.conversion_log_batch_id.clone())
        );
        assert_eq!(
            prepared_a.album_batch_track.as_ref().map(|track| track.track_number),
            Some(1)
        );
        assert_eq!(
            prepared_b.album_batch_track.as_ref().map(|track| track.track_number),
            Some(2)
        );
        assert!(!prepared_a.suppress_incremental_conversion_log_append);
        assert!(!prepared_b.suppress_incremental_conversion_log_append);
    }

    fn batch_probe(
        path: impl Into<String>,
        album: Option<&str>,
        album_artist: Option<&str>,
        artist: Option<&str>,
        date: Option<&str>,
        disc_number: Option<u32>,
        total_discs: Option<u32>,
        track_number: Option<u32>,
    ) -> BatchIdentityProbe {
        BatchIdentityProbe {
            path_key: path.into(),
            album: album.map(str::to_string),
            album_artist: album_artist.map(str::to_string),
            artist: artist.map(str::to_string),
            date: date.map(str::to_string),
            disc_number,
            total_discs,
            track_number,
            scheduler_track_number: track_number.map(DispatchTrackNumber::Numeric),
        }
    }

    #[test]
    fn batch_identity_merges_eat_a_peach_catalog_variants_under_one_album() {
        let probes = vec![
            batch_probe(
                "/library/abb/eat/disc 01/01.flac",
                Some("Eat a Peach (Japan / Polydor P58P 25005)"),
                Some("The Allman Brothers Band"),
                None,
                Some("1972"),
                Some(1),
                Some(2),
                Some(1),
            ),
            batch_probe(
                "/library/abb/eat/disc 02/01.flac",
                Some("Eat a Peach (Japan / Polydor P58P 25006)"),
                Some("The Allman Brothers Band"),
                None,
                Some("1972"),
                Some(2),
                Some(2),
                Some(1),
            ),
        ];

        let identity = resolve_batch_album_identity_from_probes(probes, None)
            .expect("catalog-variant two-disc set resolves");
        assert_eq!(identity.album.as_deref(), Some("Eat a Peach (Japan / Polydor P58P 25005-6)"));
        assert_eq!(identity.album_artist.as_deref(), Some("The Allman Brothers Band"));
        assert_eq!(identity.date.as_deref(), Some("1972"));
        assert_eq!(identity.total_discs, Some(2));
        assert_eq!(identity.disc_number_for_path(std::path::Path::new("/library/abb/eat/disc 01/01.flac")), Some(1));
        assert_eq!(identity.disc_number_for_path(std::path::Path::new("/library/abb/eat/disc 02/01.flac")), Some(2));
    }

    #[test]
    fn batch_identity_refuses_broad_trailing_number_title_merge() {
        let probes = vec![
            batch_probe(
                "/library/classical/set/disc 01/01.flac",
                Some("Symphony 1"),
                Some("Example Orchestra"),
                None,
                Some("1980"),
                Some(1),
                Some(2),
                Some(1),
            ),
            batch_probe(
                "/library/classical/set/disc 02/01.flac",
                Some("Symphony 2"),
                Some("Example Orchestra"),
                None,
                Some("1980"),
                Some(2),
                Some(2),
                Some(1),
            ),
        ];

        assert!(
            resolve_batch_album_identity_from_probes(probes, None).is_none(),
            "shared source-root and disc evidence alone must not merge ordinary trailing title numbers"
        );
    }

    #[test]
    fn batch_identity_models_real_dreams_four_disc_majority_with_one_album_artist_outlier() {
        let mut probes = Vec::new();
        let disc_counts = [(1_u32, 17_u32), (2, 13), (3, 13), (4, 12)];
        for (disc, count) in disc_counts {
            for track in 1..=count {
                let outlier = disc == 1 && track == 1;
                probes.push(batch_probe(
                    format!("/library/abb/dreams/disc {disc:02}/{track:02}.flac"),
                    Some(&format!("Dreams (Disc {disc})")),
                    Some(if outlier { "Duane Allman" } else { "The Allman Brothers Band" }),
                    None,
                    Some("1989"),
                    Some(disc),
                    Some(4),
                    Some(track),
                ));
            }
        }
        assert_eq!(probes.len(), 55, "test fixture should model the real 55-track box");

        let identity = resolve_batch_album_identity_from_probes(probes, None)
            .expect("four-disc majority evidence resolves");
        assert_eq!(identity.album.as_deref(), Some("Dreams"));
        assert_eq!(identity.album_artist.as_deref(), Some("The Allman Brothers Band"));
        assert_eq!(identity.date.as_deref(), Some("1989"));
        assert_eq!(identity.total_discs, Some(4));
        assert_eq!(identity.disc_number_for_path(std::path::Path::new("/library/abb/dreams/disc 01/01.flac")), Some(1));
        assert_eq!(identity.disc_number_for_path(std::path::Path::new("/library/abb/dreams/disc 04/12.flac")), Some(4));
    }

    #[test]
    fn batch_identity_uses_sibling_disc_folders_when_disc_tags_are_absent() {
        let probes = vec![
            batch_probe(
                "/library/untagged/Album/disc 01/01.flac",
                Some("Album"),
                Some("Artist"),
                None,
                Some("1971"),
                None,
                None,
                Some(1),
            ),
            batch_probe(
                "/library/untagged/Album/disc 02/01.flac",
                Some("Album"),
                Some("Artist"),
                None,
                Some("1971"),
                None,
                None,
                Some(1),
            ),
        ];

        let identity = resolve_batch_album_identity_from_probes(probes, None)
            .expect("sibling disc directories prove multi-disc identity without disc tags");
        assert_eq!(identity.album.as_deref(), Some("Album"));
        assert_eq!(identity.total_discs, Some(2));
        assert_eq!(identity.disc_number_for_path(std::path::Path::new("/library/untagged/Album/disc 01/01.flac")), Some(1));
        assert_eq!(identity.disc_number_for_path(std::path::Path::new("/library/untagged/Album/disc 02/01.flac")), Some(2));
    }

    #[test]
    fn batch_identity_resolution_is_deterministic_across_reruns_and_input_order() {
        let forward = vec![
            batch_probe(
                "/library/abb/eat/disc 01/01.flac",
                Some("Eat a Peach (Japan / Polydor P58P 25005)"),
                Some("The Allman Brothers Band"),
                None,
                Some("1972"),
                Some(1),
                Some(2),
                Some(1),
            ),
            batch_probe(
                "/library/abb/eat/disc 02/01.flac",
                Some("Eat a Peach (Japan / Polydor P58P 25006)"),
                Some("The Allman Brothers Band"),
                None,
                Some("1972"),
                Some(2),
                Some(2),
                Some(1),
            ),
        ];
        let mut reverse = forward.clone();
        reverse.reverse();

        let first = resolve_batch_album_identity_from_probes(forward, None).expect("first run");
        let second = resolve_batch_album_identity_from_probes(reverse, None).expect("second run");
        assert_eq!(first, second, "same tree must resolve to same identity independent of enumeration order");
    }

    #[test]
    fn batch_identity_probe_uses_materializer_metadata_shape_for_single_files() {
        let mut metadata = TrackMetadata::default();
        metadata.artist = Some("The Allman Brothers Band".to_string());
        metadata.album_artist = Some("The Allman Brothers Band".to_string());
        metadata.date = Some("1972".to_string());
        metadata.disc_number = Some(1);
        metadata.track_number = Some(1);
        metadata.extra.insert("album".to_string(), "Eat a Peach".to_string());
        metadata.extra.insert("disctotal".to_string(), "2".to_string());

        let probe = batch_identity_probe_from_track_metadata(&metadata)
            .expect("canonical materializer metadata should supply batch identity evidence");
        assert_eq!(probe.album.as_deref(), Some("Eat a Peach"));
        assert_eq!(probe.album_artist.as_deref(), Some("The Allman Brothers Band"));
        assert_eq!(probe.total_discs, Some(2));
        assert_eq!(probe.disc_number, Some(1));
        assert_eq!(probe.track_number, Some(1));
    }

    #[test]
    fn cue_image_disc_batches_receive_set_level_log_identity_and_track_count() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("The Allman Brothers Band - Dreams");
        let disc1 = album_root.join("disc 01");
        let disc2 = album_root.join("disc 02");
        std::fs::create_dir_all(&disc1).expect("disc1 dir");
        std::fs::create_dir_all(&disc2).expect("disc2 dir");
        let cue_1 = disc1.join("dreams-disc1.cue");
        let cue_2 = disc2.join("dreams-disc2.cue");
        std::fs::write(
            &cue_1,
            r#"REM DATE 1989
PERFORMER "The Allman Brothers Band"
TITLE "Dreams (Disc 1)"
FILE "disc1.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Track 1"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Track 2"
    INDEX 01 03:00:00
"#,
        )
        .expect("cue 1");
        std::fs::write(
            &cue_2,
            r#"REM DATE 1989
PERFORMER "The Allman Brothers Band"
TITLE "Dreams (Disc 2)"
FILE "disc2.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Track 1"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Track 2"
    INDEX 01 03:00:00
  TRACK 03 AUDIO
    TITLE "Track 3"
    INDEX 01 06:00:00
"#,
        )
        .expect("cue 2");

        let mut items = vec![
            conversion_item_with_pipeline_request(
                "dreams-cue-1",
                processor_dispatch_request_for_path(temp.path(), "dreams-cue-1", "job-cue-1", cue_1.clone()),
            ),
            conversion_item_with_pipeline_request(
                "dreams-cue-2",
                processor_dispatch_request_for_path(temp.path(), "dreams-cue-2", "job-cue-2", cue_2.clone()),
            ),
        ];
        for item in &mut items {
            item.options.create_disc_subfolders = true;
        }

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let first = items[0].pipeline_request.as_ref().expect("first cue prepared");
        let second = items[1].pipeline_request.as_ref().expect("second cue prepared");
        let batch = first.album_batch.as_ref().expect("first cue enters album batch");
        assert_eq!(Some(batch), second.album_batch.as_ref());
        assert_eq!(batch.expected_track_count, 5, "expected count is selected CUE tracks, not number of CUE files");
        let identity = batch.resolved_identity().expect("cue batch resolved identity");
        assert_eq!(identity.album.as_deref(), Some("Dreams"));
        assert_eq!(identity.album_artist.as_deref(), Some("The Allman Brothers Band"));
        assert_eq!(identity.disc_number_for_path(&cue_1), Some(1));
        assert_eq!(identity.disc_number_for_path(&cue_2), Some(2));
    }

    #[test]
    fn disc_subfolders_off_does_not_apply_batch_identity_resolution() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("The Allman Brothers Band - Eat a Peach");
        let disc1 = album_root.join("disc 01");
        let disc2 = album_root.join("disc 02");
        std::fs::create_dir_all(&disc1).expect("disc1 dir");
        std::fs::create_dir_all(&disc2).expect("disc2 dir");
        let track_01 = disc1.join("01 - First.flac");
        let track_02 = disc2.join("01 - Second.flac");
        std::fs::write(&track_01, b"not real audio; dispatch test only").expect("track 1");
        std::fs::write(&track_02, b"not real audio; dispatch test only").expect("track 2");

        let mut items = vec![
            conversion_item_with_pipeline_request(
                "eat-1",
                processor_dispatch_request_for_path(temp.path(), "eat-1", "job-eat-1", track_01),
            ),
            conversion_item_with_pipeline_request(
                "eat-2",
                processor_dispatch_request_for_path(temp.path(), "eat-2", "job-eat-2", track_02),
            ),
        ];
        for item in &mut items {
            item.options.create_disc_subfolders = false;
        }

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        for item in &items {
            let request = item.pipeline_request.as_ref().expect("prepared request");
            assert!(
                request
                    .album_batch
                    .as_ref()
                    .and_then(|batch| batch.resolved_identity())
                    .is_none(),
                "disc-subfolder switch off must preserve pre-existing per-tag identity behavior"
            );
            assert!(request.batch_resolved_identity.is_none());
        }
    }

    #[test]
    fn flac_metadata_track_context_parses_track_and_disc_ordinals() {
        let bytes = fake_flac_with_vorbis_comments(&[
            ("DISCNUMBER", "2/3"),
            ("TRACKNUMBER", "07/12"),
        ]);
        assert_eq!(parse_flac_vorbis_comment_track_context(&bytes), Some((Some(2), 7)));
    }

    #[test]
    fn flac_metadata_track_context_preserves_generic_vinyl_scheduler_order() {
        let bytes = fake_flac_with_vorbis_comments(&[("TRACKNUMBER", "c10")]);
        let context = parse_flac_vorbis_comment_track_order_context(&bytes)
            .expect("vinyl FLAC track context");
        assert_eq!(context.disc_number, None);
        assert_eq!(
            context.track_number,
            DispatchTrackNumber::Vinyl(VinylTrackOrderKey {
                side: "C".to_string(),
                position: 10,
            })
        );
    }

    #[test]
    fn flac_metadata_reader_streams_metadata_blocks_without_audio_payload() {
        let vorbis_file = fake_flac_with_vorbis_comments(&[
            ("DISCNUMBER", "2/3"),
            ("TRACKNUMBER", "07/12"),
        ]);
        let vorbis_block = &vorbis_file[8..];
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"fLaC");
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 34]);
        bytes.extend_from_slice(&[0u8; 34]);
        bytes.push(0x84);
        bytes.push(((vorbis_block.len() >> 16) & 0xff) as u8);
        bytes.push(((vorbis_block.len() >> 8) & 0xff) as u8);
        bytes.push((vorbis_block.len() & 0xff) as u8);
        bytes.extend_from_slice(vorbis_block);
        bytes.extend_from_slice(&[0xff; 1024]);

        let mut cursor = std::io::Cursor::new(bytes);
        assert_eq!(
            read_flac_vorbis_comment_track_context(&mut cursor),
            Some((Some(2), 7))
        );
    }

    #[test]
    fn id3v2_metadata_track_context_parses_track_and_disc_ordinals() {
        let bytes = fake_id3v23_with_text_frames(&[
            ("TPOS", "2/3"),
            ("TRCK", "07/12"),
        ]);
        assert_eq!(parse_id3v2_track_context(3, 0, &bytes[10..bytes.len() - b"fake audio payload".len()]), Some((Some(2), 7)));
    }

    #[test]
    fn id3v2_metadata_track_context_deunsynchronizes_tag_body() {
        let bytes = fake_unsynchronized_id3v23_with_utf16_track("07/12");
        assert_eq!(
            parse_id3v2_track_context(3, 0x80, &bytes[10..bytes.len() - b"fake audio payload".len()]),
            Some((None, 7))
        );
    }

    #[test]
    fn id3v1_metadata_track_context_parses_track_ordinal() {
        let bytes = fake_id3v1_with_track(7);
        let tag = &bytes[bytes.len() - 128..];
        assert_eq!(parse_id3v1_track_context(tag), Some((None, 7)));
    }

    #[test]
    fn apev2_metadata_track_context_parses_track_and_disc_ordinals() {
        let mut item_bytes = Vec::new();
        for (key, value) in [("Disc", "2/3"), ("Track", "07/12")] {
            item_bytes.extend_from_slice(&(value.as_bytes().len() as u32).to_le_bytes());
            item_bytes.extend_from_slice(&0u32.to_le_bytes());
            item_bytes.extend_from_slice(key.as_bytes());
            item_bytes.push(0);
            item_bytes.extend_from_slice(value.as_bytes());
        }
        assert_eq!(parse_apev2_track_context(&item_bytes, 2), Some((Some(2), 7)));
    }

    #[test]
    fn dispatch_reads_id3v1_tracknumber_metadata_for_unnumbered_mp3_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let track_a = album_root.join("Speak To Me.mp3");
        let track_b = album_root.join("Breathe.mp3");
        std::fs::write(&track_a, fake_id3v1_with_track(1))
            .expect("track a id3v1");
        std::fs::write(&track_b, fake_id3v1_with_track(2))
            .expect("track b id3v1");

        let req_a = processor_dispatch_request_for_path(
            temp.path(),
            "item-a",
            "job-a",
            track_a,
        );
        let req_b = processor_dispatch_request_for_path(
            temp.path(),
            "item-b",
            "job-b",
            track_b,
        );
        let mut items = vec![
            conversion_item_with_pipeline_request("item-b", req_b),
            conversion_item_with_pipeline_request("item-a", req_a),
        ];

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let prepared_b = items[0]
            .pipeline_request
            .as_ref()
            .expect("track b prepared request");
        let prepared_a = items[1]
            .pipeline_request
            .as_ref()
            .expect("track a prepared request");
        assert_eq!(
            prepared_a.album_batch_track.as_ref().map(|track| track.track_number),
            Some(1)
        );
        assert_eq!(
            prepared_b.album_batch_track.as_ref().map(|track| track.track_number),
            Some(2)
        );
        assert_eq!(
            prepared_a.album_batch.as_ref().map(|batch| batch.conversion_log_batch_id.clone()),
            prepared_b.album_batch.as_ref().map(|batch| batch.conversion_log_batch_id.clone())
        );
    }

    #[test]
    fn dispatch_reads_id3v2_tracknumber_metadata_for_unnumbered_mp3_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let track_a = album_root.join("Speak To Me.mp3");
        let track_b = album_root.join("Breathe.mp3");
        std::fs::write(&track_a, fake_id3v23_with_text_frames(&[("TRCK", "1/2")]))
            .expect("track a id3");
        std::fs::write(&track_b, fake_id3v23_with_text_frames(&[("TRCK", "2/2")]))
            .expect("track b id3");

        let req_a = processor_dispatch_request_for_path(
            temp.path(),
            "item-a",
            "job-a",
            track_a,
        );
        let req_b = processor_dispatch_request_for_path(
            temp.path(),
            "item-b",
            "job-b",
            track_b,
        );
        let mut items = vec![
            conversion_item_with_pipeline_request("item-b", req_b),
            conversion_item_with_pipeline_request("item-a", req_a),
        ];

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let prepared_b = items[0]
            .pipeline_request
            .as_ref()
            .expect("track b prepared request");
        let prepared_a = items[1]
            .pipeline_request
            .as_ref()
            .expect("track a prepared request");
        assert_eq!(
            prepared_a.album_batch_track.as_ref().map(|track| track.track_number),
            Some(1)
        );
        assert_eq!(
            prepared_b.album_batch_track.as_ref().map(|track| track.track_number),
            Some(2)
        );
        assert_eq!(
            prepared_a.album_batch.as_ref().map(|batch| batch.conversion_log_batch_id.clone()),
            prepared_b.album_batch.as_ref().map(|batch| batch.conversion_log_batch_id.clone())
        );
    }

    #[test]
    fn dispatch_reads_apev2_tracknumber_metadata_for_unnumbered_wavpack_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let track_a = album_root.join("Speak To Me.wv");
        let track_b = album_root.join("Breathe.wv");
        std::fs::write(&track_a, fake_apev2_with_items(&[("Track", "1/2")]))
            .expect("track a apev2");
        std::fs::write(&track_b, fake_apev2_with_items(&[("Track", "2/2")]))
            .expect("track b apev2");

        let req_a = processor_dispatch_request_for_path(
            temp.path(),
            "item-a",
            "job-a",
            track_a,
        );
        let req_b = processor_dispatch_request_for_path(
            temp.path(),
            "item-b",
            "job-b",
            track_b,
        );
        let mut items = vec![
            conversion_item_with_pipeline_request("item-b", req_b),
            conversion_item_with_pipeline_request("item-a", req_a),
        ];

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let prepared_b = items[0]
            .pipeline_request
            .as_ref()
            .expect("track b prepared request");
        let prepared_a = items[1]
            .pipeline_request
            .as_ref()
            .expect("track a prepared request");
        assert_eq!(
            prepared_a.album_batch_track.as_ref().map(|track| track.track_number),
            Some(1)
        );
        assert_eq!(
            prepared_b.album_batch_track.as_ref().map(|track| track.track_number),
            Some(2)
        );
        assert_eq!(
            prepared_a.album_batch.as_ref().map(|batch| batch.conversion_log_batch_id.clone()),
            prepared_b.album_batch.as_ref().map(|batch| batch.conversion_log_batch_id.clone())
        );
    }

    #[test]
    fn dispatch_disc_directory_parser_accepts_common_disc_folder_names() {
        for name in ["Disc1", "Disc 1", "Disc-1", "Disc_1", "Disk1", "Disk 1", "CD1", "CD 1"] {
            let path = std::path::Path::new(name);
            assert!(is_disc_directory(path), "{name} should be recognized as a disc directory");
            assert_eq!(disc_number_from_directory_name(name), Some(1));
        }

        assert!(is_disc_directory(std::path::Path::new("disc")));
        assert!(is_disc_directory(std::path::Path::new("disk")));
        assert!(is_disc_directory(std::path::Path::new("cd")));
        assert!(!is_disc_directory(std::path::Path::new("Compact Discs")));
        assert_eq!(disc_number_from_directory_name("Discography"), None);
        assert_eq!(disc_number_from_directory_name("CDs"), None);
    }

    #[test]
    fn queued_folder_dispatch_folds_cd_numbered_disc_dirs_into_one_album_batch() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        let cd1 = album_root.join("CD1");
        let cd2 = album_root.join("CD 2");
        std::fs::create_dir_all(&cd1).expect("cd1 dir");
        std::fs::create_dir_all(&cd2).expect("cd2 dir");
        let track_01 = cd1.join("01 - First.flac");
        let track_02 = cd2.join("01 - Second.flac");
        std::fs::write(&track_01, b"not real audio; dispatch test only").expect("track 1 file");
        std::fs::write(&track_02, b"not real audio; dispatch test only").expect("track 2 file");

        let req_01 = processor_dispatch_request_for_path(
            temp.path(),
            "item-disc-1",
            "job-disc-1",
            track_01,
        );
        let req_02 = processor_dispatch_request_for_path(
            temp.path(),
            "item-disc-2",
            "job-disc-2",
            track_02,
        );
        let mut items = vec![
            conversion_item_with_pipeline_request("item-disc-1", req_01),
            conversion_item_with_pipeline_request("item-disc-2", req_02),
        ];

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let prepared_01 = items[0]
            .pipeline_request
            .as_ref()
            .expect("disc 1 item has prepared request");
        let prepared_02 = items[1]
            .pipeline_request
            .as_ref()
            .expect("disc 2 item has prepared request");
        let batch_01 = prepared_01.album_batch.as_ref().expect("disc 1 enters batch");
        let batch_02 = prepared_02.album_batch.as_ref().expect("disc 2 enters same batch");
        assert_eq!(batch_01.conversion_log_batch_id, batch_02.conversion_log_batch_id);
        assert_eq!(batch_01.expected_track_count, 2);
        assert_eq!(batch_02.expected_track_count, 2);
        assert_eq!(batch_01.source_grouping_root, album_root);
        assert_eq!(batch_02.source_grouping_root, album_root);

        let order_01 = prepared_01.album_batch_track.as_ref().expect("disc 1 order");
        let order_02 = prepared_02.album_batch_track.as_ref().expect("disc 2 order");
        assert_eq!(order_01.disc_number, Some(1));
        assert_eq!(order_02.disc_number, Some(2));
        assert_eq!(order_01.track_number, 1);
        assert_eq!(order_02.track_number, 1);
        assert!(order_01.source_ordinal < order_02.source_ordinal);
    }

    #[test]
    fn build_initial_work_preserves_prepared_album_batch_request_at_scheduler_boundary() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let track_01 = album_root.join("01 - First.flac");
        let track_02 = album_root.join("02 - Second.flac");
        std::fs::write(&track_01, b"not real audio; dispatch test only").expect("track 1 file");
        std::fs::write(&track_02, b"not real audio; dispatch test only").expect("track 2 file");

        let req_01 = processor_dispatch_request_for_path(
            temp.path(),
            "item-01",
            "prepared-job-01",
            track_01,
        );
        let req_02 = processor_dispatch_request_for_path(
            temp.path(),
            "item-02",
            "prepared-job-02",
            track_02,
        );
        let mut items = vec![
            conversion_item_with_pipeline_request("item-01", req_01),
            conversion_item_with_pipeline_request("item-02", req_02),
        ];

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let item = items[0].clone();
        let boundary_request = build_pipeline_request(&item)
            .expect("build_initial_work boundary must reuse the prepared request");
        assert!(boundary_request.album_batch.is_some());
        assert!(boundary_request.album_batch_track.is_some());
        assert_eq!(boundary_request.job_id, "prepared-job-01");
        assert_eq!(boundary_request.item_id, "item-01");

        let pool = SharedWorkerPool::<QueueWorkOutput>::new_with_limits(
            Some(1),
            CancellationToken::new(),
            PoolLimits::default(),
        );
        let mut terminal = BTreeMap::new();
        let mut job_to_item = BTreeMap::new();
        let tool_paths = HashMap::new();
        let (progress_tx, _progress_rx) = broadcast::channel(4);
        let work = build_initial_work(
            item,
            &pool,
            &mut terminal,
            &mut job_to_item,
            &tool_paths,
            Arc::new(Mutex::new(HashMap::new())),
            &progress_tx,
            None,
            2,
            Arc::new(ToolConcurrencyLimits::new(1, 1, 1, 1)),
            None,
        )
        .expect("prepared single-file item produces scheduler work");

        assert_eq!(work.kind, WorkKind::SingleFile);
        assert_eq!(work.job_id, "prepared-job-01");
        assert_eq!(job_to_item.get("prepared-job-01").map(String::as_str), Some("item-01"));
        assert!(terminal.is_empty());
    }

    #[test]
    fn queued_folder_dispatch_uses_structural_completion_order_for_duplicate_track_identities() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let track_a = album_root.join("01 - First.flac");
        let track_b = album_root.join("01 - Duplicate.flac");
        std::fs::write(&track_a, b"not real audio; dispatch test only").expect("track a file");
        std::fs::write(&track_b, b"not real audio; dispatch test only").expect("track b file");

        let req_a = processor_dispatch_request_for_path(
            temp.path(),
            "item-a",
            "job-a",
            track_a,
        );
        let req_b = processor_dispatch_request_for_path(
            temp.path(),
            "item-b",
            "job-b",
            track_b,
        );
        let mut items = vec![
            conversion_item_with_pipeline_request("item-a", req_a),
            conversion_item_with_pipeline_request("item-b", req_b),
        ];

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let batch_ids = items
            .iter()
            .map(|item| {
                let request = item
                    .pipeline_request
                    .as_ref()
                    .expect("duplicate item keeps explicit request");
                assert!(request.album_batch_track.is_some());
                assert!(!request.suppress_incremental_conversion_log_append);
                let batch = request
                    .album_batch
                    .as_ref()
                    .expect("duplicate identities still share the structural publish root");
                assert!(batch.uses_completion_order());
                batch.conversion_log_batch_id.clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(batch_ids[0], batch_ids[1]);
    }

    #[test]
    fn queued_folder_dispatch_uses_structural_completion_order_when_track_identity_is_absent() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let track_a = album_root.join("Speak To Me.flac");
        let track_b = album_root.join("Breathe.flac");
        std::fs::write(&track_a, b"not real audio; dispatch test only").expect("track a file");
        std::fs::write(&track_b, b"not real audio; dispatch test only").expect("track b file");

        let req_a = processor_dispatch_request_for_path(
            temp.path(),
            "item-a",
            "job-a",
            track_a,
        );
        let req_b = processor_dispatch_request_for_path(
            temp.path(),
            "item-b",
            "job-b",
            track_b,
        );
        let mut items = vec![
            conversion_item_with_pipeline_request("item-a", req_a),
            conversion_item_with_pipeline_request("item-b", req_b),
        ];

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let batch_ids = items
            .iter()
            .map(|item| {
                let request = item
                    .pipeline_request
                    .as_ref()
                    .expect("unproven item keeps explicit request");
                assert!(request.album_batch_track.is_some());
                assert!(!request.suppress_incremental_conversion_log_append);
                let batch = request
                    .album_batch
                    .as_ref()
                    .expect("unproven order still has structural album membership");
                assert!(batch.uses_completion_order());
                batch.conversion_log_batch_id.clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(batch_ids[0], batch_ids[1]);
    }

    #[test]
    fn queued_folder_dispatch_does_not_treat_embedded_digits_as_track_numbers() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_root = temp.path().join("Composer").join("Symphony");
        std::fs::create_dir_all(&album_root).expect("album dir");
        let track_a = album_root.join("Symphony No. 5.flac");
        let track_b = album_root.join("2024 Remaster.flac");
        std::fs::write(&track_a, b"not real audio; dispatch test only").expect("track a file");
        std::fs::write(&track_b, b"not real audio; dispatch test only").expect("track b file");

        let req_a = processor_dispatch_request_for_path(
            temp.path(),
            "item-a",
            "job-a",
            track_a,
        );
        let req_b = processor_dispatch_request_for_path(
            temp.path(),
            "item-b",
            "job-b",
            track_b,
        );
        let mut items = vec![
            conversion_item_with_pipeline_request("item-a", req_a),
            conversion_item_with_pipeline_request("item-b", req_b),
        ];

        prepare_album_batches_for_queued_independent_single_file_jobs(&mut items);

        let batch_ids = items
            .iter()
            .map(|item| {
                let request = item.pipeline_request.as_ref()?;
                let batch = request.album_batch.as_ref()?;
                (request.album_batch_track.is_some()
                    && !request.suppress_incremental_conversion_log_append
                    && batch.uses_completion_order())
                    .then(|| batch.conversion_log_batch_id.clone())
            })
            .collect::<Option<Vec<_>>>()
            .expect("embedded digits must route through one completion-order batch");
        assert_eq!(batch_ids[0], batch_ids[1]);
    }

    fn prepared_track_for_processor_limit_test(root: &std::path::Path) -> PreparedTrack {
        PreparedTrack {
            id: TrackId {
                source_ordinal: 1,
                disc_number: None,
                track_number: 1,
            },
            source_ref: TrackSourceRef::StagedFile(root.join("input.flac")),
            metadata: TrackMetadata {
                title: Some("Track 1".to_string()),
                track_number: Some(1),
                ..TrackMetadata::default()
            },
            expected_samples: Some(44_100),
            sample_rate: Some(44_100),
            bit_depth: Some(16),
            source_audio: SourceAudioDescriptor::default(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn submission_pump_large_album_fanout_generates_only_one_deferred_unit_when_pool_is_full() {
        let pool = SharedWorkerPool::<QueueWorkOutput>::new_with_limits(
            Some(1),
            CancellationToken::new(),
            PoolLimits { ready_capacity: Some(1) },
        );
        pool.try_submit(synthetic_queue_work_unit("resident", "resident".to_string()))
            .expect("resident unit fills ready queue");

        let mut pump = SubmissionPump::new(Vec::new());
        pump.enqueue_album_fanout("album".to_string());
        let mut generated = 0usize;
        let progress = pump.flush_album_fanout_once(&pool, |job_id| {
            assert_eq!(job_id, "album");
            let unit_id = format!("album-track-{generated}");
            generated += 1;
            Some(synthetic_queue_work_unit(job_id, unit_id))
        });

        assert_eq!(progress, PumpFlushProgress::Blocked);
        assert_eq!(generated, 1, "large fanout must stop after producing one deferred WorkUnit");
        assert_eq!(pump.pending_work_units(), 1);
        assert_eq!(pump.album_fanout.len(), 1, "album cursor remains pending for later capacity");
        assert_eq!(pool.metrics().snapshot().pool_ready_queue_depth, 1);
    }

    #[tokio::test]
    async fn processor_orchestrator_large_fanout_drains_saturated_results_without_unbounded_backlog() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<QueueWorkOutput>::new_with_limits(
            Some(1),
            cancel.clone(),
            PoolLimits { ready_capacity: Some(1) },
        );
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        pool.metrics().set_test_event_sender(events_tx);
        let mut run = pool.start();

        for index in 0..5usize {
            pool.submit(synthetic_queue_work_unit_for_processor_test(
                "preload",
                format!("preload-{index}"),
                index,
            ))
            .await;
        }
        // With ready_capacity: 1, submits are serialized through the semaphore.
        // The worker interleaves Started/Finished events for each unit. Wait for
        // 5 Started events (ignoring Finished events between them). The 5th Started
        // is the blocked send (channel full at capacity 4, 5th send pending).
        {
            let mut started_count = 0usize;
            while started_count < 5 {
                let event = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    events_rx.recv(),
                )
                .await
                .expect("scheduler event arrives before timeout")
                .expect("event sender remains live");
                if event == crate::convert::pipeline::SchedulerTestEvent::WorkerResultSendStarted {
                    started_count += 1;
                }
            }
        }

        let (resident_started_tx, resident_started_rx) = tokio::sync::oneshot::channel();
        let (release_resident_tx, release_resident_rx) = tokio::sync::oneshot::channel();
        pool.try_submit(blocking_synthetic_processor_unit(
            "resident",
            "resident",
            10_000,
            resident_started_tx,
            release_resident_rx,
        ))
        .expect("blocked worker leaves exactly one ready slot to fill");
        assert_eq!(pool.metrics().snapshot().pool_ready_queue_depth, 1);

        let mut submissions = SubmissionPump::new(Vec::new());
        let mut pending_albums = BTreeMap::new();
        let mut terminal = BTreeMap::new();
        let mut job_to_item = BTreeMap::new();
        let tool_paths = HashMap::new();
        let (progress_tx, _progress_rx) = broadcast::channel(16);

        submissions.enqueue_synthetic_album_fanout(
            "large-album".to_string(),
            "large-item".to_string(),
            64,
            pool.metrics(),
        );
        submissions.record_backlog(pool.metrics(), &pending_albums);
        assert_eq!(pool.metrics().snapshot().submission_backlog_depth, 64);

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            submissions.flush(
                &pool,
                &mut pending_albums,
                &mut terminal,
                &mut job_to_item,
                &tool_paths,
                Arc::new(Mutex::new(HashMap::new())),
                &progress_tx,
                None,
                &cancel,
                1,
                Arc::new(ToolConcurrencyLimits::new(1, 1, 1, 1)),
                None,
            );
        })
        .await
        .expect("processor submission flush returns while result channel and ready queue are full");
        submissions.record_backlog(pool.metrics(), &pending_albums);
        let blocked = pool.metrics().snapshot();
        assert_eq!(submissions.pending_work_units(), 1);
        assert_eq!(blocked.pool_ready_queue_depth, 1);
        assert_eq!(blocked.submission_backlog_depth, 64);
        assert_eq!(blocked.ready_queue_depth, 65);

        let first_drained = tokio::time::timeout(std::time::Duration::from_secs(5), run.results.recv())
            .await
            .expect("processor can drain one result even while a large fanout is pending")
            .expect("preloaded result is present");
        assert!(matches!(
            first_drained.outcome,
            Ok(QueueWorkOutput::SyntheticEncoded { job_id, .. }) if job_id == "preload"
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), resident_started_rx)
            .await
            .expect("resident work starts after draining a result")
            .expect("resident start signal is delivered");

        submissions.flush(
            &pool,
            &mut pending_albums,
            &mut terminal,
            &mut job_to_item,
            &tool_paths,
            Arc::new(Mutex::new(HashMap::new())),
            &progress_tx,
            None,
            &cancel,
            1,
            Arc::new(ToolConcurrencyLimits::new(1, 1, 1, 1)),
            None,
        );
        submissions.record_backlog(pool.metrics(), &pending_albums);
        let resumed = pool.metrics().snapshot();
        assert_eq!(submissions.pending_work_units(), 1);
        assert_eq!(resumed.pool_ready_queue_depth, 1);
        assert_eq!(resumed.submission_backlog_depth, 63);
        assert_eq!(resumed.ready_queue_depth, 64);
        assert_eq!(resumed.peak_submission_backlog_depth, 64);

        for _ in 0..4 {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), run.results.recv())
                .await
                .expect("preloaded results drain after progress resumes")
                .expect("preloaded result remains available");
        }
        release_resident_tx.send(()).expect("release resident worker");
        let resident = tokio::time::timeout(std::time::Duration::from_secs(5), run.results.recv())
            .await
            .expect("resident result drains")
            .expect("resident result exists");
        assert!(matches!(
            resident.outcome,
            Ok(QueueWorkOutput::SyntheticEncoded { job_id, .. }) if job_id == "resident"
        ));
        let fanout = tokio::time::timeout(std::time::Duration::from_secs(5), run.results.recv())
            .await
            .expect("queued fanout unit drains")
            .expect("fanout result exists");
        assert!(matches!(
            fanout.outcome,
            Ok(QueueWorkOutput::SyntheticEncoded { job_id, .. }) if job_id == "large-album"
        ));

        run.shutdown().await;
    }

    #[test]
    fn single_file_and_realized_work_units_capture_the_same_tool_limit_arc() {
        let temp = tempfile::tempdir().expect("temp dir");
        let request = pipeline_request_for_processor_limit_test(temp.path());
        let track = prepared_track_for_processor_limit_test(temp.path());
        let (progress_tx, _progress_rx) = broadcast::channel(4);
        let tool_paths = HashMap::new();
        let limits = Arc::new(ToolConcurrencyLimits::new(2, 8, 6, 8));
        let initial_count = Arc::strong_count(&limits);

        let single_file_unit = build_single_file_work(
            request.clone(),
            &tool_paths,
            Arc::new(Mutex::new(HashMap::new())),
            &progress_tx,
            None,
            limits.clone(),
        );
        assert_eq!(
            Arc::strong_count(&limits),
            initial_count + 1,
            "single-file fallback work captures a clone of the shared tool limit Arc"
        );

        let realized = ScheduledRealizedTrack {
            index: 0,
            track,
            final_path: temp.path().join("out/track.flac"),
            realized_path: temp.path().join("realized.wav"),
            realized_dsd_dst_stats: None,
            req: request,
            staging_root: temp.path().join("staging"),
            staging_job: "processor-limit-job".to_string(),
            convert_root: temp.path().join("converted"),
            cancel: CancellationToken::new(),
        };
        let realized_unit = build_realized_encode_work(
            "processor-limit-job".to_string(),
            realized,
            &tool_paths,
            Arc::new(Mutex::new(HashMap::new())),
            &progress_tx,
            None,
            CancellationToken::new(),
            limits.clone(),
        );
        assert_eq!(
            Arc::strong_count(&limits),
            initial_count + 2,
            "realized-track encode work captures another clone of the same shared tool limit Arc"
        );

        drop(single_file_unit);
        drop(realized_unit);
        assert_eq!(
            Arc::strong_count(&limits),
            initial_count,
            "dropping the work units releases only their captured clones, not a separate limit object"
        );
    }

    #[test]
    fn configured_worker_count_is_escalated_to_max_tool_concurrency() {
        let limits = ToolConcurrencyLimits::new(2, 8, 6, 8);

        assert_eq!(
            effective_worker_count_for_tool_limits(1, &limits),
            8,
            "configured worker_count=1 is raised so the largest tool family cannot be starved"
        );
        assert_eq!(effective_worker_count_for_tool_limits(0, &limits), 8);
        assert_eq!(effective_worker_count_for_tool_limits(12, &limits), 12);
    }

    #[test]
    fn conversion_processor_exposes_stable_scheduler_metrics_handle() {
        let processor = ConversionProcessor::new(ProcessorConfig {
            worker_count: 1,
            tool_paths: HashMap::new(),
            default_destination_directory: None,
            scratch_directory: None,
            scratch_memory_limit_percent: DEFAULT_SCRATCH_MEMORY_LIMIT_PERCENT,
        });

        let metrics = processor.scheduler_metrics();
        metrics.record_jobs_queued(2);
        metrics.record_submission_backlog_depth(1);

        let snapshot = processor.scheduler_metrics_snapshot();
        assert_eq!(snapshot.jobs_queued, 2);
        assert_eq!(snapshot.submission_backlog_depth, 1);
        assert_eq!(snapshot.ready_queue_depth, 1);
    }
}
