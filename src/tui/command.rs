//! Vi-style command mode: parsing and execution

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tokio::task;

use super::app::*;
use super::message::AppMessage;
use crate::convert::formats::AudioFormat;
use crate::convert::queue_expansion::{
    queue_path_key, split_cue_album_grouping_key_for_queue, QueueExpansionResult,
    QueueSplitCueAlbumGroupingDecisions,
};
use crate::convert::simple_wizard::DitherType;

const CD_FRAMES_PER_SECOND: f64 = 75.0;
const CD_TOC_PREGAP_FRAMES: u32 = 150;

/// Audio-file threshold above which expensive bulk actions require confirmation.
pub const BULK_AUDIO_GUARD_THRESHOLD: usize = 50;
/// Count exactly up to this many files for the confirmation copy; above this
/// bound the prompt intentionally uses a capped wording to keep counting fast.
pub const BULK_AUDIO_GUARD_EXACT_LIMIT: usize = 500;
/// Hard cap for Browse folder expansion performed during Convert handoff. The
/// cap applies inside the blocking worker so pathological trees cannot consume
/// unbounded resources after the raw-mode reducer has handed the job off.
pub const BROWSE_CONVERT_FOLDER_EXPANSION_MAX_VISITED: usize = 50_000;

fn consume_bulk_guard_bypass(app: &mut AppState, operation: BulkOperationKind) -> bool {
    if app.bulk_guard_bypass == Some(operation) {
        app.bulk_guard_bypass = None;
        true
    } else {
        false
    }
}

fn bulk_guard_message(operation: BulkOperationKind, count: usize) -> String {
    let count_text = if count > BULK_AUDIO_GUARD_EXACT_LIMIT {
        format!("more than {}", BULK_AUDIO_GUARD_EXACT_LIMIT)
    } else {
        count.to_string()
    };
    format!(
        "This will {} {} audio files.\nContinue? [Enter = yes, Esc = cancel]",
        operation.label(),
        count_text,
    )
}

/// Open a confirmation overlay when an operation would touch more than the
/// configured bulk-audio threshold. Returns true when the caller must stop
/// dispatch because the confirmation overlay has taken over.
pub fn maybe_confirm_bulk_operation(
    app: &mut AppState,
    operation: BulkOperationKind,
    command: BulkGuardCommand,
    paths: &[PathBuf],
) -> bool {
    if consume_bulk_guard_bypass(app, operation) {
        return false;
    }
    let count = crate::convert::queue_expansion::count_audio_files_bounded(paths, BULK_AUDIO_GUARD_EXACT_LIMIT);
    if count <= BULK_AUDIO_GUARD_THRESHOLD {
        return false;
    }
    app.active_overlay = ActiveOverlay::Confirmation {
        message: bulk_guard_message(operation, count),
        action: ConfirmAction::BulkOperation {
            operation,
            command,
            paths: paths.to_vec(),
            count,
        },
    };
    true
}

fn current_bulk_guard_paths(app: &AppState) -> Vec<PathBuf> {
    if let Some(paths) = app.bulk_guard_frozen_paths.as_ref() {
        return paths.clone();
    }
    match app.current_screen {
        AppScreen::Browse => collect_selection_for_file_ops(app),
        AppScreen::Convert => app.convert.source.mode.all_paths(),
        _ => Vec::new(),
    }
}
pub(crate) fn expand_audio_paths_for_metadata(paths: &[PathBuf]) -> Vec<PathBuf> {
    expand_audio_paths(paths)
}

fn expand_audio_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    // Metadata/analysis semantics: every audio file, including single-image
    // CUE carriers that queue expansion would suppress in favor of the CUE.
    crate::convert::queue_expansion::expand_paths_to_all_audio(paths)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseConvertPostLoad {
    /// Leave the expanded source on the Convert screen for manual review.
    ReviewOnly,
    /// Preserve context-menu Convert → Last used / preset behavior: after the
    /// async expansion publishes the Convert source, commit it exactly as the
    /// old synchronous Queue path did.
    Commit { start: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowseConvertExpansionTarget {
    /// Install the expanded paths as the Convert source for direct Browse load.
    ConvertSource,
    /// Add the expanded paths directly to the processing queue.
    ConvertQueueItems,
    /// Install the expanded paths as a Convert review source for `:queue` and
    /// context-menu Convert actions. The preset is loaded only after expansion
    /// succeeds, so failed/stale jobs do not mutate Convert pills. `post_load`
    /// carries the continuation needed by context-menu actions that previously
    /// committed immediately after synchronous Queue publication.
    ConvertReview {
        preset: Option<String>,
        post_load: BrowseConvertPostLoad,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseConvertExpansionRequest {
    pub target: BrowseConvertExpansionTarget,
    pub selection_snapshot: Vec<PathBuf>,
    pub browse_in_archive: bool,
    /// Number of stale Browse multi-select marks pruned when this request was
    /// created. Folder expansion publishes status asynchronously, so the count
    /// travels with the request to preserve stale-mark observability after the
    /// final queue/convert status is written.
    pub dropped_stale_selection_count: usize,
}

#[derive(Debug, Clone)]
pub struct BrowseConvertExpansion {
    pub queue: QueueExpansionResult,
    pub expanded_folder_count: usize,
    pub empty_audio_folders: Vec<PathBuf>,
    pub expansion_errors: Vec<String>,
    pub visited: usize,
    pub cancelled: bool,
}


fn cleanup_discarded_browse_convert_expansion(expansion: &BrowseConvertExpansion) {
    crate::convert::queue_expansion::cleanup_synthetic_cue_artifacts(
        &expansion.queue.synthetic_cue_artifacts,
    );
}

impl BrowseConvertExpansion {
    fn cancelled(visited: usize) -> Self {
        Self {
            queue: QueueExpansionResult::default(),
            expanded_folder_count: 0,
            empty_audio_folders: Vec::new(),
            expansion_errors: Vec::new(),
            visited,
            cancelled: true,
        }
    }

    fn failed(message: String) -> Self {
        Self {
            queue: QueueExpansionResult::default(),
            expanded_folder_count: 0,
            empty_audio_folders: Vec::new(),
            expansion_errors: vec![message],
            visited: 0,
            cancelled: false,
        }
    }
}

fn normalized_path_snapshot(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    crate::convert::queue_expansion::sort_dedup_paths_by_queue_identity(&mut paths);
    paths
}

/// Return true when a real filesystem directory should be expanded into audio
/// files before conversion instead of installed as an opaque source. This is
/// the shared Browse conversion/queue routing predicate for context-menu
/// Convert, command-mode `:queue`, and direct Browse source loading. It is
/// intentionally cheap: no recursive filesystem traversal may run here.
#[must_use]
pub(crate) fn is_regular_filesystem_audio_folder_convert_candidate(
    app: &AppState,
    path: &Path,
) -> bool {
    is_regular_filesystem_audio_folder_convert_candidate_raw(app.browse.is_in_archive(), path)
}

#[must_use]
fn is_regular_filesystem_audio_folder_convert_candidate_raw(
    browse_in_archive: bool,
    path: &Path,
) -> bool {
    !browse_in_archive
        && path.is_dir()
        && !crate::disc::bluray_utils::is_bluray_backend_open_candidate(path)
        && !crate::disc::dvda_utils::is_dvda_source(path)
        && !crate::disc::dvdv_utils::is_dvdv_source(path)
}

/// Blocking implementation for background workers and narrow tests only. Do
/// not call this from key handlers, reducers, context-menu dispatch, or command
/// execution: use `start_browse_convert_folder_expansion` for Browse flows.
#[must_use]
#[allow(dead_code)]
pub(crate) fn regular_filesystem_audio_folder_paths_for_convert_blocking(
    browse_in_archive: bool,
    path: &Path,
) -> Result<Option<(Vec<PathBuf>, usize)>, String> {
    regular_filesystem_audio_folder_paths_for_convert_blocking_with_cancel(
        browse_in_archive,
        path,
        None,
    )
}

fn regular_filesystem_audio_folder_paths_for_convert_blocking_with_cancel(
    browse_in_archive: bool,
    path: &Path,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<Option<(Vec<PathBuf>, usize)>, String> {
    if !is_regular_filesystem_audio_folder_convert_candidate_raw(browse_in_archive, path) {
        return Ok(None);
    }

    let inputs = vec![path.to_path_buf()];
    let (queue, visited) = crate::convert::queue_expansion::expand_paths_to_audio_with_metadata_limited(
        &inputs,
        BROWSE_CONVERT_FOLDER_EXPANSION_MAX_VISITED,
        || cancel.is_some_and(|token| token.is_cancelled()),
    )
    .map_err(|err| err.message)?;

    if let Some(err) = queue.first_error() {
        if queue.paths.is_empty() {
            crate::convert::queue_expansion::cleanup_synthetic_cue_artifacts(&queue.synthetic_cue_artifacts);
            return Err(err.to_string());
        }
        log::warn!("folder conversion expansion warning: {err}");
    }
    if !queue.synthetic_cue_artifacts.is_empty() {
        crate::convert::queue_expansion::cleanup_synthetic_cue_artifacts(&queue.synthetic_cue_artifacts);
        return Err(
            "Browse folder conversion produced transient synthetic CUE artifacts; use the ownership-bearing queue expansion API"
                .to_string(),
        );
    }

    Ok(Some((queue.paths, visited)))
}

/// Backward-compatible test seam. Production Browse flows must not use this;
/// the source-text tests below assert that queue/load paths use the async
/// worker entry point instead.
#[must_use]
#[allow(dead_code)]
pub(crate) fn regular_filesystem_audio_folder_paths_for_convert(
    app: &AppState,
    path: &Path,
) -> Result<Option<Vec<PathBuf>>, String> {
    regular_filesystem_audio_folder_paths_for_convert_blocking(app.browse.is_in_archive(), path)
        .map(|value| value.map(|(paths, _visited)| paths))
}

#[must_use]
pub(crate) fn browse_selection_contains_regular_audio_folder_for_convert(
    app: &AppState,
    paths: &[PathBuf],
) -> bool {
    paths
        .iter()
        .any(|path| is_regular_filesystem_audio_folder_convert_candidate(app, path))
}

#[must_use]
pub(crate) fn resolved_split_cue_album_grouping_decisions_for_conversion(
    app: &AppState,
    _paths: &[PathBuf],
) -> QueueSplitCueAlbumGroupingDecisions {
    let mut decisions = QueueSplitCueAlbumGroupingDecisions::new();

    // Reducer-safe snapshot only: carry every already-resolved session
    // decision. Do not discover folders, parse CUEs, probe media, or construct
    // TOC evidence here. WavPack/FFmpeg probing can block; conversion-specific
    // evidence construction is performed inside the blocking expansion worker.
    for decision in app.split_cue_album_grouping_cache.values() {
        insert_queue_grouping_decision(&mut decisions, decision.clone());
    }

    decisions
}

fn augment_split_cue_album_grouping_decisions_for_conversion_blocking(
    decisions: &mut QueueSplitCueAlbumGroupingDecisions,
    paths: &[PathBuf],
    cancel: &tokio_util::sync::CancellationToken,
) {
    if cancel.is_cancelled() {
        return;
    }
    let infos = collect_single_image_cue_infos_for_sources(paths, &[]);
    if cancel.is_cancelled() {
        return;
    }
    let db = crate::db::Database::open().ok();
    insert_cached_split_cue_grouping_decisions_for_infos_with_cache(
        &infos,
        decisions,
        |toc| db.as_ref().and_then(|db| db.get_cached_mb_response(toc)),
    );
}

fn insert_queue_grouping_decision(
    decisions: &mut QueueSplitCueAlbumGroupingDecisions,
    decision: SplitCueAlbumGroupingDecision,
) {
    let cue_paths: Vec<PathBuf> = decision
        .groups
        .iter()
        .flat_map(|group| group.iter().cloned())
        .collect();
    if cue_paths.len() < 2 {
        return;
    }
    decisions.insert(split_cue_album_grouping_key_for_queue(&cue_paths), decision);
}

#[cfg(test)]
fn insert_cached_split_cue_grouping_decisions_for_infos(
    app: &AppState,
    infos: &[super::cue_parser::SingleImageInfo],
    decisions: &mut QueueSplitCueAlbumGroupingDecisions,
) {
    insert_cached_split_cue_grouping_decisions_for_infos_with_cache(
        infos,
        decisions,
        |toc| app.db.get_cached_mb_response(toc),
    );
}

fn insert_cached_split_cue_grouping_decisions_for_infos_with_cache<F>(
    infos: &[super::cue_parser::SingleImageInfo],
    decisions: &mut QueueSplitCueAlbumGroupingDecisions,
    mut cached_mb_response: F,
) where
    F: FnMut(&str) -> Option<String>,
{
    let mut by_parent: std::collections::BTreeMap<PathBuf, Vec<super::cue_parser::SingleImageInfo>> =
        std::collections::BTreeMap::new();
    for info in infos {
        let Some(parent) = info.cue_path.parent() else {
            continue;
        };
        by_parent
            .entry(cue_info_path_key(parent))
            .or_default()
            .push(info.clone());
    }

    for mut group in by_parent.into_values() {
        if group.len() < 2 {
            continue;
        }
        group.sort_by(|left, right| left.cue_path.cmp(&right.cue_path));
        let cue_paths: Vec<PathBuf> = group.iter().map(|info| info.cue_path.clone()).collect();
        let queue_key = split_cue_album_grouping_key_for_queue(&cue_paths);
        if decisions.contains_key(&queue_key) {
            continue;
        }
        let decision = split_cue_title_rung_decision(&group).or_else(|| {
            cached_split_cue_album_grouping_ladder_decision_with_cache(&group, |toc| {
                cached_mb_response(toc)
            })
        });
        if let Some(decision) = decision {
            decisions.insert(queue_key, decision);
        }
    }
}

/// Blocking expansion implementation for the Browse Convert worker. It expands
/// only regular filesystem audio folders; real disc/archive/CUE/single-file
/// sources remain opaque. The result is deterministic and explicit about empty
/// folders versus scan failures.
#[cfg(test)]
pub(crate) fn expand_regular_filesystem_audio_folders_for_convert_blocking(
    browse_in_archive: bool,
    paths: Vec<PathBuf>,
    cancel: tokio_util::sync::CancellationToken,
) -> BrowseConvertExpansion {
    expand_regular_filesystem_audio_folders_for_convert_blocking_with_grouping_decisions(
        browse_in_archive,
        paths,
        cancel,
        QueueSplitCueAlbumGroupingDecisions::new(),
    )
}

pub(crate) fn expand_regular_filesystem_audio_folders_for_convert_blocking_with_grouping_decisions(
    browse_in_archive: bool,
    paths: Vec<PathBuf>,
    cancel: tokio_util::sync::CancellationToken,
    mut grouping_decisions: QueueSplitCueAlbumGroupingDecisions,
) -> BrowseConvertExpansion {
    let selection = normalized_path_snapshot(paths);
    let mut regular_folders = Vec::new();
    let mut preserved_roots = Vec::new();

    for path in &selection {
        if cancel.is_cancelled() {
            return BrowseConvertExpansion::cancelled(0);
        }
        if is_regular_filesystem_audio_folder_convert_candidate_raw(browse_in_archive, path) {
            regular_folders.push(path.clone());
        } else if path.is_dir() {
            // Real disc/source directories must stay opaque, but they still have
            // to participate in the same global queue plan so path ordering and
            // deduplication remain deterministic.
            preserved_roots.push(path.clone());
        }
    }

    if regular_folders.is_empty() {
        return BrowseConvertExpansion {
            queue: QueueExpansionResult {
                paths: selection,
                cue_artifact_audio: std::collections::HashSet::new(),
                synthetic_cue_artifacts: std::collections::HashSet::new(),
                expansion_errors: Vec::new(),
            },
            expanded_folder_count: 0,
            empty_audio_folders: Vec::new(),
            expansion_errors: Vec::new(),
            visited: 0,
            cancelled: false,
        };
    }

    augment_split_cue_album_grouping_decisions_for_conversion_blocking(
        &mut grouping_decisions,
        &regular_folders,
        &cancel,
    );
    if cancel.is_cancelled() {
        return BrowseConvertExpansion::cancelled(0);
    }

    match crate::convert::queue_expansion::expand_paths_to_audio_with_preserved_disc_roots_limited_using_grouping_decisions(
        &selection,
        &preserved_roots,
        BROWSE_CONVERT_FOLDER_EXPANSION_MAX_VISITED,
        || cancel.is_cancelled(),
        &grouping_decisions,
    ) {
        Ok((queue, visited)) => {
            let empty_audio_folders = regular_folders
                .iter()
                .filter(|folder| {
                    let folder_key = queue_path_key(folder);
                    !queue
                        .paths
                        .iter()
                        .any(|path| queue_path_key(path).starts_with(&folder_key))
                })
                .cloned()
                .collect::<Vec<_>>();
            let expanded_folder_count = regular_folders
                .len()
                .saturating_sub(empty_audio_folders.len());

            BrowseConvertExpansion {
                queue,
                expanded_folder_count,
                empty_audio_folders,
                expansion_errors: Vec::new(),
                visited,
                cancelled: false,
            }
        }
        Err(err) if err.cancelled || cancel.is_cancelled() => {
            BrowseConvertExpansion::cancelled(err.visited)
        }
        Err(err) => BrowseConvertExpansion {
            queue: QueueExpansionResult::default(),
            expanded_folder_count: 0,
            empty_audio_folders: Vec::new(),
            expansion_errors: vec![err.message],
            visited: err.visited,
            cancelled: false,
        },
    }
}

/// Start a Browse regular-folder expansion on the blocking worker pool. The
/// caller has already determined that at least one selected path is a regular
/// filesystem folder candidate. `probe_generation` is the active generation id:
/// starting a newer Convert/probe/expansion request supersedes this job, and
/// late completions are ignored by the event-loop handler.
pub(crate) fn start_browse_convert_folder_expansion(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    target: BrowseConvertExpansionTarget,
    paths: Vec<PathBuf>,
    dropped_stale_selection_count: usize,
) {
    let selection_snapshot = normalized_path_snapshot(paths);
    if selection_snapshot.is_empty() {
        app.set_status(status_with_stale_selection_notice(
            dropped_stale_selection_count,
            "queue: no selection",
        ));
        return;
    }

    let request = BrowseConvertExpansionRequest {
        target,
        selection_snapshot,
        browse_in_archive: app.browse.is_in_archive(),
        dropped_stale_selection_count,
    };
    let (generation, cancel) = app.begin_browse_convert_expansion(request.clone());

    let folder_count = request
        .selection_snapshot
        .iter()
        .filter(|path| is_regular_filesystem_audio_folder_convert_candidate_raw(
            request.browse_in_archive,
            path,
        ))
        .count();
    let status = if folder_count > 1 {
        "Expanding selected folders…".to_string()
    } else {
        "Expanding folder…".to_string()
    };
    app.set_status(status_with_stale_selection_notice(
        dropped_stale_selection_count,
        status,
    ));

    let grouping_decisions = resolved_split_cue_album_grouping_decisions_for_conversion(
        app,
        &request.selection_snapshot,
    );
    let tx_for_worker = tx.clone();
    let request_for_worker = request.clone();
    tokio::spawn(async move {
        let paths_for_worker = request_for_worker.selection_snapshot.clone();
        let browse_in_archive = request_for_worker.browse_in_archive;
        let cancel_for_worker = cancel.clone();
        let worker_result = task::spawn_blocking(move || {
            expand_regular_filesystem_audio_folders_for_convert_blocking_with_grouping_decisions(
                browse_in_archive,
                paths_for_worker,
                cancel_for_worker,
                grouping_decisions,
            )
        })
        .await
        .unwrap_or_else(|err| BrowseConvertExpansion::failed(format!("folder expansion worker failed: {err}")));

        let _ = tx_for_worker
            .send(AppMessage::BrowseConvertExpansionComplete {
                generation,
                request: request_for_worker,
                expansion: worker_result,
            })
            .await;
    });
}

fn browse_convert_expansion_selection_still_current(
    app: &AppState,
    request: &BrowseConvertExpansionRequest,
) -> bool {
    if app.current_screen != AppScreen::Browse {
        return false;
    }

    let current_paths = match &request.target {
        BrowseConvertExpansionTarget::ConvertSource => app
            .browse
            .selected_entry()
            .map(|entry| vec![entry.path.clone()])
            .unwrap_or_default(),
        BrowseConvertExpansionTarget::ConvertQueueItems
        | BrowseConvertExpansionTarget::ConvertReview { .. } => {
            // Compare against the raw Browse selection: expansion requests are
            // created from raw paths (before directory expansion), so the
            // freshness snapshot must be collected the same way.
            app.browse.action_selection_in_current_directory()
        }
    };

    normalized_path_snapshot(current_paths) == request.selection_snapshot
}

pub(crate) fn handle_browse_convert_expansion_complete(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    generation: u64,
    request: BrowseConvertExpansionRequest,
    expansion: BrowseConvertExpansion,
) {
    if !app.browse_convert_expansion_pending_for(generation, &request) {
        cleanup_discarded_browse_convert_expansion(&expansion);
        log::debug!("discarded stale Browse Convert expansion generation {generation}");
        return;
    }
    if generation != app.probe_generation {
        cleanup_discarded_browse_convert_expansion(&expansion);
        log::debug!("discarded superseded Browse Convert expansion generation {generation}");
        return;
    }
    if !browse_convert_expansion_selection_still_current(app, &request) {
        cleanup_discarded_browse_convert_expansion(&expansion);
        let _ = app.complete_browse_convert_expansion(generation, &request);
        log::debug!("discarded Browse Convert expansion after selection/screen changed");
        return;
    }
    let _ = app.complete_browse_convert_expansion(generation, &request);

    if expansion.cancelled {
        app.set_status(status_with_stale_selection_notice(
            request.dropped_stale_selection_count,
            "folder expansion cancelled",
        ));
        return;
    }
    if let Some(err) = expansion.expansion_errors.first() {
        if expansion.queue.paths.is_empty() {
            app.set_status(status_with_stale_selection_notice(
                request.dropped_stale_selection_count,
                err.clone(),
            ));
            return;
        }
        // Currently unreachable (constructors only populate this field with
        // empty queue paths), but if it ever fires the status below is
        // overwritten in the same reducer pass — keep a durable record.
        log::warn!("browse expansion warning: {}", err);
        app.set_status(status_with_stale_selection_notice(
            request.dropped_stale_selection_count,
            format!("{}; continuing with queueable sources", err),
        ));
    }
    if expansion.queue.paths.is_empty() {
        let status = if let Some(folder) = expansion.empty_audio_folders.first() {
            format!("No supported audio files found in {}", folder.display())
        } else {
            "No supported sources selected".to_string()
        };
        app.set_status(status_with_stale_selection_notice(
            request.dropped_stale_selection_count,
            status,
        ));
        return;
    }

    match request.target {
        BrowseConvertExpansionTarget::ConvertSource => {
            install_browse_convert_source_paths(
                app,
                tx,
                expansion.queue,
                expansion.expanded_folder_count,
                true,
            );
            prefix_current_status_with_stale_selection_notice(
                app,
                request.dropped_stale_selection_count,
            );
        }
        BrowseConvertExpansionTarget::ConvertQueueItems => {
            queue_browse_convert_paths_for_processing(app, expansion.queue);
            app.browse.clear_multi_selection();
            app.current_screen = AppScreen::Queue;
            app.browse.return_target = super::browse::BrowseReturnTarget::None;
            prefix_current_status_with_stale_selection_notice(
                app,
                request.dropped_stale_selection_count,
            );
        }
        BrowseConvertExpansionTarget::ConvertReview { preset, post_load } => {
            if finish_browse_queue_review_after_expansion(
                app,
                tx,
                preset,
                expansion.queue,
                expansion.expanded_folder_count,
            ) {
                apply_browse_convert_post_load_action(app, tx, post_load);
            }
            prefix_current_status_with_stale_selection_notice(
                app,
                request.dropped_stale_selection_count,
            );
        }
    }
}

fn current_audio_paths(app: &AppState, include_convert: bool) -> Vec<PathBuf> {
    if let Some(paths) = app.bulk_guard_frozen_paths.as_ref() {
        return expand_audio_paths(paths);
    }
    match app.current_screen {
        AppScreen::Browse => {
            let sel = collect_selection_for_file_ops(app);
            expand_audio_paths(&sel)
        }
        AppScreen::Convert if include_convert => app.convert.source.mode.all_paths(),
        _ => Vec::new(),
    }
}



fn cue_info_path_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn collect_cue_paths_from_source(
    path: &Path,
    out: &mut Vec<PathBuf>,
    seen: &mut std::collections::BTreeSet<PathBuf>,
) {
    if crate::convert::classify::is_cue_sheet_path(path) {
        if !super::cue_parser::is_user_visible_cue_path(path) {
            return;
        }
        let key = cue_info_path_key(path);
        if seen.insert(key) {
            out.push(path.to_path_buf());
        }
        return;
    }

    if path.is_dir() {
        for cue in super::gnudb::find_cues_in_dir(path) {
            if !super::cue_parser::is_user_visible_cue_path(&cue) {
                continue;
            }
            let key = cue_info_path_key(&cue);
            if seen.insert(key) {
                out.push(cue);
            }
        }
    }
}

/// Find the single-image CUE surfaces associated with a Browse selection or
/// its expanded audio paths. This deliberately pairs through the CUE FILE
/// reference, then filters against the expanded audio set when present, so a
/// split-side folder such as `side_a.cue`/`side_a.wv` +
/// `side_b.cue`/`side_b.wv` stays side-aware instead of becoming two ordinary
/// audio image files.

#[derive(Debug, Clone)]
struct CueMetadataSurface {
    audio_path: PathBuf,
    sheet: super::cue_parser::CueSheet,
}

fn resolve_cue_metadata_surface(cue_path: &Path) -> Option<CueMetadataSurface> {
    let sheet = super::cue_parser::parse_cue_file(cue_path).ok()?;
    if sheet.tracks.len() < 2 {
        return None;
    }
    let first_file = sheet.tracks.first().and_then(|track| track.file.as_deref())?;
    if sheet
        .tracks
        .iter()
        .any(|track| track.file.as_deref() != Some(first_file))
    {
        return None;
    }
    let dir = cue_path.parent().unwrap_or_else(|| Path::new("."));
    let audio_path = super::accuraterip::resolve_cue_file_reference(dir, first_file)?;
    if !matches!(
        crate::convert::classify::classify_file(&audio_path),
        crate::convert::classify::EntryKind::AudioFile(_)
    ) {
        return None;
    }
    Some(CueMetadataSurface { audio_path, sheet })
}

fn collect_cue_metadata_surfaces_for_sources(
    sources: &[PathBuf],
    audio_paths: &[PathBuf],
) -> Vec<CueMetadataSurface> {
    let audio_keys: std::collections::BTreeSet<PathBuf> = audio_paths
        .iter()
        .map(|path| cue_info_path_key(path))
        .collect();

    let mut cue_paths = Vec::new();
    let mut seen_cues = std::collections::BTreeSet::new();
    for source in sources {
        collect_cue_paths_from_source(source, &mut cue_paths, &mut seen_cues);
    }
    for audio in audio_paths {
        if let Some(parent) = audio.parent() {
            collect_cue_paths_from_source(parent, &mut cue_paths, &mut seen_cues);
        }
    }

    let mut surfaces = Vec::new();
    for cue_path in cue_paths {
        let Some(surface) = resolve_cue_metadata_surface(&cue_path) else {
            continue;
        };
        if !audio_keys.is_empty() && !audio_keys.contains(&cue_info_path_key(&surface.audio_path)) {
            continue;
        }
        surfaces.push(surface);
    }
    surfaces
}

fn paths_for_cue_metadata_surfaces(surfaces: &[CueMetadataSurface]) -> Vec<PathBuf> {
    surfaces
        .iter()
        .flat_map(|surface| {
            (0..surface.sheet.tracks.len()).map(move |_| surface.audio_path.clone())
        })
        .collect()
}

/// Album title for an MB text search over a MULTI-part cue album. Side-split
/// rips title each cue per side ("... (Side A)" / "... (Side B)"); searching with
/// one side's verbatim title biases the query. Use the longest common prefix
/// of every part's title, trimmed of a dangling side/part designator opener.
/// Falls back to the first title when the parts share no meaningful prefix.
pub(crate) fn common_cue_album_title(titles: &[String]) -> Option<String> {
    crate::convert::split_cue_album::common_cue_album_title(titles)
}

/// Return the decisive TITLE-rung shared prefix used by the split-CUE album
/// grouping ladder. This is deliberately stricter than `common_cue_album_title`:
/// callers use `Some(_)` as merge evidence. `None` is never split evidence;
/// it means the ladder must continue to concat-TOC, per-CUE TOC, and finally
/// conservative merge.
pub(crate) fn meaningful_common_cue_album_prefix(titles: &[String]) -> Option<String> {
    crate::convert::split_cue_album::meaningful_common_cue_album_prefix(titles)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SplitCueAlbumGroupingKey(Vec<PathBuf>);

pub use crate::convert::split_cue_album::{
    SplitCueAlbumGroupingDecision, SplitCueAlbumGroupingReason,
};

#[derive(Debug)]
pub struct SplitCueAlbumGroupingRequest {
    pub operation_id: super::message::TagsMbOperationId,
    pub key: SplitCueAlbumGroupingKey,
    pub infos: Vec<super::cue_parser::SingleImageInfo>,
    pub editor_park: bool,
    pub active_audio_path: Option<PathBuf>,
    pub editor_session: Option<super::message::MetadataEditorSessionGuard>,
}

#[derive(Debug)]
pub struct SplitCueAlbumGroupingAsyncOutcome {
    pub decision: SplitCueAlbumGroupingDecision,
    pub toc_outcome: Option<super::musicbrainz::MbCascadeOutcome>,
    pub cache_writes: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InEditorSplitCueMusicBrainzSource {
    UnifiedAlbum,
    PresentationTabs,
    SingleSourceFolder,
}

#[derive(Debug)]
pub struct InEditorSplitCueMusicBrainzInfoRequest {
    pub operation_id: super::message::TagsMbOperationId,
    pub source: InEditorSplitCueMusicBrainzSource,
    pub sources: Vec<PathBuf>,
    pub audio_paths: Vec<PathBuf>,
    pub active_audio_path: Option<PathBuf>,
    pub editor_session: super::message::MetadataEditorSessionGuard,
}

pub(crate) fn split_cue_album_grouping_key_from_paths(
    paths: &[PathBuf],
) -> SplitCueAlbumGroupingKey {
    SplitCueAlbumGroupingKey(crate::convert::split_cue_album::grouping_key_from_paths(paths))
}

pub(crate) fn split_cue_album_grouping_key(
    infos: &[super::cue_parser::SingleImageInfo],
) -> SplitCueAlbumGroupingKey {
    let cue_paths: Vec<PathBuf> = infos.iter().map(|info| info.cue_path.clone()).collect();
    split_cue_album_grouping_key_from_paths(&cue_paths)
}

fn split_cue_album_grouping_decision_merge(
    infos: &[super::cue_parser::SingleImageInfo],
    reason: SplitCueAlbumGroupingReason,
) -> SplitCueAlbumGroupingDecision {
    let cue_paths: Vec<PathBuf> = infos.iter().map(|info| info.cue_path.clone()).collect();
    crate::convert::split_cue_album::merge_decision(&cue_paths, reason)
}

fn split_cue_album_grouping_decision_split_each(
    infos: &[super::cue_parser::SingleImageInfo],
    reason: SplitCueAlbumGroupingReason,
) -> SplitCueAlbumGroupingDecision {
    let cue_paths: Vec<PathBuf> = infos.iter().map(|info| info.cue_path.clone()).collect();
    crate::convert::split_cue_album::split_each_decision(&cue_paths, reason)
}

fn split_cue_infos_for_decision_group(
    infos: &[super::cue_parser::SingleImageInfo],
    group: &[PathBuf],
) -> Vec<super::cue_parser::SingleImageInfo> {
    let keys: std::collections::BTreeSet<PathBuf> = group.iter().cloned().collect();
    infos
        .iter()
        .filter(|info| keys.contains(&cue_info_path_key(&info.cue_path)))
        .cloned()
        .collect()
}

pub(crate) fn split_cue_decision_groups_as_infos(
    decision: &SplitCueAlbumGroupingDecision,
    infos: &[super::cue_parser::SingleImageInfo],
) -> Vec<Vec<super::cue_parser::SingleImageInfo>> {
    decision
        .groups
        .iter()
        .map(|group| split_cue_infos_for_decision_group(infos, group))
        .filter(|group| !group.is_empty())
        .collect()
}

pub(crate) fn same_folder_split_cue_infos(infos: &[super::cue_parser::SingleImageInfo]) -> bool {
    if infos.len() < 2 {
        return false;
    }
    let Some(first_dir) = infos.first().and_then(|info| info.cue_path.parent()) else {
        return false;
    };
    infos.iter().all(|info| info.cue_path.parent() == Some(first_dir))
}

fn split_cue_title_rung_decision(
    infos: &[super::cue_parser::SingleImageInfo],
) -> Option<SplitCueAlbumGroupingDecision> {
    let titles: Vec<String> = infos
        .iter()
        .map(|info| info.sheet.title.clone().unwrap_or_default())
        .collect();
    meaningful_common_cue_album_prefix(&titles).map(|_| {
        split_cue_album_grouping_decision_merge(
            infos,
            SplitCueAlbumGroupingReason::TitleSharedPrefix,
        )
    })
}

fn seed_mb_query_from_cue_metadata_surfaces(
    surfaces: &[CueMetadataSurface],
) -> Option<SacdMbSeed> {
    let first = surfaces.first()?;
    let artist = first.sheet.performer.clone().unwrap_or_default();
    let titles: Vec<String> = surfaces
        .iter()
        .map(|surface| surface.sheet.title.clone().unwrap_or_default())
        .collect();
    let album = common_cue_album_title(&titles).unwrap_or_default();
    let catalog = first.sheet.catalog.clone();
    let year = first.sheet.date.clone();

    if artist.trim().is_empty()
        && album.trim().is_empty()
        && catalog.as_deref().unwrap_or_default().trim().is_empty()
        && year.as_deref().unwrap_or_default().trim().is_empty()
    {
        None
    } else {
        Some(SacdMbSeed {
            artist,
            album,
            catalog,
            year,
        })
    }
}

fn dispatch_split_cue_musicbrainz_text_fallback_from_surfaces(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    operation_id: super::message::TagsMbOperationId,
    surfaces: &[CueMetadataSurface],
) -> bool {
    let Some(seed) = seed_mb_query_from_cue_metadata_surfaces(surfaces) else {
        return false;
    };
    let paths = paths_for_cue_metadata_surfaces(surfaces);
    if paths.is_empty() {
        return false;
    }
    let ctx = super::message::TagsMbContext {
        operation_id,
        paths,
        editor_park: false,
        fallback_seed: None,
        editor_session: None,
    };
    super::event_loop::spawn_tags_mb_text_search(
        app,
        tx,
        seed,
        ctx,
        super::event_loop::TextSearchMode::DirectRequest,
    );
    true
}

pub(crate) fn collect_single_image_cue_infos_for_sources(
    sources: &[PathBuf],
    audio_paths: &[PathBuf],
) -> Vec<super::cue_parser::SingleImageInfo> {
    let audio_keys: std::collections::BTreeSet<PathBuf> = audio_paths
        .iter()
        .map(|path| cue_info_path_key(path))
        .collect();

    let mut cue_paths = Vec::new();
    let mut seen_cues = std::collections::BTreeSet::new();
    for source in sources {
        collect_cue_paths_from_source(source, &mut cue_paths, &mut seen_cues);
    }
    for audio in audio_paths {
        if let Some(parent) = audio.parent() {
            collect_cue_paths_from_source(parent, &mut cue_paths, &mut seen_cues);
        }
    }

    let mut infos = Vec::new();
    for cue_path in cue_paths {
        let Some(info) = super::cue_parser::detect_single_image_cue(&cue_path) else {
            continue;
        };
        if !audio_keys.is_empty() && !audio_keys.contains(&cue_info_path_key(&info.audio_path)) {
            continue;
        }
        infos.push(info);
    }
    infos.sort_by(|a, b| a.cue_path.cmp(&b.cue_path));
    infos
}

pub(crate) fn paths_for_single_image_cue_infos(
    infos: &[super::cue_parser::SingleImageInfo],
) -> Vec<PathBuf> {
    infos
        .iter()
        .flat_map(|info| {
            (0..info.sheet.tracks.len()).map(move |_| info.audio_path.clone())
        })
        .collect()
}

fn seed_mb_query_from_single_image_cues(
    infos: &[super::cue_parser::SingleImageInfo],
) -> Option<SacdMbSeed> {
    let first = infos.first()?;
    let artist = first.sheet.performer.clone().unwrap_or_default();
    let titles: Vec<String> = infos
        .iter()
        .map(|info| info.sheet.title.clone().unwrap_or_default())
        .collect();
    let album = common_cue_album_title(&titles).unwrap_or_default();
    let catalog = first.sheet.catalog.clone();
    let year = first.sheet.date.clone();

    if artist.trim().is_empty()
        && album.trim().is_empty()
        && catalog.as_deref().unwrap_or_default().trim().is_empty()
        && year.as_deref().unwrap_or_default().trim().is_empty()
    {
        None
    } else {
        Some(SacdMbSeed {
            artist,
            album,
            catalog,
            year,
        })
    }
}

fn concat_single_image_cue_infos_to_cd_sectors(
    infos: &[super::cue_parser::SingleImageInfo],
) -> Option<Vec<u32>> {
    if infos.len() < 2 || infos.iter().any(|info| info.track_boundaries.is_empty()) {
        return None;
    }
    let mut sectors = Vec::new();
    let mut frame: u64 = CD_TOC_PREGAP_FRAMES as u64;
    for info in infos {
        let samples_per_frame = (info.sample_rate / 75).max(1) as u64;
        for &(_, sample_count) in &info.track_boundaries {
            sectors.push(frame as u32);
            frame += sample_count / samples_per_frame;
        }
    }
    sectors.push(frame as u32);
    if super::musicbrainz::build_mb_toc(&sectors).is_some() {
        Some(sectors)
    } else {
        None
    }
}

fn single_image_cue_info_to_cd_sectors(
    info: &super::cue_parser::SingleImageInfo,
) -> Option<Vec<u32>> {
    if info.track_boundaries.is_empty() {
        return None;
    }
    let mut sectors = Vec::with_capacity(info.track_boundaries.len() + 1);
    let samples_per_frame = (info.sample_rate / 75).max(1) as u64;
    let mut frame: u64 = CD_TOC_PREGAP_FRAMES as u64;
    for &(_, sample_count) in &info.track_boundaries {
        sectors.push(frame as u32);
        frame += sample_count / samples_per_frame;
    }
    sectors.push(frame as u32);
    super::musicbrainz::build_mb_toc(&sectors).map(|_| sectors)
}

fn split_cue_album_grouping_probe_inputs(
    app: &AppState,
    infos: &[super::cue_parser::SingleImageInfo],
) -> (
    Vec<super::musicbrainz::TocCandidate>,
    Vec<Option<String>>,
    Vec<(Vec<u32>, Option<String>)>,
) {
    split_cue_album_grouping_probe_inputs_with_cache(infos, |toc| {
        app.db.get_cached_mb_response(toc)
    })
}

fn split_cue_album_grouping_probe_inputs_with_cache<F>(
    infos: &[super::cue_parser::SingleImageInfo],
    mut cached_mb_response: F,
) -> (
    Vec<super::musicbrainz::TocCandidate>,
    Vec<Option<String>>,
    Vec<(Vec<u32>, Option<String>)>,
) where
    F: FnMut(&str) -> Option<String>,
{
    let concat_candidates = concat_single_image_cue_infos_to_cd_sectors(infos)
        .map(|sectors| super::musicbrainz::toc_candidates_from_sectors(&sectors))
        .unwrap_or_default();
    let concat_cached = concat_candidates
        .iter()
        .map(|candidate| {
            super::musicbrainz::build_mb_toc(&candidate.sectors)
                .and_then(|toc| cached_mb_response(&toc))
        })
        .collect();
    let per_cue_cached: Vec<(Vec<u32>, Option<String>)> = infos
        .iter()
        .filter_map(|info| {
            let sectors = single_image_cue_info_to_cd_sectors(info)?;
            let cached = super::musicbrainz::build_mb_toc(&sectors)
                .and_then(|toc| cached_mb_response(&toc));
            Some((sectors, cached))
        })
        .collect();
    (concat_candidates, concat_cached, per_cue_cached)
}

fn cached_split_cue_album_grouping_ladder_decision(
    app: &AppState,
    infos: &[super::cue_parser::SingleImageInfo],
) -> Option<SplitCueAlbumGroupingDecision> {
    cached_split_cue_album_grouping_ladder_decision_with_cache(infos, |toc| {
        app.db.get_cached_mb_response(toc)
    })
}

fn cached_split_cue_album_grouping_ladder_decision_with_cache<F>(
    infos: &[super::cue_parser::SingleImageInfo],
    cached_mb_response: F,
) -> Option<SplitCueAlbumGroupingDecision>
where
    F: FnMut(&str) -> Option<String>,
{
    let (concat_candidates, concat_cached, per_cue_cached) =
        split_cue_album_grouping_probe_inputs_with_cache(infos, cached_mb_response);
    for (candidate, cached) in concat_candidates.iter().zip(concat_cached.iter()) {
        let body = cached.as_ref()?;
        let releases = super::musicbrainz::parse_mb_response_all(
            body,
            candidate.sectors.len().saturating_sub(1),
        )
        .ok()?;
        if !releases.is_empty() {
            return Some(split_cue_album_grouping_decision_merge(
                infos,
                SplitCueAlbumGroupingReason::ConcatTocHit,
            ));
        }
    }

    if per_cue_cached.len() != infos.len() {
        return Some(split_cue_album_grouping_decision_merge(
            infos,
            SplitCueAlbumGroupingReason::AmbiguousMerge,
        ));
    }

    let mut release_ids = Vec::new();
    for (sectors, cached) in &per_cue_cached {
        let body = cached.as_ref()?;
        let releases = super::musicbrainz::parse_mb_response_all(
            body,
            sectors.len().saturating_sub(1),
        )
        .ok()?;
        let Some(release) = releases.first() else {
            return Some(split_cue_album_grouping_decision_merge(
                infos,
                SplitCueAlbumGroupingReason::AmbiguousMerge,
            ));
        };
        if release.release_id.trim().is_empty() {
            return Some(split_cue_album_grouping_decision_merge(
                infos,
                SplitCueAlbumGroupingReason::AmbiguousMerge,
            ));
        }
        release_ids.push(release.release_id.clone());
    }

    let unique: std::collections::BTreeSet<String> = release_ids.iter().cloned().collect();
    if unique.len() == infos.len() {
        Some(split_cue_album_grouping_decision_split_each(
            infos,
            SplitCueAlbumGroupingReason::PerCueDistinctTocHits,
        ))
    } else {
        Some(split_cue_album_grouping_decision_merge(
            infos,
            SplitCueAlbumGroupingReason::AmbiguousMerge,
        ))
    }
}

pub(crate) fn cached_or_title_split_cue_album_grouping_decision(
    app: &mut AppState,
    infos: &[super::cue_parser::SingleImageInfo],
) -> Option<SplitCueAlbumGroupingDecision> {
    if !same_folder_split_cue_infos(infos) {
        return None;
    }
    let key = split_cue_album_grouping_key(infos);
    if let Some(decision) = app.split_cue_album_grouping_cache.get(&key).cloned() {
        return Some(decision);
    }
    if let Some(decision) = split_cue_title_rung_decision(infos) {
        app.split_cue_album_grouping_cache
            .insert(key, decision.clone());
        return Some(decision);
    }
    if let Some(decision) = cached_split_cue_album_grouping_ladder_decision(app, infos) {
        app.split_cue_album_grouping_cache
            .insert(key, decision.clone());
        return Some(decision);
    }
    None
}

pub(crate) fn store_split_cue_album_grouping_outcome(
    app: &mut AppState,
    infos: &[super::cue_parser::SingleImageInfo],
    outcome: &SplitCueAlbumGroupingAsyncOutcome,
) {
    for (toc, body) in &outcome.cache_writes {
        if let Err(err) = app.db.store_mb_response(toc, body) {
            log::warn!("MB TOC cache store failed during split-CUE grouping: {err}");
        }
    }
    let key = split_cue_album_grouping_key(infos);
    app.split_cue_album_grouping_cache
        .insert(key, outcome.decision.clone());
}

pub(crate) fn spawn_split_cue_album_grouping_ladder_for_gnudb(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    infos: Vec<super::cue_parser::SingleImageInfo>,
    active_audio_path: Option<PathBuf>,
) -> bool {
    if !same_folder_split_cue_infos(&infos) {
        return false;
    }
    let (concat_candidates, concat_cached, per_cue_cached) =
        split_cue_album_grouping_probe_inputs(app, &infos);
    app.set_status(format!(
        "GNUDB: grouping {} same-folder CUE parts...",
        infos.len(),
    ));
    let tx = tx.clone();
    tokio::spawn(async move {
        let result = compute_split_cue_album_grouping_ladder(
            infos.clone(),
            concat_candidates,
            concat_cached,
            per_cue_cached,
        )
        .await
        .map(Box::new);
        let _ = tx
            .send(AppMessage::GnudbSplitCueAlbumGroupingComplete {
                infos,
                active_audio_path,
                result,
            })
            .await;
    });
    true
}

pub(crate) fn spawn_split_cue_album_grouping_ladder_for_metadata_editor(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    infos: Vec<super::cue_parser::SingleImageInfo>,
    active_cue_path: Option<PathBuf>,
) -> bool {
    if !same_folder_split_cue_infos(&infos) {
        return false;
    }
    let (concat_candidates, concat_cached, per_cue_cached) =
        split_cue_album_grouping_probe_inputs(app, &infos);
    app.set_status(format!(
        "metadata: grouping {} same-folder CUE parts...",
        infos.len(),
    ));
    let tx = tx.clone();
    tokio::spawn(async move {
        let result = compute_split_cue_album_grouping_ladder(
            infos.clone(),
            concat_candidates,
            concat_cached,
            per_cue_cached,
        )
        .await
        .map(Box::new);
        let _ = tx
            .send(AppMessage::MetadataEditorSplitCueAlbumGroupingComplete {
                infos,
                active_cue_path,
                result,
            })
            .await;
    });
    true
}

pub(crate) fn split_cue_active_group<'a>(
    groups: &'a [Vec<super::cue_parser::SingleImageInfo>],
    active_audio_path: Option<&Path>,
) -> Option<&'a [super::cue_parser::SingleImageInfo]> {
    if let Some(active) = active_audio_path {
        if let Some(group) = groups.iter().find(|group| {
            group.iter().any(|info| {
                info.audio_path == active
                    || match (info.audio_path.canonicalize(), active.canonicalize()) {
                        (Ok(a), Ok(b)) => a == b,
                        _ => false,
                    }
            })
        }) {
            return Some(group.as_slice());
        }
    }
    groups.first().map(Vec::as_slice)
}

fn dispatch_split_cue_musicbrainz_for_decision(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    operation_id: super::message::TagsMbOperationId,
    infos: &[super::cue_parser::SingleImageInfo],
    decision: &SplitCueAlbumGroupingDecision,
    editor_park: bool,
    active_audio_path: Option<&Path>,
    concat_already_missed: bool,
    editor_session: Option<super::message::MetadataEditorSessionGuard>,
) -> bool {
    let groups = split_cue_decision_groups_as_infos(decision, infos);
    if groups.is_empty() {
        return false;
    }

    let merged = decision.groups.len() == 1;
    if merged {
        let group = groups[0].as_slice();
        let paths = paths_for_single_image_cue_infos(group);
        if paths.is_empty() {
            return false;
        }
        let fallback_seed = seed_mb_query_from_single_image_cues(group);
        if !concat_already_missed {
            if let Some(sectors) = concat_single_image_cue_infos_to_cd_sectors(group) {
                let candidates = super::musicbrainz::toc_candidates_from_sectors(&sectors);
                if !candidates.is_empty() {
                    spawn_tags_mb_toc_lookup(app, tx, operation_id, candidates, paths, editor_park, fallback_seed, editor_session);
                    return true;
                }
            }
        }
        let Some(seed) = fallback_seed else {
            return false;
        };
        let ctx = super::message::TagsMbContext {
            operation_id,
            paths,
            editor_park,
            fallback_seed: None,
            editor_session,
        };
        super::event_loop::spawn_tags_mb_text_search(
            app,
            tx,
            seed,
            ctx,
            super::event_loop::TextSearchMode::DirectRequest,
        );
        return true;
    }

    let Some(group) = split_cue_active_group(&groups, active_audio_path) else {
        return false;
    };
    let paths = paths_for_single_image_cue_infos(group);
    if paths.is_empty() {
        return false;
    }
    let fallback_seed = seed_mb_query_from_single_image_cues(group);
    if let Some(info) = group.first() {
        if group.len() == 1 {
            if let Some(sectors) = single_image_cue_info_to_cd_sectors(info) {
                spawn_tags_mb_toc_lookup(
                    app,
                    tx,
                    operation_id,
                    vec![super::musicbrainz::TocCandidate::exact(sectors)],
                    paths,
                    editor_park,
                    fallback_seed,
                    editor_session,
                );
                return true;
            }
        }
    }
    let Some(seed) = fallback_seed else {
        return false;
    };
    let ctx = super::message::TagsMbContext {
        operation_id,
        paths,
        editor_park,
        fallback_seed: None,
        editor_session,
    };
    super::event_loop::spawn_tags_mb_text_search(
        app,
        tx,
        seed,
        ctx,
        super::event_loop::TextSearchMode::DirectRequest,
    );
    true
}

async fn compute_split_cue_album_grouping_ladder(
    infos: Vec<super::cue_parser::SingleImageInfo>,
    concat_candidates: Vec<super::musicbrainz::TocCandidate>,
    concat_cached: Vec<Option<String>>,
    per_cue_cached: Vec<(Vec<u32>, Option<String>)>,
) -> Result<SplitCueAlbumGroupingAsyncOutcome, String> {
    let mut cache_writes = Vec::new();

    if !concat_candidates.is_empty() {
        let concat_outcome =
            super::musicbrainz::lookup_release_by_toc_cascading(&concat_candidates, concat_cached)
                .await?;
        if !concat_outcome.releases.is_empty() {
            return Ok(SplitCueAlbumGroupingAsyncOutcome {
                decision: split_cue_album_grouping_decision_merge(
                    &infos,
                    SplitCueAlbumGroupingReason::ConcatTocHit,
                ),
                toc_outcome: Some(concat_outcome),
                cache_writes,
            });
        }
        cache_writes.extend(concat_outcome.cache_writes);
    }

    let mut release_ids = Vec::new();
    for (idx, (sectors, cached)) in per_cue_cached.into_iter().enumerate() {
        let outcome = super::musicbrainz::lookup_release_by_toc(&sectors, cached).await?;
        if let Some(body) = outcome.cache_response {
            if let Some(toc) = super::musicbrainz::build_mb_toc(&sectors) {
                cache_writes.push((toc, body));
            }
        }
        let Some(release) = outcome.releases.first() else {
            release_ids.clear();
            break;
        };
        let release_id = release.release_id.clone();
        if release_id.trim().is_empty() {
            release_ids.clear();
            break;
        }
        release_ids.push((idx, release_id));
    }

    if release_ids.len() == infos.len() {
        let unique: std::collections::BTreeSet<String> =
            release_ids.iter().map(|(_, id)| id.clone()).collect();
        if unique.len() == infos.len() {
            return Ok(SplitCueAlbumGroupingAsyncOutcome {
                decision: split_cue_album_grouping_decision_split_each(
                    &infos,
                    SplitCueAlbumGroupingReason::PerCueDistinctTocHits,
                ),
                toc_outcome: None,
                cache_writes,
            });
        }
    }

    Ok(SplitCueAlbumGroupingAsyncOutcome {
        decision: split_cue_album_grouping_decision_merge(
            &infos,
            SplitCueAlbumGroupingReason::AmbiguousMerge,
        ),
        toc_outcome: None,
        cache_writes,
    })
}

fn dispatch_split_cue_musicbrainz_concat_or_text_fallback(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    operation_id: super::message::TagsMbOperationId,
    infos: &[super::cue_parser::SingleImageInfo],
    editor_park: bool,
    active_audio_path: Option<PathBuf>,
    editor_session: Option<super::message::MetadataEditorSessionGuard>,
) -> bool {
    let paths = paths_for_single_image_cue_infos(infos);
    if paths.is_empty() {
        return false;
    }

    if !same_folder_split_cue_infos(infos) {
        let decision = split_cue_album_grouping_decision_merge(
            infos,
            SplitCueAlbumGroupingReason::AmbiguousMerge,
        );
        if !super::event_loop::transition_tags_mb_operation_phase(
            app,
            operation_id,
            super::app::TagsMbOperationPhase::Grouping,
            super::app::TagsMbOperationPhase::Lookup,
        ) {
            return false;
        }
        return dispatch_split_cue_musicbrainz_for_decision(
            app,
            tx,
            operation_id,
            infos,
            &decision,
            editor_park,
            active_audio_path.as_deref(),
            false,
            editor_session,
        );
    }

    let key = split_cue_album_grouping_key(infos);
    if let Some(decision) = cached_or_title_split_cue_album_grouping_decision(app, infos) {
        if !super::event_loop::transition_tags_mb_operation_phase(
            app,
            operation_id,
            super::app::TagsMbOperationPhase::Grouping,
            super::app::TagsMbOperationPhase::Lookup,
        ) {
            return false;
        }
        return dispatch_split_cue_musicbrainz_for_decision(
            app,
            tx,
            operation_id,
            infos,
            &decision,
            editor_park,
            active_audio_path.as_deref(),
            false,
            editor_session,
        );
    }

    let (concat_candidates, concat_cached, per_cue_cached) =
        split_cue_album_grouping_probe_inputs(app, infos);

    if !super::event_loop::tags_mb_operation_is_current_phase(
        app,
        operation_id,
        super::app::TagsMbOperationPhase::Grouping,
    ) {
        return false;
    }
    let request = SplitCueAlbumGroupingRequest {
        operation_id,
        key,
        infos: infos.to_vec(),
        editor_park,
        active_audio_path,
        editor_session,
    };
    app.set_status(format!(
        ":tags-mb: grouping {} same-folder CUE parts...",
        request.infos.len(),
    ));
    let tx = tx.clone();
    tokio::spawn(async move {
        let result = compute_split_cue_album_grouping_ladder(
            request.infos.clone(),
            concat_candidates,
            concat_cached,
            per_cue_cached,
        )
        .await
        .map(Box::new);
        let _ = tx
            .send(AppMessage::SplitCueAlbumGroupingComplete {
                request: Box::new(request),
                result,
            })
            .await;
    });
    true
}

/// Test-facing composition of the production grouping pieces: the real call
/// sites (context_menu GNUDB dispatch, the MB grouping dispatchers, and the
/// metadata-surface grouping in keybindings) inline this decision->groups->
/// active-group sequence with flow-specific handling.
#[cfg(test)]
pub(crate) fn split_cue_infos_for_cached_or_title_grouping(
    app: &mut AppState,
    infos: Vec<super::cue_parser::SingleImageInfo>,
    active_audio_path: Option<&Path>,
) -> Vec<super::cue_parser::SingleImageInfo> {
    if !same_folder_split_cue_infos(&infos) {
        return infos;
    }
    let Some(decision) = cached_or_title_split_cue_album_grouping_decision(app, &infos) else {
        return infos;
    };
    if decision.groups.len() <= 1 {
        return infos;
    }
    let groups = split_cue_decision_groups_as_infos(&decision, &infos);
    split_cue_active_group(&groups, active_audio_path)
        .map(|group| group.to_vec())
        .unwrap_or(infos)
}

pub(super) fn handle_split_cue_album_grouping_complete(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    request: SplitCueAlbumGroupingRequest,
    result: Result<Box<SplitCueAlbumGroupingAsyncOutcome>, String>,
) {
    if !super::event_loop::tags_mb_operation_is_current_phase(
        app,
        request.operation_id,
        super::app::TagsMbOperationPhase::Grouping,
    ) {
        return;
    }

    let outcome = match result {
        Ok(outcome) => *outcome,
        Err(err) => {
            if super::event_loop::finish_tags_mb_operation_if_current(
                app,
                request.operation_id,
            ) {
                app.set_status(format!(":tags-mb: split-CUE grouping failed: {err}"));
            }
            return;
        }
    };

    if !super::event_loop::transition_tags_mb_operation_phase(
        app,
        request.operation_id,
        super::app::TagsMbOperationPhase::Grouping,
        super::app::TagsMbOperationPhase::Lookup,
    ) {
        return;
    }

    store_split_cue_album_grouping_outcome(app, &request.infos, &outcome);

    if let Some(toc_outcome) = outcome.toc_outcome {
        let ctx = super::message::TagsMbContext {
            operation_id: request.operation_id,
            paths: paths_for_single_image_cue_infos(&request.infos),
            editor_park: request.editor_park,
            fallback_seed: seed_mb_query_from_single_image_cues(&request.infos),
            editor_session: request.editor_session,
        };
        super::event_loop::dispatch_tags_from_mb_complete(
            app,
            tx,
            super::message::MbOutcome::Toc {
                outcome: Ok(toc_outcome),
            },
            ctx,
        );
        return;
    }

    if !dispatch_split_cue_musicbrainz_for_decision(
        app,
        tx,
        request.operation_id,
        &request.infos,
        &outcome.decision,
        request.editor_park,
        request.active_audio_path.as_deref(),
        true,
        request.editor_session,
    ) && super::event_loop::finish_tags_mb_operation_if_current(app, request.operation_id)
    {
        app.set_status(
            ":tags-mb: split-CUE grouping produced no usable MusicBrainz seed".to_string(),
        );
    }
}


fn metadata_editor_session_guard(
    state: &super::app::MetadataEditorState,
) -> super::message::MetadataEditorSessionGuard {
    let details = &state.active_surface().technical_details;
    super::message::MetadataEditorSessionGuard {
        session_id: details.session_id,
        save_generation: details.save_generation,
        editor_generation: state.model.editor_save_generation,
    }
}

fn metadata_editor_tags_mb_context_paths(
    state: &super::app::MetadataEditorState,
) -> Vec<PathBuf> {
    if let Some(sheet) = state.active_surface().cue_album_synthetic_sheet.as_ref() {
        return sheet
            .track_sources
            .iter()
            .map(|source| source.audio_path.clone())
            .collect();
    }
    if state.presentation_tabs.len() > 1 {
        let mut paths = Vec::new();
        for tab in &state.presentation_tabs {
            let Some(path) = tab.paths.first() else {
                return state.active_surface().paths.clone();
            };
            let count = tab.file_labels.len().max(1);
            paths.extend(std::iter::repeat(path.clone()).take(count));
        }
        return paths;
    }
    state.active_surface().paths.clone()
}

fn in_editor_split_cue_mb_request_from_metadata_tabs(
    state: &super::app::MetadataEditorState,
) -> Option<InEditorSplitCueMusicBrainzInfoRequest> {
    if state.presentation_tabs.len() < 2 {
        return None;
    }
    let mut audio_paths = Vec::new();
    for tab in &state.presentation_tabs {
        if tab.paths.len() == 1 {
            audio_paths.push(tab.paths[0].clone());
        } else {
            return None;
        }
    }
    Some(InEditorSplitCueMusicBrainzInfoRequest {
        operation_id: super::message::TagsMbOperationId::UNASSIGNED,
        source: InEditorSplitCueMusicBrainzSource::PresentationTabs,
        sources: audio_paths.clone(),
        audio_paths,
        active_audio_path: state.active_surface().paths.first().cloned(),
        editor_session: metadata_editor_session_guard(state),
    })
}

fn in_editor_split_cue_mb_request_from_unified_album(
    state: &super::app::MetadataEditorState,
) -> Option<InEditorSplitCueMusicBrainzInfoRequest> {
    let sheet = state.active_surface().cue_album_synthetic_sheet.as_ref()?;
    Some(InEditorSplitCueMusicBrainzInfoRequest {
        operation_id: super::message::TagsMbOperationId::UNASSIGNED,
        source: InEditorSplitCueMusicBrainzSource::UnifiedAlbum,
        sources: sheet.audio_paths.clone(),
        audio_paths: sheet.audio_paths.clone(),
        active_audio_path: state
            .active_surface()
            .paths
            .first()
            .cloned()
            .or_else(|| sheet.audio_paths.first().cloned()),
        editor_session: metadata_editor_session_guard(state),
    })
}


fn same_path_for_split_cue(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn in_editor_split_cue_mb_request_from_single_editor_source_folder(
    state: &super::app::MetadataEditorState,
) -> Option<InEditorSplitCueMusicBrainzInfoRequest> {
    // Ordinary file-backed metadata editors store their only editable surface in
    // `file_surface` and leave `presentation_tabs` empty.  The reducer may only
    // capture the active audio path and parent directory here; CUE discovery and
    // parsing happen later in the blocking worker.
    if state.presentation_tabs.len() > 1 {
        return None;
    }
    let surface = state.active_surface();
    if surface.paths.len() != 1
        || surface.sacd_area_kind.is_some()
        || surface.dvdv_track_durations.is_some()
        || surface.bluray_chapter_durations.is_some()
    {
        return None;
    }
    let active_audio = surface.paths.first()?.clone();
    let parent = active_audio.parent()?.to_path_buf();
    Some(InEditorSplitCueMusicBrainzInfoRequest {
        operation_id: super::message::TagsMbOperationId::UNASSIGNED,
        source: InEditorSplitCueMusicBrainzSource::SingleSourceFolder,
        sources: vec![parent],
        audio_paths: Vec::new(),
        active_audio_path: Some(active_audio),
        editor_session: metadata_editor_session_guard(state),
    })
}

/// Full list of command-mode tokens (including aliases) recognised by
/// `parse_command`. Used by the tab-completion machinery.
///
/// Ordered so more-typed commands come first — matters for UX because
/// the first match is what Tab shows initially before the user cycles.
pub const COMMAND_NAMES: &[&str] = &[
    "q",
    "quit",
    "exit",
    "w",
    "write",
    "save",
    "wq",
    "e",
    "edit",
    "o",
    "output",
    "max",
    "maximize",
    "adv",
    "advanced",
    "cd",
    "queue",
    "queue!",
    "qa",
    "qa!",
    "c",
    "convert",
    "commit",
    "Commit",
    "go",
    "start",
    "expand",
    "x",
    "batch",
    "preset",
    "presets",
    "saveas",
    "set",
    "fx",
    "effects",
    "info",
    "tools",
    "h",
    "help",
    "sort",
    "sortdir",
    "filter",
    "refresh",
    "rename",
    "del",
    "delete",
    "cp",
    "cp!",
    "copy",
    "copy!",
    "mv",
    "mv!",
    "move",
    "move!",
    "browse",
    "b",
    "recent",
    "recents",
    "bookmarks",
    "bm",
    "rename-all",
    "renameall",
    "bulk-rename",
    "actions",
    "actions-run",
    "actions-identity-import",
    "password",
    "pw",
    "analyze",
    "analyze!",
    "analysis",
    "dr",
    "write-dr",
    "writedr",
    "write-rg-track",
    "write-rg-album",
    "import-cue",
    "cue",
    "cue!",
    "cue-mb",
    "cue-mb!",
    "cue-fill",
    "cue-enrich",
    "cue-view",
    "tags-cue-sidecar",
    "cuesheet-delete",
    "cuesheet-edit",
    "tags-mb",
    "mb-tags",
    "musicbrainz-tags",
    "revert",
    "restore",
    "g",
    "G",
    "top",
    "bot",
    "bottom",
    "fix-caps",
    "fixcaps",
    "search",
    "s",
    "rs",
    "rsearch",
    "context",
    "menu",
    "ar",
    "ar!",
    "accuraterip",
    "accuraterip!",
    "ar-fix",
    "ar-batch",
    "ctdb",
    "ctdb-repair",
    "cuetools",
    "cuetools-repair",
    "view",
    "cat",
    "edit-file",
    "ef",
];

/// Commands that take a preset name as their argument. Used by the
/// completion machinery to decide whether the word after the command
/// should be completed against preset file names.
pub const PRESET_TAKING_COMMANDS: &[&str] =
    &["queue", "queue!", "qa", "qa!", "c", "convert", "preset"];

/// Compute tab-completion candidates from a CommandInput's current text
/// and cursor position. Returns `None` if no completion is applicable
/// (no matching candidates, or we're in a context that doesn't complete).
///
/// Completion kinds:
/// - **Command name** — prefix is the first word of the input and the
///   cursor is within it. Candidates come from `COMMAND_NAMES`.
/// - **Preset name** — first word is in `PRESET_TAKING_COMMANDS` and the
///   cursor is in the second word. Candidates come from the user's
///   preset directory via `presets::list_presets()`. Case-insensitive
///   prefix match.
///
/// Returns a `CompletionState` with `cursor = 0` (first candidate).
pub fn compute_completion(text: &str, cursor: usize) -> Option<CompletionState> {
    let before_cursor = &text[..cursor.min(text.len())];

    // Start of the word being completed: just after the last whitespace,
    // or 0 if there is none. Uses char_indices for multibyte safety.
    let prefix_start = before_cursor
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let prefix = &before_cursor[prefix_start..];

    // Command name completion: nothing before the prefix (prefix_start == 0).
    if prefix_start == 0 {
        let candidates: Vec<String> = COMMAND_NAMES
            .iter()
            .filter(|n| n.starts_with(prefix))
            .map(|n| n.to_string())
            .collect();
        if candidates.is_empty() {
            return None;
        }
        return Some(CompletionState {
            candidates,
            cursor: 0,
            prefix_start,
        });
    }

    // Preset-argument completion: first word is a preset-taking command.
    let first_word_end = before_cursor
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(before_cursor.len());
    let first_word = &before_cursor[..first_word_end];

    if !PRESET_TAKING_COMMANDS.contains(&first_word) {
        return None;
    }

    // Case-insensitive prefix match against preset names on disk.
    let presets = super::presets::list_presets();
    let prefix_lower = prefix.to_lowercase();
    let candidates: Vec<String> = presets
        .into_iter()
        .filter(|p| p.to_lowercase().starts_with(&prefix_lower))
        .collect();

    if candidates.is_empty() {
        return None;
    }

    Some(CompletionState {
        candidates,
        cursor: 0,
        prefix_start,
    })
}

/// Advance the completion cycle by `direction` (+1 forward, -1 backward)
/// and apply the new selection to the input. Called on every Tab /
/// Shift+Tab press while a completion is active.
pub fn cycle_completion(
    input: &mut super::text_input::TextInputState,
    state: &mut CompletionState,
    direction: i32,
) {
    let len = state.candidates.len();
    if len == 0 {
        return;
    }
    state.cursor = if direction >= 0 {
        (state.cursor + 1) % len
    } else {
        (state.cursor + len - 1) % len
    };
    apply_completion_to_input(input, state);
}

/// Replace the active completion range (`prefix_start..input.cursor`)
/// with the currently-selected candidate, moving the cursor to the end
/// of the inserted text.
pub fn apply_completion_to_input(
    input: &mut super::text_input::TextInputState,
    state: &CompletionState,
) {
    let candidate = &state.candidates[state.cursor];
    let prefix_end = input.cursor.min(input.text.len());
    input
        .text
        .replace_range(state.prefix_start..prefix_end, candidate);
    input.cursor = state.prefix_start + candidate.len();
}

/// Argument to `:area` for SACD area switching in the metadata
/// editor. `Toggle` flips between stereo and multi-channel when
/// both are present; an explicit kind is a no-op if that area
/// isn't on the disc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SacdAreaTarget {
    Stereo,
    MultiChannel,
    Toggle,
}

/// Parsed command from the command line
pub enum Command {
    Quit,
    Write,
    WriteQuit,
    Edit(String),
    Output(String),
    Cd(String),
    /// Open the Convert screen for batch review, inheriting the current
    /// browse selection if any. Triggered by `:queue`, `:queue!`, `:qa`,
    /// `:qa!`, `:convert`, `:c`. Optional preset name is loaded into the
    /// format pills before review.
    Queue {
        preset: Option<String>,
    },
    /// Commit the currently-reviewed file/batch from the Convert screen to
    /// the queue. `:commit` (lowercase) enqueues only; `:Commit` (capital)
    /// enqueues AND starts processing, jumping to the Queue screen.
    Commit {
        start: bool,
    },
    /// Commit after applying a final transform to the pipeline source options.
    CommitWithSourceOptionsTransform {
        start: bool,
        transform: Box<
            dyn FnOnce(crate::convert::pipeline::SourceOptions) -> crate::convert::pipeline::SourceOptions
                + Send,
        >,
    },
    /// Toggle maximize/restore for the focused Convert pane.
    Maximize,
    /// Toggle the focused Convert pane's advanced section.
    Advanced,
    /// Start processing whatever's already in the queue. No new batch.
    /// Triggered by `:go` / `:start`.
    Go,
    /// Open the BatchList expand overlay. Only valid when the Convert
    /// source is a multi-file `Batch`. Triggered by `:expand` / `:x`.
    Expand,
    Batch(String),
    Preset(String),
    SaveAs(String),
    Presets,
    Set(String, String),
    Fx(Vec<String>),
    Info,
    Tools,
    Help,
    /// Sort command for the browse screen. Args: (field?, dir?)
    Sort(Option<String>, Option<String>),
    /// Toggle sort direction
    SortDir,
    /// Filter command for the browse screen. Arg: format name or empty (cycle)
    Filter(Option<String>),
    /// Refresh the current browse directory from the filesystem.
    Refresh,
    /// Rename the current browse selection to the given name.
    /// Empty arg opens the rename overlay seeded with the current name.
    Rename(String),
    /// Permanently delete selected browse file(s). Shows confirmation first.
    Delete,
    /// Copy selected file(s) to a destination. Empty arg opens a directory
    /// picker. `:cp!` variant replaces existing files.
    Copy {
        dest: String,
        force: bool,
    },
    /// Move selected file(s) to a destination. Empty arg opens picker.
    /// `:mv!` replaces existing. Falls back to copy+delete across filesystems.
    Move {
        dest: String,
        force: bool,
    },
    /// Switch to the browse screen. On the convert screen, sets
    /// the return target so a selected file loads back into the source pane.
    Browse,
    /// Open the recent-files overlay.
    Recent,
    /// Open the bookmarks overlay (browse-only). With no args, opens in
    /// browsing mode. With "add [name]", quick-adds the current browse
    /// directory as a bookmark without opening the overlay.
    Bookmarks(String),
    /// Open the bulk rename wizard for the current selection.
    BulkRename,
    /// Open the ordered pre/post conversion-actions wizard.
    Actions,
    /// Preview and explicitly apply the configured POST action pipeline to an
    /// existing album directory. No argument uses the current Browse directory.
    ActionsRun(Option<String>),
    /// Import a reviewed canonical action-identity payload for a pre-feature
    /// album. No argument uses `.tonepoet-action-identity.import.json` in the
    /// current Browse directory.
    ActionsIdentityImport(Option<String>),
    /// Analyze selected audio file(s) — DR, peak, clipping, etc.
    /// `force`: if true, skip cache and re-analyze.
    Analyze {
        force: bool,
    },
    /// Write DR analysis report to each album directory.
    WriteDr,
    /// Write ReplayGain track tags via loudgain.
    WriteRgTrack,
    /// Write ReplayGain album + track tags via loudgain.
    WriteRgAlbum,
    /// Import metadata from a CUE sheet via external editor + review.
    ImportCue,
    /// Apply capitalization rules to TITLE, ARTIST, ALBUM fields.
    FixCaps,
    /// Verify integrity of selected audio file(s).
    Verify,
    /// Generate a CUE sheet from the selected audio files.
    /// `single_image`: false = multi-file, true = single image with cumulative timestamps.
    GenerateCue {
        single_image: bool,
    },
    /// Generate a CUE sheet driven by a MusicBrainz disc-TOC lookup. Title,
    /// performer, ISRC, catalog, barcode all come from MB; durations and
    /// pregaps come from local probe + EAC log.
    GenerateCueMb {
        single_image: bool,
    },
    /// Read a colocated CUE, fill empty/absent fields from a MusicBrainz
    /// disc-TOC lookup, write back. Preserves the existing CUE form
    /// (single-image vs multi-file) and user-typed values.
    CueFill,
    /// View the embedded CUE sheet on the metadata-editor cursor row
    /// (synthetic-preview entry, e.g. CUESHEET) in a read-only
    /// CuePreview overlay. Parks the metadata editor; Esc restores.
    CueView,
    /// Scroll the parked CUE preview overlay to the top.
    CueScrollTop,
    /// Scroll the parked CUE preview overlay to the bottom.
    CueScrollBottom,
    /// Begin editing the 1-based `line` of the parked CUE preview.
    CueEditLine(usize),
    /// Open the metadata editor pre-populated with track-level + album-level
    /// values from a MusicBrainz disc-TOC lookup.
    ///
    /// All fields `None` (the bare `:tags-mb` form) keeps today's
    /// behavior: TOC-primary with seed-from-editor text fallback.
    /// Any field `Some` switches to **direct text search** (TOC is
    /// skipped) using the supplied values as the seed. Free-form text
    /// goes to the album field; `--catno` and `--year` flags fill
    /// `catalog` and `year` directly.
    TagsFromMb {
        query: Option<String>,
        catno: Option<String>,
        year: Option<String>,
    },
    /// Toggle the cursor row of the metadata editor between its
    /// MB-proposed value and the file's original (pre-MB) value.
    /// No-op when the row wasn't touched by MB or has been manually
    /// edited away from both endpoints.
    MbRevert,
    MbRestore,
    /// Metadata editor: open the "add new field" prompt. Colon-form
    /// of the `a` bare-char key.
    MetaAdd,
    /// Metadata editor: mark the cursor row for deletion (visible as
    /// strikethrough until save). Colon-form of `d`.
    MetaDelete,
    /// Metadata editor: un-delete the cursor row. Colon-form of `u`.
    MetaUndelete,
    /// Metadata editor: force-open the per-file detail overlay (even
    /// for non-mixed entries with multi-track dim). Colon-form of `D`.
    MetaDetail,
    /// Metadata editor: re-load per-track TITLE / ARTIST / ISRC from
    /// the sidecar `.cue` file alongside the audio. Useful when the
    /// user has edited the sidecar externally OR when the file has
    /// both an embedded CUESHEET and a sidecar and the user wants
    /// the sidecar's values to win.
    TagsCueSidecar,
    /// Metadata editor: stage deletion of the active surface's embedded
    /// CUESHEET tag and remove synthetic rows derived from it.
    CueSheetDelete,
    /// Metadata editor: edit the active surface's embedded CUESHEET in the
    /// user's system editor, validate it, write it back through lofty, and
    /// refresh derived per-track rows.
    CueSheetEdit,
    /// Return from the metadata editor to the MbSelect picker
    /// (cached release list — no MB requery). Confirmation if the
    /// editor is dirty (any edits / proposed-from-MB values not yet
    /// reverted). No-op when the editor wasn't reached through MB.
    MbBack,
    /// Return from the metadata editor to the GnudbReview surface
    /// (cached review state — no gnudb requery). Mirror of MbBack
    /// for the gnudb flow. No-op when the editor wasn't reached
    /// through gnudb.
    GnudbBack,
    /// Switch the SACD metadata editor to a specific area. The
    /// argument distinguishes stereo / mch / toggle (which flips
    /// to the area not currently shown). No-op when the editor
    /// isn't open on a SACD ISO, or when the requested area isn't
    /// present on the disc. Triggers a full editor rebuild from
    /// the parsed SacdMetadata + sidecar; unsaved edits are
    /// preserved on a best-effort basis (per-track values for
    /// fields the new area also has, by track index).
    SacdSwitchArea(SacdAreaTarget),
    /// Switch a DVD-Audio metadata editor to an explicit audio group.
    DvdaSwitchGroup(u8),
    /// Mark the current browse selection as the bit-compare reference.
    MarkCompareRef,
    /// Run bit comparison: current selection vs stored reference.
    BitCompare,
    /// Clear the stored bit-compare reference.
    ClearCompareRef,
    /// Direct comparison of two explicit paths (`:compare path1 path2`).
    ComparePaths {
        path1: String,
        path2: String,
    },
    /// Detect CD pre-emphasis on selected audio file(s).
    DetectPreemphasis,
    /// Train the pre-emphasis corpus model from a directory of non-PE audio.
    TrainPreemphCorpus {
        path: String,
    },
    /// Calibrate the pre-emphasis LDA classifier from labeled PE and non-PE directories.
    CalibratePreemphasis {
        pe_dir: String,
        non_pe_dir: String,
    },
    /// Open the search panel.
    Search {
        recursive: bool,
    },
    /// Set an archive password for the selected archive in Browse.
    Password,
    /// Open the context menu at the current selection.
    ContextMenu,
    /// AccurateRip verification on selected audio files/folder.
    /// `force`: if true, full offset scan (-1200 to +1200).
    AccurateRip {
        force: bool,
    },
    /// Apply AccurateRip offset correction.
    ArFix,
    /// Batch AccurateRip verification of current browse directory.
    ArBatch,
    /// CUETools DB verification.
    Ctdb,
    /// CUETools DB Reed-Solomon repair.
    CtdbRepair,
    /// View a text file in read-only mode.
    ViewFile(std::path::PathBuf),
    /// Edit a text file (not .log files).
    EditFile(std::path::PathBuf),
    Unknown(String),

}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::Quit => f.write_str("Quit"),
            Command::Write => f.write_str("Write"),
            Command::WriteQuit => f.write_str("WriteQuit"),
            Command::Edit(arg) => f.debug_tuple("Edit").field(arg).finish(),
            Command::Output(arg) => f.debug_tuple("Output").field(arg).finish(),
            Command::Cd(arg) => f.debug_tuple("Cd").field(arg).finish(),
            Command::Queue { preset } => f.debug_struct("Queue").field("preset", preset).finish(),
            Command::Commit { start } => f.debug_struct("Commit").field("start", start).finish(),
            Command::CommitWithSourceOptionsTransform { start, .. } => f
                .debug_struct("CommitWithSourceOptionsTransform")
                .field("start", start)
                .field("transform", &"<source-options-transform>")
                .finish(),
            Command::Maximize => f.write_str("Maximize"),
            Command::Advanced => f.write_str("Advanced"),
            Command::Go => f.write_str("Go"),
            Command::Expand => f.write_str("Expand"),
            Command::Batch(arg) => f.debug_tuple("Batch").field(arg).finish(),
            Command::Preset(arg) => f.debug_tuple("Preset").field(arg).finish(),
            Command::SaveAs(arg) => f.debug_tuple("SaveAs").field(arg).finish(),
            Command::Presets => f.write_str("Presets"),
            Command::Set(key, value) => f.debug_tuple("Set").field(key).field(value).finish(),
            Command::Fx(args) => f.debug_tuple("Fx").field(args).finish(),
            Command::Info => f.write_str("Info"),
            Command::Tools => f.write_str("Tools"),
            Command::Help => f.write_str("Help"),
            Command::Sort(field, dir) => f.debug_tuple("Sort").field(field).field(dir).finish(),
            Command::SortDir => f.write_str("SortDir"),
            Command::Filter(arg) => f.debug_tuple("Filter").field(arg).finish(),
            Command::Refresh => f.write_str("Refresh"),
            Command::Rename(arg) => f.debug_tuple("Rename").field(arg).finish(),
            Command::Delete => f.write_str("Delete"),
            Command::Copy { dest, force } => f
                .debug_struct("Copy")
                .field("dest", dest)
                .field("force", force)
                .finish(),
            Command::Move { dest, force } => f
                .debug_struct("Move")
                .field("dest", dest)
                .field("force", force)
                .finish(),
            Command::Browse => f.write_str("Browse"),
            Command::Recent => f.write_str("Recent"),
            Command::Bookmarks(arg) => f.debug_tuple("Bookmarks").field(arg).finish(),
            Command::BulkRename => f.write_str("BulkRename"),
            Command::Actions => f.write_str("Actions"),
            Command::ActionsRun(path) => f.debug_tuple("ActionsRun").field(path).finish(),
            Command::ActionsIdentityImport(path) => f
                .debug_tuple("ActionsIdentityImport")
                .field(path)
                .finish(),
            Command::Analyze { force } => f.debug_struct("Analyze").field("force", force).finish(),
            Command::WriteDr => f.write_str("WriteDr"),
            Command::WriteRgTrack => f.write_str("WriteRgTrack"),
            Command::WriteRgAlbum => f.write_str("WriteRgAlbum"),
            Command::ImportCue => f.write_str("ImportCue"),
            Command::FixCaps => f.write_str("FixCaps"),
            Command::Verify => f.write_str("Verify"),
            Command::GenerateCue { single_image } => f
                .debug_struct("GenerateCue")
                .field("single_image", single_image)
                .finish(),
            Command::GenerateCueMb { single_image } => f
                .debug_struct("GenerateCueMb")
                .field("single_image", single_image)
                .finish(),
            Command::CueFill => f.write_str("CueFill"),
            Command::CueView => f.write_str("CueView"),
            Command::CueScrollTop => f.write_str("CueScrollTop"),
            Command::CueScrollBottom => f.write_str("CueScrollBottom"),
            Command::CueEditLine(line) => f.debug_tuple("CueEditLine").field(line).finish(),
            Command::TagsFromMb { query, catno, year } => f
                .debug_struct("TagsFromMb")
                .field("query", query)
                .field("catno", catno)
                .field("year", year)
                .finish(),
            Command::MbRevert => f.write_str("MbRevert"),
            Command::MbRestore => f.write_str("MbRestore"),
            Command::MetaAdd => f.write_str("MetaAdd"),
            Command::MetaDelete => f.write_str("MetaDelete"),
            Command::MetaUndelete => f.write_str("MetaUndelete"),
            Command::MetaDetail => f.write_str("MetaDetail"),
            Command::TagsCueSidecar => f.write_str("TagsCueSidecar"),
            Command::CueSheetDelete => f.write_str("CueSheetDelete"),
            Command::CueSheetEdit => f.write_str("CueSheetEdit"),
            Command::MbBack => f.write_str("MbBack"),
            Command::GnudbBack => f.write_str("GnudbBack"),
            Command::SacdSwitchArea(target) => f.debug_tuple("SacdSwitchArea").field(target).finish(),
            Command::DvdaSwitchGroup(group) => f.debug_tuple("DvdaSwitchGroup").field(group).finish(),
            Command::MarkCompareRef => f.write_str("MarkCompareRef"),
            Command::BitCompare => f.write_str("BitCompare"),
            Command::ClearCompareRef => f.write_str("ClearCompareRef"),
            Command::ComparePaths { path1, path2 } => f
                .debug_struct("ComparePaths")
                .field("path1", path1)
                .field("path2", path2)
                .finish(),
            Command::DetectPreemphasis => f.write_str("DetectPreemphasis"),
            Command::TrainPreemphCorpus { path } => f
                .debug_struct("TrainPreemphCorpus")
                .field("path", path)
                .finish(),
            Command::CalibratePreemphasis { pe_dir, non_pe_dir } => f
                .debug_struct("CalibratePreemphasis")
                .field("pe_dir", pe_dir)
                .field("non_pe_dir", non_pe_dir)
                .finish(),
            Command::Search { recursive } => f
                .debug_struct("Search")
                .field("recursive", recursive)
                .finish(),
            Command::Password => f.write_str("Password"),
            Command::ContextMenu => f.write_str("ContextMenu"),
            Command::AccurateRip { force } => f
                .debug_struct("AccurateRip")
                .field("force", force)
                .finish(),
            Command::ArFix => f.write_str("ArFix"),
            Command::ArBatch => f.write_str("ArBatch"),
            Command::Ctdb => f.write_str("Ctdb"),
            Command::CtdbRepair => f.write_str("CtdbRepair"),
            Command::ViewFile(path) => f.debug_tuple("ViewFile").field(path).finish(),
            Command::EditFile(path) => f.debug_tuple("EditFile").field(path).finish(),
            Command::Unknown(arg) => f.debug_tuple("Unknown").field(arg).finish(),
        }
    }
}

/// Parse a command string (without the leading ':')
pub fn parse_command(input: &str) -> Command {
    let input = input.trim();
    let mut parts = input.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let args = parts.next().unwrap_or("").trim();

    match cmd {
        "q" | "quit" | "exit" => Command::Quit,
        "w" | "write" | "save" => {
            if args.is_empty() {
                Command::Write
            } else {
                Command::SaveAs(args.to_string())
            }
        }
        "wq" => Command::WriteQuit,
        // Metadata-editor actions. Route via execute_command's
        // with_editor_state helper to the corresponding helper in
        // keybindings.rs. Bare-char a/d/D/u/w keys these commands
        // replaced have been removed (no-bare-char-keys rule);
        // KeyCode::Delete remains as a convenience shortcut for :d.
        // Editor `:w` save lives in Command::Write (overlay-aware).
        // `delete` (without short alias) is taken by browse file deletion;
        // editor delete is `:d` only. `add` overlaps with bookmarks
        // sub-args but bookmarks parses its own `add` inside the
        // execute_bookmarks helper (top-level `:add` here is safe).
        "a" | "add" => Command::MetaAdd,
        "d" => Command::MetaDelete,
        "u" | "undelete" => Command::MetaUndelete,
        "D" | "detail" => Command::MetaDetail,
        "tags-cue-sidecar" | "tags-cue" => Command::TagsCueSidecar,
        "cuesheet-delete" | "cue-delete" => Command::CueSheetDelete,
        "cuesheet-edit" | "cue-edit" => Command::CueSheetEdit,
        "mb-back" | "tags-mb-back" => Command::MbBack,
        "gnudb-back" | "tags-gnudb-back" => Command::GnudbBack,
        "area" | "sacd-area" => {
            let target = match args.trim().to_ascii_lowercase().as_str() {
                "stereo" | "2ch" | "two-channel" | "2.0" => SacdAreaTarget::Stereo,
                "mch" | "mc" | "multi-channel" | "multichannel" | "5.1" => {
                    SacdAreaTarget::MultiChannel
                }
                "" | "toggle" => SacdAreaTarget::Toggle,
                other => {
                    return Command::Unknown(format!(":area unknown target '{}'", other));
                }
            };
            Command::SacdSwitchArea(target)
        }
        "dvda-group" | "dvd-audio-group" => {
            let trimmed = args.trim();
            let Ok(group_nr) = trimmed.parse::<u8>() else {
                return Command::Unknown(
                    "usage: :dvda-group <group-number>".to_string(),
                );
            };
            if group_nr == 0 {
                return Command::Unknown(
                    "usage: :dvda-group <group-number> (group numbers start at 1)".to_string(),
                );
            }
            Command::DvdaSwitchGroup(group_nr)
        }
        "e" | "edit" => {
            // `:e <N>` (positive integer) targets a line in the parked
            // CUE preview overlay. Anything else falls through to the
            // existing path-or-field handler.
            if let Ok(line) = args.trim().parse::<usize>() {
                if line >= 1 {
                    return Command::CueEditLine(line);
                }
            }
            Command::Edit(args.to_string())
        }
        "o" | "output" => Command::Output(args.to_string()),
        "cd" => Command::Cd(args.to_string()),
        "c" | "convert" | "qa" | "queue" | "qa!" | "queue!" => {
            let preset = if args.is_empty() {
                None
            } else {
                Some(args.to_string())
            };
            Command::Queue { preset }
        }
        "commit" => Command::Commit { start: false },
        "Commit" => Command::Commit { start: true },
        "max" | "maximize" => Command::Maximize,
        "adv" | "advanced" => Command::Advanced,
        "go" | "start" => Command::Go,
        "expand" | "x" => Command::Expand,
        "batch" => Command::Batch(args.to_string()),
        "preset" => Command::Preset(args.to_string()),
        "saveas" => Command::SaveAs(args.to_string()),
        "presets" => Command::Presets,
        "set" => {
            let mut set_parts = args.splitn(2, char::is_whitespace);
            let key = set_parts.next().unwrap_or("").to_string();
            let value = set_parts.next().unwrap_or("").trim().to_string();
            Command::Set(key, value)
        }
        "fx" | "effects" => {
            let fx_args: Vec<String> = args.split_whitespace().map(|s| s.to_string()).collect();
            Command::Fx(fx_args)
        }
        "info" => Command::Info,
        "tools" => Command::Tools,
        "h" | "help" => Command::Help,
        "sort" => {
            let mut sort_parts = args.split_whitespace();
            let field = sort_parts.next().map(|s| s.to_string());
            let dir = sort_parts.next().map(|s| s.to_string());
            Command::Sort(field, dir)
        }
        "sortdir" => Command::SortDir,
        "filter" => {
            let arg = if args.is_empty() {
                None
            } else {
                Some(args.to_string())
            };
            Command::Filter(arg)
        }
        "refresh" => Command::Refresh,
        "del" | "delete" => Command::Delete,
        "rename" => Command::Rename(args.to_string()),
        "cp" | "copy" => Command::Copy {
            dest: args.to_string(),
            force: false,
        },
        "cp!" | "copy!" => Command::Copy {
            dest: args.to_string(),
            force: true,
        },
        "mv" | "move" => Command::Move {
            dest: args.to_string(),
            force: false,
        },
        "mv!" | "move!" => Command::Move {
            dest: args.to_string(),
            force: true,
        },
        "browse" | "b" => Command::Browse,
        "recent" | "recents" => Command::Recent,
        "bookmarks" | "bm" => Command::Bookmarks(args.to_string()),
        "rename-all" | "renameall" | "bulk-rename" => Command::BulkRename,
        "actions" => Command::Actions,
        "actions-run" => Command::ActionsRun(
            (!args.trim().is_empty()).then(|| args.trim().to_string()),
        ),
        "actions-identity-import" => Command::ActionsIdentityImport(
            (!args.trim().is_empty()).then(|| args.trim().to_string()),
        ),
        "analyze" | "analysis" | "dr" => Command::Analyze { force: false },
        "analyze!" => Command::Analyze { force: true },
        "verify" | "test" => Command::Verify,
        "cue" => Command::GenerateCue {
            single_image: false,
        },
        "cue!" => Command::GenerateCue { single_image: true },
        "cue-mb" => Command::GenerateCueMb {
            single_image: false,
        },
        "cue-mb!" => Command::GenerateCueMb { single_image: true },
        "cue-fill" | "cue-enrich" => Command::CueFill,
        "cue-view" => Command::CueView,
        "tags-mb" | "mb-tags" | "musicbrainz-tags" => parse_tags_mb_args(args),
        "revert" => Command::MbRevert,
        "restore" => Command::MbRestore,
        "g" | "top" => Command::CueScrollTop,
        "G" | "bot" | "bottom" => Command::CueScrollBottom,
        "preemphasis" | "preemph" | "pe" => Command::DetectPreemphasis,
        "preemph-calibrate" | "pe-calibrate" => {
            let mut parts = args.splitn(2, char::is_whitespace);
            let pe_dir = parts.next().unwrap_or("").trim().to_string();
            let non_pe_dir = parts.next().unwrap_or("").trim().to_string();
            if pe_dir.is_empty() || non_pe_dir.is_empty() {
                Command::Unknown("usage: :preemph-calibrate <pe_dir> <non_pe_dir>".into())
            } else {
                Command::CalibratePreemphasis { pe_dir, non_pe_dir }
            }
        }
        "preemph-train" | "pe-train" | "train-corpus" => {
            let path = if args.is_empty() {
                dirs::home_dir()
                    .map(|h| h.join("preemph-dev").to_string_lossy().to_string())
                    .unwrap_or_else(|| "~/preemph-dev".to_string())
            } else {
                args.to_string()
            };
            Command::TrainPreemphCorpus { path }
        }
        "mark" => Command::MarkCompareRef,
        "unmark" | "clearref" => Command::ClearCompareRef,
        "compare" | "cmp" => {
            // :compare path1 path2
            let mut parts = args.splitn(2, char::is_whitespace);
            let p1 = parts.next().unwrap_or("").trim().to_string();
            let p2 = parts.next().unwrap_or("").trim().to_string();
            if p1.is_empty() {
                // No args: compare selection vs reference.
                Command::BitCompare
            } else if p2.is_empty() {
                Command::Unknown(format!("compare: need two paths (got one: {})", p1))
            } else {
                Command::ComparePaths {
                    path1: p1,
                    path2: p2,
                }
            }
        }
        "search" | "s" => Command::Search { recursive: false },
        "rs" | "rsearch" => Command::Search { recursive: true },
        "write-dr" | "writedr" => Command::WriteDr,
        "write-rg-track" => Command::WriteRgTrack,
        "write-rg-album" => Command::WriteRgAlbum,
        "import-cue" => Command::ImportCue,
        "fix-caps" | "fixcaps" => Command::FixCaps,
        "password" | "pw" => Command::Password,
        "context" | "menu" => Command::ContextMenu,
        "ar" | "accuraterip" => Command::AccurateRip { force: false },
        "ar!" | "accuraterip!" => Command::AccurateRip { force: true },
        "ar-fix" => Command::ArFix,
        "ar-batch" => Command::ArBatch,
        "ctdb" | "cuetools" => Command::Ctdb,
        "ctdb-repair" | "cuetools-repair" => Command::CtdbRepair,
        "view" | "cat" => Command::ViewFile(std::path::PathBuf::from(args)),
        "edit-file" | "ef" => Command::EditFile(std::path::PathBuf::from(args)),
        _ => Command::Unknown(input.to_string()),
    }
}

const TAGS_MB_USAGE: &str = "usage: :tags-mb [--catno VALUE] [--year YYYY] [text query]";

/// Tokenize a `:tags-mb` arg string into whitespace-separated tokens,
/// respecting double-quoted strings (no escapes). Catalog numbers
/// commonly contain spaces (`SRGS 4520`, `ESGA 509`) so `--catno
/// "SRGS 4520"` must be a single token.
fn tokenize_tags_mb_args(input: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_quote {
                    out.push(std::mem::take(&mut cur));
                    in_quote = false;
                } else {
                    in_quote = true;
                }
            }
            c if c.is_whitespace() && !in_quote => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if in_quote {
        return Err("unterminated quoted string".to_string());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// Parse the arg string for the `:tags-mb` family of commands.
/// Bare (empty args) form maps to `Command::TagsFromMb { None, None, None }`,
/// preserving today's TOC-primary-with-seed-fallback behavior.
/// Any non-empty arg returns the struct populated for direct text
/// search (TOC will be skipped at dispatch). Errors land as
/// `Command::Unknown(usage_or_specific_error)`.
fn parse_tags_mb_args(args: &str) -> Command {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Command::TagsFromMb {
            query: None,
            catno: None,
            year: None,
        };
    }
    let tokens = match tokenize_tags_mb_args(trimmed) {
        Ok(t) => t,
        Err(e) => return Command::Unknown(format!(":tags-mb: {} — {}", e, TAGS_MB_USAGE)),
    };

    let mut catno: Option<String> = None;
    let mut year: Option<String> = None;
    let mut text_parts: Vec<String> = Vec::new();
    let mut iter = tokens.into_iter();
    while let Some(tok) = iter.next() {
        match tok.as_str() {
            "--catno" => {
                let Some(val) = iter.next() else {
                    return Command::Unknown(format!(
                        ":tags-mb: --catno requires a value — {}",
                        TAGS_MB_USAGE
                    ));
                };
                if val.is_empty() {
                    return Command::Unknown(format!(
                        ":tags-mb: --catno value must be non-empty — {}",
                        TAGS_MB_USAGE
                    ));
                }
                if catno.is_some() {
                    return Command::Unknown(format!(
                        ":tags-mb: --catno specified twice — {}",
                        TAGS_MB_USAGE
                    ));
                }
                catno = Some(val);
            }
            "--year" => {
                let Some(val) = iter.next() else {
                    return Command::Unknown(format!(
                        ":tags-mb: --year requires a value — {}",
                        TAGS_MB_USAGE
                    ));
                };
                if val.is_empty() {
                    return Command::Unknown(format!(
                        ":tags-mb: --year value must be non-empty — {}",
                        TAGS_MB_USAGE
                    ));
                }
                if year.is_some() {
                    return Command::Unknown(format!(
                        ":tags-mb: --year specified twice — {}",
                        TAGS_MB_USAGE
                    ));
                }
                year = Some(val);
            }
            s if s.starts_with("--") => {
                return Command::Unknown(format!(
                    ":tags-mb: unknown flag '{}' — {}",
                    s, TAGS_MB_USAGE
                ));
            }
            _ => text_parts.push(tok),
        }
    }

    let query = if text_parts.is_empty() {
        None
    } else {
        Some(text_parts.join(" "))
    };

    if query.is_none() && catno.is_none() && year.is_none() {
        // Tokenization consumed everything (e.g. just whitespace
        // inside a quoted block). Surface as a usage error rather
        // than dispatching an empty search.
        return Command::Unknown(format!(":tags-mb: empty query — {}", TAGS_MB_USAGE));
    }

    Command::TagsFromMb { query, catno, year }
}

/// Execute a parsed command against app state
/// Run `f` on the active metadata editor state. State may be parked
/// in `pending_metadata_editor` (colon command from the command bar)
/// or live in `active_overlay` (mouse-pill click that restored before
/// dispatching). Restores state to active_overlay after `f` runs,
/// unless `f` (or something it called) set a different overlay.
fn with_editor_state<F>(app: &mut AppState, mut f: F)
where
    F: FnMut(&mut super::app::MetadataEditorState) -> Option<String>,
{
    let mut state = if let Some(parked) = app.pending_metadata_editor.take() {
        parked
    } else if matches!(
        app.active_overlay,
        super::app::ActiveOverlay::MetadataEditor(_)
    ) {
        let prev = std::mem::replace(&mut app.active_overlay, super::app::ActiveOverlay::None);
        if let super::app::ActiveOverlay::MetadataEditor(s) = prev {
            s
        } else {
            unreachable!()
        }
    } else {
        app.set_status("metadata-editor command requires the editor to be active");
        return;
    };
    let status = f(&mut state);
    if let Some(status) = status {
        app.set_status(status);
    }
    if matches!(app.active_overlay, super::app::ActiveOverlay::None) {
        app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
    }
}

/// Like `with_editor_state` but also threads `app` and `tx` into the
/// closure. Used by `:w` save which needs both.
fn with_editor_state_and_tx<F>(app: &mut AppState, tx: &mpsc::Sender<AppMessage>, mut f: F)
where
    F: FnMut(&mut AppState, &mut super::app::MetadataEditorState, &mpsc::Sender<AppMessage>),
{
    let mut state = if let Some(parked) = app.pending_metadata_editor.take() {
        parked
    } else if matches!(
        app.active_overlay,
        super::app::ActiveOverlay::MetadataEditor(_)
    ) {
        let prev = std::mem::replace(&mut app.active_overlay, super::app::ActiveOverlay::None);
        if let super::app::ActiveOverlay::MetadataEditor(s) = prev {
            s
        } else {
            unreachable!()
        }
    } else {
        app.set_status("metadata-editor command requires the editor to be active");
        return;
    };
    f(app, &mut state, tx);
    if matches!(app.active_overlay, super::app::ActiveOverlay::None) {
        app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
    }
}

fn toggle_convert_advanced(app: &mut AppState, focus: ConvertFocus) {
    if app.convert.is_collapsed(focus) {
        app.convert.layout = ConvertLayout::Maximized(focus);
    }
    app.convert.focus = focus;
    match focus {
        ConvertFocus::Source => {
            app.convert.source.advanced_open = !app.convert.source.advanced_open;
        }
        ConvertFocus::Metadata => {
            app.convert.metadata.advanced_open = !app.convert.metadata.advanced_open;
        }
        ConvertFocus::Format => {
            app.convert.format.advanced_open = !app.convert.format.advanced_open;
        }
        ConvertFocus::OutputOptions => {
            app.convert.output_options.advanced_open = !app.convert.output_options.advanced_open;
        }
    }
}

/// Status shown when a conversion-actions command is used while the feature
/// gate (`[ui] show_conversion_actions`) is off.
pub(super) const CONVERSION_ACTIONS_GATED_STATUS: &str =
    "conversion actions are gated off — set show_conversion_actions = true under [ui] in config.toml";

pub fn execute_command(app: &mut AppState, cmd: Command, tx: &mpsc::Sender<AppMessage>) {
    match cmd {
        Command::Quit => {
            // If a CUE preview is parked, :q just cancels the preview
            // rather than quitting the app.
            if app.pending_cue_preview.take().is_some() {
                app.set_status("CUE preview cancelled".to_string());
                return;
            }

            let editor = if let Some(parked) = app.pending_metadata_editor.take() {
                Some(parked)
            } else if matches!(app.active_overlay, super::app::ActiveOverlay::MetadataEditor(_)) {
                match std::mem::replace(
                    &mut app.active_overlay,
                    super::app::ActiveOverlay::None,
                ) {
                    super::app::ActiveOverlay::MetadataEditor(state) => Some(state),
                    other => {
                        app.active_overlay = other;
                        None
                    }
                }
            } else {
                None
            };
            if let Some(state) = editor {
                if state.any_presentation_dirty() {
                    app.should_quit = false;
                    app.quit_after_browse_archive_metadata_resolution = true;
                    app.pending_metadata_editor = Some(state);
                    app.active_overlay = super::app::ActiveOverlay::Confirmation {
                        message: concat!(
                            "Discard unsaved metadata changes before quitting? ",
                            "Y discards them; N/Esc returns to the editor."
                        )
                        .to_string(),
                        action: super::app::ConfirmAction::DiscardMetadataEditorChanges,
                    };
                    app.set_status(
                        "quit deferred: confirm unsaved metadata changes".to_string(),
                    );
                    return;
                }
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
            }
            app.should_quit = true;
        }
        Command::Write => {
            // If the metadata editor is parked or active, :w saves
            // tags (Phase 4 regen + per-file lofty write) via
            // metadata_editor_save (extracted helper in keybindings.rs).
            if app.pending_metadata_editor.is_some()
                || matches!(
                    app.active_overlay,
                    super::app::ActiveOverlay::MetadataEditor(_)
                )
            {
                with_editor_state_and_tx(app, tx, |app, state, tx| {
                    super::keybindings::metadata_editor_save(app, state, tx)
                });
                return;
            }
            // If a CUE preview is parked, :w writes the previewed CUE.
            if let Some(state) = app.pending_cue_preview.take() {
                if let Err(reason) = super::cue_generate::validate_cue_content(&state.content) {
                    app.pending_cue_preview = Some(state);
                    app.set_status(format!("CUE invalid, not written: {}", reason));
                    return;
                }
                let path = state.write_path.clone();
                match std::fs::write(&path, &state.content) {
                    Ok(()) => {
                        let name = path
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string());
                        app.set_status(format!("CUE written: {}", name));
                        if app.current_screen == AppScreen::Browse {
                            app.browse.refresh_with_search(Some(tx));
                            app.browse.probe_current_with_db(tx, Some(&app.db));
                        }
                    }
                    Err(e) => {
                        // Re-park so the user can retry.
                        app.pending_cue_preview = Some(state);
                        app.set_status(format!("CUE write failed: {}", e));
                    }
                }
                return;
            }
            if let Some(name) = app.preset.active_preset.clone() {
                let path = app
                    .preset
                    .active_preset_save_path()
                    .unwrap_or_else(|| super::presets::preset_file_path(&name));
                let preset = super::presets::TuiPreset::from_pill_state(
                    &name,
                    &app.convert.format,
                    &app.convert.output_options,
                    &app.convert.metadata,
                );
                match super::presets::save_preset_to_path_with_db(&preset, &path, &app.db) {
                    Ok(_) => {
                        app.preset.set_active_preset_path(name.clone(), path.clone());
                        app.preset.modified = false;
                        app.set_status(format!("Saved preset: {}", path.display()));
                    }
                    Err(e) => app.set_status(format!("Save failed: {}", e)),
                }
            } else {
                app.set_status("No active preset. Use :saveas <name>");
            }
        }
        Command::WriteQuit => {
            // If a CUE preview is parked, :wq writes the CUE then closes
            // the overlay (does not quit the app).
            if let Some(state) = app.pending_cue_preview.take() {
                if let Err(reason) = super::cue_generate::validate_cue_content(&state.content) {
                    app.pending_cue_preview = Some(state);
                    app.set_status(format!("CUE invalid, not written: {}", reason));
                    return;
                }
                let path = state.write_path.clone();
                match std::fs::write(&path, &state.content) {
                    Ok(()) => {
                        let name = path
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string());
                        app.set_status(format!("CUE written: {}", name));
                        if app.current_screen == AppScreen::Browse {
                            app.browse.refresh_with_search(Some(tx));
                            app.browse.probe_current_with_db(tx, Some(&app.db));
                        }
                    }
                    Err(e) => {
                        app.pending_cue_preview = Some(state);
                        app.set_status(format!("CUE write failed: {}", e));
                    }
                }
                return;
            }
            if let Some(name) = app.preset.active_preset.clone() {
                let path = app
                    .preset
                    .active_preset_save_path()
                    .unwrap_or_else(|| super::presets::preset_file_path(&name));
                let preset = super::presets::TuiPreset::from_pill_state(
                    &name,
                    &app.convert.format,
                    &app.convert.output_options,
                    &app.convert.metadata,
                );
                if super::presets::save_preset_to_path_with_db(&preset, &path, &app.db).is_ok() {
                    app.preset.set_active_preset_path(name, path);
                    app.preset.modified = false;
                }
            }
            app.should_quit = true;
        }
        Command::Edit(path) => {
            if path.is_empty() {
                app.set_status("Usage: :e <path>  or  :e title|artist|album|genre|year");
                return;
            }

            // Context-sensitive: on Browse with an audio file selected,
            // if the arg is a known metadata field name, open the tag
            // editor for that field instead of loading a source path.
            if app.current_screen == AppScreen::Browse {
                use crate::tui::probe::MetadataField;
                let field = match path.to_lowercase().as_str() {
                    "title" => Some(MetadataField::Title),
                    "artist" => Some(MetadataField::Artist),
                    "album" => Some(MetadataField::Album),
                    "genre" => Some(MetadataField::Genre),
                    "year" => Some(MetadataField::Year),
                    _ => None,
                };
                if let Some(field) = field {
                    execute_edit_metadata(app, field);
                    return;
                }
            }

            let expanded = expand_path(&path);
            let p = PathBuf::from(&expanded);
            if !p.exists() {
                app.set_status(format!("Path not found: {}", expanded));
                return;
            }
            // Install the source immediately and complete heavyweight source
            // discovery on a background worker. Archives use the Phase 3
            // extract+discover+probe preview path; ordinary audio/disc sources
            // use the existing async probe path.
            if is_nonprobeable_source_for_probe(&p) {
                install_archive_preview_convert_source(app, p, tx.clone());
                return;
            }

            app.probe_generation = app.probe_generation.saturating_add(1);
            let generation = app.probe_generation;
            let probe_notice = source_probe_initial_notice(&p);

            clear_source_metadata_in_convert(&mut app.convert);
            app.convert.set_source_mode(SourceMode::from_single_pending_probe(
                p.clone(),
                probe_notice.clone(),
            ));
            app.convert.apply_source_defaults();
            let probe_baseline = ConvertProbeBaseline::capture(&app.convert);
            app.current_screen = AppScreen::Convert;
            app.recent.record_use_with_db(&p, &app.db);

            if probe_notice.is_some() {
                spawn_convert_source_probe(generation, p.clone(), probe_baseline, tx.clone());
                app.set_status(format!(
                    "Probing: {}",
                    p.file_name().unwrap_or_default().to_string_lossy()
                ));
            } else {
                app.set_status(format!(
                    "Loaded: {}",
                    p.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
        Command::Output(path) => {
            if path.is_empty() {
                app.set_status("Usage: :o <path>");
                return;
            }
            let expanded = expand_path(&path);
            app.convert.output_options.dest_path = Some(PathBuf::from(&expanded));
            app.set_status(format!("Output destination: {}", expanded));
        }
        Command::Cd(path) => {
            if app.current_screen != AppScreen::Browse {
                app.set_status(":cd only works on the browse screen");
                return;
            }
            if path.is_empty() {
                app.set_status("Usage: :cd <path>");
                return;
            }
            match app.browse.navigate_to_str(&path) {
                Ok(()) => {
                    // If async, the status + probe happen in the
                    // PathValidationComplete / DirScanComplete handlers.
                    // If sync fallback, current_dir is already updated.
                    if !app.browse.is_async_enabled() {
                        let p = app.browse.current_dir.display().to_string();
                        app.set_status(format!("cd: {}", p));
                        app.browse.probe_current_with_db(tx, Some(&app.db));
                    } else {
                        app.set_status(format!("Resolving: {}", path));
                    }
                }
                Err(e) => app.set_status(format!("cd: {}", e)),
            }
        }
        Command::Queue { preset } => {
            execute_queue(app, tx, preset);
        }
        Command::Commit { start } => {
            if matches!(
                &app.convert.source.mode,
                SourceMode::MultiTrack {
                    selected_presentation_id: Some(_),
                    ..
                }
            ) {
                execute_commit_with_disc_selection_bridge(app, start, tx);
            } else {
                execute_commit(app, tx, start);
            }
        }
        Command::CommitWithSourceOptionsTransform { start, transform } => {
            execute_commit_with_source_options_transform(app, tx, start, Some(transform));
        }
        Command::Maximize => {
            if app.current_screen == AppScreen::Convert {
                app.convert.toggle_maximize(app.convert.focus);
            } else {
                app.set_status(":max only works on the Convert screen");
            }
        }
        Command::Advanced => {
            if app.current_screen == AppScreen::Convert {
                toggle_convert_advanced(app, app.convert.focus);
            } else {
                app.set_status(":adv only works on the Convert screen");
            }
        }
        Command::Go => {
            execute_go(app, tx);
        }
        Command::Expand => {
            execute_expand(app);
        }
        Command::Batch(dir) => {
            if dir.is_empty() {
                app.set_status("Usage: :batch <directory>");
            } else {
                app.set_status(format!("Batch scan not yet implemented: {}", dir));
            }
        }
        Command::Preset(name) => {
            if name.is_empty() {
                app.set_status("Usage: :preset <name>");
            } else {
                match super::presets::load_preset(&name) {
                    Ok(preset) => {
                        let report = preset.apply_to_pills(
                            &mut app.convert.format,
                            &mut app.convert.output_options,
                            &mut app.convert.metadata,
                        );
                        app.preset
                            .set_active_preset_path(name.clone(), super::presets::preset_file_path(&name));
                        app.preset.modified = false;
                        app.set_status(format!(
                            "Loaded preset: {}{}",
                            name,
                            report.status_suffix()
                        ));
                    }
                    Err(e) => app.set_status(format!("Load failed: {}", e)),
                }
            }
        }
        Command::SaveAs(name) => {
            if name.is_empty() {
                app.set_status("Usage: :saveas <name>");
            } else {
                let preset = super::presets::TuiPreset::from_pill_state(
                    &name,
                    &app.convert.format,
                    &app.convert.output_options,
                    &app.convert.metadata,
                );
                let path = super::presets::preset_file_path(&name);
                match super::presets::save_preset_to_path_with_db(&preset, &path, &app.db) {
                    Ok(_) => {
                        app.preset.set_active_preset_path(name.clone(), path.clone());
                        app.preset.modified = false;
                        app.set_status(format!("Saved preset: {}", path.display()));
                    }
                    Err(e) => app.set_status(format!("Save failed: {}", e)),
                }
            }
        }
        Command::Presets => {
            let names = super::presets::list_presets();
            if names.is_empty() {
                app.set_status("No presets saved. Use :saveas <name>");
            } else {
                app.set_status(format!("Presets: {}", names.join(", ")));
            }
        }
        Command::Set(key, value) => {
            execute_set(app, &key, &value);
        }
        Command::Fx(args) => {
            if args.is_empty() || (args.len() == 1 && args[0] == "list") {
                app.set_status("Effects chains not yet implemented");
            } else {
                app.set_status("Effects: not yet implemented");
            }
        }
        Command::Info => {
            if let Some(info) = app.convert.source.mode.current_info() {
                app.set_status(format!(
                    "{} | {} | {} | {}",
                    info.format_name,
                    info.codec_display(),
                    info.sample_rate_display(),
                    info.channels_display(),
                ));
            } else {
                app.set_status("No source file loaded. Use :e <path>");
            }
        }
        Command::Tools => {
            app.current_screen = AppScreen::Config;
            app.set_status("Showing config/tools");
        }
        Command::Help => {
            app.active_overlay = ActiveOverlay::Help {
                screen: app.current_screen,
                scroll: 0,
            };
        }
        Command::Sort(field, dir) => {
            execute_sort(app, field.as_deref(), dir.as_deref(), tx);
        }
        Command::SortDir => {
            if app.current_screen != AppScreen::Browse {
                app.set_status(":sortdir only works on the browse screen");
                return;
            }
            app.browse.toggle_sort_dir_with_search(Some(tx));
            let msg = format!(
                "Sort: {} {}",
                app.browse.sort_by.label(),
                app.browse.sort_dir.label()
            );
            app.set_status(msg);
        }
        Command::Filter(arg) => {
            execute_filter(app, arg.as_deref(), tx);
        }
        Command::Refresh => {
            if app.current_screen == AppScreen::Browse {
                app.browse.refresh_with_search(Some(tx));
                app.browse.probe_current_with_db(tx, Some(&app.db));
                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
                app.set_status("browse refreshed");
            } else {
                app.set_status(":refresh is available on the browse screen");
            }
        }
        Command::Delete => {
            execute_delete(app, tx);
        }
        Command::Rename(new_name) => {
            execute_rename(app, &new_name, tx);
        }
        Command::Copy { dest, force } => {
            execute_file_op(app, &dest, force, false, tx);
        }
        Command::Move { dest, force } => {
            execute_file_op(app, &dest, force, true, tx);
        }
        Command::Browse => {
            // If invoked from the convert screen, set return_target so the
            // selected file loads back into the source pane. From any other
            // screen, just switch to browse without a return target.
            if app.current_screen == AppScreen::Convert {
                app.browse.return_target = super::browse::BrowseReturnTarget::ConvertSource;
            }
            app.current_screen = AppScreen::Browse;
            app.browse.probe_current_with_db(tx, Some(&app.db));
        }
        Command::Recent => {
            app.recent.open_overlay();
        }
        Command::Bookmarks(args) => {
            execute_bookmarks(app, &args);
        }
        Command::Password => {
            if app.current_screen != AppScreen::Browse {
                app.set_status(":password only works on the browse screen");
            } else if let Some(entry) = app.browse.selected_entry() {
                if matches!(entry.kind, crate::convert::classify::EntryKind::Archive) {
                    let path = entry.path.clone();
                    app.active_overlay = ActiveOverlay::TextEdit {
                        input: super::text_input::TextInputState::empty(),
                        target: TextEditTarget::ArchivePassword(path),
                        label: "archive password".to_string(),
                    };
                } else {
                    app.set_status("Selected file is not an archive");
                }
            } else {
                app.set_status("No file selected");
            }
        }
        Command::Analyze { force } => {
            let guard_paths = current_bulk_guard_paths(app);
            if maybe_confirm_bulk_operation(
                app,
                BulkOperationKind::Analyze,
                BulkGuardCommand::Analyze { force },
                &guard_paths,
            ) {
                return;
            }

            // Collect paths to analyze from the current context.
            // On Browse, directories are expanded recursively to find
            // nested audio files (e.g., disc 01/disc 02 folders).
            let mut paths: Vec<std::path::PathBuf> = current_audio_paths(app, true);
            // Sort by disc/track for logical result order.
            super::probe::sort_paths_by_track(&mut paths);
            // Check for single-image CUE layout.
            if paths.len() <= 1 {
                let dir = if paths.is_empty() {
                    let sel = collect_selection_for_file_ops(app);
                    sel.first().and_then(|p| {
                        if p.is_dir() {
                            Some(p.clone())
                        } else {
                            p.parent().map(|d| d.to_path_buf())
                        }
                    })
                } else {
                    paths[0].parent().map(|d| d.to_path_buf())
                };
                if let Some(ref dir) = dir {
                    if let Some(info) = super::cue_parser::detect_single_image(dir) {
                        let n = info.track_boundaries.len();
                        let can_seek = super::cue_parser::can_ffmpeg_read(&info.audio_path);
                        app.set_status(format!("Analyzing {} tracks (single image)...", n,));
                        app.analysis_results.clear();
                        app.analysis_pending = n;

                        // Build display names from CUE metadata.
                        let display_paths: Vec<std::path::PathBuf> = info
                            .sheet
                            .tracks
                            .iter()
                            .map(|t| {
                                let name = format!(
                                    "{:02} - {}.flac",
                                    t.number,
                                    t.title.as_deref().unwrap_or("Track"),
                                );
                                info.audio_path
                                    .parent()
                                    .unwrap_or(std::path::Path::new("."))
                                    .join(name)
                            })
                            .collect();

                        if can_seek {
                            // Fast path: seek-based analysis (no temp files).
                            for (i, &(start, count)) in info.track_boundaries.iter().enumerate() {
                                let audio_path = info.audio_path.clone();
                                let display_path = display_paths[i].clone();
                                let original_path = info.audio_path.clone();
                                let tx = tx.clone();
                                tokio::spawn(async move {
                                    let pcm_path = audio_path.clone();

                                    let seek = start as f64 / info.sample_rate as f64;
                                    let dur = count as f64 / info.sample_rate as f64;
                                    let (pcm_result, hdcd_result) = tokio::join!(
                                        tokio::task::spawn_blocking(move || {
                                            super::analyze::analyze_file(
                                                &pcm_path,
                                                Some(start),
                                                Some(count),
                                            )
                                        }),
                                        super::analyze::detect_hdcd(
                                            &audio_path,
                                            Some(seek),
                                            Some(dur)
                                        ),
                                    );
                                    // LUFS: skip for seek-based (loudgain needs a real file per track).

                                    let final_result = match pcm_result {
                                        Ok(Ok(mut result)) => {
                                            result.path = display_path;

                                            let pe_result = super::preemphasis::detect_preemphasis_metadata_catalog(
                                                original_path.clone(),
                                            );
                                            result.preemphasis = Some(pe_result.confidence);
                                            result.preemphasis_detail = if pe_result.detail.is_empty() {
                                                None
                                            } else {
                                                Some(pe_result.detail)
                                            };

                                            if result.declared_bit_depth == Some(16)
                                                || (result.declared_bit_depth.is_none()
                                                    && result.actual_bit_depth <= 16)
                                            {
                                                if let Some(hdcd) = hdcd_result {
                                                    result.hdcd_detected = Some(hdcd.detected);
                                                    if hdcd.detected {
                                                        result.hdcd_detail = Some(hdcd.detail);
                                                    }
                                                }
                                            }

                                            Ok(Box::new(result))
                                        }
                                        Ok(Err(e)) => Err(format!("track {}: {}", i + 1, e)),
                                        Err(e) => Err(format!("task panicked: {}", e)),
                                    };
                                    let _ = tx
                                        .send(AppMessage::AnalysisComplete {
                                            result: final_result,
                                        })
                                        .await;
                                });
                            }
                        } else {
                            // Slow path: extract to temp files (WavPack v4, etc.).
                            let tmp_dir = std::env::temp_dir()
                                .join(format!("tonepoet-analyze-{}", std::process::id()));
                            app.analysis_temp_dir = Some(tmp_dir.clone());
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                if let Err(e) = std::fs::create_dir_all(&tmp_dir) {
                                    for _ in 0..n {
                                        let _ = tx
                                            .send(AppMessage::AnalysisComplete {
                                                result: Err(format!("temp dir failed: {}", e)),
                                            })
                                            .await;
                                    }
                                    return;
                                }
                                let track_paths = match tokio::task::spawn_blocking({
                                    let info = info.clone();
                                    let tmp_dir = tmp_dir.clone();
                                    move || {
                                        super::cue_parser::extract_single_image_tracks(
                                            &info, &tmp_dir,
                                        )
                                    }
                                })
                                .await
                                {
                                    Ok(Ok(paths)) => paths,
                                    result => {
                                        let msg = match result {
                                            Ok(Err(e)) => format!("extraction failed: {}", e),
                                            Err(e) => format!("extraction task failed: {}", e),
                                            _ => unreachable!(),
                                        };
                                        for _ in 0..n {
                                            let _ = tx
                                                .send(AppMessage::AnalysisComplete {
                                                    result: Err(msg.clone()),
                                                })
                                                .await;
                                        }
                                        return;
                                    }
                                };
                                for (i, temp_path) in track_paths.into_iter().enumerate() {
                                    let display_path = display_paths[i].clone();
                                    let original_path = info.audio_path.clone();
                                    let tx = tx.clone();
                                    tokio::spawn(async move {
                                        let pcm_path = temp_path.clone();
                                        let lufs_path = temp_path.clone();
                                        let hdcd_path = temp_path;
                                        let (pcm_result, lufs_result, hdcd_result) = tokio::join!(
                                            tokio::task::spawn_blocking(move || {
                                                super::analyze::analyze_file(&pcm_path, None, None)
                                            }),
                                            super::analyze::measure_loudness(&lufs_path),
                                            super::analyze::detect_hdcd(&hdcd_path, None, None),
                                        );
                                        let final_result = match pcm_result {
                                            Ok(Ok(mut result)) => {
                                                result.path = display_path;
                                                if let Some((lufs, tp)) = lufs_result {
                                                    result.lufs = Some(lufs);
                                                    result.true_peak_dbtp = Some(tp);
                                                }
                                                let pe_result = super::preemphasis::detect_preemphasis_metadata_catalog(
                                                    original_path.clone(),
                                                );
                                                result.preemphasis = Some(pe_result.confidence);
                                                result.preemphasis_detail = if pe_result.detail.is_empty() {
                                                    None
                                                } else {
                                                    Some(pe_result.detail)
                                                };
                                                if result.declared_bit_depth == Some(16)
                                                    || (result.declared_bit_depth.is_none()
                                                        && result.actual_bit_depth <= 16)
                                                {
                                                    if let Some(hdcd) = hdcd_result {
                                                        result.hdcd_detected = Some(hdcd.detected);
                                                        if hdcd.detected {
                                                            result.hdcd_detail = Some(hdcd.detail);
                                                        }
                                                    }
                                                }
                                                Ok(Box::new(result))
                                            }
                                            Ok(Err(e)) => Err(format!("track {}: {}", i + 1, e)),
                                            Err(e) => Err(format!("task panicked: {}", e)),
                                        };
                                        let _ = tx
                                            .send(AppMessage::AnalysisComplete {
                                                result: final_result,
                                            })
                                            .await;
                                    });
                                }
                            });
                        }
                        return;
                    }
                }
            }
            if paths.is_empty() {
                app.set_status("No audio files to analyze");
            } else {
                app.analysis_results.clear();

                // Check DB cache for each path; only spawn analysis for misses.
                // :analyze! skips the cache entirely.
                let mut to_analyze = Vec::new();
                if force {
                    to_analyze = paths.clone();
                } else {
                    for path in &paths {
                        let cached = std::fs::metadata(path).ok().and_then(|meta| {
                            let mtime = meta
                                .modified()
                                .map(crate::db::systemtime_to_unix)
                                .unwrap_or(0);
                            app.db.get_cached_analysis(
                                &path.display().to_string(),
                                mtime,
                                meta.len(),
                            )
                        });
                        if let Some(result) = cached {
                            app.analysis_results.push(result);
                        } else {
                            to_analyze.push(path.clone());
                        }
                    }
                }

                if to_analyze.is_empty() {
                    // All results served from cache — show overlay immediately.
                    let count = app.analysis_results.len();
                    let last = &app.analysis_results[count - 1];
                    let name = last
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    app.set_status(format!(
                        "Analyzed: {} — DR{} ({}) [cached]",
                        name,
                        last.dr_value,
                        super::analyze::dr_label(last.dr_value),
                    ));
                    app.active_overlay = super::app::ActiveOverlay::Analysis { scroll: 0 };
                } else {
                    app.analysis_pending = to_analyze.len();
                    for path in to_analyze {
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            let pcm_path = path.clone();
                            let lufs_path = path.clone();
                            let hdcd_path = path;

                            // Run PCM analysis, loudgain, and HDCD detection in parallel.
                            let (pcm_result, lufs_result, hdcd_result) = tokio::join!(
                                tokio::task::spawn_blocking(move || {
                                    super::analyze::analyze_file(&pcm_path, None, None)
                                }),
                                super::analyze::measure_loudness(&lufs_path),
                                super::analyze::detect_hdcd(&hdcd_path, None, None),
                            );

                            // Merge results and send (always send to decrement pending counter).
                            let final_result = match pcm_result {
                                Ok(Ok(mut result)) => {
                                    if let Some((lufs, tp)) = lufs_result {
                                        result.lufs = Some(lufs);
                                        result.true_peak_dbtp = Some(tp);
                                    }

                                    // Fast Phase 2-safe pre-emphasis detection (metadata/CUE PRE flag + catalog only; no spectral analysis).
                                    let pe_result = super::preemphasis::detect_preemphasis_metadata_catalog(
                                        result.path.clone(),
                                    );
                                    result.preemphasis = Some(pe_result.confidence);
                                    result.preemphasis_detail = if pe_result.detail.is_empty() {
                                        None
                                    } else {
                                        Some(pe_result.detail)
                                    };

                                    // HDCD detection (only meaningful for 16-bit sources).
                                    if result.declared_bit_depth == Some(16)
                                        || (result.declared_bit_depth.is_none()
                                            && result.actual_bit_depth <= 16)
                                    {
                                        if let Some(hdcd) = hdcd_result {
                                            result.hdcd_detected = Some(hdcd.detected);
                                            if hdcd.detected {
                                                result.hdcd_detail = Some(hdcd.detail);
                                            }
                                        }
                                    }

                                    Ok(Box::new(result))
                                }
                                Ok(Err(e)) => {
                                    let name = lufs_path
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    Err(format!("{}: {}", name, e))
                                }
                                Err(e) => Err(format!("task panicked: {}", e)),
                            };
                            let _ = tx
                                .send(AppMessage::AnalysisComplete {
                                    result: final_result,
                                })
                                .await;
                        });
                    }
                }
            }
        }
        Command::Verify => {
            let guard_paths = current_bulk_guard_paths(app);
            if maybe_confirm_bulk_operation(
                app,
                BulkOperationKind::VerifyIntegrity,
                BulkGuardCommand::Verify,
                &guard_paths,
            ) {
                return;
            }

            let paths: Vec<std::path::PathBuf> = current_audio_paths(app, true);
            if paths.is_empty() {
                app.set_status("No audio files to verify");
            } else {
                app.verify_results.clear();
                app.verify_pending = paths.len();
                for path in paths {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let result = super::verify::verify_file(path).await;
                        let _ = tx.send(AppMessage::VerifyComplete { result }).await;
                    });
                }
                app.set_status(format!("Verifying {} file(s)...", app.verify_pending));
            }
        }
        Command::GenerateCue { single_image } => {
            let mut paths: Vec<std::path::PathBuf> = match app.current_screen {
                AppScreen::Browse => {
                    let sel = collect_selection_for_file_ops(app);
                    crate::convert::queue_expansion::expand_paths_to_all_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                crate::convert::classify::classify_file(p),
                                crate::convert::classify::EntryKind::AudioFile(_)
                            )
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            if paths.is_empty() {
                app.set_status("No audio files for CUE generation");
            } else {
                super::probe::sort_paths_by_track(&mut paths);

                // CUE file goes in the root of the selection. When a
                // directory was selected, use that directory; when individual
                // files were selected, use their parent.
                let output_dir = if app.current_screen == AppScreen::Browse {
                    let sel = collect_selection_for_file_ops(app);
                    if sel.len() == 1 && sel[0].is_dir() {
                        sel[0].clone()
                    } else {
                        paths[0]
                            .parent()
                            .unwrap_or_else(|| std::path::Path::new("."))
                            .to_path_buf()
                    }
                } else {
                    paths[0]
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .to_path_buf()
                };

                let refresh_browse = app.current_screen == AppScreen::Browse;
                let tx = tx.clone();
                app.set_status("CUE generation: probing selected files…".to_string());
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
                        let (album, tracks) = super::cue_generate::gather_cue_info_blocking(&paths, &output_dir)?;
                        let pregap_count = tracks.iter().filter(|t| t.pregap_frames.is_some()).count();
                        let cue_content = if single_image {
                            let image_name = super::cue_generate::derive_image_filename(&album, &paths[0]);
                            let ext = paths[0]
                                .extension()
                                .and_then(|e| e.to_str())
                                .unwrap_or("flac");
                            let fmt = super::cue_generate::cue_format_tag(ext);
                            super::cue_generate::generate_single_image_cue(
                                &album,
                                &tracks,
                                &image_name,
                                fmt,
                            )
                        } else {
                            super::cue_generate::generate_multifile_cue(&album, &tracks)
                        };

                        let cue_filename = super::cue_generate::cue_output_filename(&album);
                        let cue_path = output_dir.join(&cue_filename);
                        std::fs::write(&cue_path, &cue_content)
                            .map_err(|e| format!("CUE write failed: {}", e))?;

                        let mode = if single_image {
                            "single image"
                        } else {
                            "multi-file"
                        };
                        let pregap_note = if pregap_count > 0 {
                            format!(
                                ", with {} EAC pregap{}",
                                pregap_count,
                                if pregap_count == 1 { "" } else { "s" }
                            )
                        } else {
                            String::new()
                        };
                        Ok(format!(
                            "CUE sheet ({}{}) written: {}",
                            mode, pregap_note, cue_filename,
                        ))
                    })
                    .await
                    .unwrap_or_else(|err| Err(format!("CUE generation failed: {}", err)));

                    let _ = tx
                        .send(AppMessage::CueWriteComplete {
                            result,
                            refresh_browse,
                        })
                        .await;
                });
            }
        }
        Command::GenerateCueMb { single_image } => {
            let mut paths: Vec<std::path::PathBuf> = match app.current_screen {
                AppScreen::Browse => {
                    let sel = collect_selection_for_file_ops(app);
                    crate::convert::queue_expansion::expand_paths_to_all_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                crate::convert::classify::classify_file(p),
                                crate::convert::classify::EntryKind::AudioFile(_)
                            )
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            if paths.is_empty() {
                app.set_status("No audio files for MusicBrainz CUE generation");
            } else {
                super::probe::sort_paths_by_track(&mut paths);
                let output_dir = if app.current_screen == AppScreen::Browse {
                    let sel = collect_selection_for_file_ops(app);
                    if sel.len() == 1 && sel[0].is_dir() {
                        sel[0].clone()
                    } else {
                        paths[0]
                            .parent()
                            .unwrap_or_else(|| std::path::Path::new("."))
                            .to_path_buf()
                    }
                } else {
                    paths[0]
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .to_path_buf()
                };

                // Compute TOC sectors (with 150 leadin). Try a colocated
                // EAC log / single-image CUE first, fall back to deriving
                // from sample counts of the selected tracks.
                let sectors: Vec<u32> = match super::accuraterip::find_toc_offsets(&output_dir) {
                    Some(s) => s,
                    None => match super::accuraterip::collect_sample_counts(&paths) {
                        Ok((sample_counts, sample_rate)) => {
                            let samples_per_frame = (sample_rate / 75) as u64;
                            let mut sectors = Vec::with_capacity(sample_counts.len() + 1);
                            let mut frame: u64 = 150;
                            for &count in &sample_counts {
                                sectors.push(frame as u32);
                                frame += count / samples_per_frame;
                            }
                            sectors.push(frame as u32);
                            sectors
                        }
                        Err(e) => {
                            app.set_status(format!("MusicBrainz CUE: {}", e));
                            return;
                        }
                    },
                };

                let toc_string = match super::musicbrainz::build_mb_toc(&sectors) {
                    Some(s) => s,
                    None => {
                        app.set_status("MusicBrainz CUE: TOC too short".to_string());
                        return;
                    }
                };
                let cached = app.db.get_cached_mb_response(&toc_string);
                let n_cached = if cached.is_some() {
                    "cached"
                } else {
                    "fetching"
                };
                app.set_status(format!(
                    "MusicBrainz CUE: {} disc TOC ({} tracks)…",
                    n_cached,
                    sectors.len() - 1,
                ));

                let tx = tx.clone();
                let toc_for_msg = toc_string.clone();
                let paths_for_msg = paths.clone();
                let output_dir_for_msg = output_dir.clone();
                tokio::spawn(async move {
                    let outcome = super::musicbrainz::lookup_release_by_toc(&sectors, cached).await;
                    let _ = tx
                        .send(AppMessage::CueMbComplete {
                            outcome,
                            paths: paths_for_msg,
                            output_dir: output_dir_for_msg,
                            single_image,
                            toc_string: toc_for_msg,
                        })
                        .await;
                });
            }
        }
        Command::MbRevert => {
            let Some(mut state) = app.pending_metadata_editor.take() else {
                app.set_status(":revert only works in the metadata editor");
                return;
            };
            // In the detail overlay, :revert is a field-level toggle
            // (operates on per_file_values vs the MB-proposed set).
            // Outside, it's the cursor-row value-based toggle.
            let in_detail = state.phase == super::app::MetadataEditorPhase::DetailEdit;
            let target_idx = if in_detail {
                state.detail_field_idx
            } else {
                state.cursor
            };
            if let Some(entry) = state.active_surface_mut().entries.get_mut(target_idx) {
                if in_detail {
                    let pill_before = super::probe::mb_pill_state_field(entry);
                    if matches!(pill_before, super::probe::MbRevertPill::None) {
                        app.set_status(
                            ":revert: field was not changed by MusicBrainz, or has manual edits",
                        );
                    } else {
                        super::probe::toggle_mb_revert_field(entry);
                        let after = super::probe::mb_pill_state_field(entry);
                        let display_key = entry.display_key.clone();
                        state.recompute_active_dirty();
                        let label = match after {
                            super::probe::MbRevertPill::Revert => "applied MB values",
                            super::probe::MbRevertPill::UseMb => "reverted to file values",
                            super::probe::MbRevertPill::None => "no change",
                        };
                        app.set_status(format!(":revert: {} ({})", display_key, label));
                    }
                } else {
                    let pill_before = super::probe::mb_pill_state(entry);
                    if matches!(pill_before, super::probe::MbRevertPill::None) {
                        app.set_status(":revert: cursor row was not changed by MusicBrainz");
                    } else {
                        super::probe::toggle_mb_revert(entry);
                        let after = super::probe::mb_pill_state(entry);
                        let display_key = entry.display_key.clone();
                        state.recompute_active_dirty();
                        let label = match after {
                            super::probe::MbRevertPill::Revert => "applied MB value",
                            super::probe::MbRevertPill::UseMb => "reverted to file value",
                            super::probe::MbRevertPill::None => "no change",
                        };
                        app.set_status(format!(":revert: {} ({})", display_key, label));
                    }
                }
            }
            app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
        }
        Command::MbRestore => {
            let Some(mut state) = app.pending_metadata_editor.take() else {
                app.set_status(":restore only works in the metadata editor");
                return;
            };
            // Field-target: in the detail overlay we use detail_field_idx;
            // in the main editor we use the cursor row, so the user can
            // restore a non-mixed row that doesn't surface the bulk pill.
            let in_detail = state.phase == super::app::MetadataEditorPhase::DetailEdit;
            let target_idx = if in_detail {
                state.detail_field_idx
            } else {
                state.cursor
            };
            if let Some(entry) = state.active_surface_mut().entries.get_mut(target_idx) {
                if !super::probe::entry_has_mb_proposed(entry) {
                    app.set_status(":restore: field was not populated from MusicBrainz");
                } else {
                    super::probe::restore_mb_proposed(entry);
                    let display_key = entry.display_key.clone();
                    state.recompute_active_dirty();
                    app.set_status(format!(":restore: {} (snapped to MB values)", display_key));
                }
            }
            app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
        }
        Command::MetaAdd => {
            with_editor_state(app, |state| {
                super::keybindings::metadata_editor_open_add(state)
            });
        }
        Command::MetaDelete => {
            let mut state = if let Some(parked) = app.pending_metadata_editor.take() {
                parked
            } else if matches!(
                app.active_overlay,
                super::app::ActiveOverlay::MetadataEditor(_)
            ) {
                let prev = std::mem::replace(&mut app.active_overlay, super::app::ActiveOverlay::None);
                if let super::app::ActiveOverlay::MetadataEditor(s) = prev {
                    s
                } else {
                    unreachable!()
                }
            } else {
                app.set_status("metadata-editor command requires the editor to be active");
                return;
            };

            if super::keybindings::metadata_editor_delete_cursor_requires_embedded_cuesheet_confirmation(&state) {
                super::keybindings::open_embedded_cuesheet_delete_confirmation(app, state);
            } else {
                if let Some(status) = super::keybindings::metadata_editor_delete_cursor(&mut state) {
                    app.set_status(status);
                }
                if matches!(app.active_overlay, super::app::ActiveOverlay::None) {
                    app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                }
            }
        }
        Command::MetaUndelete => {
            with_editor_state(app, |state| {
                super::keybindings::metadata_editor_undelete_cursor(state);
                None
            });
        }
        Command::MetaDetail => {
            with_editor_state(app, |state| super::keybindings::metadata_editor_open_detail(state));
        }
        Command::MbBack => {
            let Some(taken) = super::event_loop::take_metadata_editor_with_restore_slot(app) else {
                app.set_status(":mb-back only works in the metadata editor");
                return;
            };
            let Some(cache) = taken.state.mb_back.clone() else {
                app.set_status(
                    ":mb-back: no MB lookup to return to (run :tags-mb first)".to_string(),
                );
                super::event_loop::restore_taken_metadata_editor(app, taken);
                return;
            };

            // Keep the exact post-apply editor (including unsaved edits) parked
            // behind the rebuilt picker. Every picker cancel path restores it;
            // accepting another release is guarded against any intervening
            // save on any presentation tab.
            let editor_session = metadata_editor_session_guard(&taken.state);
            app.pending_metadata_editor = Some(taken.state);
            let mut mb_state = super::app::MbSelectState::new_with_editor_session(
                cache.releases,
                cache.paths,
                Some(editor_session),
            );
            mb_state.selected = cache.selected;
            app.active_overlay = super::app::ActiveOverlay::MbSelect(Box::new(mb_state));
            app.set_status(":mb-back: pick a different release".to_string());
        }
        Command::GnudbBack => {
            // Mirror of Command::MbBack for the gnudb flow.
            let state = if let Some(parked) = app.pending_metadata_editor.take() {
                parked
            } else if matches!(
                app.active_overlay,
                super::app::ActiveOverlay::MetadataEditor(_)
            ) {
                let prev =
                    std::mem::replace(&mut app.active_overlay, super::app::ActiveOverlay::None);
                if let super::app::ActiveOverlay::MetadataEditor(s) = prev {
                    s
                } else {
                    unreachable!()
                }
            } else {
                app.set_status(":gnudb-back only works in the metadata editor");
                return;
            };
            let Some(review) = state.gnudb_back.clone() else {
                app.set_status(
                    ":gnudb-back: no gnudb review to return to (run :tags-gnudb first)".to_string(),
                );
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                return;
            };
            if state.any_presentation_dirty() {
                app.pending_metadata_editor = Some(state);
                app.active_overlay = super::app::ActiveOverlay::Confirmation {
                    action: super::app::ConfirmAction::GnudbBack(review),
                    message: "Discard editor changes and return to gnudb review?".to_string(),
                };
                return;
            }
            drop(state);
            app.active_overlay = super::app::ActiveOverlay::GnudbReview(review);
            app.set_status(":gnudb-back: review per-track values".to_string());
        }
        Command::SacdSwitchArea(target) => {
            let mut state = if let Some(parked) = app.pending_metadata_editor.take() {
                parked
            } else if matches!(
                app.active_overlay,
                super::app::ActiveOverlay::MetadataEditor(_)
            ) {
                let prev =
                    std::mem::replace(&mut app.active_overlay, super::app::ActiveOverlay::None);
                if let super::app::ActiveOverlay::MetadataEditor(s) = prev {
                    s
                } else {
                    unreachable!()
                }
            } else {
                app.set_status(":area only works in the metadata editor");
                return;
            };
            // SACD only.
            let Some(_) = state.active_surface().sacd_area_kind else {
                app.set_status(":area: editor is not on a SACD ISO");
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                return;
            };
            let iso_path = match state.active_surface().paths.first().cloned() {
                Some(p) => p,
                None => {
                    app.set_status(":area: no source path on editor state");
                    app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                    return;
                }
            };
            if state.active_surface().dirty {
                app.set_status(
                    ":area: editor has unsaved edits — save (:w) or discard (:q!) first",
                );
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                return;
            }
            match super::keybindings::switch_sacd_editor_area(&mut state, &iso_path, target) {
                Ok(label) => app.set_status(format!(":area → {}", label)),
                Err(reason) => app.set_status(reason),
            }
            app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
        }
        Command::DvdaSwitchGroup(group_nr) => {
            let mut state = if let Some(parked) = app.pending_metadata_editor.take() {
                parked
            } else if matches!(
                app.active_overlay,
                super::app::ActiveOverlay::MetadataEditor(_)
            ) {
                let prev =
                    std::mem::replace(&mut app.active_overlay, super::app::ActiveOverlay::None);
                if let super::app::ActiveOverlay::MetadataEditor(s) = prev {
                    s
                } else {
                    unreachable!()
                }
            } else {
                app.set_status(":dvda-group only works in the metadata editor");
                return;
            };
            let source_path = match state.active_surface().paths.first().cloned() {
                Some(p) if crate::disc::dvda_utils::is_dvda_source(&p) => p,
                _ => {
                    app.set_status(":dvda-group: editor is not on a DVD-Audio source");
                    app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                    return;
                }
            };
            if state.active_surface().dirty {
                app.set_status(
                    ":dvda-group: editor has unsaved edits — save (:w) or discard (:q!) first",
                );
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                return;
            }
            match super::keybindings::switch_dvda_editor_group(&mut state, &source_path, group_nr) {
                Ok(label) => app.set_status(format!(":dvda-group → {}", label)),
                Err(reason) => app.set_status(reason),
            }
            app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
        }
        Command::TagsCueSidecar => {
            let Some(mut state) = app.pending_metadata_editor.take() else {
                app.set_status(":tags-cue-sidecar only works in the metadata editor");
                return;
            };
            match super::keybindings::reload_from_sidecar_cue(&mut state) {
                Ok(msg) => app.set_status(msg),
                Err(reason) => app.set_status(reason),
            }
            app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
        }
        Command::CueSheetDelete => {
            let Some(state) = app.pending_metadata_editor.take() else {
                app.set_status(":cuesheet-delete only works in the metadata editor");
                return;
            };
            super::keybindings::open_embedded_cuesheet_delete_confirmation(app, state);
        }
        Command::CueSheetEdit => {
            let Some(mut state) = app.pending_metadata_editor.take() else {
                app.set_status(":cuesheet-edit only works in the metadata editor");
                return;
            };
            let msg = super::keybindings::metadata_editor_edit_embedded_cuesheet_with_system_editor(&mut state);
            app.force_redraw = true;
            app.set_status(msg);
            app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
        }
        Command::CueView => {
            let Some(state) = app.pending_metadata_editor.take() else {
                app.set_status(":cue-view only works in the metadata editor");
                return;
            };
            let cursor = state.cursor;
            let entry = match state.active_surface().entries.get(cursor) {
                Some(e) => e,
                None => {
                    app.set_status(":cue-view: no entry at cursor");
                    app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                    return;
                }
            };
            if !super::probe::is_synthetic_preview(entry) {
                app.set_status(format!(
                    ":cue-view: row '{}' has no embedded CUE sheet",
                    entry.display_key,
                ));
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                return;
            }
            let content = entry
                .per_file_values
                .first()
                .cloned()
                .unwrap_or_else(|| entry.value.clone());
            let summary = format!(
                "{} (read-only · {})",
                entry.display_key,
                super::probe::cue_summary_string(&content),
            );
            app.pending_metadata_editor = Some(state);
            app.active_overlay = super::app::ActiveOverlay::CuePreview(Box::new(
                super::app::CuePreviewState::new_readonly(content, summary),
            ));
        }
        Command::TagsFromMb { query, catno, year } => {
            let guard_paths = current_bulk_guard_paths(app);
            if maybe_confirm_bulk_operation(
                app,
                BulkOperationKind::MusicBrainzTagging,
                BulkGuardCommand::TagsFromMb {
                    query: query.clone(),
                    catno: catno.clone(),
                    year: year.clone(),
                },
                &guard_paths,
            ) {
                return;
            }

            let explicit_args = query.is_some() || catno.is_some() || year.is_some();
            let direct_seed = if explicit_args {
                Some(SacdMbSeed {
                    artist: String::new(),
                    album: query.clone().unwrap_or_default(),
                    catalog: catno.clone(),
                    year: year.clone(),
                })
            } else {
                None
            };

            // C-2d: Browse + single SACD ISO selection + no editor
            // open → auto-open the SACD editor first, then fall
            // through to in-editor dispatch which uses the TOC path.
            // Without this, right-click → "Get tags from MusicBrainz"
            // on an ISO surfaces the editor-first hint instead of
            // doing what the user asked.
            if app.current_screen == AppScreen::Browse
                && !matches!(
                    app.active_overlay,
                    super::app::ActiveOverlay::MetadataEditor(_)
                )
                && app.pending_metadata_editor.is_none()
            {
                let sel = collect_selection_for_file_ops(app);
                // Look for SACD ISO, DVD-Audio, or DVD-Video sources in the selection.
                // When the selection is a directory, scan one level deep for disc images
                // or authored-disc child directories.
                let mut sacd_isos: Vec<std::path::PathBuf> = Vec::new();
                let mut dvda_sources: Vec<std::path::PathBuf> = Vec::new();
                let mut dvdv_sources: Vec<std::path::PathBuf> = Vec::new();
                let mut bluray_sources: Vec<std::path::PathBuf> = Vec::new();
                let mut has_audio = false;
                for path in &sel {
                    if path.is_dir() {
                        if crate::disc::dvda_utils::is_dvda_directory(path) {
                            dvda_sources.push(path.clone());
                            continue;
                        }
                        if is_dvdv_source_for_tags_mb(path) {
                            dvdv_sources.push(path.clone());
                            continue;
                        }
                        if is_bluray_source_for_tags_mb(path) {
                            bluray_sources.push(path.clone());
                            continue;
                        }
                        if let Ok(entries) = std::fs::read_dir(path) {
                            for entry in entries.flatten() {
                                let p = entry.path();
                                if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("iso"))
                                    && super::sacd::is_sacd_iso(&p)
                                {
                                    sacd_isos.push(p);
                                } else if crate::disc::dvda_utils::is_dvda_source(&p) {
                                    dvda_sources.push(p);
                                } else if is_dvdv_source_for_tags_mb(&p) {
                                    dvdv_sources.push(p);
                                } else if is_bluray_source_for_tags_mb(&p) {
                                    bluray_sources.push(p);
                                } else if matches!(
                                    crate::convert::classify::classify_file(&p),
                                    crate::convert::classify::EntryKind::AudioFile(_)
                                ) {
                                    has_audio = true;
                                }
                            }
                        }
                    } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("iso"))
                        && super::sacd::is_sacd_iso(path)
                    {
                        sacd_isos.push(path.clone());
                    } else if crate::disc::dvda_utils::is_dvda_source(path) {
                        dvda_sources.push(path.clone());
                    } else if is_dvdv_source_for_tags_mb(path) {
                        dvdv_sources.push(path.clone());
                    } else if is_bluray_source_for_tags_mb(path) {
                        bluray_sources.push(path.clone());
                    } else if matches!(
                        crate::convert::classify::classify_file(path),
                        crate::convert::classify::EntryKind::AudioFile(_)
                    ) {
                        has_audio = true;
                    }
                }

                let disc_count = sacd_isos.len() + dvda_sources.len() + dvdv_sources.len() + bluray_sources.len();
                if disc_count > 1 {
                    app.set_status(":tags-mb: multiple disc sources selected — select one at a time");
                    return;
                }
                if disc_count > 0 && has_audio {
                    app.set_status(
                        ":tags-mb: mixed selection (disc source + audio files) — select one type",
                    );
                    return;
                }
                if let Some(iso) = sacd_isos.into_iter().next() {
                    super::keybindings::open_metadata_editor_for_sacd(app, iso);
                    if !matches!(
                        app.active_overlay,
                        super::app::ActiveOverlay::MetadataEditor(_),
                    ) {
                        return;
                    }
                } else if let Some(source) = dvda_sources.into_iter().next() {
                    super::keybindings::open_metadata_editor_for_dvda(app, source);
                    if !matches!(
                        app.active_overlay,
                        super::app::ActiveOverlay::MetadataEditor(_),
                    ) {
                        return;
                    }
                } else if let Some(source) = dvdv_sources.into_iter().next() {
                    let selected_presentation_id =
                        selected_dvdv_presentation_id_for_tags_mb_open(app, &source);
                    match open_metadata_editor_for_dvdv_with_sidecar_preload(
                        app,
                        source,
                        selected_presentation_id,
                    ) {
                        Ok(_) => {}
                        Err(err) => app.set_status(format!(
                            ":tags-mb: could not load DVD-Video metadata sidecar: {err}",
                        )),
                    }
                    if !matches!(
                        app.active_overlay,
                        super::app::ActiveOverlay::MetadataEditor(_),
                    ) {
                        return;
                    }
                } else if let Some(source) = bluray_sources.into_iter().next() {
                    super::keybindings::open_metadata_editor_for_bluray(app, source);
                    if !matches!(
                        app.active_overlay,
                        super::app::ActiveOverlay::MetadataEditor(_),
                    ) {
                        return;
                    }
                }
            }

            // In-editor dispatch (SACD or regular file). Returns Some
            // when an editor was in scope; the Browse-path fall-through
            // below runs only when no editor is open. `direct_seed` is
            // `Some` when the user supplied explicit args — the
            // in-editor dispatch then skips TOC and goes straight to
            // text search.
            if let Some(dispatched) = try_dispatch_in_editor_tags_mb(app, tx, direct_seed.clone()) {
                if dispatched {
                    return;
                }
            }

            let mut paths: Vec<std::path::PathBuf> = current_audio_paths(app, false);
            let browse_selection_for_cues = if app.current_screen == AppScreen::Browse {
                collect_selection_for_file_ops(app)
            } else {
                Vec::new()
            };
            let cue_infos = collect_single_image_cue_infos_for_sources(
                &browse_selection_for_cues,
                &paths,
            );
            let cue_metadata_surfaces = collect_cue_metadata_surfaces_for_sources(
                &browse_selection_for_cues,
                &paths,
            );

            if paths.is_empty() && cue_infos.is_empty() && cue_metadata_surfaces.is_empty() {
                // SACD ISOs are handled by the auto-open block at the
                // top of this arm (C-2d); anything reaching here is
                // genuinely a "no audio files" case (empty selection,
                // data-disc `.iso`, or unrecognized extensions).
                app.set_status(":tags-mb: no audio files in selection");
                return;
            }
            super::probe::sort_paths_by_track(&mut paths);

            // Explicit args from the user override the TOC primary
            // path: fire a direct text search instead. Explicit CUE-file
            // selections may not expand to ordinary audio paths, so use the
            // CUE-derived per-track image path vector when needed.
            if let Some(seed) = direct_seed {
                let ctx_paths = if paths.is_empty() && !cue_metadata_surfaces.is_empty() {
                    paths_for_cue_metadata_surfaces(&cue_metadata_surfaces)
                } else if paths.is_empty() && !cue_infos.is_empty() {
                    paths_for_single_image_cue_infos(&cue_infos)
                } else {
                    paths
                };
                let ctx = super::message::TagsMbContext {
                    operation_id: super::message::TagsMbOperationId::UNASSIGNED,
                    paths: ctx_paths,
                    editor_park: false,
                    fallback_seed: None,
                    editor_session: None,
                };
                super::event_loop::spawn_tags_mb_text_search(
                    app,
                    tx,
                    seed,
                    ctx,
                    super::event_loop::TextSearchMode::DirectRequest,
                );
                return;
            }

            // Same-folder split-CUE albums get a whole-album synthetic TOC
            // before text fallback. A concat-TOC hit is stronger evidence than
            // title search, while misses remain non-decisive and fall through to
            // the common-title seed.
            if cue_metadata_surfaces.len() > 1 {
                let operation_id = match super::event_loop::begin_tags_mb_prelookup_operation(
                    app,
                    false,
                    super::app::TagsMbOperationPhase::Grouping,
                ) {
                    Ok(operation_id) => operation_id,
                    Err(error) => {
                        app.set_status(error);
                        return;
                    }
                };
                let active_audio_path = paths
                    .first()
                    .cloned()
                    .or_else(|| cue_infos.first().map(|info| info.audio_path.clone()));
                if dispatch_split_cue_musicbrainz_concat_or_text_fallback(
                    app,
                    tx,
                    operation_id,
                    &cue_infos,
                    /* editor_park */ false,
                    active_audio_path,
                    None,
                ) {
                    return;
                }
                let _ = super::event_loop::transition_tags_mb_operation_phase(
                    app,
                    operation_id,
                    super::app::TagsMbOperationPhase::Grouping,
                    super::app::TagsMbOperationPhase::Lookup,
                );
                if super::event_loop::tags_mb_operation_is_current(app, operation_id)
                    && dispatch_split_cue_musicbrainz_text_fallback_from_surfaces(
                        app,
                        tx,
                        operation_id,
                        &cue_metadata_surfaces,
                    )
                {
                    return;
                }
                if super::event_loop::finish_tags_mb_operation_if_current(app, operation_id) {
                    app.set_status(
                        ":tags-mb: split CUE album has no album/artist metadata for text fallback"
                            .to_string(),
                    );
                }
                return;
            }
            if cue_infos.len() > 1 {
                let operation_id = match super::event_loop::begin_tags_mb_prelookup_operation(
                    app,
                    false,
                    super::app::TagsMbOperationPhase::Grouping,
                ) {
                    Ok(operation_id) => operation_id,
                    Err(error) => {
                        app.set_status(error);
                        return;
                    }
                };
                let active_audio_path = paths
                    .first()
                    .cloned()
                    .or_else(|| cue_infos.first().map(|info| info.audio_path.clone()));
                if dispatch_split_cue_musicbrainz_concat_or_text_fallback(
                    app,
                    tx,
                    operation_id,
                    &cue_infos,
                    /* editor_park */ false,
                    active_audio_path,
                    None,
                ) {
                    return;
                }
                if super::event_loop::finish_tags_mb_operation_if_current(app, operation_id) {
                    app.set_status(
                        ":tags-mb: split CUE album has no album/artist metadata for text fallback"
                            .to_string(),
                    );
                }
                return;
            }

            if paths.is_empty() {
                app.set_status(":tags-mb: no audio files in selection");
                return;
            }

            let dir = paths[0]
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_path_buf();

            // Single-image CUE albums: one audio file carries every track.
            // Mirror the GNUDB path — compute the TOC from the CUE's own
            // track boundaries and repeat the image path per track so
            // per-track results map onto editor rows backed by the image.
            if paths.len() == 1 {
                if let Some(info) = super::cue_parser::detect_single_image(&dir) {
                    // Canonicalize both sides: the CUE resolver and the audio
                    // walk can produce differently-normalized spellings of the
                    // same file.
                    let same_image = info.audio_path == paths[0]
                        || match (info.audio_path.canonicalize(), paths[0].canonicalize()) {
                            (Ok(a), Ok(b)) => a == b,
                            _ => false,
                        };
                    if same_image && !info.track_boundaries.is_empty() {
                        let samples_per_frame = (info.sample_rate / 75).max(1) as u64;
                        let mut sectors = Vec::with_capacity(info.track_boundaries.len() + 1);
                        let mut frame: u64 = 150;
                        for &(_, count) in &info.track_boundaries {
                            sectors.push(frame as u32);
                            frame += count / samples_per_frame;
                        }
                        sectors.push(frame as u32);
                        if super::musicbrainz::build_mb_toc(&sectors).is_none() {
                            app.set_status(":tags-mb: TOC too short".to_string());
                            return;
                        }
                        let image_paths: Vec<std::path::PathBuf> = (0..info.track_boundaries.len())
                            .map(|_| info.audio_path.clone())
                            .collect();
                        // Real rip geometry: exact TOC, no stub-drop cascade.
                        spawn_tags_mb_toc_lookup(
                            app, tx,
                            super::message::TagsMbOperationId::UNASSIGNED,
                            vec![super::musicbrainz::TocCandidate::exact(sectors)],
                            image_paths,
                            /* editor_park */ false, /* fallback_seed */ None,
                            /* editor_session */ None,
                        );
                        return;
                    }
                }
            }

            let sectors: Vec<u32> = match super::accuraterip::find_toc_offsets(&dir) {
                Some(s) => s,
                None => match super::accuraterip::collect_sample_counts(&paths) {
                    Ok((sample_counts, sample_rate)) => {
                        let samples_per_frame = (sample_rate / 75) as u64;
                        let mut sectors = Vec::with_capacity(sample_counts.len() + 1);
                        let mut frame: u64 = 150;
                        for &count in &sample_counts {
                            sectors.push(frame as u32);
                            frame += count / samples_per_frame;
                        }
                        sectors.push(frame as u32);
                        sectors
                    }
                    Err(e) => {
                        app.set_status(format!(":tags-mb: {}", e));
                        return;
                    }
                },
            };
            if super::musicbrainz::build_mb_toc(&sectors).is_none() {
                app.set_status(":tags-mb: TOC too short".to_string());
                return;
            }
            // Real rip geometry: exact TOC, no stub-drop cascade.
            spawn_tags_mb_toc_lookup(
                app, tx,
                super::message::TagsMbOperationId::UNASSIGNED,
                vec![super::musicbrainz::TocCandidate::exact(sectors)],
                paths, /* editor_park */ false,
                /* fallback_seed */ None,
                /* editor_session */ None,
            );
        }
        Command::CueFill => {
            let mut paths: Vec<std::path::PathBuf> = match app.current_screen {
                AppScreen::Browse => {
                    let sel = collect_selection_for_file_ops(app);
                    crate::convert::queue_expansion::expand_paths_to_all_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                crate::convert::classify::classify_file(p),
                                crate::convert::classify::EntryKind::AudioFile(_)
                            )
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            if paths.is_empty() {
                app.set_status(":cue-fill: no audio files in selection");
                return;
            }
            super::probe::sort_paths_by_track(&mut paths);
            let output_dir = if app.current_screen == AppScreen::Browse {
                let sel = collect_selection_for_file_ops(app);
                if sel.len() == 1 && sel[0].is_dir() {
                    sel[0].clone()
                } else {
                    paths[0]
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .to_path_buf()
                }
            } else {
                paths[0]
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .to_path_buf()
            };

            let cue_path = match super::accuraterip::find_cue_file(&output_dir) {
                Some(p) => p,
                None => {
                    app.set_status(
                        ":cue-fill: no .cue file in directory (use :cue-mb to generate one)",
                    );
                    return;
                }
            };
            let sheet = match super::cue_parser::parse_cue_file(&cue_path) {
                Ok(s) => s,
                Err(e) => {
                    app.set_status(format!(":cue-fill: parse failed: {}", e));
                    return;
                }
            };
            if sheet.tracks.is_empty() {
                app.set_status(":cue-fill: parsed CUE has no tracks");
                return;
            }

            // Detect layout. Single-image = unique audio files in CUE == 1
            // and multiple tracks. We pass either 1 or N audio paths into
            // the bridge accordingly.
            let unique_files: std::collections::HashSet<&str> = sheet
                .tracks
                .iter()
                .filter_map(|t| t.file.as_deref())
                .collect();
            let single_image = unique_files.len() == 1 && sheet.tracks.len() > 1;
            let bridge_paths: Vec<std::path::PathBuf> = if single_image {
                vec![paths[0].clone()]
            } else {
                if paths.len() != sheet.tracks.len() {
                    app.set_status(format!(
                        ":cue-fill: CUE has {} tracks but {} audio files in selection",
                        sheet.tracks.len(),
                        paths.len(),
                    ));
                    return;
                }
                paths.clone()
            };

            let layout = if single_image {
                let image_filename = bridge_paths[0]
                    .strip_prefix(&output_dir)
                    .unwrap_or(&bridge_paths[0])
                    .to_string_lossy()
                    .to_string();
                let ext = bridge_paths[0]
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("flac");
                super::message::CueFillLayout::SingleImage {
                    image_filename,
                    format_tag: super::cue_generate::cue_format_tag(ext).to_string(),
                }
            } else {
                super::message::CueFillLayout::MultiFile
            };

            let toc_paths: Vec<std::path::PathBuf> = if single_image {
                vec![bridge_paths[0].clone()]
            } else {
                paths.clone()
            };
            let tx = tx.clone();
            app.set_status(":cue-fill: probing selected files…".to_string());
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    let (album, tracks) = super::cue_generate::cue_sheet_to_track_info_blocking(
                        &sheet,
                        &bridge_paths,
                        &output_dir,
                    )?;
                    let sectors: Vec<u32> = match super::accuraterip::find_toc_offsets(&output_dir) {
                        Some(s) => s,
                        None => {
                            let (sample_counts, sample_rate) =
                                super::accuraterip::collect_sample_counts(&toc_paths)?;
                            let samples_per_frame = (sample_rate / 75) as u64;
                            let mut sectors = Vec::with_capacity(sample_counts.len() + 1);
                            let mut frame: u64 = 150;
                            for &count in &sample_counts {
                                sectors.push(frame as u32);
                                frame += count / samples_per_frame;
                            }
                            sectors.push(frame as u32);
                            sectors
                        }
                    };
                    Ok((Box::new(album), tracks, layout, sectors))
                })
                .await
                .unwrap_or_else(|err| Err(format!("cue-fill preparation task failed: {}", err)));

                let _ = tx
                    .send(AppMessage::CueFillPrepComplete { cue_path, result })
                    .await;
            });
        }
        Command::CueScrollTop => {
            if let Some(mut state) = app.pending_cue_preview.take() {
                state.scroll = 0;
                app.active_overlay = super::app::ActiveOverlay::CuePreview(state);
            }
        }
        Command::CueScrollBottom => {
            if let Some(mut state) = app.pending_cue_preview.take() {
                let last = state.content.lines().count().saturating_sub(1);
                state.scroll = last;
                app.active_overlay = super::app::ActiveOverlay::CuePreview(state);
            }
        }
        Command::CueEditLine(line_1based) => {
            let Some(mut state) = app.pending_cue_preview.take() else {
                app.set_status("no CUE preview to edit");
                return;
            };
            let idx = line_1based.saturating_sub(1);
            if state.begin_edit(idx) {
                // Auto-scroll so the edited line is visible.
                state.scroll = idx.saturating_sub(2);
                app.active_overlay = super::app::ActiveOverlay::CuePreview(state);
            } else {
                let total = state.line_count();
                app.pending_cue_preview = Some(state);
                app.set_status(format!("line {} out of range (1..={})", line_1based, total,));
            }
        }
        Command::MarkCompareRef => {
            if app.current_screen != AppScreen::Browse {
                app.set_status(":mark only works on the browse screen");
            } else {
                let sel = collect_selection_for_file_ops(app);
                let mut paths: Vec<std::path::PathBuf> = crate::convert::queue_expansion::expand_paths_to_all_audio(&sel)
                    .into_iter()
                    .filter(|p| {
                        matches!(
                            crate::convert::classify::classify_file(p),
                            crate::convert::classify::EntryKind::AudioFile(_)
                        )
                    })
                    .collect();
                if paths.is_empty() {
                    app.set_status("No audio files to mark as reference");
                } else {
                    super::probe::sort_paths_by_track(&mut paths);
                    let count = paths.len();
                    app.compare_reference = paths;
                    app.set_status(format!("Marked {} file(s) as compare reference", count,));
                }
            }
        }
        Command::ClearCompareRef => {
            if app.compare_reference.is_empty() {
                app.set_status("No compare reference set");
            } else {
                app.compare_reference.clear();
                app.set_status("Compare reference cleared");
            }
        }
        Command::BitCompare => {
            if app.compare_reference.is_empty() {
                app.set_status("No compare reference set (use :mark first)");
            } else if app.current_screen != AppScreen::Browse {
                app.set_status(":compare only works on the browse screen");
            } else {
                let sel = collect_selection_for_file_ops(app);
                let mut targets: Vec<std::path::PathBuf> =
                    crate::convert::queue_expansion::expand_paths_to_all_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                crate::convert::classify::classify_file(p),
                                crate::convert::classify::EntryKind::AudioFile(_)
                            )
                        })
                        .collect();
                if targets.is_empty() {
                    app.set_status("No audio files selected for comparison");
                } else {
                    super::probe::sort_paths_by_track(&mut targets);
                    let refs = &app.compare_reference;
                    if refs.len() != targets.len() {
                        app.set_status(format!(
                            "Reference has {} file(s) but target has {} — counts must match",
                            refs.len(),
                            targets.len(),
                        ));
                    } else {
                        app.compare_results.clear();
                        app.compare_pending = refs.len();
                        for (ref_path, target_path) in refs.iter().zip(targets.iter()) {
                            let tx = tx.clone();
                            let rp = ref_path.clone();
                            let tp = target_path.clone();
                            tokio::spawn(async move {
                                let result = super::bit_compare::compare_files(rp, tp).await;
                                let _ = tx.send(AppMessage::CompareComplete { result }).await;
                            });
                        }
                        app.set_status(format!("Comparing {} pair(s)...", app.compare_pending,));
                    }
                }
            }
        }
        Command::ComparePaths { path1, path2 } => {
            let p1 = std::path::PathBuf::from(&path1);
            let p2 = std::path::PathBuf::from(&path2);
            if !p1.exists() {
                app.set_status(format!("compare: not found: {}", path1));
            } else if !p2.exists() {
                app.set_status(format!("compare: not found: {}", path2));
            } else {
                app.compare_results.clear();
                app.compare_pending = 1;
                let tx = tx.clone();
                tokio::spawn(async move {
                    let result = super::bit_compare::compare_files(p1, p2).await;
                    let _ = tx.send(AppMessage::CompareComplete { result }).await;
                });
                app.set_status("Comparing...");
            }
        }
        Command::DetectPreemphasis => {
            let guard_paths = current_bulk_guard_paths(app);
            if maybe_confirm_bulk_operation(
                app,
                BulkOperationKind::PreemphasisDetection,
                BulkGuardCommand::DetectPreemphasis,
                &guard_paths,
            ) {
                return;
            }

            let paths: Vec<std::path::PathBuf> = current_audio_paths(app, false);
            if paths.is_empty() {
                app.set_status("No audio files for pre-emphasis detection");
            } else {
                app.preemph_results.clear();
                app.preemph_pending = paths.len();
                for path in paths {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let result = super::preemphasis::detect_preemphasis_metadata_catalog_async(path).await;
                        let _ = tx.send(AppMessage::PreemphasisComplete { result }).await;
                    });
                }
                app.set_status(format!(
                    "Detecting pre-emphasis on {} file(s)...",
                    app.preemph_pending,
                ));
            }
        }
        Command::TrainPreemphCorpus { path } => {
            let scan_path = std::path::PathBuf::from(&path);
            if !scan_path.is_dir() {
                app.set_status(format!("Not a directory: {}", path));
            } else {
                let tx = tx.clone();
                app.set_status(format!("Training corpus from {}...", path));
                tokio::spawn(async move {
                    let result =
                        super::preemphasis::corpus::train_corpus_from_dir(&scan_path).await;
                    let _ = tx
                        .send(super::message::AppMessage::CorpusTrainComplete {
                            result: result.map(|m| (m.n_tracks, m.n_frames)),
                        })
                        .await;
                });
            }
        }
        Command::CalibratePreemphasis { pe_dir, non_pe_dir } => {
            let pe_path = std::path::PathBuf::from(&pe_dir);
            let non_pe_path = std::path::PathBuf::from(&non_pe_dir);
            if !pe_path.is_dir() {
                app.set_status(format!("Not a directory: {}", pe_dir));
            } else if !non_pe_path.is_dir() {
                app.set_status(format!("Not a directory: {}", non_pe_dir));
            } else {
                let tx = tx.clone();
                app.set_status(format!("Calibrating pre-emphasis detector..."));
                tokio::spawn(async move {
                    let result =
                        super::preemphasis::corpus::calibrate(&pe_path, &non_pe_path).await;
                    let _ = tx
                        .send(super::message::AppMessage::CalibrationComplete {
                            result: result.map(|r| {
                                (r.n_pe, r.n_non_pe, r.cv_accuracy, r.cv_fpr, r.threshold)
                            }),
                        })
                        .await;
                });
            }
        }
        Command::Search { recursive } => {
            if app.current_screen != AppScreen::Browse {
                app.set_status(":search only works on the browse screen");
            } else {
                app.browse.open_search();
                app.browse.search.recursive = recursive;
            }
        }
        Command::BulkRename => {
            if app.current_screen != AppScreen::Browse {
                app.set_status(":rename-all only works on the browse screen");
            } else {
                let selection = collect_selection_for_file_ops_scoped(app);
                let dropped_stale_count = selection.dropped_stale_count;
                let audio_paths: Vec<PathBuf> = selection
                    .paths
                    .into_iter()
                    .filter(|p| {
                        app.browse.entries.iter().any(|e| {
                            e.path == *p && matches!(e.kind, crate::convert::classify::EntryKind::AudioFile(_))
                        })
                    })
                    .collect();
                super::keybindings::open_bulk_rename(app, audio_paths);
                surface_stale_selection_notice(app, dropped_stale_count);
            }
        }
        Command::Actions => {
            if !app.conversion_actions_ui_enabled() {
                app.set_status(CONVERSION_ACTIONS_GATED_STATUS);
                return;
            }
            let state = super::conversion_actions_ui::ConversionActionsWizardState::new(
                app.convert.output_options.actions.clone(),
            );
            app.active_overlay = super::app::ActiveOverlay::ConversionActionsWizard(Box::new(state));
            app.set_status("Editing conversion actions");
        }
        Command::ActionsRun(path) => {
            if !app.conversion_actions_ui_enabled() {
                app.set_status(CONVERSION_ACTIONS_GATED_STATUS);
                return;
            }
            let state = super::conversion_actions_ui::start_actions_run_preparation(
                app.browse.current_dir.clone(),
                app.convert.output_options.actions.clone(),
                path,
                tx,
            );
            app.active_overlay = super::app::ActiveOverlay::ActionsRun(Box::new(state));
            app.set_status("Preparing exact action preview… Esc cancels");
        }
        Command::ActionsIdentityImport(path) => {
            if !app.conversion_actions_ui_enabled() {
                app.set_status(CONVERSION_ACTIONS_GATED_STATUS);
                return;
            }
            if app.current_screen != AppScreen::Browse {
                app.set_status(":actions-identity-import only works on the browse screen");
            } else {
                match super::conversion_actions_ui::import_actions_identity(
                    &app.browse.current_dir,
                    path.as_deref(),
                ) {
                    Ok(message) => app.set_status(message),
                    Err(error) => app.set_status(error),
                }
            }
        }
        Command::WriteRgTrack | Command::WriteRgAlbum => {
            let album = matches!(cmd, Command::WriteRgAlbum);
            let paths: Vec<std::path::PathBuf> = app
                .analysis_results
                .iter()
                .map(|r| r.path.clone())
                .collect();
            if paths.is_empty() {
                app.set_status("No analysis results — run :analyze first");
            } else {
                let tx = tx.clone();
                let db_paths: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                app.set_status(format!(
                    "Writing {} ReplayGain tags...",
                    if album { "album + track" } else { "track" },
                ));
                tokio::spawn(async move {
                    let mut args = vec!["-s".to_string(), "i".to_string(), "-k".to_string()];
                    if album {
                        args.push("-a".to_string());
                    } else {
                        args.push("-r".to_string());
                    }
                    for p in &paths {
                        args.push(p.to_string_lossy().to_string());
                    }
                    let output = tokio::process::Command::new("loudgain")
                        .args(&args)
                        .output()
                        .await;
                    let msg = match output {
                        Ok(o) if o.status.success() => {
                            format!(
                                "ReplayGain tags written ({} file{})",
                                db_paths.len(),
                                if db_paths.len() == 1 { "" } else { "s" }
                            )
                        }
                        Ok(o) => {
                            let stderr = String::from_utf8_lossy(&o.stderr);
                            format!(
                                "loudgain failed: {}",
                                stderr.lines().next().unwrap_or("unknown error")
                            )
                        }
                        Err(e) => format!("loudgain not found: {}", e),
                    };
                    let _ = tx
                        .send(super::message::AppMessage::StatusMessage(msg))
                        .await;
                });
                // Invalidate probe cache for the written files.
                for r in &app.analysis_results {
                    app.browse.remove_probe_cache_entry(&r.path);
                    let _ = app.db.invalidate_probe(&r.path.display().to_string());
                }
            }
        }
        Command::WriteDr => {
            if app.analysis_results.is_empty() {
                app.set_status("No analysis results — run :analyze first");
            } else {
                let reports = super::dr_report::format_dr_reports(&app.analysis_results);
                let mut written = Vec::new();
                let mut errors = Vec::new();
                for (dir, text) in &reports {
                    let path = dir.join("dr_analysis.txt");
                    match std::fs::write(&path, text) {
                        Ok(()) => written.push(path),
                        Err(e) => errors.push(format!("{}: {}", dir.display(), e)),
                    }
                }
                if errors.is_empty() {
                    if written.len() == 1 {
                        app.set_status(format!("DR report written to {}", written[0].display(),));
                    } else {
                        app.set_status(format!(
                            "DR reports written to {} directories",
                            written.len(),
                        ));
                    }
                } else {
                    app.set_status(format!("DR report errors: {}", errors.join("; "),));
                }
            }
        }
        Command::ImportCue => {
            // Collect audio paths from current browse selection.
            let mut paths: Vec<std::path::PathBuf> = match app.current_screen {
                AppScreen::Browse => {
                    let sel = collect_selection_for_file_ops(app);
                    crate::convert::queue_expansion::expand_paths_to_all_audio(&sel)
                        .into_iter()
                        .filter(|p| {
                            matches!(
                                crate::convert::classify::classify_file(p),
                                crate::convert::classify::EntryKind::AudioFile(_)
                            )
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            super::probe::sort_paths_by_track(&mut paths);

            if paths.is_empty() {
                app.set_status("No audio files for CUE import");
                return;
            }

            let groups = super::gnudb::group_by_disc(&paths);

            let cue_audio_paths =
                |cue_path: &std::path::Path, sheet: &super::cue_parser::CueSheet| {
                let dir = cue_path.parent().unwrap_or(std::path::Path::new("."));
                let mut resolved = Vec::new();
                for filename in sheet.tracks.iter().filter_map(|track| track.file.as_deref()) {
                    if let Some(path) =
                        super::accuraterip::resolve_cue_file_reference(dir, filename)
                    {
                        if !resolved.iter().any(|existing| existing == &path) {
                            resolved.push(path);
                        }
                    }
                }
                resolved
            };

            if groups.len() <= 1 {
                // Single directory: handle one CUE as the traditional path,
                // or N CUE/image pairs as ordered album parts/sides.
                let (_, group_paths) = groups.into_iter().next().unwrap();
                let dir = group_paths[0].parent().unwrap_or(std::path::Path::new("."));
                let cue_paths = super::gnudb::find_cues_in_dir(dir);
                if cue_paths.is_empty() {
                    app.set_status("No CUE file found in directory");
                    return;
                }

                if cue_paths.len() == 1 {
                    let cue_path = cue_paths[0].clone();
                    let sheet = match super::cue_parser::parse_cue_file(&cue_path) {
                        Ok(s) => s,
                        Err(e) => {
                            app.set_status(format!("CUE parse error: {}", e));
                            return;
                        }
                    };
                    let n_tracks = sheet.tracks.len();
                    let cue_paths = cue_audio_paths(&cue_path, &sheet);
                    let review_paths = if cue_paths.is_empty() {
                        group_paths
                    } else {
                        cue_paths
                    };
                    let review = super::gnudb::build_review_state_from_cue(&sheet, review_paths);
                    app.set_status(format!(
                        "CUE import: {} tracks from {}",
                        n_tracks,
                        cue_path.file_name().unwrap_or_default().to_string_lossy(),
                    ));
                    app.active_overlay = ActiveOverlay::GnudbReview(Box::new(review));
                } else {
                    let mut entries = Vec::new();
                    for cue_path in cue_paths {
                        if let Ok(sheet) = super::cue_parser::parse_cue_file(&cue_path) {
                            let label = cue_path
                                .file_stem()
                                .map(|stem| stem.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "cue".to_string());
                            let resolved_paths = cue_audio_paths(&cue_path, &sheet);
                            if !resolved_paths.is_empty() {
                                entries.push((label, sheet, resolved_paths));
                            }
                        }
                    }
                    if entries.is_empty() {
                        app.set_status("No usable CUE/image pairs found in directory");
                        return;
                    }
                    let n_parts = entries.len();
                    let n_tracks: usize = entries.iter().map(|(_, s, _)| s.tracks.len()).sum();
                    let review = super::gnudb::build_multi_disc_review_state_from_cue(&entries);
                    app.set_status(format!(
                        "CUE import: {} part{}, {} tracks",
                        n_parts,
                        if n_parts == 1 { "" } else { "s" },
                        n_tracks,
                    ));
                    app.active_overlay = ActiveOverlay::GnudbReview(Box::new(review));
                }
            } else {
                // Multi-disc: find every CUE in each subdirectory, preserving
                // the CUE-to-image association instead of choosing one file by
                // sort order.
                let mut entries = Vec::new();
                for (label, group_paths) in groups {
                    let dir = group_paths[0].parent().unwrap_or(std::path::Path::new("."));
                    let cue_paths = super::gnudb::find_cues_in_dir(dir);
                    for cue_path in cue_paths {
                        if let Ok(sheet) = super::cue_parser::parse_cue_file(&cue_path) {
                            let mut entry_label = label.clone();
                            if let Some(stem) = cue_path.file_stem() {
                                entry_label = format!("{} {}", entry_label, stem.to_string_lossy());
                            }
                            let resolved_paths = cue_audio_paths(&cue_path, &sheet);
                            entries.push((
                                entry_label,
                                sheet,
                                if resolved_paths.is_empty() {
                                    group_paths.clone()
                                } else {
                                    resolved_paths
                                },
                            ));
                        }
                    }
                }
                if entries.is_empty() {
                    app.set_status("No CUE files found in any disc directory");
                    return;
                }
                let n_discs = entries.len();
                let n_tracks: usize = entries.iter().map(|(_, s, _)| s.tracks.len()).sum();
                let review = super::gnudb::build_multi_disc_review_state_from_cue(&entries);
                app.set_status(format!(
                    "CUE import: {} disc{}, {} tracks",
                    n_discs,
                    if n_discs == 1 { "" } else { "s" },
                    n_tracks,
                ));
                app.active_overlay = ActiveOverlay::GnudbReview(Box::new(review));
            }
        }
        Command::FixCaps => {
            // Restore parked metadata editor if coming from command mode.
            if let Some(parked) = app.pending_metadata_editor.take() {
                app.active_overlay = ActiveOverlay::MetadataEditor(parked);
            }
            if let super::app::ActiveOverlay::MetadataEditor(ref mut state) = app.active_overlay {
                use super::app::MetadataEditorPhase;
                // Phase-scoped: main editor skips mixed (user can't
                // see them and the placeholder shouldn't get rewritten);
                // detail overlay capitalizes only the focused entry's
                // per-track values; other phases (InlineEdit / AddingKey
                // / Saving) refuse cleanly.
                let focus = match state.phase {
                    MetadataEditorPhase::Editing => None,
                    MetadataEditorPhase::DetailEdit => Some(state.detail_field_idx),
                    _ => {
                        app.set_status(":fix-caps not available in this phase");
                        return;
                    }
                };
                let result = super::keybindings::fix_caps_for_state(state, focus);
                if result.changed_values > 0 {
                    state.active_surface_mut().dirty = true;
                }
                let mut msg = format!(
                    "Capitalization applied ({} values changed",
                    result.changed_values
                );
                if result.skipped_deleted > 0 {
                    msg.push_str(&format!(
                        "; {} deleted entries skipped",
                        result.skipped_deleted
                    ));
                }
                msg.push(')');
                app.set_status(msg);
            } else {
                app.set_status(":fix-caps only works in the metadata editor");
            }
        }
        Command::AccurateRip { force } => {
            let guard_paths = current_bulk_guard_paths(app);
            let operation = if force {
                BulkOperationKind::AccurateRipFullScan
            } else {
                BulkOperationKind::AccurateRipVerify
            };
            if maybe_confirm_bulk_operation(
                app,
                operation,
                BulkGuardCommand::AccurateRip { force },
                &guard_paths,
            ) {
                return;
            }

            // Collect audio file paths from the current context.
            let mut paths: Vec<std::path::PathBuf> = current_audio_paths(app, true);
            super::probe::sort_paths_by_track(&mut paths);
            // Check for single-image CUE layout before the normal multi-file flow.
            if paths.len() <= 1 {
                let dir = if paths.is_empty() {
                    let sel = collect_selection_for_file_ops(app);
                    sel.first().and_then(|p| {
                        if p.is_dir() {
                            Some(p.clone())
                        } else {
                            p.parent().map(|d| d.to_path_buf())
                        }
                    })
                } else {
                    paths[0].parent().map(|d| d.to_path_buf())
                };
                if let Some(ref dir) = dir {
                    if let Some(info) = super::cue_parser::detect_single_image(dir) {
                        let n = info.track_boundaries.len();
                        let full_scan = force;
                        let tx = tx.clone();
                        app.set_status(format!(
                            "AccurateRip: verifying {} tracks (single image)...",
                            n,
                        ));
                        tokio::spawn(async move {
                            let result =
                                super::accuraterip::verify_single_image(&info, full_scan).await;
                            let _ = tx
                                .send(AppMessage::AccurateRipComplete {
                                    pages: vec![super::app::ArVerifyPage {
                                        label: String::new(),
                                        result,
                                    }],
                                })
                                .await;
                        });
                        return;
                    }
                }
            }
            if paths.is_empty() {
                app.set_status("No audio files for AccurateRip verification");
            } else {
                let groups = super::gnudb::group_by_disc(&paths);
                let n_groups = groups.len();
                let n_tracks: usize = groups.iter().map(|(_, p)| p.len()).sum();
                let full_scan = force;
                let tx = tx.clone();

                if n_groups <= 1 {
                    // Single disc — existing flow.
                    let group_paths = groups.into_iter().next().unwrap().1;
                    let sample_data = super::accuraterip::collect_sample_counts(&group_paths);
                    match sample_data {
                        Err(e) => {
                            app.set_status(format!("AccurateRip: {}", e));
                        }
                        Ok((sample_counts, sample_rate)) => {
                            app.set_status(format!(
                                "AccurateRip: verifying {} tracks...",
                                n_tracks,
                            ));
                            tokio::spawn(async move {
                                let result = super::accuraterip::verify_album(
                                    &group_paths,
                                    &sample_counts,
                                    sample_rate,
                                    full_scan,
                                )
                                .await;
                                let _ = tx
                                    .send(AppMessage::AccurateRipComplete {
                                        pages: vec![super::app::ArVerifyPage {
                                            label: String::new(),
                                            result,
                                        }],
                                    })
                                    .await;
                            });
                        }
                    }
                } else {
                    // Multi-disc — verify each disc sequentially in one task.
                    app.set_status(format!(
                        "AccurateRip: verifying {} discs, {} tracks...",
                        n_groups, n_tracks,
                    ));
                    tokio::spawn(async move {
                        let mut pages = Vec::with_capacity(n_groups);
                        for (label, group_paths) in groups {
                            let sample_data =
                                super::accuraterip::collect_sample_counts(&group_paths);
                            match sample_data {
                                Ok((sample_counts, sample_rate)) => {
                                    let result = super::accuraterip::verify_album(
                                        &group_paths,
                                        &sample_counts,
                                        sample_rate,
                                        full_scan,
                                    )
                                    .await;
                                    pages.push(super::app::ArVerifyPage { label, result });
                                }
                                Err(e) => {
                                    log::warn!("AccurateRip: skipping disc '{}': {}", label, e);
                                }
                            }
                        }
                        if !pages.is_empty() {
                            let _ = tx.send(AppMessage::AccurateRipComplete { pages }).await;
                        }
                    });
                }
            }
        }
        Command::ArFix => {
            let guard_paths = if let ActiveOverlay::AccurateRipVerify(ref state) = app.active_overlay {
                let page = &state.pages[state.active_page];
                page.result.tracks.iter().map(|t| t.path.clone()).collect::<Vec<_>>()
            } else {
                current_bulk_guard_paths(app)
            };
            if maybe_confirm_bulk_operation(
                app,
                BulkOperationKind::AccurateRipFixOffset,
                BulkGuardCommand::AccurateRipFixOffset,
                &guard_paths,
            ) {
                return;
            }

            // Check that the AR verification overlay is active.
            if let ActiveOverlay::AccurateRipVerify(ref state) = app.active_overlay {
                let page = &state.pages[state.active_page];
                if let Some(offset) = super::accuraterip::detect_uniform_offset(&page.result) {
                    let paths: Vec<std::path::PathBuf> =
                        page.result.tracks.iter().map(|t| t.path.clone()).collect();
                    let n = paths.len();
                    app.active_overlay = ActiveOverlay::Confirmation {
                        message: format!(
                            "Apply offset correction ({:+} samples) to {} tracks?\n\
                             Files will be re-encoded to FLAC and verified at offset +0\n\
                             before replacing originals.",
                            offset, n,
                        ),
                        action: super::app::ConfirmAction::OffsetCorrection { paths, offset },
                    };
                } else {
                    app.set_status(
                        "No uniform non-zero offset detected — correction not applicable",
                    );
                }
            } else {
                // No overlay open — run verification first, then auto-fix.
                // A confirmed fix-offset bulk guard already covers the
                // verification pass it must run to discover the offset, so
                // suppress a duplicate count prompt for the nested AR verify.
                app.auto_fix_on_complete = true;
                if app.bulk_guard_frozen_paths.is_some() {
                    app.bulk_guard_bypass = Some(BulkOperationKind::AccurateRipVerify);
                }
                execute_command(app, Command::AccurateRip { force: false }, tx);
            }
        }
        Command::Ctdb => {
            let guard_paths = current_bulk_guard_paths(app);
            if maybe_confirm_bulk_operation(
                app,
                BulkOperationKind::CtdbVerify,
                BulkGuardCommand::Ctdb,
                &guard_paths,
            ) {
                return;
            }

            // Same path collection as :ar.
            let mut paths: Vec<std::path::PathBuf> = current_audio_paths(app, true);
            super::probe::sort_paths_by_track(&mut paths);
            // Check for single-image CUE layout.
            if paths.len() <= 1 {
                let dir = if paths.is_empty() {
                    let sel = collect_selection_for_file_ops(app);
                    sel.first().and_then(|p| {
                        if p.is_dir() {
                            Some(p.clone())
                        } else {
                            p.parent().map(|d| d.to_path_buf())
                        }
                    })
                } else {
                    paths[0].parent().map(|d| d.to_path_buf())
                };
                if let Some(ref dir) = dir {
                    if let Some(info) = super::cue_parser::detect_single_image(dir) {
                        let n = info.track_boundaries.len();
                        let tx = tx.clone();
                        app.set_status(format!(
                            "CUETools DB: verifying {} tracks (single image)...",
                            n,
                        ));
                        // Cache lookup before spawn (needs &app.db on main thread).
                        let cache_paths = vec![info.audio_path.clone()];
                        let cache_key = super::ctdb::compute_ctdb_parity_cache_key(&cache_paths);
                        let cached_parity = cache_key
                            .as_deref()
                            .and_then(|k| app.db.get_cached_ctdb_parity(k, 16));
                        tokio::spawn(async move {
                            let result = super::ctdb::verify_ctdb_single_image(
                                &info,
                                cache_key,
                                cached_parity,
                            )
                            .await;
                            let _ = tx
                                .send(AppMessage::CtdbComplete {
                                    pages: vec![super::app::CtdbVerifyPage {
                                        label: String::new(),
                                        result,
                                    }],
                                })
                                .await;
                        });
                        return;
                    }
                }
            }
            if paths.is_empty() {
                app.set_status("No audio files for CTDB verification");
            } else {
                let groups = super::gnudb::group_by_disc(&paths);
                let n_groups = groups.len();
                let n_tracks: usize = groups.iter().map(|(_, p)| p.len()).sum();
                let tx = tx.clone();

                if n_groups <= 1 {
                    // Single disc.
                    let group_paths = groups.into_iter().next().unwrap().1;
                    let sample_data = super::accuraterip::collect_sample_counts(&group_paths);
                    match sample_data {
                        Err(e) => {
                            app.set_status(format!("CTDB: {}", e));
                        }
                        Ok((sample_counts, sample_rate)) => {
                            app.set_status(format!(
                                "CUETools DB: verifying {} tracks...",
                                n_tracks,
                            ));
                            let cache_key =
                                super::ctdb::compute_ctdb_parity_cache_key(&group_paths);
                            let cached_parity = cache_key
                                .as_deref()
                                .and_then(|k| app.db.get_cached_ctdb_parity(k, 16));
                            tokio::spawn(async move {
                                let result = super::ctdb::verify_ctdb(
                                    &group_paths,
                                    &sample_counts,
                                    sample_rate,
                                    cache_key,
                                    cached_parity,
                                )
                                .await;
                                let _ = tx
                                    .send(AppMessage::CtdbComplete {
                                        pages: vec![super::app::CtdbVerifyPage {
                                            label: String::new(),
                                            result,
                                        }],
                                    })
                                    .await;
                            });
                        }
                    }
                } else {
                    // Multi-disc — verify each disc sequentially.
                    app.set_status(format!(
                        "CUETools DB: verifying {} discs, {} tracks...",
                        n_groups, n_tracks,
                    ));
                    tokio::spawn(async move {
                        let mut pages = Vec::with_capacity(n_groups);
                        for (idx, (label, mut group_paths)) in groups.into_iter().enumerate() {
                            let disc_name = if label.is_empty() {
                                format!("disc {}", idx + 1)
                            } else {
                                label.clone()
                            };
                            let _ = tx
                                .send(AppMessage::StatusMessage(format!(
                                    "CUETools DB: verifying {}/{}  — {}...",
                                    idx + 1,
                                    n_groups,
                                    disc_name
                                )))
                                .await;
                            super::probe::sort_paths_by_track(&mut group_paths);
                            let dir = group_paths[0]
                                .parent()
                                .unwrap_or(std::path::Path::new("."))
                                .to_path_buf();

                            // Per-disc single-image detection. Cache key is computed
                            // inside the spawn (file-metadata-only, no DB access).
                            // cached_parity is None here — multi-disc spawns populate
                            // the cache from each result's parity_cache_write after
                            // the spawn completes; the cache helps subsequent
                            // single-disc verifies.
                            let result = if let Some(info) =
                                super::cue_parser::detect_single_image(&dir)
                            {
                                let cache_paths = vec![info.audio_path.clone()];
                                let cache_key =
                                    super::ctdb::compute_ctdb_parity_cache_key(&cache_paths);
                                super::ctdb::verify_ctdb_single_image(&info, cache_key, None).await
                            } else {
                                match super::accuraterip::collect_sample_counts(&group_paths) {
                                    Ok((sample_counts, sample_rate)) => {
                                        let cache_key = super::ctdb::compute_ctdb_parity_cache_key(
                                            &group_paths,
                                        );
                                        super::ctdb::verify_ctdb(
                                            &group_paths,
                                            &sample_counts,
                                            sample_rate,
                                            cache_key,
                                            None,
                                        )
                                        .await
                                    }
                                    Err(e) => {
                                        log::warn!("CTDB: skipping disc '{}': {}", label, e);
                                        continue;
                                    }
                                }
                            };

                            pages.push(super::app::CtdbVerifyPage { label, result });
                        }
                        if !pages.is_empty() {
                            let _ = tx.send(AppMessage::CtdbComplete { pages }).await;
                        }
                    });
                }
            }
        }
        Command::CtdbRepair => {
            // If the CTDB overlay is open with parity available, extract
            // repair parameters from it. Otherwise, run CTDB verify first.
            if let ActiveOverlay::CtdbVerify(ref state) = app.active_overlay {
                let page = &state.pages[state.active_page];
                let result = &page.result;

                // Check that parity is available.
                let parity_url = match &result.parity_url {
                    Some(url) => url.clone(),
                    None => {
                        app.set_status("No parity data available for this disc");
                        return;
                    }
                };

                let npar = match result.npar {
                    Some(n) => n as usize,
                    None => {
                        app.set_status("CTDB entry missing npar value");
                        return;
                    }
                };

                // Check if any tracks have mismatches.
                let has_mismatch = result
                    .tracks
                    .iter()
                    .any(|t| t.status == super::ctdb::CtdbTrackStatus::Mismatch);
                if !has_mismatch {
                    app.set_status("No mismatches detected — repair not needed");
                    return;
                }

                let paths: Vec<std::path::PathBuf> =
                    result.tracks.iter().map(|t| t.path.clone()).collect();
                let n = paths.len();

                // Extract expected CRCs from the CTDB entry for post-repair
                // verification. These come from the database, not our computation.
                let expected_crcs: Vec<u32> = result
                    .tracks
                    .iter()
                    .filter_map(|t| t.expected_crc32)
                    .collect();
                if expected_crcs.len() != n {
                    app.set_status("Cannot repair: missing expected CRC for some tracks");
                    return;
                }

                // Detect single-image CUE layout: all tracks point at the
                // same file. Repair flow is different (decode once, repair
                // whole image, re-encode single file).
                let single_image: Option<Box<super::cue_parser::SingleImageInfo>> = if n > 1
                    && paths.iter().all(|p| p == &paths[0])
                {
                    let dir = paths[0].parent().unwrap_or(std::path::Path::new("."));
                    match super::cue_parser::detect_single_image(dir) {
                        Some(info) => Some(Box::new(info)),
                        None => {
                            app.set_status("Single-image CTDB repair: failed to detect CUE layout");
                            return;
                        }
                    }
                } else {
                    None
                };

                // For single-image, the path list seen by AR is the same
                // file repeated N times. We want to query AR cache by the
                // unique audio path, not N times.
                let cache_query_paths: Vec<std::path::PathBuf> = if single_image.is_some() {
                    vec![paths[0].clone()]
                } else {
                    paths.clone()
                };

                // Auto-detect drive read offset from AR cache. `None` means
                // we don't have enough cached data to be sure — run :ar
                // first and resume the confirmation when it completes.
                match detect_ar_offset_from_cache(&app.db, &cache_query_paths) {
                    Some(offset) => {
                        let offset_note = if offset != 0 {
                            format!("offset: {:+} samples (from AR cache)", offset)
                        } else {
                            "offset: +0 (verified by AR)".to_string()
                        };

                        let message = format!(
                            "Apply CTDB Reed-Solomon repair to {} tracks?\n\
                             Parity: {} symbols, {}\n\
                             Files will be re-encoded and verified before replacing originals.",
                            n, npar, offset_note,
                        );

                        let action = match single_image {
                            Some(info) => super::app::ConfirmAction::CtdbRepairSingleImage {
                                info,
                                parity_url,
                                npar,
                                offset,
                                expected_crcs,
                            },
                            None => super::app::ConfirmAction::CtdbRepair {
                                paths,
                                parity_url,
                                npar,
                                offset,
                                expected_crcs,
                            },
                        };

                        app.active_overlay = ActiveOverlay::Confirmation { message, action };
                    }
                    None => {
                        // No usable AR cache — defer the repair until AR
                        // verification completes and gives us an offset.
                        app.pending_ctdb_repair = Some(super::app::PendingCtdbRepair {
                            paths,
                            parity_url,
                            npar,
                            expected_crcs,
                            single_image,
                        });
                        app.set_status(
                            "No AR offset cached — running AccurateRip to detect drive offset...",
                        );
                        execute_command(app, Command::AccurateRip { force: false }, tx);
                    }
                }
            } else {
                // No CTDB overlay open (e.g. invoked from the "CUETools DB
                // repair" context menu, or directly via :ctdb-repair). Run
                // CTDB verify first; the auto-repair flag tells the
                // CtdbComplete handler to re-dispatch :ctdb-repair once
                // the verification overlay is installed.
                app.auto_repair_on_ctdb_complete = true;
                app.set_status("Running CUETools DB verification first to detect mismatches...");
                execute_command(app, Command::Ctdb, tx);
            }
        }
        Command::ArBatch => {
            if app.current_screen != AppScreen::Browse {
                app.set_status(":ar-batch only works on the browse screen");
                return;
            }
            // Use the selected entry if it's a directory, otherwise
            // use the current browse directory.
            let scan_dir = app
                .bulk_guard_frozen_paths
                .as_ref()
                .and_then(|paths| paths.first().cloned())
                .or_else(|| {
                    app.browse
                        .selected_entry()
                        .filter(|e| e.path.is_dir())
                        .map(|e| e.path.clone())
                })
                .unwrap_or_else(|| app.browse.current_dir.clone());
            if maybe_confirm_bulk_operation(
                app,
                BulkOperationKind::AccurateRipBatch,
                BulkGuardCommand::AccurateRipBatch,
                &[scan_dir.clone()],
            ) {
                return;
            }
            app.set_status(format!(
                "AccurateRip batch: scanning {}...",
                scan_dir.display()
            ));

            let tx = tx.clone();
            tokio::spawn(async move {
                let result = super::accuraterip::batch_verify(&scan_dir, tx.clone()).await;
                let _ = tx.send(AppMessage::ArBatchComplete { result }).await;
            });
        }
        Command::ViewFile(path) => {
            // Resolve path: if empty, use current browse selection.
            let target = if path.as_os_str().is_empty() {
                if app.current_screen != AppScreen::Browse {
                    app.set_status(":view only works on the browse screen");
                    return;
                }
                let entry = app.browse.selected_entry();
                match entry {
                    Some(e) if super::browse::is_viewable_text_file(&e.path) => e.path.clone(),
                    Some(_) => {
                        app.set_status("Selected file is not a viewable text file");
                        return;
                    }
                    None => {
                        app.set_status("No file selected");
                        return;
                    }
                }
            } else {
                path
            };
            match super::external_editor::open_in_viewer(&target) {
                Ok(_) => {
                    app.force_redraw = true;
                }
                Err(e) => {
                    app.force_redraw = true;
                    app.set_status(format!("View error: {}", e));
                }
            }
        }
        Command::EditFile(path) => {
            let target = if path.as_os_str().is_empty() {
                if app.current_screen != AppScreen::Browse {
                    app.set_status(":edit-file only works on the browse screen");
                    return;
                }
                let entry = app.browse.selected_entry();
                match entry {
                    Some(e) if super::browse::is_editable_text_file(&e.path) => e.path.clone(),
                    Some(e) if super::browse::is_viewable_text_file(&e.path) => {
                        app.set_status("Cannot edit log files — use :view instead");
                        return;
                    }
                    Some(_) => {
                        app.set_status("Selected file is not an editable text file");
                        return;
                    }
                    None => {
                        app.set_status("No file selected");
                        return;
                    }
                }
            } else {
                if !super::browse::is_editable_text_file(&path)
                    && super::browse::is_viewable_text_file(&path)
                {
                    app.set_status("Cannot edit log files — use :view instead");
                    return;
                }
                path
            };
            match super::external_editor::open_in_editor(&target) {
                Ok(_) => {
                    app.force_redraw = true;
                }
                Err(e) => {
                    app.force_redraw = true;
                    app.set_status(format!("Edit error: {}", e));
                }
            }
        }
        Command::ContextMenu => {
            let origin = match app.current_screen {
                AppScreen::Browse => app
                    .button_map
                    .find_button_rect(&super::button_map::TuiButton::BrowseEntry(
                        app.browse.selected_index,
                    ))
                    .map(|r| (r.x + 2, r.y)),
                AppScreen::Queue => app
                    .button_map
                    .find_button_rect(&super::button_map::TuiButton::QueueItem(app.selected_index))
                    .map(|r| (r.x + 2, r.y)),
                _ => None,
            }
            .unwrap_or_else(|| {
                crossterm::terminal::size()
                    .map(|(w, h)| (w / 3, h / 3))
                    .unwrap_or((20, 10))
            });
            super::keybindings::open_context_menu(app, origin.0, origin.1);
        }
        Command::Unknown(input) => {
            app.set_status(format!("Unknown command: {}", input));
        }
    }
}

/// Execute a :sort command for the browse screen
fn execute_sort(
    app: &mut AppState,
    field: Option<&str>,
    dir: Option<&str>,
    tx: &mpsc::Sender<AppMessage>,
) {
    use super::browse::{SortBy, SortDir};

    if app.current_screen != AppScreen::Browse {
        app.set_status(":sort only works on the browse screen");
        return;
    }

    // No args → cycle to next field
    if field.is_none() {
        app.browse.cycle_sort_by_with_search(Some(tx));
        let msg = format!(
            "Sort: {} {}",
            app.browse.sort_by.label(),
            app.browse.sort_dir.label()
        );
        app.set_status(msg);
        return;
    }

    // Parse explicit field
    let requested_field = field.unwrap();
    let new_field = match SortBy::from_label(requested_field) {
        Some(field) => field,
        None => {
            app.set_status(format!(
                "Unknown sort field: {}. Try: name, size, date, type, format, codec, sample_rate, channels, duration, artist, album",
                requested_field
            ));
            return;
        }
    };

    // Parse optional direction
    let new_dir = match dir {
        None => app.browse.sort_dir, // preserve current
        Some(d) => match d.to_lowercase().as_str() {
            "asc" | "a" | "ascending" => SortDir::Asc,
            "desc" | "d" | "descending" => SortDir::Desc,
            other => {
                app.set_status(format!("Unknown sort direction: {}. Try: asc, desc", other));
                return;
            }
        },
    };

    app.browse.set_sort_with_search(new_field, new_dir, Some(tx));
    let msg = format!(
        "Sort: {} {}",
        app.browse.sort_by.label(),
        app.browse.sort_dir.label()
    );
    app.set_status(msg);
}

/// Execute a :filter command for the browse screen
fn execute_filter(app: &mut AppState, arg: Option<&str>, tx: &mpsc::Sender<AppMessage>) {
    use super::browse::FormatFilter;
    use crate::convert::formats::AudioFormat;

    if app.current_screen != AppScreen::Browse {
        app.set_status(":filter only works on the browse screen");
        return;
    }

    // No arg → cycle to next filter
    if arg.is_none() {
        app.browse.cycle_format_filter_with_search(Some(tx));
        let msg = format!("Filter: {}", app.browse.format_filter.label());
        app.set_status(msg);
        return;
    }

    // Parse explicit filter value
    let new_filter = match arg.unwrap().to_lowercase().as_str() {
        "off" | "none" | "all" => FormatFilter::Off,
        "audio" => FormatFilter::AudioOnly,
        "flac" => FormatFilter::Only(AudioFormat::Flac),
        "opus" => FormatFilter::Only(AudioFormat::Opus),
        "aac" => FormatFilter::Only(AudioFormat::Aac),
        "mp3" => FormatFilter::Only(AudioFormat::Mp3),
        "alac" => FormatFilter::Only(AudioFormat::Alac),
        "wav" => FormatFilter::Only(AudioFormat::Wav),
        "wavpack" | "wv" => FormatFilter::Only(AudioFormat::WavPack),
        "aiff" => FormatFilter::Only(AudioFormat::Aiff),
        "dsf" => FormatFilter::Only(AudioFormat::Dsf),
        "dff" => FormatFilter::Only(AudioFormat::Dff),
        "ape" => FormatFilter::Only(AudioFormat::Ape),
        "shn" | "shorten" => FormatFilter::Only(AudioFormat::Shorten),
        "ogg" | "oga" | "vorbis" => FormatFilter::Only(AudioFormat::Ogg),
        "tta" | "trueaudio" => FormatFilter::Only(AudioFormat::Tta),
        other => {
            app.set_status(format!(
                "Unknown filter: {}. Try: off, audio, flac, opus, aac, mp3, alac, wav, wavpack, aiff, dsf, dff, ape, shn, ogg, tta",
                other
            ));
            return;
        }
    };

    app.browse.set_format_filter_with_search(new_filter, Some(tx));
    let msg = format!("Filter: {}", app.browse.format_filter.label());
    app.set_status(msg);
}

/// Execute a `:bookmarks` / `:bm` command. Browse-only.
///   `:bm`              → open the bookmarks overlay
///   `:bm add`          → quick-add current browse dir with default name (last
///                        path component), no overlay
///   `:bm add <name>`   → quick-add current browse dir with the given name
fn execute_bookmarks(app: &mut AppState, args: &str) {
    if app.current_screen != AppScreen::Browse {
        app.set_status(":bookmarks only works on the browse screen");
        return;
    }

    let trimmed = args.trim();
    if trimmed.is_empty() {
        app.bookmarks.open_overlay();
        return;
    }

    // Parse subcommand.
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let sub = parts.next().unwrap_or("").to_lowercase();
    let rest = parts.next().unwrap_or("").trim();

    match sub.as_str() {
        "add" => {
            let path = app.browse.current_dir.clone();
            let name = if rest.is_empty() {
                super::bookmarks::BookmarksState::default_name_for_path(&path)
            } else {
                rest.to_string()
            };
            app.bookmarks.add_with_db(name.clone(), path, &app.db);
            app.set_status(format!("bookmark added: {}", name));
        }
        other => {
            app.set_status(format!("unknown :bm subcommand: {}", other));
        }
    }
}

/// Execute a `:queue` / `:queue!` / `:convert` / `:c` command.
///
/// Opens the Convert screen for batch review with the current Browse
/// selection inherited. Every enqueue operation must pass through this
/// review step — there is no back door to the queue.
///
/// Context-sensitive behavior:
/// - From Browse: collects selection (multi-selected or cursor entry),
///   probes the first file, optionally loads a preset, populates the
///   source pane, and switches to Convert. `previous_screen` is set so
///   the user returns to Browse on `:commit` / Esc.
/// - From Convert: with a preset arg, loads that preset in place; without,
///   reminds the user to pick files in Browse first.
/// - From Library: placeholder — selection inheritance arrives in Phase 6c.
/// - From Queue / elsewhere: with a preset arg, loads the preset without
///   switching; without, shows an error.
///
/// Phase 6b limitation: multi-file selections are rejected with a message
/// directing the user to deselect extras. Real batch support (summary +
fn load_queue_preset_into_pills(
    app: &mut AppState,
    name: &str,
) -> Result<super::presets::PresetApplyReport, String> {
    let path = super::presets::preset_file_path(name);
    match super::presets::load_preset_from_path(&path) {
        Ok(preset) => {
            let previous_format = app.convert.format.clone();
            let previous_output = app.convert.output_options.clone();
            let previous_metadata = app.convert.metadata.clone();
            let report = preset.apply_to_pills(
                &mut app.convert.format,
                &mut app.convert.output_options,
                &mut app.convert.metadata,
            );
            if !report.is_complete() {
                app.convert.format = previous_format;
                app.convert.output_options = previous_output;
                app.convert.metadata = previous_metadata;
                return Err(format!(
                    "preset '{}' refused fields: {}",
                    name,
                    report.refused_fields.join(", ")
                ));
            }
            app.preset.set_active_preset_path(name.to_string(), path);
            app.preset.modified = false;
            Ok(report)
        }
        Err(err) => Err(format!("preset '{}' failed: {}", name, err)),
    }
}

fn queue_browse_convert_paths_for_processing(app: &mut AppState, queue: QueueExpansionResult) {
    let mut options = crate::convert::ConversionOptions::default();
    options.append_lineage_to_comment = app.config.conversion.append_lineage_to_comment;
    options.write_log_file = app.config.conversion.write_log_file;
    options.generate_cue_files = app.config.conversion.generate_cue_files;
    options.cue_generation_mode = app.config.conversion.cue_generation_mode.clone();
    app.convert
        .output_options
        .apply_companion_copying_to_conversion_options(&mut options);

    let mut count = 0usize;
    let mut errors = 0usize;
    let mut first_error: Option<String> = None;
    let QueueExpansionResult { paths, cue_artifact_audio, synthetic_cue_artifacts, expansion_errors } = queue;
    let expansion_warning = if let Some(err) = expansion_errors.first() {
        if paths.is_empty() {
            crate::convert::queue_expansion::cleanup_synthetic_cue_artifacts(&synthetic_cue_artifacts);
            app.set_status(err.clone());
            return;
        }
        log::warn!("queue expansion warning: {err}");
        Some(err.clone())
    } else {
        None
    };
    for path in paths {
        let is_synthetic_cue_artifact = synthetic_cue_artifacts.contains(&path);
        let archive_password = if crate::is_encrypted_archive_ext(&path) {
            app.archive_passwords
                .get(&path)
                .cloned()
                .or_else(|| {
                    app.keychain.ensure_loaded();
                    app.keychain.passwords.first().cloned()
                })
                .or_else(|| app.config.conversion.archive_password.clone())
        } else {
            None
        };
        let cue_sidecar_override = crate::convert::queue_expansion::cue_sidecar_override_for_commit_path(
            &path,
            &cue_artifact_audio,
        );
        match app.manager.add_file_ready_for_processing_with_cue_sidecar_override(
            path.clone(),
            options.clone(),
            archive_password,
            cue_sidecar_override,
        ) {
            Ok(_item_id) => {
                // Synthetic album-CUE artifacts are transactionally registered
                // by the manager's ready-admission helper while the queue write
                // lock is still held. Re-registering here would split lifecycle
                // ownership from queue admission and can mask registry failures.
                count = count.saturating_add(1);
            }
            Err(crate::convert::ConversionError::SyntheticCueArtifactOwnershipFailed {
                artifact,
                reason,
            }) => {
                errors = errors.saturating_add(1);
                let message = format!(
                    "queue add failed: synthetic CUE artifact ownership registration failed; artifact preserved at {}: {}",
                    artifact.display(),
                    reason
                );
                first_error.get_or_insert_with(|| message.clone());
                log::error!("{message}");
            }
            Err(err) => {
                if is_synthetic_cue_artifact {
                    crate::convert::queue_expansion::cleanup_synthetic_cue_artifact(&path);
                }
                errors = errors.saturating_add(1);
                let message = format!("queue add failed during Browse Convert expansion: {err}");
                first_error.get_or_insert_with(|| message.clone());
                log::warn!("{message}");
            }
        }
    }

    let mut status = if errors == 0 {
        format!("Queued {} files", count)
    } else if let Some(message) = first_error {
        format!("Queued {} files; {} failed; {}", count, errors, message)
    } else {
        format!("Queued {} files; {} failed", count, errors)
    };
    if let Some(warning) = expansion_warning {
        // A separate set_status is overwritten by this one in the same
        // reducer pass; fold the expansion warning into the visible status.
        status = format!("{status} — note: {warning}");
    }
    app.set_status(status);
    app.save_queue();
}

pub(crate) fn install_browse_convert_source_paths(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    queue: QueueExpansionResult,
    expanded_folder_count: usize,
    from_folder_expansion: bool,
) {
    app.cancel_browse_convert_expansion();
    let QueueExpansionResult { paths, cue_artifact_audio, synthetic_cue_artifacts, expansion_errors } = queue;
    // A set_status here would be overwritten by the final review-settings
    // status in the same reducer pass; capture the warning and fold it into
    // that status instead (and keep a durable log record).
    let mut expansion_warning: Option<String> = None;
    if let Some(err) = expansion_errors.first() {
        if paths.is_empty() {
            crate::convert::queue_expansion::cleanup_synthetic_cue_artifacts(&synthetic_cue_artifacts);
            app.set_status(err.clone());
            return;
        }
        log::warn!("browse expansion warning: {}", err);
        expansion_warning = Some(err.clone());
    }
    let paths = normalized_path_snapshot(paths);
    if paths.is_empty() {
        crate::convert::queue_expansion::cleanup_synthetic_cue_artifacts(&synthetic_cue_artifacts);
        app.set_status("No supported sources selected");
        return;
    }

    let first = paths[0].clone();
    let path_count = paths.len();
    let archive_preview_single = path_count == 1 && is_nonprobeable_source_for_probe(&first);

    app.probe_generation = app.probe_generation.saturating_add(1);
    let generation = app.probe_generation;
    let probe_notice = if archive_preview_single {
        Some(ARCHIVE_PREVIEW_EXTRACTING_NOTICE.to_string())
    } else {
        source_probe_initial_notice(&first)
    };

    clear_source_metadata_in_convert(&mut app.convert);
    let mut mode = if path_count == 1 {
        SourceMode::from_single_pending_probe(first.clone(), probe_notice.clone())
    } else {
        SourceMode::from_paths(paths.clone())
    };
    match &mut mode {
        SourceMode::Single { probe_notice: notice_slot, .. }
        | SourceMode::MultiTrack { probe_notice: notice_slot, .. } => {
            if notice_slot.is_none() {
                *notice_slot = probe_notice.clone();
            }
        }
        SourceMode::Batch {
            probe_notice: batch_probe_notice,
            cursor_probe_notice,
            ..
        } => {
            *batch_probe_notice = probe_notice.clone();
            *cursor_probe_notice = None;
        }
        SourceMode::Empty => {}
    }

    app.convert.set_source_mode(mode);
    app.convert.source.cue_artifact_audio = cue_artifact_audio;
    app.convert.source.cue_artifact_audio.retain(|path| {
        crate::convert::queue_expansion::path_list_contains_queue_identity(&paths, path)
    });
    app.convert.source.synthetic_cue_artifacts = synthetic_cue_artifacts;
    let retained_synthetic_cue_artifacts = app.convert.source.synthetic_cue_artifacts.clone();
    app.convert.source.synthetic_cue_artifacts.retain(|path| {
        crate::convert::queue_expansion::path_list_contains_queue_identity(&paths, path)
    });
    for artifact in retained_synthetic_cue_artifacts.difference(&app.convert.source.synthetic_cue_artifacts) {
        crate::convert::queue_expansion::cleanup_synthetic_cue_artifact(artifact);
    }
    app.convert.apply_source_defaults();
    let probe_baseline = ConvertProbeBaseline::capture(&app.convert);
    app.recent.record_use_with_db(&first, &app.db);

    if archive_preview_single {
        let pending = create_pending_archive_preview(generation, first.clone());
        let staging_dir = pending.staging_dir.clone();
        let cancel = pending.cancel.clone();
        app.convert.install_pending_archive_preview(pending);
        let password = archive_preview_password_for_path(app, &first);
        let tool_paths = app.manager.config.tool_paths.clone();
        spawn_archive_preview(
            generation,
            first.clone(),
            probe_baseline,
            staging_dir,
            cancel,
            password,
            tool_paths,
            tx.clone(),
        );
    } else if probe_notice.is_some() {
        spawn_convert_source_probe(generation, first.clone(), probe_baseline, tx.clone());
    }

    let batch_paths = app.convert.source.mode.all_paths();
    let _ = app
        .db
        .save_batch_state(&batch_paths, None, None, None, None, None);

    app.previous_screen = Some(AppScreen::Browse);
    app.current_screen = AppScreen::Convert;

    let mut status = if archive_preview_single {
        format!(
            "Extracting archive: {} — review settings, then :commit or :Commit",
            first.file_name().unwrap_or_default().to_string_lossy()
        )
    } else if probe_notice.is_some() {
        format!(
            "Probing: {} — review settings, then :commit or :Commit",
            first.file_name().unwrap_or_default().to_string_lossy()
        )
    } else if from_folder_expansion && expanded_folder_count > 0 {
        format!(
            "expanded {} folder{} into {} files — review settings, then :commit or :Commit",
            expanded_folder_count,
            if expanded_folder_count == 1 { "" } else { "s" },
            path_count
        )
    } else if path_count == 1 {
        "review settings, then :commit or :Commit".to_string()
    } else {
        format!(
            "batch of {} files — review settings, then :commit or :Commit",
            path_count
        )
    };
    if let Some(warning) = expansion_warning {
        status = format!("{} — note: {}", status, warning);
    }
    app.set_status(status);
}

fn finish_browse_queue_review_after_expansion(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    preset: Option<String>,
    queue: QueueExpansionResult,
    expanded_folder_count: usize,
) -> bool {
    let QueueExpansionResult { paths, mut cue_artifact_audio, synthetic_cue_artifacts, expansion_errors } = queue;
    if let Some(err) = expansion_errors.first() {
        if paths.is_empty() {
            crate::convert::queue_expansion::cleanup_synthetic_cue_artifacts(&synthetic_cue_artifacts);
            app.set_status(err.clone());
            return false;
        }
        // Don't set a status here — install_browse_convert_source_paths's
        // final status would overwrite it in the same reducer pass. The
        // errors are passed through below and folded into that status.
    }
    let paths = normalized_path_snapshot(paths);
    if paths.is_empty() {
        crate::convert::queue_expansion::cleanup_synthetic_cue_artifacts(&synthetic_cue_artifacts);
        app.set_status("queue: no supported sources in selection");
        return false;
    }

    cue_artifact_audio.retain(|path| {
        crate::convert::queue_expansion::path_list_contains_queue_identity(&paths, path)
    });

    if let Some(name) = &preset {
        if let Err(msg) = load_queue_preset_into_pills(app, name) {
            if let Some(err) = expansion_errors.first() {
                log::warn!("queue expansion warning dropped by preset failure: {}", err);
            }
            crate::convert::queue_expansion::cleanup_synthetic_cue_artifacts(&synthetic_cue_artifacts);
            app.set_status(msg);
            return false;
        }
    } else {
        app.convert.format = super::app::FormatState::new();
        // Reset to configured defaults, not to an empty state: AppState
        // construction seeds dest_path from config, and the post-load Commit
        // continuation (Convert -> Last used) is blocked without a destination.
        app.convert.output_options = super::app::OutputOptionsState::new();
        app.convert.output_options.dest_path = app.config.conversion.default_destination.clone();
        app.preset.clear_active_preset();
    }

    install_browse_convert_source_paths(
        app,
        tx,
        QueueExpansionResult { paths, cue_artifact_audio, synthetic_cue_artifacts, expansion_errors },
        expanded_folder_count,
        expanded_folder_count > 0,
    );
    app.current_screen == AppScreen::Convert && !app.convert.source.mode.is_empty()
}

fn apply_browse_convert_post_load_action(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    post_load: BrowseConvertPostLoad,
) {
    match post_load {
        BrowseConvertPostLoad::ReviewOnly => {}
        BrowseConvertPostLoad::Commit { start } => {
            if app.current_screen == AppScreen::Convert {
                execute_commit_with_disc_selection_bridge(app, start, tx);
            }
        }
    }
}

pub(crate) fn execute_queue_with_post_load_commit(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    preset: Option<String>,
    start: bool,
) {
    execute_queue_with_post_load(
        app,
        tx,
        preset,
        BrowseConvertPostLoad::Commit { start },
    );
}

/// expand overlay + bulk commit) arrives in Phase 6c/6d.
fn execute_queue(app: &mut AppState, tx: &mpsc::Sender<AppMessage>, preset: Option<String>) {
    execute_queue_with_post_load(app, tx, preset, BrowseConvertPostLoad::ReviewOnly);
}

fn execute_queue_with_post_load(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    preset: Option<String>,
    post_load: BrowseConvertPostLoad,
) {
    match app.current_screen {
        AppScreen::Browse => {
            let dropped_stale_count = app
                .browse
                .prune_stale_multi_selection_for_current_directory();

            // Check the raw selection before collect_selection_for_queue():
            // that collection expands directories with a synchronous recursive
            // walk, which both blocks the reducer on large trees and erases
            // the directory the async-expansion candidate check needs to see.
            let raw_selection: Vec<PathBuf> = app.browse.action_selection_in_current_directory();

            if browse_selection_contains_regular_audio_folder_for_convert(app, &raw_selection) {
                start_browse_convert_folder_expansion(
                    app,
                    tx,
                    BrowseConvertExpansionTarget::ConvertReview { preset, post_load },
                    raw_selection,
                    dropped_stale_count,
                );
                return;
            }

            let selection = app.browse.collect_selection_for_queue();
            if let Some(err) = selection.first_error() {
                app.set_status(status_with_stale_selection_notice(
                    dropped_stale_count,
                    err.to_string(),
                ));
                return;
            }
            if selection.paths.is_empty() {
                app.set_status(status_with_stale_selection_notice(
                    dropped_stale_count,
                    "queue: no selection",
                ));
                return;
            }

            if finish_browse_queue_review_after_expansion(app, tx, preset, selection, 0) {
                apply_browse_convert_post_load_action(app, tx, post_load);
            }
            prefix_current_status_with_stale_selection_notice(app, dropped_stale_count);
        }
        AppScreen::Library => {
            // Placeholder screen. Selection inheritance arrives in 6c.
            if let Some(name) = &preset {
                match load_queue_preset_into_pills(app, name) {
                    Ok(report) => app.set_status(format!("preset loaded: {}{}", name, report.status_suffix())),
                    Err(msg) => app.set_status(msg),
                }
            } else {
                app.set_status("library → Convert inheritance arrives in Phase 6c");
            }
        }
        AppScreen::Convert => {
            // Already on Convert. A preset arg loads in place; without an
            // arg, remind the user to pick files in Browse first.
            if let Some(name) = &preset {
                match load_queue_preset_into_pills(app, name) {
                    Ok(report) => app.set_status(format!("preset loaded: {}{}", name, report.status_suffix())),
                    Err(msg) => app.set_status(msg),
                }
            } else {
                app.set_status("switch to Browse to pick files, then :queue");
            }
        }
        AppScreen::Queue => {
            if let Some(name) = &preset {
                match load_queue_preset_into_pills(app, name) {
                    Ok(report) => app.set_status(format!("preset loaded: {}{}", name, report.status_suffix())),
                    Err(msg) => app.set_status(msg),
                }
            } else {
                app.set_status(":queue: switch to Browse to pick files first");
            }
        }
        _ => {
            app.set_status(":queue not supported on this screen");
        }
    }
}

/// Merge Convert-screen multi-track state into pipeline source options.
///
/// `commit_batch()` may construct or attach a full `PipelineRequest` before
/// `execute_commit()` can add UI-only state such as the selected DVD-Audio
/// presentation. This helper is intentionally usable for both newly-created
/// and already-prebuilt requests, so the final request that reaches the
/// processor has the same source selection the user saw in the Convert screen.
fn apply_multitrack_convert_state_to_source_options(
    mode: &SourceMode,
    source: &mut crate::convert::pipeline::SourceOptions,
    selected_track_numbers: Option<&std::collections::BTreeSet<u32>>,
) {
    source.track_selection = match selected_track_numbers {
        Some(numbers) => crate::convert::pipeline::TrackSelection::Set(numbers.clone()),
        None => crate::convert::pipeline::TrackSelection::All,
    };

    super::disc_browser::apply_source_mode_disc_selection_to_source_options(mode, source);
}

/// Apply a queue-time CUE sidecar override to source options after any
/// Convert-screen source transforms have run.
fn apply_queue_item_cue_sidecar_override_to_source_options(
    item: &crate::convert::ConversionItem,
    source: &mut crate::convert::pipeline::SourceOptions,
) {
    if let Some(cue_sidecar_override) = item.cue_sidecar_override {
        source.cue_sidecar = cue_sidecar_override;
    }
}

/// Build the post-publish companion-copy policy from the already-projected
/// conversion options that this commit path enqueues.  This keeps manually
/// patched/prebuilt PipelineRequest values consistent with ConversionItem
/// options, including explicit empty strings that disable loose files or
/// folders via the legacy boolean gates.
#[must_use]
fn companion_copy_policy_from_conversion_options(
    options: &crate::convert::formats::ConversionOptions,
) -> crate::convert::pipeline::CompanionCopyPolicy {
    crate::convert::pipeline::CompanionCopyPolicy {
        extensions: options.effective_companion_extensions(),
        folders: options.effective_companion_folders(),
        exclude_files: options.effective_companion_exclude_files(),
    }
}


fn archive_preview_track_relative_path(
    preview: &ArchivePreview,
    track: &PreviewTrack,
) -> PathBuf {
    if let Ok(relative) = track.path.strip_prefix(&preview.staging_dir) {
        return relative.to_path_buf();
    }
    if let Ok(canonical_staging) = std::fs::canonicalize(&preview.staging_dir) {
        if let Ok(relative) = track.path.strip_prefix(&canonical_staging) {
            return relative.to_path_buf();
        }
    }
    track
        .path
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("track-{}", track.original_name)))
}

fn archive_metadata_overrides_from_source_mode(
    mode: &SourceMode,
) -> Vec<crate::convert::pipeline::ArchiveTrackMetadataOverride> {
    let SourceMode::MultiTrack {
        archive_preview: Some(preview),
        ..
    } = mode
    else {
        return Vec::new();
    };

    preview
        .tracks
        .iter()
        .enumerate()
        .filter_map(|(idx, track)| {
            let relative_path = archive_preview_track_relative_path(preview, track);
            let metadata = &track.metadata;
            let original = &track.original_metadata;
            let override_set = crate::convert::pipeline::ArchiveTrackMetadataOverride {
                source_ordinal: (idx + 1) as u32,
                relative_path,
                title: crate::convert::pipeline::MetadataTextOverride::from_optional_change(
                    &original.title,
                    &metadata.title,
                ),
                artist: crate::convert::pipeline::MetadataTextOverride::from_optional_change(
                    &original.artist,
                    &metadata.artist,
                ),
                album: crate::convert::pipeline::MetadataTextOverride::from_optional_change(
                    &original.album,
                    &metadata.album,
                ),
                genre: crate::convert::pipeline::MetadataTextOverride::from_optional_change(
                    &original.genre,
                    &metadata.genre,
                ),
                date: crate::convert::pipeline::MetadataTextOverride::from_optional_change(
                    &original.year,
                    &metadata.year,
                ),
            };
            override_set.has_changes().then_some(override_set)
        })
        .collect()
}

/// Apply the selected disc presentation stored in the Convert source mode to
/// freshly-built pipeline source options.
pub fn apply_convert_source_disc_selection_to_source_options(
    mode: &SourceMode,
    options: &mut crate::convert::pipeline::SourceOptions,
) {
    super::disc_browser::apply_source_mode_disc_selection_to_source_options(mode, options);
}

/// Return `options` with any Convert-screen disc-browser presentation selection
/// applied.
#[must_use]
pub fn source_options_with_convert_source_disc_selection(
    mode: &SourceMode,
    mut options: crate::convert::pipeline::SourceOptions,
) -> crate::convert::pipeline::SourceOptions {
    apply_convert_source_disc_selection_to_source_options(mode, &mut options);
    options
}

/// Apply the Convert-screen disc selection directly to a `PipelineRequest`.
pub fn apply_convert_source_disc_selection_to_pipeline_request(
    mode: &SourceMode,
    request: &mut crate::convert::pipeline::PipelineRequest,
) {
    apply_convert_source_disc_selection_to_source_options(mode, &mut request.source);
}

/// Return `request` after applying the Convert-screen selected presentation to
/// `request.source`.
#[must_use]
pub fn pipeline_request_with_convert_source_disc_selection(
    mode: &SourceMode,
    mut request: crate::convert::pipeline::PipelineRequest,
) -> crate::convert::pipeline::PipelineRequest {
    apply_convert_source_disc_selection_to_pipeline_request(mode, &mut request);
    request
}

/// Apply the Convert-screen selected disc presentation at the queue/request
/// boundary.
pub fn execute_commit_with_disc_selection_bridge(
    app: &mut AppState,
    start: bool,
    tx: &mpsc::Sender<AppMessage>,
) {
    let mode = app.convert.source.mode.clone();
    execute_command(
        app,
        Command::CommitWithSourceOptionsTransform {
            start,
            transform: Box::new(move |source_options| {
                source_options_with_convert_source_disc_selection(&mode, source_options)
            }),
        },
        tx,
    );
}

/// Apply the Convert-screen selected disc presentation to a request at the
/// queue/request boundary.
#[must_use]
pub fn commit_pipeline_request_with_convert_source_disc_selection(
    mode: &SourceMode,
    request: crate::convert::pipeline::PipelineRequest,
) -> crate::convert::pipeline::PipelineRequest {
    pipeline_request_with_convert_source_disc_selection(mode, request)
}

/// Execute a `:commit` / `:Commit` command. Only valid on the Convert
/// screen with a source file or batch loaded. Enqueues the batch (and
/// optionally starts processing), then navigates away:
/// - `:commit` (start=false): enqueue, return to origin (previous_screen)
/// - `:Commit` (start=true): enqueue, start processing, jump to Queue
///
/// Phase 6d: reads `source.mode.all_paths()` which yields 1 path for
/// Single, N for Batch. The pill state (options) is applied uniformly
/// to all files in the commit — the whole point of reviewing once per
/// batch.
fn execute_commit(app: &mut AppState, tx: &mpsc::Sender<AppMessage>, start: bool) {
    execute_commit_with_source_options_transform(app, tx, start, None);
}

fn execute_commit_with_source_options_transform(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    start: bool,
    source_options_transform: Option<
        Box<
            dyn FnOnce(crate::convert::pipeline::SourceOptions) -> crate::convert::pipeline::SourceOptions
                + Send,
        >,
    >,
) {
    if app.current_screen != AppScreen::Convert {
        app.set_status(":commit only works on the Convert screen");
        return;
    }

    // Determine what to commit from the current source mode.
    let batch = app.convert.source.mode.all_paths();
    let archive_preview_staging = app.convert.source.mode.archive_preview_staging_dir().cloned();
    let archive_metadata_overrides =
        archive_metadata_overrides_from_source_mode(&app.convert.source.mode);
    if batch.is_empty() {
        app.set_status("nothing to commit — no source file loaded");
        return;
    }

    // Block commit when no destination path is set.
    if app.convert.output_options.dest_path.is_none() {
        app.set_status("no destination path set — enter a path in the output options");
        return;
    }

    // Block commit when all tracks are deselected.
    if let SourceMode::MultiTrack { selected, .. } = &app.convert.source.mode {
        if selected.iter().all(|s| !s) {
            app.set_status("no tracks selected");
            return;
        }
    }

    // Build options from the current pill state, then project the editable
    // Output Options companion fields into the ConversionOptions that are
    // actually enqueued.  The post-publish copy stage reads
    // ConversionOptions::effective_companion_*(), so this command path must
    // share the same canonical projection helper used by the direct queue-add
    // paths instead of relying on ConversionOptions::default() semantics.
    let mut options = super::convert_actions::pills_to_options(
        &app.convert.format,
        &app.convert.output_options,
        &app.config,
    );
    app.convert
        .output_options
        .apply_companion_copying_to_conversion_options(&mut options);
    options.album_artist_override = app
        .convert
        .metadata
        .album_artist_for_conversion
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    let format_name = options.output_format.name();

    // Build the exact post-admission PipelineRequest projection before queue
    // admission. The manager applies this closure while holding the same queue
    // write guard that inserts each item, so :commit never has to reacquire a
    // blocking queue lock from the async TUI event loop.
    use crate::convert::pipeline::*;
    let mut source_options_transform = source_options_transform;
    let mut has_deselected_tracks = false;
    let mut selected_track_numbers = std::collections::BTreeSet::new();

    if let SourceMode::MultiTrack {
        tracks,
        selected,
        ..
    } = &app.convert.source.mode
    {
        has_deselected_tracks = selected.iter().any(|s| !s);
        selected_track_numbers = tracks
            .iter()
            .zip(selected.iter())
            .filter(|(_, &sel)| sel)
            .map(|(t, _)| t.number)
            .collect();
    }

    let mut commit_source = SourceOptions {
        archive_password: None,
        sacd_area: None,
        dvda_group: None,
        dvda_group_selection: DvdaGroupSelection::Default,
        dvda_assume_decrypted: false,
        dvda_downmix_policy: DvdaDownmixPolicy::Auto,
        cue_sidecar: CueSidecarPolicy::PreferSidecar,
        track_selection: TrackSelection::All,
        dvdv_vts: None,
        dvdv_title: None,
        dvdv_audio_stream: None,
        dvdv_angle: None,
        bluray_playlist: None,
        bluray_audio_pid: None,
        bluray_audio_stream: None,
        bluray_angle: None,
    };

    if matches!(&app.convert.source.mode, SourceMode::MultiTrack { .. }) {
        apply_multitrack_convert_state_to_source_options(
            &app.convert.source.mode,
            &mut commit_source,
            if has_deselected_tracks {
                Some(&selected_track_numbers)
            } else {
                None
            },
        );
    }

    if let Some(transform) = source_options_transform.take() {
        commit_source = transform(commit_source);
    }

    let rg_enabled = options.calculate_replaygain;
    let companion_policy = companion_copy_policy_from_conversion_options(&options);
    let pipeline_settings = options.pipeline_settings.clone().unwrap_or_else(|| {
        crate::convert::pipeline::unified_request::pipeline_settings_from_legacy_options(&options)
    });
    let canonical_naming_template = options.effective_naming_template("%NN% - %TITLE%");
    let mut archive_passwords_by_path: BTreeMap<std::path::PathBuf, String> = BTreeMap::new();
    for path in &batch {
        if crate::is_encrypted_archive_ext(path) {
            if let Some(password) = app
                .archive_passwords
                .get(path)
                .cloned()
                .or_else(|| {
                    app.keychain.ensure_loaded();
                    app.keychain.passwords.first().cloned()
                })
                .or_else(|| app.config.conversion.archive_password.clone())
            {
                archive_passwords_by_path.insert(path.clone(), password);
            }
        }
    }

    // Enqueue the whole batch via the manager transaction API. CUE sidecar
    // override metadata lives on the Convert source state, because that state
    // is the ownership boundary between Browse expansion and Commit. The
    // transaction result is authoritative: it names exact queue item ids
    // admitted, exact synthetic artifacts transferred to manager ownership,
    // exact artifacts that remain caller-owned, skipped inputs, and rollback
    // state. The TUI must not reconstruct ownership with a post-commit path
    // scan.
    let cue_artifact_audio = app.convert.source.cue_artifact_audio.clone();
    let source_synthetic_cue_artifacts = app.convert.source.synthetic_cue_artifacts.clone();
    let transaction = app.manager.commit_batch_with_cue_artifacts(
        &batch,
        &cue_artifact_audio,
        &source_synthetic_cue_artifacts,
        &options,
        |item| {
            if item.input_path == batch[0] {
                item.pre_extracted_staging = archive_preview_staging.clone();
                item.archive_metadata_overrides = archive_metadata_overrides.clone();
            }

            if let Some(password) = archive_passwords_by_path.get(&item.input_path) {
                item.archive_password = Some(password.clone());
            }
            let mut item_source = commit_source.clone();
            if let Some(ref pw) = item.archive_password {
                item_source.archive_password = Some(SecretString::new(pw.clone()));
            }
            apply_queue_item_cue_sidecar_override_to_source_options(item, &mut item_source);

            if let Some(existing_req) = item.pipeline_request.as_mut() {
                // `commit_batch_with_cue_artifacts()` may already have attached
                // a full PipelineRequest from an earlier admission path. Replace
                // the source and request metadata inside the admission
                // transaction so transform/state projection cannot be skipped.
                existing_req.container = item.input_path.clone();
                existing_req.item_id = item.id.clone();
                existing_req.job_id = format!("job-{}", item.id);
                existing_req.source = item_source;
                existing_req.pre_extracted_staging = item.pre_extracted_staging.clone();
                existing_req.archive_metadata_overrides = item.archive_metadata_overrides.clone();
                existing_req.naming.template = canonical_naming_template.clone();
                existing_req.naming.folder_template = options.folder_template.clone();
                existing_req.settings = pipeline_settings.clone();
                existing_req.merge = options.merge_to_single;
                existing_req.companion = companion_policy.clone();
                existing_req.actions = options.actions.clone();
            } else {
                let output_root = options.output_dir.clone()
                    .map(|p| crate::convert::pipeline::unified_request::expand_tilde(&p))
                    .unwrap_or_else(|| {
                        item.input_path
                            .parent()
                            .unwrap_or(std::path::Path::new("."))
                            .to_path_buf()
                    });
                item.pipeline_request = Some(PipelineRequest {
                    actions: options.actions.clone(),
                    worker_count: None,
                    scratch_staging: None,
                    job_id: format!("job-{}", item.id),
                    item_id: item.id.clone(),
                    container: item.input_path.clone(),
                    source: item_source,
                    settings: pipeline_settings.clone(),
                    merge: options.merge_to_single,
                    output_root: output_root.clone(),
                    naming: NamingPolicy {
                        template: canonical_naming_template.clone(),
                        folder_template: options.folder_template.clone(),
                        per_album_subdir: true,
                        collision_policy: NamingCollisionPolicy::Fail,
                    },
                    publish: PublishPolicy {
                        overwrite: OverwritePolicy::FailIfExists,
                        same_filesystem_required: false,
                        write_manifest: false,
                    },
                    log: LogPolicy {
                        root: output_root.join(".tonepoet-logs"),
                        write_for_blocked: true,
                        write_json_log: false,
                        write_conversion_log: options.write_log_file,
                    },
                    stages: StagePolicy {
                        metadata: if options.preserve_metadata {
                            StageRequirement::Enabled
                        } else {
                            StageRequirement::Disabled
                        },
                        replaygain: if rg_enabled {
                            StageRequirement::Enabled
                        } else {
                            StageRequirement::Disabled
                        },
                        features: StageRequirement::Enabled,
                        generate_cue: false,
                    },
                    failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
                    pre_extracted_staging: item.pre_extracted_staging.clone(),
                    archive_metadata_overrides: item.archive_metadata_overrides.clone(),
                    metadata_overrides: Default::default(),
                    batch_resolved_identity: None,
                    container_extension: options.container_extension.clone(),
                    container_ffmpeg_flags: options.container_ffmpeg_flags.clone(),
                    album_batch: None,
                    album_batch_track: None,
                    expected_album_track_count: None,
                    suppress_incremental_conversion_log_append: false,
                    companion: companion_policy.clone(),
                });
            }
        },
    );
    let outcome = transaction.outcome.clone();
    app.convert.source.synthetic_cue_artifacts = transaction.artifacts_remaining_caller_owned.clone();

    // Nothing enqueued → don't clear state or navigate; user sees error. The
    // transaction has either made no queue mutation or has already rolled back
    // exact newly admitted ids before returning.
    if outcome.enqueued == 0 {
        if let Some(rollback) = &transaction.rollback {
            if !rollback.completed {
                app.set_status(format!(
                    "commit failed: rollback incomplete for {} item(s)",
                    rollback.failed_item_ids.len()
                ));
                return;
            }
        }
        if outcome.skipped > 0 && outcome.errors == 0 {
            app.set_status(format!(
                "commit: all {} file(s) already queued",
                outcome.skipped
            ));
        } else if let Some(ref err) = outcome.last_error {
            app.set_status(format!(
                "commit failed: {}",
                err
            ));
        } else {
            app.set_status(format!(
                "commit failed: {} errors, {} skipped",
                outcome.errors, outcome.skipped
            ));
        }
        return;
    }

    // Retain only caller-owned artifacts that the transaction explicitly says
    // remain with SourceState. Artifacts transferred to manager ownership are
    // removed from source ownership without cleanup; there is no ownerless or
    // duplicate-ownership intermediate state. All PipelineRequest projection was
    // already applied inside the manager admission transaction while the queue
    // write guard was held, so the async TUI never reacquires a blocking queue lock here.
    let synthetic_artifact_lifecycle_warning: Option<String> = None;

    // Build the success status message.
    let mut success_status = if batch.len() == 1 {
        let filename = batch[0]
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        format!("Queued: {} → {}", filename, format_name)
    } else {
        let mut parts = vec![format!("Queued {} → {}", outcome.enqueued, format_name)];
        if outcome.skipped > 0 {
            parts.push(format!("{} skipped", outcome.skipped));
        }
        if outcome.errors > 0 {
            parts.push(format!("{} errors", outcome.errors));
        }
        if outcome.previously_converted > 0 {
            parts.push(format!("{} re-converting", outcome.previously_converted));
        }
        parts.join(", ")
    };
    if let Some(warning) = synthetic_artifact_lifecycle_warning {
        success_status.push_str("; ");
        success_status.push_str(&warning);
    }
    if outcome.errors > 0 {
        if let Some(ref err) = outcome.last_error {
            success_status.push_str("; last error: ");
            success_status.push_str(err);
        }
    }

    // Clear source pane so a subsequent `:queue` arrives fresh. Transfer any
    // archive preview staging to the queued item before dropping the source.
    let _ = app.convert.source.mode.disarm_archive_preview_cleanup();
    app.convert.source.cleanup_synthetic_cue_artifacts();
    app.convert.set_source_mode(SourceMode::Empty);
    app.convert.source.cue_artifact_audio.clear();
    app.convert.metadata = MetadataState::default();
    let _ = app.db.clear_batch_state();
    app.save_queue();

    // Remove only the committed paths from browse.multi_selected so the
    // user's unrelated selection state is preserved. This handles:
    // - Multi-file batches sourced from multi_selected (all committed
    //   paths get removed)
    // - Single-file commits where the file happened to be the only
    //   entry in multi_selected (that entry gets removed)
    // - Single-file commits from `:e` / recent-files / Browse Enter for
    //   a file that is NOT in multi_selected (nothing removed)
    app.browse.multi_selected.retain(|p| !batch.contains(p));

    if start {
        // :Commit — start processing if not already active, land on Queue.
        if !app.processing_active {
            super::convert_actions::start_processing(app, tx);
            let mut status = format!(
                "Processing {} file(s) → {}",
                outcome.enqueued, format_name
            );
            if outcome.errors > 0 {
                status.push_str(&format!("; {} error(s)", outcome.errors));
                if let Some(err) = &outcome.last_error {
                    status.push_str(&format!("; last error: {err}"));
                }
            }
            app.set_status(status);
        } else {
            app.set_status(format!("{} (processing active)", success_status));
        }
        app.previous_screen = None;
        app.current_screen = AppScreen::Queue;
    } else {
        // :commit — return to origin, defaulting to Browse.
        app.set_status(success_status);
        let origin = app.previous_screen.take().unwrap_or(AppScreen::Browse);
        app.current_screen = origin;
        if origin == AppScreen::Browse {
            app.browse.probe_current_with_db(tx, Some(&app.db));
        }
    }
}

/// Execute a `:go` / `:start` command — begin processing whatever's in
/// the queue. No new batch involved. No-op if already processing or if
/// nothing is ready. Works from any screen.
fn execute_go(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    if app.processing_active {
        app.set_status("already processing");
        return;
    }
    // start_processing handles the zero-ready-items case itself.
    super::convert_actions::start_processing(app, tx);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeleteCommandDispatch {
    NotBrowse,
    ArchiveStagedDelete,
    FilesystemPermanentDelete,
}

fn delete_command_dispatch_for(
    current_screen: AppScreen,
    is_in_archive: bool,
) -> DeleteCommandDispatch {
    if current_screen != AppScreen::Browse {
        DeleteCommandDispatch::NotBrowse
    } else if is_in_archive {
        DeleteCommandDispatch::ArchiveStagedDelete
    } else {
        DeleteCommandDispatch::FilesystemPermanentDelete
    }
}

fn delete_command_dispatch(app: &AppState) -> DeleteCommandDispatch {
    delete_command_dispatch_for(app.current_screen, app.browse.is_in_archive())
}

/// Execute `:del` / `:delete`. Filesystem browsing uses a permanent-delete
/// confirmation; archive browsing uses the archive-aware staged edit path.
fn execute_delete(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    match delete_command_dispatch(app) {
        DeleteCommandDispatch::NotBrowse => {
            app.set_status(":del only works on the Browse screen");
        }
        DeleteCommandDispatch::ArchiveStagedDelete => {
            super::keybindings::start_browse_archive_entry_delete(app, tx);
        }
        DeleteCommandDispatch::FilesystemPermanentDelete => {
            let selection = collect_selection_for_file_ops_scoped(app);
            let paths = selection.paths;
            if paths.is_empty() {
                app.set_status("no files selected");
                return;
            }

            let count = paths.len();
            app.active_overlay = ActiveOverlay::Confirmation {
                message: format!(
                    "Permanently delete {} item(s)?\n\nThis cannot be undone.",
                    count
                ),
                action: ConfirmAction::DeleteSelection(paths),
            };
            surface_stale_selection_notice(app, selection.dropped_stale_count);
        }
    }
}

#[cfg(test)]
mod delete_command_dispatch_tests {
    use super::*;

    #[test]
    fn archive_del_uses_staged_archive_delete_not_filesystem_confirmation() {
        let dispatch = delete_command_dispatch_for(AppScreen::Browse, true);

        assert_eq!(dispatch, DeleteCommandDispatch::ArchiveStagedDelete);
        assert_ne!(dispatch, DeleteCommandDispatch::FilesystemPermanentDelete);
    }

    #[test]
    fn filesystem_del_still_uses_permanent_delete_confirmation() {
        assert_eq!(
            delete_command_dispatch_for(AppScreen::Browse, false),
            DeleteCommandDispatch::FilesystemPermanentDelete
        );
    }

    #[test]
    fn del_is_rejected_outside_browse() {
        assert_eq!(
            delete_command_dispatch_for(AppScreen::Queue, false),
            DeleteCommandDispatch::NotBrowse
        );
    }
}

/// Execute a `:cp` / `:mv` command. Collects selected files on Browse,
/// then either opens a directory picker for the destination (if no arg)
/// or performs the operation directly (if arg provided).
fn execute_file_op(
    app: &mut AppState,
    dest: &str,
    force: bool,
    is_move: bool,
    tx: &mpsc::Sender<AppMessage>,
) {
    if app.current_screen != AppScreen::Browse {
        let cmd = if is_move { ":mv" } else { ":cp" };
        app.set_status(format!("{} only works on the Browse screen", cmd));
        return;
    }

    // Collect sources: multi-selected or cursor entry. Unlike
    // collect_selection_for_queue, we DON'T expand directories — copy/move
    // operates on the entry itself, not its contents.
    let selection = collect_selection_for_file_ops_scoped(app);
    let sources = selection.paths;
    let dropped_stale_count = selection.dropped_stale_count;
    if sources.is_empty() {
        app.set_status("no files selected for copy/move");
        return;
    }

    if dest.trim().is_empty() {
        open_file_picker_for_copy_move(app, sources, force, is_move, dropped_stale_count);
        return;
    }

    let target = if is_move {
        TextEditTarget::BrowseMove { sources, force }
    } else {
        TextEditTarget::BrowseCopy { sources, force }
    };

    // Destination provided — perform directly through the tx-aware path so
    // the browse screen gets the same immediate probe refresh as text-edit
    // completions and picker completions.
    let dest_expanded = expand_path(dest.trim());
    super::keybindings::apply_file_op_with_tx(app, target, &dest_expanded, tx);
    surface_stale_selection_notice(app, dropped_stale_count);
}

/// Open the reusable file picker as a Browse-screen destination chooser for
/// copy/move operations. The source set is captured by the caller before this
/// function installs the modal overlay, making the eventual completion
/// idempotent with respect to later browse cursor or selection changes.
pub(super) fn open_file_picker_for_copy_move(
    app: &mut AppState,
    sources: Vec<PathBuf>,
    force: bool,
    is_move: bool,
    dropped_stale_count: usize,
) {
    if app.current_screen != AppScreen::Browse {
        let cmd = if is_move { ":mv" } else { ":cp" };
        app.set_status(format!("{} only works on the Browse screen", cmd));
        return;
    }
    if sources.is_empty() {
        app.set_status("no files selected for copy/move");
        return;
    }

    let start_dir = if app.browse.current_dir.is_dir() {
        app.browse.current_dir.clone()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    let title = if is_move { "Move to..." } else { "Copy to..." }.to_string();
    let picker = tui_file_picker::FilePickerState::new(tui_file_picker::FilePickerConfig {
        start_dir,
        filter: tui_file_picker::FilePickerFilter::All,
        title: title.clone(),
        theme: super::keybindings::file_picker_theme_from_theme(&app.theme),
        selection_mode: tui_file_picker::FilePickerSelectionMode::Directories,
        // Destination pickers always start in explicit "Ask" mode. Even `:cp!`
        // / `:mv!` force commands should not silently pre-select overwrite after
        // the user has chosen to go through the interactive destination picker;
        // the user can still click "Overwrite" before starting the operation.
        conflict_policy: Some(tui_file_picker::ConflictPolicyPreset::Ask),
        operation_policy: directory_destination_picker_policy(),
        ..tui_file_picker::FilePickerConfig::default()
    });
    let purpose = if is_move {
        FilePickerPurpose::MoveTo { sources, force }
    } else {
        FilePickerPurpose::CopyTo { sources, force }
    };
    app.active_overlay = ActiveOverlay::FilePicker(MetadataFilePickerState::new(purpose, picker));
    let status = format!("{}: choose a destination folder", title);
    app.set_status(status_with_stale_selection_notice(
        dropped_stale_count,
        status,
    ));
}

fn directory_destination_picker_policy() -> tui_file_picker::FileOperationPolicy {
    tui_file_picker::FileOperationPolicy {
        allow_new_file: false,
        allow_new_folder: true,
        allow_cut: false,
        allow_copy: false,
        allow_paste: false,
        allow_delete: false,
        symlink_copy: tui_file_picker::SymlinkCopyPolicy::Reject,
        cross_device_cut: tui_file_picker::CrossDeviceCutPolicy::Reject,
        delete: tui_file_picker::DeletePolicy::FilesAndEmptyDirectories,
    }
}

/// Open the Convert output-options destination picker. The picker starts in the
/// currently configured destination directory when possible, otherwise in the
/// user's home directory/current directory fallback.
pub(super) fn open_file_picker_for_convert_destination(app: &mut AppState) {
    let start_dir = app
        .convert
        .output_options
        .dest_path
        .as_ref()
        .filter(|path| path.is_dir())
        .cloned()
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let picker = tui_file_picker::FilePickerState::new(tui_file_picker::FilePickerConfig {
        start_dir,
        filter: tui_file_picker::FilePickerFilter::All,
        title: "Select destination folder".to_string(),
        theme: super::keybindings::file_picker_theme_from_theme(&app.theme),
        selection_mode: tui_file_picker::FilePickerSelectionMode::Directories,
        operation_policy: directory_destination_picker_policy(),
        ..tui_file_picker::FilePickerConfig::default()
    });
    app.active_overlay = ActiveOverlay::FilePicker(MetadataFilePickerState::new(
        FilePickerPurpose::SelectDestination,
        picker,
    ));
    app.set_status("choose a destination folder");
}

/// Open the Convert preset picker in load mode.
pub(super) fn open_file_picker_for_preset_load(app: &mut AppState) {
    let start_dir = super::presets::presets_dir();
    let _ = fs::create_dir_all(&start_dir);
    let picker = tui_file_picker::FilePickerState::new(tui_file_picker::FilePickerConfig {
        start_dir,
        filter: tui_file_picker::FilePickerFilter::Custom { label: "Presets".to_string(), extensions: vec!["toml".to_string()] },
        title: "Load preset".to_string(),
        theme: super::keybindings::file_picker_theme_from_theme(&app.theme),
        selection_mode: tui_file_picker::FilePickerSelectionMode::Files,
        hide_extension: Some(".toml".to_string()),
        operation_policy: preset_picker_policy(),
        ..tui_file_picker::FilePickerConfig::default()
    });
    app.active_overlay = ActiveOverlay::FilePicker(MetadataFilePickerState::new(
        FilePickerPurpose::SelectPreset,
        picker,
    ));
    app.set_status("choose a preset");
}

/// Open the Convert preset picker in reusable save-as mode.
pub(super) fn open_file_picker_for_preset_save_as(app: &mut AppState) {
    let start_dir = super::presets::presets_dir();
    let _ = fs::create_dir_all(&start_dir);
    let default_name = app.preset.active_preset.clone().unwrap_or_default();
    let picker = tui_file_picker::FilePickerState::new(tui_file_picker::FilePickerConfig {
        start_dir,
        filter: tui_file_picker::FilePickerFilter::Custom { label: "Presets".to_string(), extensions: vec!["toml".to_string()] },
        title: "Save preset".to_string(),
        theme: super::keybindings::file_picker_theme_from_theme(&app.theme),
        selection_mode: tui_file_picker::FilePickerSelectionMode::Files,
        hide_extension: Some(".toml".to_string()),
        save_mode: Some(tui_file_picker::SaveModeConfig {
            default_name,
            confirm_overwrite: true,
            hide_extension: Some(".toml".to_string()),
            style: tui_file_picker::SaveModeStyle::Inline,
        }),
        operation_policy: preset_picker_policy(),
        ..tui_file_picker::FilePickerConfig::default()
    });
    app.active_overlay = ActiveOverlay::FilePicker(MetadataFilePickerState::new(
        FilePickerPurpose::SavePreset,
        picker,
    ));
    app.set_status("enter a preset name and press Enter to save");
}

fn preset_picker_policy() -> tui_file_picker::FileOperationPolicy {
    tui_file_picker::FileOperationPolicy {
        allow_new_file: false,
        allow_new_folder: false,
        allow_cut: false,
        allow_copy: false,
        allow_paste: false,
        allow_delete: true,
        symlink_copy: tui_file_picker::SymlinkCopyPolicy::Reject,
        cross_device_cut: tui_file_picker::CrossDeviceCutPolicy::Reject,
        delete: tui_file_picker::DeletePolicy::FilesAndEmptyDirectories,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ScopedFileSelection {
    pub paths: Vec<PathBuf>,
    pub dropped_stale_count: usize,
}

fn stale_selection_notice(dropped_stale_count: usize) -> Option<String> {
    if dropped_stale_count == 0 {
        None
    } else {
        Some(format!(
            "Ignored {} stale selection mark{} outside the current directory",
            dropped_stale_count,
            if dropped_stale_count == 1 { "" } else { "s" }
        ))
    }
}

pub(super) fn status_with_stale_selection_notice(
    dropped_stale_count: usize,
    status: impl Into<String>,
) -> String {
    let status = status.into();
    match stale_selection_notice(dropped_stale_count) {
        Some(notice) => format!("{notice}; {status}"),
        None => status,
    }
}

pub(super) fn surface_stale_selection_notice(app: &mut AppState, dropped_stale_count: usize) {
    if let Some(notice) = stale_selection_notice(dropped_stale_count) {
        app.set_status(notice);
    }
}

pub(super) fn prefix_current_status_with_stale_selection_notice(
    app: &mut AppState,
    dropped_stale_count: usize,
) {
    let Some(notice) = stale_selection_notice(dropped_stale_count) else {
        return;
    };
    let current = app
        .status_message
        .as_ref()
        .map(|(message, _)| message.clone())
        .unwrap_or_default();
    if current.starts_with(&notice) {
        return;
    }
    if current.is_empty() {
        app.set_status(notice);
    } else {
        app.set_status(format!("{notice}; {current}"));
    }
}

/// Collect selected entries for file ops (copy/move/delete/rename). Unlike
/// `collect_selection_for_queue`, directories are NOT expanded — the
/// op targets the directory itself. Multi-select marks are defensively scoped
/// to the current Browse directory before any destructive consumer sees them;
/// a stale cross-directory mark is never actionable even if navigation forgot
/// to clear it. This immutable helper remains useful for pure planning paths;
/// mutating/destructive consumers should use
/// `collect_selection_for_file_ops_scoped()` so stale raw state is repaired and
/// the user can be told when an upstream invariant failed.
pub(super) fn collect_selection_for_file_ops(app: &AppState) -> Vec<PathBuf> {
    if let Some(paths) = app.bulk_guard_frozen_paths.as_ref() {
        return paths.clone();
    }
    app.browse.action_selection_in_current_directory()
}

/// Mutating file-op selection boundary. It preserves the bulk-guard frozen
/// snapshot contract, otherwise prunes stale raw Browse marks before deriving
/// the actionable path list. The returned count lets destructive consumers
/// surface observability instead of silently hiding an invariant failure.
pub(super) fn collect_selection_for_file_ops_scoped(app: &mut AppState) -> ScopedFileSelection {
    if let Some(paths) = app.bulk_guard_frozen_paths.as_ref() {
        return ScopedFileSelection {
            paths: paths.clone(),
            dropped_stale_count: 0,
        };
    }
    let dropped_stale_count = app.browse.prune_stale_multi_selection_for_current_directory();
    let paths = app.browse.action_selection_in_current_directory();
    ScopedFileSelection {
        paths,
        dropped_stale_count,
    }
}

/// Phase C-2 seed values for `search_releases_by_query` extracted
/// from a SACD metadata editor's current state. Per-track rows
/// with mixed values are skipped — MB's Lucene query has no notion
/// of "or these track-level values", so a divergent ARTIST column
/// can't seed a useful album-level search. `ALBUMARTIST` is the
/// canonical album-level row and is preferred over `ARTIST` when
/// both are present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SacdMbSeed {
    pub artist: String,
    pub album: String,
    pub catalog: Option<String>,
    pub year: Option<String>,
}

/// Extract a search-query seed from the editor's current entries.
/// Returns `None` when none of ARTIST/ALBUMARTIST/ALBUM/CATALOGNUMBER/
/// DATE yields a usable value (all empty or all mixed). MB's text
/// search can be partial — having only an album title still produces
/// results — so any single non-empty term is enough.
pub(super) fn seed_sacd_mb_query(state: &super::app::MetadataEditorState) -> Option<SacdMbSeed> {
    let entry_value = |k: &str| -> Option<String> {
        state.active_surface()
            .entries
            .iter()
            .find(|e| e.display_key == k)
            .map(|e| e.value.trim().to_string())
            .filter(|s| !s.is_empty() && s != "<multiple values>")
    };
    let artist = entry_value("ALBUMARTIST")
        .or_else(|| entry_value("ARTIST"))
        .unwrap_or_default();
    let album = entry_value("ALBUM").unwrap_or_default();
    let catalog = entry_value("CATALOGNUMBER");
    // DATE on SACDs is a year string like "1959"; MB's `date:`
    // Lucene term accepts that form directly so no further parsing.
    let year = entry_value("DATE");

    if artist.is_empty() && album.is_empty() && catalog.is_none() && year.is_none() {
        return None;
    }
    Some(SacdMbSeed {
        artist,
        album,
        catalog,
        year,
    })
}

/// Convert per-track SACD durations (seconds) to a sector vector
/// in the shape `lookup_release_by_toc` and `build_mb_toc` expect:
/// `[off1, off2, …, offN, leadout]`, where `off1 = 150` (the standard
/// 2-second CD pre-gap) and each subsequent offset is the prior plus
/// the prior track's length in CD frames (75 fps).
///
/// SACD `PlayTime` uses CD-style 75 fps natively, so the conversion
/// `(seconds * 75.0).round() as u32` matches MB's expected geometry
/// directly — no DSD-frame complication. `saturating_add` guards
/// against arithmetic overflow on pathological inputs (a 24-hour
/// compilation is ~6.5M frames, well within u32, so this is purely
/// defensive).
pub fn sacd_durations_to_sectors(durations: &[f64]) -> Vec<u32> {
    durations_to_cd_sectors(durations.iter().copied())
}

/// Convert per-track durations in seconds to MusicBrainz-compatible CD-frame
/// offsets: `[off1, off2, ..., offN, leadout]`, with `off1 = 150`.
pub fn durations_to_cd_sectors<I>(durations: I) -> Vec<u32>
where
    I: IntoIterator<Item = f64>,
{
    let mut sectors = Vec::new();
    let mut cur = CD_TOC_PREGAP_FRAMES;
    sectors.push(cur);
    for duration in durations {
        let frames = (duration * CD_FRAMES_PER_SECOND).round().max(0.0) as u32;
        cur = cur.saturating_add(frames);
        sectors.push(cur);
    }
    sectors
}

/// Build a MusicBrainz-compatible synthetic CD TOC from a DVD-Video source.
///
/// This feeds the DVD-Video `:tags-mb` primary path:
/// `dvdv_source_to_cd_sectors` -> `spawn_tags_mb_toc_lookup` ->
/// `musicbrainz::lookup_release_by_toc`, with `search_releases_by_query`
/// used only as the user-initiated text-search fallback.
pub fn dvdv_source_to_cd_sectors(path: &Path) -> Result<Vec<u32>, String> {
    let contents = crate::disc::dvdv_utils::map_dvdv_source(path)?;
    let presentation_index = select_default_disc_presentation_index(&contents).ok_or_else(|| {
        format!(
            "DVD-Video source has no selectable presentations: {}",
            path.display()
        )
    })?;
    let presentation = contents.presentations.get(presentation_index).ok_or_else(|| {
        format!(
            "DVD-Video default presentation index vanished: {}",
            path.display()
        )
    })?;
    dvdv_presentation_to_cd_sectors(presentation)
}

/// Convert one mapped DVD-Video presentation to MusicBrainz CD-frame offsets.
pub fn dvdv_presentation_to_cd_sectors(
    presentation: &crate::disc::model::DiscPresentation,
) -> Result<Vec<u32>, String> {
    let mut durations = Vec::with_capacity(presentation.tracks.len());
    for track in &presentation.tracks {
        let duration = track.duration_secs.ok_or_else(|| {
            format!(
                "DVD-Video chapter {} has no PGC playback duration",
                track.number
            )
        })?;
        if !(duration.is_finite() && duration > 0.0) {
            return Err(format!(
                "DVD-Video chapter {} has invalid PGC playback duration: {}",
                track.number, duration
            ));
        }
        durations.push(duration);
    }

    if durations.is_empty() {
        return Err("DVD-Video presentation has no chapter tracks".to_string());
    }

    let sectors = durations_to_cd_sectors(durations.into_iter());
    if sectors.len() < 2 {
        return Err("DVD-Video presentation is too short for a MusicBrainz TOC".to_string());
    }
    Ok(sectors)
}

/// Validate DVD-Video editor durations before synthetic MusicBrainz TOC lookup.
/// The editor uses 0.0 as a display sentinel for unknown chapter durations;
/// feeding those values into the CD-frame conversion would create a false TOC.
pub fn dvdv_editor_durations_to_cd_sectors(durations: &[f64]) -> Result<Vec<u32>, String> {
    if durations.is_empty() {
        return Err("DVD-Video editor has no chapter durations for TOC lookup".to_string());
    }
    for (idx, duration) in durations.iter().copied().enumerate() {
        if !(duration.is_finite() && duration > 0.0) {
            return Err(format!(
                "DVD-Video editor chapter {} has missing or invalid duration for TOC lookup",
                idx + 1
            ));
        }
    }
    let sectors = durations_to_cd_sectors(durations.iter().copied());
    if sectors.len() < 2 {
        return Err("DVD-Video editor duration list is too short for a MusicBrainz TOC".to_string());
    }
    Ok(sectors)
}

/// Validate Blu-ray editor chapter durations before synthetic MusicBrainz TOC lookup.
/// Blu-ray editors use 0.0 as the unknown-duration sentinel; do not feed those
/// into MusicBrainz disc-id construction.
pub fn bluray_editor_durations_to_cd_sectors(durations: &[f64]) -> Result<Vec<u32>, String> {
    if durations.is_empty() {
        return Err("Blu-ray editor has no chapter durations for TOC lookup".to_string());
    }
    for (idx, duration) in durations.iter().copied().enumerate() {
        if !(duration.is_finite() && duration > 0.0) {
            return Err(format!(
                "Blu-ray editor chapter {} has missing or invalid duration for TOC lookup",
                idx + 1
            ));
        }
    }
    let sectors = durations_to_cd_sectors(durations.iter().copied());
    if sectors.len() < 2 {
        return Err("Blu-ray editor duration list is too short for a MusicBrainz TOC".to_string());
    }
    Ok(sectors)
}

/// Select the presentation that a generic default-stream or TOC operation
/// should use for an already-mapped disc.
pub fn select_default_disc_presentation_index(
    contents: &crate::disc::model::DiscContents,
) -> Option<usize> {
    if contents.presentations.is_empty() {
        return None;
    }

    if !matches!(contents.format, crate::disc::model::DiscFormat::DvdVideo) {
        return Some(0);
    }

    contents
        .presentations
        .iter()
        .enumerate()
        .filter(|(_, presentation)| {
            matches!(
                presentation.id,
                crate::disc::model::PresentationId::DvdVideoTitle { .. }
            )
        })
        .max_by(|(_, left), (_, right)| {
            dvdv_default_presentation_score(left).cmp(&dvdv_default_presentation_score(right))
        })
        .map(|(index, _)| index)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct DvdvTocScore {
    has_sidecar_metadata: bool,
    stereo: bool,
    bit_depth: u32,
    duration_complete: bool,
    track_count: usize,
    duration_frames: u64,
    lossless: bool,
    coding_rank: u8,
    sample_rate: u32,
    reverse_identity: ReverseDvdvIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ReverseDvdvIdentity {
    vts_number: u8,
    title_number: u8,
    audio_stream_index: u8,
}

fn dvdv_default_presentation_score(
    presentation: &crate::disc::model::DiscPresentation,
) -> DvdvTocScore {
    let (vts_number, title_number, audio_stream_index) = presentation
        .id
        .dvd_video_parts()
        .unwrap_or((u8::MAX, u8::MAX, u8::MAX));

    DvdvTocScore {
        has_sidecar_metadata: presentation.album_title.is_some(),
        stereo: presentation.format.channels == Some(2)
            || presentation
                .format
                .channel_layout
                .as_deref()
                .is_some_and(|layout| layout.eq_ignore_ascii_case("stereo")),
        bit_depth: presentation.format.bit_depth.unwrap_or(0),
        duration_complete: dvdv_presentation_has_complete_positive_durations(presentation),
        track_count: presentation.tracks.len(),
        duration_frames: (presentation.total_duration_secs.max(0.0) * CD_FRAMES_PER_SECOND)
            .round() as u64,
        lossless: presentation.format.lossless,
        coding_rank: dvdv_codec_rank(presentation.format.codec.as_deref()),
        sample_rate: presentation.format.sample_rate.unwrap_or(0),
        reverse_identity: ReverseDvdvIdentity {
            vts_number: u8::MAX.saturating_sub(vts_number),
            title_number: u8::MAX.saturating_sub(title_number),
            audio_stream_index: u8::MAX.saturating_sub(audio_stream_index),
        },
    }
}

fn dvdv_presentation_has_complete_positive_durations(
    presentation: &crate::disc::model::DiscPresentation,
) -> bool {
    !presentation.tracks.is_empty()
        && presentation
            .tracks
            .iter()
            .all(|track| track.duration_secs.is_some_and(|d| d.is_finite() && d > 0.0))
}

fn dvdv_codec_rank(codec: Option<&str>) -> u8 {
    match codec.unwrap_or_default().to_ascii_lowercase().as_str() {
        "lpcm" | "pcm" => 4,
        "dts" => 3,
        "ac-3" | "ac3" | "dolby digital" => 2,
        "mpeg" | "mp2" | "mpa" => 1,
        _ => 0,
    }
}

fn is_dvdv_source_for_tags_mb(path: &Path) -> bool {
    if path.is_dir() {
        crate::disc::dvdv_utils::dvdv_directory_root(path).is_some()
    } else if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("iso"))
    {
        crate::disc::dvdv_utils::map_dvdv_source(path).is_ok()
    } else {
        false
    }
}

fn is_bluray_source_for_tags_mb(path: &Path) -> bool {
    crate::disc::bluray_utils::is_bluray_source(path)
}

fn dvda_source_to_cd_sectors(
    path: &std::path::Path,
    group_nr: Option<u8>,
) -> Result<Vec<u32>, String> {
    let volume: Box<dyn crate::tui::dvda::DvdaVolume> =
        if crate::disc::dvda_utils::is_dvda_directory(path) {
            Box::new(crate::tui::dvda::DirectoryDvdaVolume::new(path))
        } else {
            Box::new(
                crate::tui::dvda::IsoUdfDvdaVolume::open(path)
                    .map_err(|e| format!("DVD-Audio ISO open failed for '{}': {}", path.display(), e))?,
            )
        };
    let disc = crate::tui::dvda::parse_dvda_volume(volume.as_ref())
        .map_err(|e| format!("DVD-Audio parse failed for '{}': {}", path.display(), e))?;

    // The stereo group's timing typically matches CD releases, so it is the
    // DEFAULT when the user has not chosen a group — but an explicit
    // :dvda-group selection takes precedence. (The old stereo-always
    // override made group switching a no-op for TOC lookups, and MB's fuzzy
    // matching absorbs multichannel duration drift in practice.)
    let group_nr = group_nr
        .or_else(|| dvda_stereo_group_for_mb_toc(volume.as_ref(), &disc, path));

    let group = super::dvda_metabase::select_group(&disc, group_nr)
        .map_err(|e| e.to_string())?;
    let durations = super::dvda_metabase::group_track_pts(&disc, group);
    if durations.is_empty() {
        return Err("DVD-Audio: selected group has zero tracks".to_string());
    }
    Ok(super::dvda_metabase::pts_durations_to_cd_sectors(&durations))
}

/// Find a stereo (2-channel) group for MusicBrainz TOC computation.
/// Returns `Some(group_nr)` if a stereo group with tracks exists.
fn dvda_stereo_group_for_mb_toc(
    volume: &dyn crate::tui::dvda::DvdaVolume,
    disc: &crate::tui::dvda::DvdaDisc,
    source_path: &std::path::Path,
) -> Option<u8> {
    let source = if source_path.is_file() {
        Some(source_path)
    } else {
        None
    };
    for group in &disc.groups {
        if super::dvda_metabase::group_track_count(disc, group) == 0 {
            continue;
        }
        if let Some(probe) = crate::disc::dvda_utils::probe_group_aob_format_with_path(
            volume, disc, group, source,
        ) {
            if probe.channels == 2 {
                return Some(group.group_nr);
            }
        }
    }
    None
}

/// TOML metadata sidecar filename used for DVD-Video directory sources.
pub const DVDV_METADATA_SIDECAR_NAME: &str = "tonepoet.dvdvideo.metadata.toml";
const DVDV_METADATA_FORMAT: &str = "tonepoet-dvdvideo-metadata";
const DVDV_METADATA_SIDECAR_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DvdVideoMetadataSidecar {
    pub schema_version: u32,
    pub source: DvdVideoMetadataSource,
    pub album: BTreeMap<String, String>,
    pub tracks: Vec<DvdVideoMetadataTrack>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DvdVideoMetadataSource {
    pub path: PathBuf,
    pub sidecar_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<DvdVideoPresentationIdentity>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DvdVideoPresentationIdentity {
    pub vts_number: u8,
    pub title_number: u8,
    pub audio_stream_index: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_number: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_fingerprint: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DvdVideoMetadataTrack {
    pub number: usize,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_title: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_chapter: Option<u16>,
    pub tags: BTreeMap<String, String>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// TOML metadata sidecar filename used for Blu-ray directory sources.
pub const BLURAY_METADATA_SIDECAR_NAME: &str = "tonepoet.bluray.metadata.toml";
pub const BLURAY_METADATA_FORMAT: &str = "tonepoet-bluray-metadata";
pub const BLURAY_METADATA_SIDECAR_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BluRayMetadataSidecar {
    pub schema_version: u32,
    pub source: BluRayMetadataSource,
    #[serde(default)]
    pub album: BTreeMap<String, String>,
    #[serde(default)]
    pub tracks: Vec<BluRayMetadataTrack>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BluRayMetadataSource {
    pub path: PathBuf,
    pub sidecar_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<BluRayPresentationIdentity>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BluRayPresentationIdentity {
    pub playlist_number: u32,
    pub audio_pid: u16,
    pub audio_stream_index: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_number: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_fingerprint: Option<String>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BluRayMetadataTrack {
    pub number: u32,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_chapter: Option<u32>,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

pub fn dvdv_metadata_sidecar_path_for_source(source: &Path) -> Result<PathBuf, String> {
    dvdv_metadata_sidecar_path_for_source_with_extension(source, "toml", DVDV_METADATA_SIDECAR_NAME)
}

fn dvdv_metadata_sidecar_path_for_source_with_extension(
    source: &Path,
    extension: &str,
    directory_name: &str,
) -> Result<PathBuf, String> {
    if source.is_file() {
        let file_name = source.file_name().and_then(|name| name.to_str()).ok_or_else(|| {
            format!("DVD-Video source has no usable file name: {}", source.display())
        })?;
        let parent = source.parent().unwrap_or_else(|| Path::new("."));
        return Ok(parent.join(format!("{file_name}.dvdvideo.metadata.{extension}")));
    }
    let root = crate::disc::dvdv_utils::dvdv_directory_root(source)
        .ok_or_else(|| format!("Not a DVD-Video directory source: {}", source.display()))?;
    Ok(root.join(directory_name))
}

pub fn dvdv_metadata_sidecar_target_is_writable(source: &Path) -> bool {
    dvdv_metadata_sidecar_path_for_source(source)
        .map(|path| dvdv_metadata_sidecar_publish_target_is_writable(&path))
        .unwrap_or(false)
}

fn dvdv_metadata_sidecar_publish_target_is_writable(path: &Path) -> bool {
    let Some(parent) = path.parent() else { return false; };
    if !super::keybindings::is_dir_writable(parent) { return false; }
    if path.exists() { OpenOptions::new().write(true).open(path).is_ok() } else { true }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DvdVideoSidecarSaveKind {
    Created,
    AddedPresentation,
    UpdatedPresentation,
}

#[derive(Debug, Clone)]
pub struct DvdVideoSidecarSaveOutcome {
    pub path: PathBuf,
    pub kind: DvdVideoSidecarSaveKind,
    pub presentation_id: Option<String>,
}

pub fn save_dvdv_metadata_sidecar(
    source: &Path,
    state: &super::app::MetadataEditorState,
) -> Result<DvdVideoSidecarSaveOutcome, String> {
    let sidecar_path = dvdv_metadata_sidecar_path_for_source(source)?;
    let target_presentation = dvdv_presentation_identity_from_state(state);
    let existing_for_presentation = load_dvdv_metadata_sidecar_presentations(source)?
        .and_then(|(_, sidecars)| {
            sidecars
                .into_iter()
                .find(|sidecar| dvdv_existing_sidecar_can_merge(sidecar, target_presentation.as_ref()))
        });
    let sidecar = dvdv_metadata_sidecar_from_state_preserving(
        source,
        state,
        existing_for_presentation.as_ref(),
    )?;
    let presentation_id = sidecar
        .source
        .presentation
        .as_ref()
        .map(dvdv_presentation_id);
    let existed_before_save = sidecar_path.exists();
    let already_present = dvdv_toml_sidecar_has_compatible_presentation(
        &sidecar_path,
        sidecar.source.presentation.as_ref(),
    )?;
    write_dvdv_metadata_sidecar_atomic(&sidecar_path, &sidecar)?;
    let kind = if already_present {
        DvdVideoSidecarSaveKind::UpdatedPresentation
    } else if existed_before_save {
        DvdVideoSidecarSaveKind::AddedPresentation
    } else {
        DvdVideoSidecarSaveKind::Created
    };
    Ok(DvdVideoSidecarSaveOutcome { path: sidecar_path, kind, presentation_id })
}

pub fn load_dvdv_metadata_sidecar(
    source: &Path,
) -> Result<Option<(PathBuf, DvdVideoMetadataSidecar)>, String> {
    Ok(load_dvdv_metadata_sidecar_presentations(source)?
        .and_then(|(path, mut sidecars)| sidecars.drain(..).next().map(|sidecar| (path, sidecar))))
}


/// Preload existing DVD-Video TOML sidecar values into the metadata editor.
///
/// `open_metadata_editor_for_dvdv` lives in `keybindings.rs`; the uploaded
/// source bundle does not include that file. This helper is therefore called
/// from the `:tags-mb` DVD-Video open path in this file, and should also be
/// called by `open_metadata_editor_for_dvdv` immediately after it constructs
/// the editor state. It mutates only editor fields that have matching sidecar
/// data, preserves non-sidecar editor entries, and leaves the editor clean.
pub fn preload_active_dvdv_metadata_editor_from_sidecar(
    app: &mut super::app::AppState,
    source: &Path,
) -> Result<bool, String> {
    preload_active_dvdv_metadata_editor_from_sidecar_for_presentation(app, source, None)
}

/// Open a DVD-Video metadata editor and immediately preload the matching
/// sidecar presentation. Callers that already know the selected disc stream
/// should pass its `PresentationId`; callers without that context may pass
/// `None` and use the legacy shape fallback.
pub fn open_metadata_editor_for_dvdv_with_sidecar_preload(
    app: &mut super::app::AppState,
    source: PathBuf,
    selected_presentation_id: Option<crate::disc::model::PresentationId>,
) -> Result<bool, String> {
    super::keybindings::open_metadata_editor_for_dvdv(app, source.clone());
    if !matches!(
        app.active_overlay,
        super::app::ActiveOverlay::MetadataEditor(_),
    ) {
        return Ok(false);
    }
    preload_active_dvdv_metadata_editor_from_sidecar_for_presentation(
        app,
        &source,
        selected_presentation_id,
    )
}

/// Best-effort selected presentation capture for command-mode DVD-Video
/// `:tags-mb` opens. The explicit Disc Browser cursor wins; otherwise use the
/// cached default presentation for the selected DVD-Video source.
fn selected_dvdv_presentation_id_for_tags_mb_open(
    app: &super::app::AppState,
    source: &Path,
) -> Option<crate::disc::model::PresentationId> {
    if let super::app::ActiveOverlay::DiscBrowser(state) = &app.active_overlay {
        if state.source_path.as_path() == source {
            if let Some(presentation) = state.selected_presentation() {
                if matches!(
                    presentation.id,
                    crate::disc::model::PresentationId::DvdVideoTitle { .. }
                ) {
                    return Some(presentation.id.clone());
                }
            }
        }
    }

    let contents = super::disc_browser_actions::cached_disc_contents(app, source)?;
    if !matches!(contents.format, crate::disc::model::DiscFormat::DvdVideo) {
        return None;
    }
    let presentation_index = select_default_disc_presentation_index(contents.as_ref())?;
    let presentation = contents.presentations.get(presentation_index)?;
    if matches!(
        presentation.id,
        crate::disc::model::PresentationId::DvdVideoTitle { .. }
    ) {
        Some(presentation.id.clone())
    } else {
        None
    }
}

/// Preload DVD-Video sidecar data into the active editor while carrying the
/// selected presentation identity from the browse/keybinding path.
///
/// `open_metadata_editor_for_dvdv` should call this overload when it has the
/// selected `DiscPresentation`/`PresentationId`. That avoids relying on the
/// single-presentation empty-tab fallback below.
pub fn preload_active_dvdv_metadata_editor_from_sidecar_for_presentation(
    app: &mut super::app::AppState,
    source: &Path,
    selected_presentation_id: Option<crate::disc::model::PresentationId>,
) -> Result<bool, String> {
    match &mut app.active_overlay {
        super::app::ActiveOverlay::MetadataEditor(state) => {
            preload_dvdv_metadata_editor_state_from_sidecar_with_presentation_id(
                source,
                state,
                selected_presentation_id,
            )
        }
        _ => Ok(false),
    }
}

pub fn preload_dvdv_metadata_editor_state_from_sidecar(
    source: &Path,
    state: &mut super::app::MetadataEditorState,
) -> Result<bool, String> {
    preload_dvdv_metadata_editor_state_from_sidecar_with_presentation_id(source, state, None)
}

pub fn preload_dvdv_metadata_editor_state_from_sidecar_with_presentation_id(
    source: &Path,
    state: &mut super::app::MetadataEditorState,
    selected_presentation_id: Option<crate::disc::model::PresentationId>,
) -> Result<bool, String> {
    let Some((_, sidecars)) = load_dvdv_metadata_sidecar_presentations(source)? else {
        return Ok(false);
    };
    if state.presentation_tabs.is_empty() {
        let shape = dvdv_editor_presentation_shape_from_state(state);
        let selected_identity = selected_presentation_id
            .as_ref()
            .and_then(|id| dvdv_presentation_identity_from_id_and_shape(id, &shape));
        let state_identity = dvdv_presentation_identity_from_state(state);
        let identity = selected_identity.as_ref().or(state_identity.as_ref());
        let Some(sidecar) = dvdv_matching_sidecar_for_editor(&sidecars, identity, &shape) else {
            return Ok(false);
        };
        let path_count = state.active_surface().paths.len();
        let source_chapters = state.active_surface().dvdv_source_chapters.clone();
        let surface = state.active_surface_mut();
        dvdv_apply_sidecar_to_editor_fields(
            &mut surface.entries,
            path_count,
            source_chapters.as_deref(),
            sidecar,
        );
        surface.deleted.clear();
        surface.dirty = false;
        return Ok(true);
    }

    let mut applied_any = false;
    for tab in &mut state.presentation_tabs {
        let identity = dvdv_presentation_identity_from_tab(tab);
        let shape = dvdv_editor_presentation_shape_from_tab(tab);
        let Some(sidecar) = dvdv_matching_sidecar_for_editor(&sidecars, identity.as_ref(), &shape) else {
            continue;
        };
        dvdv_apply_sidecar_to_editor_fields(
            &mut tab.entries,
            tab.paths.len(),
            tab.dvdv_source_chapters.as_deref(),
            sidecar,
        );
        tab.deleted.clear();
        tab.dirty = false;
        applied_any = true;
    }

    if applied_any {
        if let Some(active) = state.presentation_tabs.get(state.active_tab).cloned() {
            state.active_surface_mut().paths = active.paths;
            state.active_surface_mut().entries = active.entries;
            state.active_surface_mut().file_labels = active.file_labels;
            state.active_surface_mut().deleted = active.deleted;
            state.active_surface_mut().dirty = active.dirty;
            state.active_surface_mut().sacd_area_kind = active.sacd_area_kind;
            state.active_surface_mut().sacd_stereo_durations = active.sacd_stereo_durations;
            state.active_surface_mut().sacd_multi_channel_durations = active.sacd_multi_channel_durations;
            state.active_surface_mut().dvdv_source_chapters = active.dvdv_source_chapters;
            state.active_surface_mut().dvdv_track_durations = active.dvdv_track_durations;
            state.active_surface_mut().dvdv_angle_number = active.dvdv_angle_number;
            state.active_surface_mut().dvdv_title_angle_count = active.dvdv_title_angle_count;
            state.active_surface_mut().bluray_playlist_number = active.bluray_playlist_number;
            state.active_surface_mut().bluray_audio_pid = active.bluray_audio_pid;
            state.active_surface_mut().bluray_audio_stream_index = active.bluray_audio_stream_index;
            state.active_surface_mut().bluray_angle_number = active.bluray_angle_number;
            state.active_surface_mut().bluray_chapter_durations = active.bluray_chapter_durations;
        }
    }
    Ok(applied_any)
}

fn dvdv_presentation_identity_from_tab(
    tab: &super::app::PresentationTab,
) -> Option<DvdVideoPresentationIdentity> {
    let (vts_number, title_number, audio_stream_index) = tab.id.dvd_video_parts()?;
    let track_count = Some(tab.paths.len());
    let duration_fingerprint = tab
        .dvdv_track_durations
        .as_deref()
        .filter(|durations| !durations.is_empty())
        .map(dvdv_track_duration_fingerprint_from_secs);
    let angle_number = match (tab.dvdv_title_angle_count, tab.dvdv_angle_number) {
        (Some(count), Some(angle)) if count > 1 => Some(angle),
        _ => None,
    };
    Some(DvdVideoPresentationIdentity {
        vts_number,
        title_number,
        audio_stream_index,
        angle_number,
        track_count,
        duration_fingerprint,
    })
}

fn dvdv_presentation_identity_from_id_and_shape(
    id: &crate::disc::model::PresentationId,
    shape: &DvdvEditorPresentationShape,
) -> Option<DvdVideoPresentationIdentity> {
    let (vts_number, title_number, audio_stream_index) = id.dvd_video_parts()?;
    Some(DvdVideoPresentationIdentity {
        vts_number,
        title_number,
        audio_stream_index,
        angle_number: shape.angle_number,
        track_count: Some(shape.track_count),
        duration_fingerprint: shape.duration_fingerprint.clone(),
    })
}

#[derive(Debug, Clone)]
struct DvdvEditorPresentationShape {
    track_count: usize,
    duration_fingerprint: Option<String>,
    angle_number: Option<u8>,
}

fn dvdv_editor_presentation_shape_from_state(
    state: &super::app::MetadataEditorState,
) -> DvdvEditorPresentationShape {
    let angle_number = match (state.active_surface().dvdv_title_angle_count, state.active_surface().dvdv_angle_number) {
        (Some(count), Some(angle)) if count > 1 => Some(angle),
        _ => None,
    };
    DvdvEditorPresentationShape {
        track_count: state.active_surface().paths.len(),
        duration_fingerprint: state.active_surface()
            .dvdv_track_durations
            .as_deref()
            .filter(|durations| !durations.is_empty())
            .map(dvdv_track_duration_fingerprint_from_secs),
        angle_number,
    }
}

fn dvdv_editor_presentation_shape_from_tab(
    tab: &super::app::PresentationTab,
) -> DvdvEditorPresentationShape {
    let angle_number = match (tab.dvdv_title_angle_count, tab.dvdv_angle_number) {
        (Some(count), Some(angle)) if count > 1 => Some(angle),
        _ => None,
    };
    DvdvEditorPresentationShape {
        track_count: tab.paths.len(),
        duration_fingerprint: tab
            .dvdv_track_durations
            .as_deref()
            .filter(|durations| !durations.is_empty())
            .map(dvdv_track_duration_fingerprint_from_secs),
        angle_number,
    }
}

fn dvdv_matching_sidecar_for_editor<'a>(
    sidecars: &'a [DvdVideoMetadataSidecar],
    identity: Option<&DvdVideoPresentationIdentity>,
    shape: &DvdvEditorPresentationShape,
) -> Option<&'a DvdVideoMetadataSidecar> {
    if let Some(identity) = identity {
        return unique_dvdv_editor_sidecar(
            "identity",
            sidecars
                .iter()
                .filter(|sidecar| dvdv_existing_sidecar_can_merge(sidecar, Some(identity))),
        );
    }

    let legacy = unique_dvdv_editor_sidecar(
        "legacy shape",
        sidecars
            .iter()
            .filter(|sidecar| dvdv_legacy_sidecar_matches_editor_shape(sidecar, shape)),
    );
    if legacy.is_some() {
        return legacy;
    }

    unique_dvdv_editor_sidecar(
        "identity shape",
        sidecars
            .iter()
            .filter(|sidecar| dvdv_identity_sidecar_matches_editor_shape(sidecar, shape)),
    )
}

fn unique_dvdv_editor_sidecar<'a, I>(
    match_kind: &str,
    candidates: I,
) -> Option<&'a DvdVideoMetadataSidecar>
where
    I: IntoIterator<Item = &'a DvdVideoMetadataSidecar>,
{
    let mut selected = None;
    for sidecar in candidates {
        if let Some(first) = selected {
            log::warn!(
                "DVD-Video metadata editor sidecar preload skipped: multiple compatible {} presentations matched (first={}, duplicate={})",
                match_kind,
                dvdv_editor_sidecar_debug_id(first),
                dvdv_editor_sidecar_debug_id(sidecar),
            );
            return None;
        }
        selected = Some(sidecar);
    }
    selected
}

fn dvdv_editor_sidecar_debug_id(sidecar: &DvdVideoMetadataSidecar) -> String {
    sidecar
        .source
        .presentation
        .as_ref()
        .map(dvdv_presentation_id)
        .unwrap_or_else(|| format!("legacy:{}", sidecar.source.path.display()))
}

fn dvdv_legacy_sidecar_matches_editor_shape(
    sidecar: &DvdVideoMetadataSidecar,
    shape: &DvdvEditorPresentationShape,
) -> bool {
    sidecar.source.sidecar_kind == "dvd_video"
        && sidecar.source.presentation.is_none()
        && (sidecar.tracks.is_empty() || sidecar.tracks.len() == shape.track_count)
}

fn dvdv_identity_sidecar_matches_editor_shape(
    sidecar: &DvdVideoMetadataSidecar,
    shape: &DvdvEditorPresentationShape,
) -> bool {
    if sidecar.source.sidecar_kind != "dvd_video" {
        return false;
    }
    let Some(stored) = sidecar.source.presentation.as_ref() else {
        return false;
    };
    let stored_track_count = stored
        .track_count
        .or_else(|| (!sidecar.tracks.is_empty()).then_some(sidecar.tracks.len()));
    if stored_track_count.is_some_and(|track_count| track_count != shape.track_count) {
        return false;
    }
    if let (Some(stored), Some(current)) = (
        stored.duration_fingerprint.as_deref(),
        shape.duration_fingerprint.as_deref(),
    ) {
        if stored != current {
            return false;
        }
    }
    dvdv_sparse_angle_identity_compatible(stored.angle_number, shape.angle_number)
}

fn dvdv_apply_sidecar_to_editor_fields(
    entries: &mut Vec<super::probe::TagEntry>,
    file_count: usize,
    source_chapters: Option<&[u16]>,
    sidecar: &DvdVideoMetadataSidecar,
) {
    let n_files = file_count.max(1);
    let mut album_keys: Vec<&str> = DVDV_ALBUM_PRIMARY_TOML_KEYS
        .iter()
        .map(|(key, _)| *key)
        .collect();
    for key in sidecar.album.keys().map(String::as_str) {
        if !album_keys.contains(&key) {
            album_keys.push(key);
        }
    }
    for key in album_keys {
        if let Some(value) = sidecar.album.get(key).map(String::as_str).filter(|v| !v.trim().is_empty()) {
            dvdv_upsert_editor_entry(entries, key, value, vec![value.to_string(); n_files]);
        }
    }

    let mut track_keys = std::collections::BTreeSet::new();
    for track in &sidecar.tracks {
        for key in track.tags.keys() {
            track_keys.insert(key.as_str());
        }
    }

    for key in track_keys {
        let per_file_values: Vec<String> = (0..n_files)
            .map(|idx| {
                dvdv_sidecar_track_for_editor_index(sidecar, source_chapters, idx)
                    .and_then(|track| track.tags.get(key))
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();
        if per_file_values.iter().all(|value| value.trim().is_empty()) {
            continue;
        }
        let first = per_file_values.first().cloned().unwrap_or_default();
        let mixed = per_file_values.iter().any(|value| value != &first);
        let display_value = if mixed { "<multiple values>".to_string() } else { first };
        dvdv_upsert_editor_entry(entries, key, &display_value, per_file_values);
    }

    super::probe::sort_entries_standard_first(entries);
}

fn dvdv_sidecar_track_for_editor_index<'a>(
    sidecar: &'a DvdVideoMetadataSidecar,
    source_chapters: Option<&[u16]>,
    idx: usize,
) -> Option<&'a DvdVideoMetadataTrack> {
    source_chapters
        .and_then(|chapters| chapters.get(idx).copied())
        .and_then(|chapter| sidecar.tracks.iter().find(|track| track.source_chapter == Some(chapter)))
        .or_else(|| sidecar.tracks.iter().find(|track| track.number == idx + 1))
        .or_else(|| sidecar.tracks.get(idx))
}

fn dvdv_upsert_editor_entry(
    entries: &mut Vec<super::probe::TagEntry>,
    key: &str,
    value: &str,
    per_file_values: Vec<String>,
) {
    let value = value.to_string();
    let is_mixed = value == "<multiple values>";
    if let Some(entry) = entries
        .iter_mut()
        .find(|entry| dvdv_editor_key_to_sidecar_key(&entry.display_key) == Some(key))
    {
        entry.value = value.clone();
        entry.original = value;
        entry.is_binary = false;
        entry.is_mixed = is_mixed;
        entry.per_file_values = per_file_values.clone();
        entry.per_file_originals = per_file_values;
        entry.mb_proposed_value = None;
        entry.mb_proposed_per_file = None;
        return;
    }

    entries.push(super::probe::TagEntry {
        row_scope: crate::tui::probe::RowScope::File,
        display_key: key.to_string(),
        item_key: lofty::tag::ItemKey::Unknown(key.to_string()),
        value: value.clone(),
        original: value,
        is_binary: false,
        is_mixed,
        per_file_originals: per_file_values.clone(),
        per_file_values,
        mb_proposed_value: None,
        mb_proposed_per_file: None,
    });
}

pub fn load_dvdv_metadata_sidecar_presentations(
    source: &Path,
) -> Result<Option<(PathBuf, Vec<DvdVideoMetadataSidecar>)>, String> {
    let toml_path = dvdv_metadata_sidecar_path_for_source(source)?;
    if toml_path.exists() {
        return parse_dvdv_metadata_sidecar_presentations(&toml_path)
            .map(|sidecars| Some((toml_path, sidecars)));
    }
    Ok(None)
}

pub fn parse_dvdv_metadata_sidecar(path: &Path) -> Result<DvdVideoMetadataSidecar, String> {
    let mut sidecars = parse_dvdv_metadata_sidecar_presentations(path)?;
    if sidecars.is_empty() {
        return Err(format!("DVD-Video TOML sidecar {} has no presentations", path.display()));
    }
    Ok(sidecars.swap_remove(0))
}

pub fn parse_dvdv_metadata_sidecar_presentations(path: &Path) -> Result<Vec<DvdVideoMetadataSidecar>, String> {
    let payload = fs::read_to_string(path)
        .map_err(|e| format!("read DVD-Video metadata sidecar {}: {e}", path.display()))?;
    parse_dvdv_metadata_toml_sidecar_presentations(path, &payload)
}

fn parse_dvdv_metadata_toml_sidecar_presentations(
    path: &Path,
    payload: &str,
) -> Result<Vec<DvdVideoMetadataSidecar>, String> {
    let doc = payload.parse::<DocumentMut>()
        .map_err(|e| format!("parse DVD-Video TOML sidecar {}: {e}", path.display()))?;
    let schema_version = doc.get("schema_version").and_then(Item::as_integer)
        .and_then(|v| u32::try_from(v).ok()).unwrap_or(DVDV_METADATA_SIDECAR_SCHEMA_VERSION);
    let format = doc.get("format").and_then(Item::as_str).unwrap_or_default();
    if !format.is_empty() && format != DVDV_METADATA_FORMAT {
        return Err(format!(
            "unsupported DVD-Video TOML sidecar format '{}' in {}",
            format,
            path.display()
        ));
    }
    let presentations = doc
        .get("presentations")
        .and_then(Item::as_array_of_tables)
        .ok_or_else(|| format!("DVD-Video TOML sidecar {} has no [[presentations]] entries", path.display()))?;
    let mut sidecars = Vec::new();
    for table in presentations.iter() {
        sidecars.push(parse_dvdv_metadata_toml_presentation(path, schema_version, table)?);
    }
    Ok(sidecars)
}

fn parse_dvdv_metadata_toml_presentation(
    path: &Path,
    schema_version: u32,
    presentation_table: &Table,
) -> Result<DvdVideoMetadataSidecar, String> {
    let presentation_id = presentation_table
        .get("id")
        .and_then(Item::as_str)
        .map(str::to_string);
    let mut source = DvdVideoMetadataSource {
        path: path.to_path_buf(),
        sidecar_kind: "dvd_video".to_string(),
        presentation: dvdv_toml_source_identity(presentation_table.get("source").and_then(Item::as_table)),
        extra: BTreeMap::new(),
    };
    source.extra.insert("sidecar_format".to_string(), serde_json::Value::String("toml".to_string()));
    if let Some(id) = &presentation_id {
        source.extra.insert("presentation_id".to_string(), serde_json::Value::String(id.clone()));
    }
    let album = dvdv_toml_album_table_to_map(presentation_table.get("album").and_then(Item::as_table));
    let tracks = dvdv_toml_tracks_to_vec(presentation_table.get("tracks").and_then(Item::as_array_of_tables));
    Ok(DvdVideoMetadataSidecar { schema_version, source, album, tracks, extra: BTreeMap::new() })
}

fn dvdv_toml_album_table_to_map(album_table: Option<&Table>) -> BTreeMap<String, String> {
    let mut album = BTreeMap::new();
    let Some(album_table) = album_table else { return album; };
    for (key, item) in album_table.iter() {
        if key == "extra" { continue; }
        let Some(internal_key) = dvdv_toml_album_key_to_internal(key) else { continue; };
        if let Some(value) = toml_item_to_string(item) {
            album.insert(internal_key.to_string(), value);
        }
    }
    if let Some(extra) = album_table.get("extra").and_then(Item::as_table) {
        for (key, item) in extra.iter() {
            if let Some(value) = toml_item_to_string(item) {
                album.insert(dvdv_toml_extra_key_to_internal(key), value);
            }
        }
    }
    album
}

fn dvdv_toml_tracks_to_vec(track_tables: Option<&ArrayOfTables>) -> Vec<DvdVideoMetadataTrack> {
    let mut tracks = Vec::new();
    let Some(track_tables) = track_tables else { return tracks; };
    for table in track_tables.iter() {
        let number = table.get("number").and_then(Item::as_integer)
            .and_then(|v| usize::try_from(v).ok()).unwrap_or_else(|| tracks.len() + 1);
        let source_title = table.get("source_title").and_then(Item::as_integer).and_then(|v| u8::try_from(v).ok());
        let source_chapter = table.get("source_chapter").and_then(Item::as_integer).and_then(|v| u16::try_from(v).ok());
        let mut tags = BTreeMap::new();
        for (key, item) in table.iter() {
            if matches!(key, "number" | "source_title" | "source_chapter" | "extra") { continue; }
            let Some(internal_key) = dvdv_toml_track_key_to_internal(key) else { continue; };
            if let Some(value) = toml_item_to_string(item) {
                tags.insert(internal_key.to_string(), value);
            }
        }
        if let Some(extra) = table.get("extra").and_then(Item::as_table) {
            for (key, item) in extra.iter() {
                if let Some(value) = toml_item_to_string(item) {
                    tags.insert(dvdv_toml_extra_key_to_internal(key), value);
                }
            }
        }
        let label = tags.get("TITLE").cloned().unwrap_or_else(|| format!("{:02}", number));
        tracks.push(DvdVideoMetadataTrack { number, label, source_title, source_chapter, tags, extra: BTreeMap::new() });
    }
    tracks
}

fn dvdv_toml_source_identity(source: Option<&Table>) -> Option<DvdVideoPresentationIdentity> {
    let source = source?;
    Some(DvdVideoPresentationIdentity {
        vts_number: source.get("vts").and_then(Item::as_integer).and_then(|v| u8::try_from(v).ok())?,
        title_number: source.get("title").and_then(Item::as_integer).and_then(|v| u8::try_from(v).ok())?,
        audio_stream_index: source.get("audio_stream").and_then(Item::as_integer).and_then(|v| u8::try_from(v).ok())?,
        angle_number: source.get("angle").and_then(Item::as_integer).and_then(|v| u8::try_from(v).ok()),
        track_count: source.get("track_count").and_then(Item::as_integer).and_then(|v| usize::try_from(v).ok()),
        duration_fingerprint: source.get("duration_fingerprint").and_then(Item::as_str).map(str::to_string),
    })
}

pub fn dvdv_presentation_id(identity: &DvdVideoPresentationIdentity) -> String {
    match identity.angle_number {
        Some(angle) => format!(
            "vts{}-title{}-stream{}-angle{}",
            identity.vts_number, identity.title_number, identity.audio_stream_index, angle
        ),
        None => format!(
            "vts{}-title{}-stream{}",
            identity.vts_number, identity.title_number, identity.audio_stream_index
        ),
    }
}

fn dvdv_toml_sidecar_has_compatible_presentation(
    path: &Path,
    target: Option<&DvdVideoPresentationIdentity>,
) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let payload = fs::read_to_string(path)
        .map_err(|e| format!("read existing DVD-Video TOML sidecar {}: {e}", path.display()))?;
    let doc = payload.parse::<DocumentMut>()
        .map_err(|e| format!("parse existing DVD-Video TOML sidecar {}: {e}", path.display()))?;
    Ok(doc
        .get("presentations")
        .and_then(Item::as_array_of_tables)
        .map(|presentations| {
            presentations.iter().any(|table| dvdv_toml_presentation_table_matches_target(table, target))
        })
        .unwrap_or(false))
}

fn dvdv_toml_presentation_table_matches_target(
    table: &Table,
    target: Option<&DvdVideoPresentationIdentity>,
) -> bool {
    let Some(target) = target else { return false; };
    let target_id = dvdv_presentation_id(target);
    if table.get("id").and_then(Item::as_str) != Some(target_id.as_str()) {
        return false;
    }
    let stored = dvdv_toml_source_identity(table.get("source").and_then(Item::as_table));
    dvdv_presentation_identity_compatible(stored.as_ref(), Some(target))
}

fn toml_item_to_string(item: &Item) -> Option<String> {
    item.as_str().map(str::to_string)
        .or_else(|| item.as_integer().map(|v| v.to_string()))
        .or_else(|| item.as_bool().map(|v| v.to_string()))
}

fn bluray_toml_table_extra_json(table: &Table, reserved: &[&str]) -> BTreeMap<String, serde_json::Value> {
    let mut extra = BTreeMap::new();
    for (key, item) in table.iter() {
        if reserved.iter().any(|reserved| key.eq_ignore_ascii_case(reserved)) {
            continue;
        }
        if let Some(value) = toml_item_to_json_value(item) {
            extra.insert(key.to_string(), value);
        }
    }
    extra
}

fn toml_item_to_json_value(item: &Item) -> Option<serde_json::Value> {
    if let Some(value) = item.as_str() {
        return Some(serde_json::Value::String(value.to_string()));
    }
    if let Some(value) = item.as_integer() {
        return Some(serde_json::Value::Number(value.into()));
    }
    if let Some(value) = item.as_float() {
        return serde_json::Number::from_f64(value).map(serde_json::Value::Number);
    }
    if let Some(value) = item.as_bool() {
        return Some(serde_json::Value::Bool(value));
    }
    if let Some(value) = item.as_datetime() {
        return Some(serde_json::Value::String(value.to_string()));
    }
    if let Some(value) = item.as_value() {
        return Some(toml_value_to_json_value(value));
    }
    if let Some(table) = item.as_table() {
        let mut out = serde_json::Map::new();
        for (key, value) in table.iter() {
            if let Some(value) = toml_item_to_json_value(value) {
                out.insert(key.to_string(), value);
            }
        }
        return Some(serde_json::Value::Object(out));
    }
    if let Some(array) = item.as_array() {
        return Some(serde_json::Value::Array(
            array.iter().map(toml_value_to_json_value).collect(),
        ));
    }
    if let Some(array) = item.as_array_of_tables() {
        let mut out = Vec::new();
        for table in array.iter() {
            let mut object = serde_json::Map::new();
            for (key, item) in table.iter() {
                if let Some(value) = toml_item_to_json_value(item) {
                    object.insert(key.to_string(), value);
                }
            }
            out.push(serde_json::Value::Object(object));
        }
        return Some(serde_json::Value::Array(out));
    }
    let rendered = item.to_string();
    (!rendered.trim().is_empty()).then_some(serde_json::Value::String(rendered))
}

fn toml_value_to_json_value(value: &Value) -> serde_json::Value {
    if let Some(value) = value.as_str() {
        return serde_json::Value::String(value.to_string());
    }
    if let Some(value) = value.as_integer() {
        return serde_json::Value::Number(value.into());
    }
    if let Some(value) = value.as_float() {
        return serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .unwrap_or_else(|| serde_json::Value::String(value.to_string()));
    }
    if let Some(value) = value.as_bool() {
        return serde_json::Value::Bool(value);
    }
    if let Some(value) = value.as_datetime() {
        return serde_json::Value::String(value.to_string());
    }
    if let Some(array) = value.as_array() {
        return serde_json::Value::Array(array.iter().map(toml_value_to_json_value).collect());
    }
    if let Some(table) = value.as_inline_table() {
        let mut out = serde_json::Map::new();
        for (key, value) in table.iter() {
            out.insert(key.to_string(), toml_value_to_json_value(value));
        }
        return serde_json::Value::Object(out);
    }
    serde_json::Value::String(value.to_string())
}

pub fn bluray_metadata_sidecar_path_for_source(source: &Path) -> PathBuf {
    bluray_metadata_sidecar_candidate_paths(source)
        .into_iter()
        .next()
        .unwrap_or_else(|| source.join(BLURAY_METADATA_SIDECAR_NAME))
}

pub fn load_bluray_metadata_sidecar_presentations(
    source: &Path,
) -> Result<Option<(PathBuf, Vec<BluRayMetadataSidecar>)>, String> {
    for toml_path in bluray_metadata_sidecar_candidate_paths(source) {
        if toml_path.exists() {
            let payload = fs::read_to_string(&toml_path)
                .map_err(|e| format!("read Blu-ray metadata sidecar {}: {e}", toml_path.display()))?;
            return parse_bluray_metadata_sidecar_presentations(&payload, &toml_path)
                .map(|sidecars| Some((toml_path, sidecars)));
        }
    }
    Ok(None)
}

pub fn parse_bluray_metadata_sidecar_presentations(
    text: &str,
    path: &Path,
) -> Result<Vec<BluRayMetadataSidecar>, String> {
    let doc = text
        .parse::<DocumentMut>()
        .map_err(|e| format!("parse Blu-ray TOML sidecar {}: {e}", path.display()))?;
    let schema_version = doc
        .get("schema_version")
        .and_then(Item::as_integer)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(BLURAY_METADATA_SIDECAR_SCHEMA_VERSION);
    if schema_version != BLURAY_METADATA_SIDECAR_SCHEMA_VERSION {
        return Err(format!(
            "unsupported Blu-ray TOML sidecar schema_version {} in {}",
            schema_version,
            path.display()
        ));
    }
    let format = doc.get("format").and_then(Item::as_str).unwrap_or_default();
    if !format.is_empty() && format != BLURAY_METADATA_FORMAT {
        return Err(format!(
            "unsupported Blu-ray TOML sidecar format '{}' in {}",
            format,
            path.display()
        ));
    }
    let presentations = doc
        .get("presentations")
        .and_then(Item::as_array_of_tables)
        .ok_or_else(|| format!("Blu-ray TOML sidecar {} has no [[presentations]] entries", path.display()))?;
    let mut sidecars = Vec::with_capacity(presentations.len());
    for table in presentations.iter() {
        sidecars.push(parse_bluray_metadata_toml_presentation(path, schema_version, table)?);
    }
    Ok(sidecars)
}

pub fn save_bluray_metadata_sidecar(
    path: &Path,
    sidecars: &[BluRayMetadataSidecar],
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("Blu-ray sidecar path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|e| format!("create Blu-ray sidecar directory {}: {e}", parent.display()))?;
    let payload = bluray_sidecars_to_toml_string_for_path(path, sidecars)?;
    let tmp = unique_sidecar_temp_path(path);
    let write_result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)
            .map_err(|e| format!("create temporary Blu-ray sidecar {}: {e}", tmp.display()))?;
        file.write_all(payload.as_bytes())
            .map_err(|e| format!("write temporary Blu-ray sidecar {}: {e}", tmp.display()))?;
        if !payload.ends_with('\n') {
            file.write_all(b"\n")
                .map_err(|e| format!("finish temporary Blu-ray sidecar {}: {e}", tmp.display()))?;
        }
        file.sync_all()
            .map_err(|e| format!("sync temporary Blu-ray sidecar {}: {e}", tmp.display()))?;
        drop(file);
        atomic_replace_file(&tmp, path)
            .map_err(|e| format!("atomically publish Blu-ray TOML sidecar {}: {e}", path.display()))?;
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BluRaySidecarSaveKind {
    Created,
    AddedPresentation,
    UpdatedPresentation,
}

#[derive(Debug, Clone)]
pub struct BluRaySidecarSaveOutcome {
    pub path: PathBuf,
    pub kind: BluRaySidecarSaveKind,
    pub presentation_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BluRaySidecarSaveAllOutcome {
    pub path: PathBuf,
    pub saved_tab_indices: Vec<usize>,
    pub saved_presentations: usize,
    pub created_file: bool,
    pub added_presentations: usize,
    pub updated_presentations: usize,
    pub skipped_clean_presentations: usize,
    pub missing_identity_presentations: usize,
}

#[derive(Debug, Clone, Default)]
pub struct BluRaySidecarPreloadReport {
    pub sidecar_count: usize,
    pub attempted_presentations: usize,
    pub applied_presentations: usize,
    pub warnings: Vec<BluRaySidecarMatchWarning>,
}

impl BluRaySidecarPreloadReport {
    pub fn applied_any(&self) -> bool {
        self.applied_presentations > 0
    }
}

pub fn save_bluray_metadata_sidecar_from_state(
    source: &Path,
    state: &super::app::MetadataEditorState,
) -> Result<BluRaySidecarSaveOutcome, String> {
    let default_path = bluray_metadata_sidecar_path_for_source(source);
    let (sidecar_path, mut sidecars) = load_bluray_metadata_sidecar_presentations(source)?
        .unwrap_or_else(|| (default_path, Vec::new()));
    let target = bluray_presentation_identity_from_state(state)
        .ok_or_else(|| "Blu-ray metadata editor is missing presentation identity".to_string())?;
    let existing_index = unique_bluray_sidecar_index(&sidecars, &target)?;
    let existing = existing_index.and_then(|idx| sidecars.get(idx).cloned());
    let sidecar = bluray_metadata_sidecar_from_state_preserving(source, state, existing.as_ref())?;
    let presentation_id = sidecar
        .source
        .presentation
        .as_ref()
        .map(bluray_presentation_id);
    let existed_before_save = sidecar_path.exists();
    let already_present = existing_index.is_some();
    if let Some(idx) = existing_index {
        sidecars[idx] = sidecar;
    } else {
        sidecars.push(sidecar);
    }
    save_bluray_metadata_sidecar(&sidecar_path, &sidecars)?;
    let kind = if already_present {
        BluRaySidecarSaveKind::UpdatedPresentation
    } else if existed_before_save {
        BluRaySidecarSaveKind::AddedPresentation
    } else {
        BluRaySidecarSaveKind::Created
    };
    Ok(BluRaySidecarSaveOutcome {
        path: sidecar_path,
        kind,
        presentation_id,
    })
}

pub fn save_bluray_metadata_sidecar_dirty_presentations_from_state(
    source: &Path,
    state: &super::app::MetadataEditorState,
) -> Result<BluRaySidecarSaveAllOutcome, String> {
    let state_snapshot = state.clone();
    let state = &state_snapshot;

    let default_path = bluray_metadata_sidecar_path_for_source(source);
    if state.presentation_tabs.is_empty() {
        if !state.active_surface().dirty {
            return Ok(BluRaySidecarSaveAllOutcome {
                path: default_path,
                saved_tab_indices: Vec::new(),
                saved_presentations: 0,
                created_file: false,
                added_presentations: 0,
                updated_presentations: 0,
                skipped_clean_presentations: 1,
                missing_identity_presentations: 0,
            });
        }
        if bluray_presentation_identity_from_state(state).is_none() {
            return Ok(BluRaySidecarSaveAllOutcome {
                path: default_path,
                saved_tab_indices: Vec::new(),
                saved_presentations: 0,
                created_file: false,
                added_presentations: 0,
                updated_presentations: 0,
                skipped_clean_presentations: 0,
                missing_identity_presentations: 1,
            });
        }
        let single = save_bluray_metadata_sidecar_from_state(source, state)?;
        return Ok(BluRaySidecarSaveAllOutcome {
            path: single.path,
            saved_tab_indices: vec![0],
            saved_presentations: 1,
            created_file: matches!(single.kind, BluRaySidecarSaveKind::Created),
            added_presentations: if matches!(
                single.kind,
                BluRaySidecarSaveKind::Created | BluRaySidecarSaveKind::AddedPresentation
            ) {
                1
            } else {
                0
            },
            updated_presentations: if matches!(
                single.kind,
                BluRaySidecarSaveKind::UpdatedPresentation
            ) {
                1
            } else {
                0
            },
            skipped_clean_presentations: 0,
            missing_identity_presentations: 0,
        });
    }

    let (sidecar_path, mut sidecars) = load_bluray_metadata_sidecar_presentations(source)?
        .unwrap_or_else(|| (default_path, Vec::new()));
    let existed_before_save = sidecar_path.exists();

    let mut dirty_states = Vec::new();
    let mut skipped_clean_presentations = 0usize;
    let mut missing_identity_presentations = 0usize;
    let mut seen_presentation_ids = BTreeMap::<String, usize>::new();

    for (tab_idx, tab) in state.presentation_tabs.iter().enumerate() {
        if !tab.dirty {
            skipped_clean_presentations += 1;
            continue;
        }
        let tab_state = bluray_metadata_editor_state_for_tab(state, tab);
        let Some(identity) = bluray_presentation_identity_from_state(&tab_state) else {
            missing_identity_presentations += 1;
            continue;
        };
        let id = bluray_presentation_id(&identity);
        if let Some(first_idx) = seen_presentation_ids.insert(id.clone(), tab_idx) {
            return Err(format!(
                "dirty Blu-ray presentation tabs {} and {} both resolve to {}; refusing ambiguous save",
                first_idx + 1,
                tab_idx + 1,
                id,
            ));
        }
        dirty_states.push((tab_idx, tab_state, identity));
    }

    let mut saved_tab_indices = Vec::new();
    let mut added_presentations = 0usize;
    let mut updated_presentations = 0usize;

    for (tab_idx, tab_state, identity) in dirty_states {
        let existing_index = unique_bluray_sidecar_index(&sidecars, &identity)?;
        let existing = existing_index.and_then(|idx| sidecars.get(idx).cloned());
        let sidecar = bluray_metadata_sidecar_from_state_preserving(
            source,
            &tab_state,
            existing.as_ref(),
        )?;
        if let Some(idx) = existing_index {
            sidecars[idx] = sidecar;
            updated_presentations += 1;
        } else {
            sidecars.push(sidecar);
            added_presentations += 1;
        }
        saved_tab_indices.push(tab_idx);
    }

    if !saved_tab_indices.is_empty() {
        save_bluray_metadata_sidecar(&sidecar_path, &sidecars)?;
    }

    Ok(BluRaySidecarSaveAllOutcome {
        path: sidecar_path,
        saved_presentations: saved_tab_indices.len(),
        saved_tab_indices,
        created_file: !existed_before_save && (added_presentations + updated_presentations) > 0,
        added_presentations,
        updated_presentations,
        skipped_clean_presentations,
        missing_identity_presentations,
    })
}

fn bluray_metadata_editor_state_for_tab(
    state: &super::app::MetadataEditorState,
    tab: &super::app::PresentationTab,
) -> super::app::MetadataEditorState {
    let mut tab_state = super::app::MetadataEditorState::for_files(
        tab.paths.clone(),
        tab.entries.clone(),
        tab.file_labels.clone(),
        tab.technical_details.clone(),
    );
    {
        let surface = tab_state.active_surface_mut();
        surface.deleted = tab.deleted.clone();
        surface.dirty = tab.dirty;
        surface.sacd_area_kind = tab.sacd_area_kind;
        surface.sacd_stereo_durations = tab.sacd_stereo_durations.clone();
        surface.sacd_multi_channel_durations = tab.sacd_multi_channel_durations.clone();
        surface.dvdv_source_chapters = tab.dvdv_source_chapters.clone();
        surface.dvdv_track_durations = tab.dvdv_track_durations.clone();
        surface.dvdv_angle_number = tab.dvdv_angle_number;
        surface.dvdv_title_angle_count = tab.dvdv_title_angle_count;
        surface.bluray_playlist_number = tab.bluray_playlist_number;
        surface.bluray_audio_pid = tab.bluray_audio_pid;
        surface.bluray_audio_stream_index = tab.bluray_audio_stream_index;
        surface.bluray_angle_number = tab.bluray_angle_number;
        surface.bluray_chapter_durations = tab.bluray_chapter_durations.clone();
    }
    tab_state.read_only = state.read_only;
    tab_state.sacd_sidecar_path = state.sacd_sidecar_path.clone();
    tab_state
}

fn unique_bluray_sidecar_index(
    sidecars: &[BluRayMetadataSidecar],
    target: &BluRayPresentationIdentity,
) -> Result<Option<usize>, String> {
    let mut selected = None;
    for (idx, sidecar) in sidecars.iter().enumerate() {
        if !bluray_metadata_sidecar_kind_supported(&sidecar.source.sidecar_kind) {
            continue;
        }
        if !bluray_presentation_identity_matches_stable_or_reliable_duration(
            sidecar.source.presentation.as_ref(),
            Some(target),
        ) {
            continue;
        }
        if let Some(first) = selected {
            return Err(format!(
                "multiple Blu-ray sidecar presentations match {}; refusing ambiguous save (matches at indexes {} and {})",
                bluray_presentation_id(target),
                first,
                idx,
            ));
        }
        selected = Some(idx);
    }
    Ok(selected)
}

pub fn bluray_metadata_sidecar_from_state(
    source: &Path,
    state: &super::app::MetadataEditorState,
) -> Result<BluRayMetadataSidecar, String> {
    bluray_metadata_sidecar_from_state_preserving(source, state, None)
}

fn bluray_metadata_sidecar_from_state_preserving(
    source: &Path,
    state: &super::app::MetadataEditorState,
    existing: Option<&BluRayMetadataSidecar>,
) -> Result<BluRayMetadataSidecar, String> {
    let n_tracks = state.active_surface().paths.len();
    if n_tracks == 0 {
        return Err("Blu-ray metadata editor has no tracks".to_string());
    }
    let presentation = bluray_presentation_identity_from_state(state)
        .ok_or_else(|| "Blu-ray metadata editor is missing presentation identity".to_string())?;
    let existing = existing.filter(|sidecar| {
        bluray_metadata_sidecar_kind_supported(&sidecar.source.sidecar_kind)
            && bluray_presentation_identity_matches_stable_or_reliable_duration(
                sidecar.source.presentation.as_ref(),
                Some(&presentation),
            )
    });
    let mut album = existing.map(|sidecar| sidecar.album.clone()).unwrap_or_default();
    let existing_tracks_by_number: BTreeMap<u32, &BluRayMetadataTrack> = existing
        .map(|sidecar| sidecar.tracks.iter().map(|track| (track.number, track)).collect())
        .unwrap_or_default();
    let existing_tracks_by_chapter: BTreeMap<u32, &BluRayMetadataTrack> = existing
        .map(|sidecar| {
            sidecar
                .tracks
                .iter()
                .filter_map(|track| track.source_chapter.map(|chapter| (chapter, track)))
                .collect()
        })
        .unwrap_or_default();
    let mut tracks: Vec<BluRayMetadataTrack> = (0..n_tracks)
        .map(|idx| {
            let number = u32::try_from(idx + 1).unwrap_or(u32::MAX);
            let existing_track = existing_tracks_by_chapter
                .get(&number)
                .copied()
                .or_else(|| existing_tracks_by_number.get(&number).copied());
            let label = state.active_surface().file_labels.get(idx).cloned().unwrap_or_else(|| {
                existing_track
                    .map(|track| track.label.clone())
                    .unwrap_or_else(|| format!("{:02}", number))
            });
            BluRayMetadataTrack {
                number,
                label,
                source_chapter: Some(number),
                tags: existing_track.map(|track| track.tags.clone()).unwrap_or_default(),
                extra: existing_track.map(|track| track.extra.clone()).unwrap_or_default(),
            }
        })
        .collect();

    for (entry_idx, entry) in state.active_surface().entries.iter().enumerate() {
        let Some(sidecar_key) = bluray_editor_key_to_sidecar_key(&entry.display_key) else {
            continue;
        };
        let entry_deleted = state.active_surface().deleted.contains(&entry_idx);
        let is_album_level_key = bluray_is_album_level_sidecar_key(&sidecar_key);
        if !is_album_level_key {
            album.remove(sidecar_key.as_str());
            for track in &mut tracks {
                track.tags.remove(sidecar_key.as_str());
            }
        }
        match bluray_editor_field_scope(&sidecar_key, entry, existing) {
            BluRayEditorFieldScope::Album => {
                if entry.is_mixed && !entry_deleted {
                    return Err(format!(
                        "album-level field {} has mixed values; cannot save Blu-ray sidecar",
                        entry.display_key
                    ));
                }
                if is_album_level_key {
                    album.remove(sidecar_key.as_str());
                    if sidecar_key == "DATE" {
                        album.remove("YEAR");
                    }
                }
                if !entry_deleted {
                    let value = entry.value.trim();
                    if !value.is_empty() {
                        album.insert(sidecar_key.clone(), value.to_string());
                    }
                }
            }
            BluRayEditorFieldScope::Track => {
                for (idx, track) in tracks.iter_mut().enumerate() {
                    if is_album_level_key {
                        track.tags.remove(sidecar_key.as_str());
                    }
                    if entry_deleted {
                        continue;
                    }
                    let value = entry
                        .per_file_values
                        .get(idx)
                        .map(|value| value.trim())
                        .unwrap_or_else(|| entry.value.trim());
                    if !value.is_empty() && value != "<multiple values>" {
                        track.tags.insert(sidecar_key.clone(), value.to_string());
                    }
                }
            }
        }
    }

    Ok(BluRayMetadataSidecar {
        schema_version: BLURAY_METADATA_SIDECAR_SCHEMA_VERSION,
        source: BluRayMetadataSource {
            path: source.to_path_buf(),
            sidecar_kind: BLURAY_METADATA_FORMAT.to_string(),
            presentation: Some(presentation),
            extra: existing.map(|sidecar| sidecar.source.extra.clone()).unwrap_or_default(),
        },
        album,
        tracks,
        extra: existing.map(|sidecar| sidecar.extra.clone()).unwrap_or_default(),
    })
}

pub fn preload_active_bluray_metadata_editor_from_sidecar(
    app: &mut super::app::AppState,
    source: &Path,
) -> Result<bool, String> {
    let mut status_note = None;
    let applied = match &mut app.active_overlay {
        super::app::ActiveOverlay::MetadataEditor(state) => {
            let report =
                preload_bluray_metadata_editor_state_from_sidecar_with_report(source, state)?;
            status_note = bluray_sidecar_preload_status_note(&report);
            report.applied_any()
        }
        _ => false,
    };
    if let Some(note) = status_note {
        app.set_status(format!("Blu-ray metadata preload: {note}"));
    }
    Ok(applied)
}

pub fn preload_bluray_metadata_editor_state_from_sidecar(
    source: &Path,
    state: &mut super::app::MetadataEditorState,
) -> Result<bool, String> {
    Ok(preload_bluray_metadata_editor_state_from_sidecar_with_report(source, state)?
        .applied_any())
}

pub fn preload_bluray_metadata_editor_state_from_sidecar_with_report(
    source: &Path,
    state: &mut super::app::MetadataEditorState,
) -> Result<BluRaySidecarPreloadReport, String> {
    let Some((_, sidecars)) = load_bluray_metadata_sidecar_presentations(source)? else {
        return Ok(BluRaySidecarPreloadReport::default());
    };
    Ok(preload_bluray_metadata_editor_state_from_sidecars_with_report(state, &sidecars))
}

/// Apply parsed Blu-ray sidecars to an editor state exactly once.
///
/// Semantics are deliberately explicit: a state without presentation tabs is
/// treated as a single active presentation, while a multi-tab state preloads
/// every tab by its own stable/fingerprint-checked identity and then mirrors
/// the active tab back into the live editor fields. Callers that already loaded
/// the sidecar should use this helper instead of re-reading the TOML file.
pub fn preload_bluray_metadata_editor_state_from_sidecars(
    state: &mut super::app::MetadataEditorState,
    sidecars: &[BluRayMetadataSidecar],
) -> bool {
    preload_bluray_metadata_editor_state_from_sidecars_with_report(state, sidecars).applied_any()
}

pub fn preload_bluray_metadata_editor_state_from_sidecars_with_report(
    state: &mut super::app::MetadataEditorState,
    sidecars: &[BluRayMetadataSidecar],
) -> BluRaySidecarPreloadReport {
    let mut preload_report = BluRaySidecarPreloadReport {
        sidecar_count: sidecars.len(),
        ..BluRaySidecarPreloadReport::default()
    };
    if sidecars.is_empty() {
        return preload_report;
    }

    if state.presentation_tabs.is_empty() {
        preload_report.attempted_presentations = 1;
        let Some(identity) = bluray_presentation_identity_from_state(state) else {
            return preload_report;
        };
        let report = find_unique_matching_bluray_metadata_sidecar(sidecars, &identity, true);
        log_bluray_sidecar_match_warnings("editor preload", &report.warnings);
        let selected = report.selected;
        preload_report.warnings.extend(report.warnings);
        let Some(sidecar) = selected else {
            return preload_report;
        };
        let track_count = state.active_surface().paths.len();
        let surface = state.active_surface_mut();
        bluray_apply_sidecar_to_editor_fields(&mut surface.entries, track_count, sidecar);
        bluray_apply_sidecar_track_labels(&mut surface.file_labels, sidecar);
        surface.deleted.clear();
        surface.dirty = false;
        preload_report.applied_presentations = 1;
        return preload_report;
    }

    preload_report.attempted_presentations = state.presentation_tabs.len();
    for tab in &mut state.presentation_tabs {
        let Some(identity) = bluray_presentation_identity_from_tab(tab) else {
            continue;
        };
        let report = find_unique_matching_bluray_metadata_sidecar(sidecars, &identity, true);
        log_bluray_sidecar_match_warnings("editor preload", &report.warnings);
        let selected = report.selected;
        preload_report.warnings.extend(report.warnings);
        let Some(sidecar) = selected else {
            continue;
        };
        bluray_apply_sidecar_to_editor_fields(&mut tab.entries, tab.paths.len(), sidecar);
        bluray_apply_sidecar_track_labels(&mut tab.file_labels, sidecar);
        if let Some(album) = sidecar.album.get("ALBUM").filter(|value| !value.trim().is_empty()) {
            tab.label = album.trim().to_string();
        }
        tab.deleted.clear();
        tab.dirty = false;
        preload_report.applied_presentations += 1;
    }

    if preload_report.applied_any() {
        bluray_sync_active_tab_to_editor_state(state);
    }
    preload_report
}

fn bluray_sync_active_tab_to_editor_state(state: &mut super::app::MetadataEditorState) {
    if let Some(active) = state.presentation_tabs.get(state.active_tab).cloned() {
        state.active_surface_mut().paths = active.paths;
        state.active_surface_mut().entries = active.entries;
        state.active_surface_mut().file_labels = active.file_labels;
        state.active_surface_mut().deleted = active.deleted;
        state.active_surface_mut().dirty = active.dirty;
        state.active_surface_mut().sacd_area_kind = active.sacd_area_kind;
        state.active_surface_mut().sacd_stereo_durations = active.sacd_stereo_durations;
        state.active_surface_mut().sacd_multi_channel_durations = active.sacd_multi_channel_durations;
        state.active_surface_mut().dvdv_source_chapters = active.dvdv_source_chapters;
        state.active_surface_mut().dvdv_track_durations = active.dvdv_track_durations;
        state.active_surface_mut().dvdv_angle_number = active.dvdv_angle_number;
        state.active_surface_mut().dvdv_title_angle_count = active.dvdv_title_angle_count;
        state.active_surface_mut().bluray_playlist_number = active.bluray_playlist_number;
        state.active_surface_mut().bluray_audio_pid = active.bluray_audio_pid;
        state.active_surface_mut().bluray_audio_stream_index = active.bluray_audio_stream_index;
        state.active_surface_mut().bluray_angle_number = active.bluray_angle_number;
        state.active_surface_mut().bluray_chapter_durations = active.bluray_chapter_durations;
    }
}

fn bluray_apply_sidecar_track_labels(
    labels: &mut [String],
    sidecar: &BluRayMetadataSidecar,
) {
    for (idx, label) in labels.iter_mut().enumerate() {
        let Some(track) = bluray_sidecar_track_for_editor_index(sidecar, idx) else {
            continue;
        };
        let candidate = track.label.trim();
        if !candidate.is_empty() {
            *label = candidate.to_string();
        }
    }
}

fn bluray_apply_sidecar_to_editor_fields(
    entries: &mut Vec<super::probe::TagEntry>,
    file_count: usize,
    sidecar: &BluRayMetadataSidecar,
) {
    let n_files = file_count.max(1);
    let mut album_keys: Vec<&str> = vec!["ALBUM", "ALBUMARTIST", "GENRE", "DATE"];
    for key in sidecar.album.keys().map(String::as_str) {
        let key = if key == "YEAR" { "DATE" } else { key };
        if !album_keys.contains(&key) {
            album_keys.push(key);
        }
    }
    for key in album_keys {
        let value = if key == "DATE" {
            sidecar
                .album
                .get("DATE")
                .or_else(|| sidecar.album.get("YEAR"))
        } else {
            sidecar.album.get(key)
        };
        if let Some(value) = value
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            bluray_upsert_editor_entry(entries, key, value, vec![value.to_string(); n_files]);
        }
    }

    let mut track_keys = std::collections::BTreeSet::new();
    for track in &sidecar.tracks {
        for key in track.tags.keys() {
            track_keys.insert(key.as_str());
        }
    }

    for key in track_keys {
        let per_file_values: Vec<String> = (0..n_files)
            .map(|idx| {
                bluray_sidecar_track_for_editor_index(sidecar, idx)
                    .and_then(|track| track.tags.get(key))
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();
        if per_file_values.iter().all(|value| value.trim().is_empty()) {
            continue;
        }
        let first = per_file_values.first().cloned().unwrap_or_default();
        let mixed = per_file_values.iter().any(|value| value != &first);
        let display_value = if mixed {
            "<multiple values>".to_string()
        } else {
            first
        };
        bluray_upsert_editor_entry(entries, key, &display_value, per_file_values);
    }

    super::probe::sort_entries_standard_first(entries);
}

fn bluray_upsert_editor_entry(
    entries: &mut Vec<super::probe::TagEntry>,
    key: &str,
    value: &str,
    per_file_values: Vec<String>,
) {
    let value = value.to_string();
    let is_mixed = value == "<multiple values>";
    if let Some(entry) = entries.iter_mut().find(|entry| {
        bluray_editor_key_to_sidecar_key(&entry.display_key).as_deref() == Some(key)
    }) {
        entry.value = value.clone();
        entry.original = value;
        entry.is_binary = false;
        entry.is_mixed = is_mixed;
        entry.per_file_values = per_file_values.clone();
        entry.per_file_originals = per_file_values;
        entry.mb_proposed_value = None;
        entry.mb_proposed_per_file = None;
        return;
    }

    entries.push(super::probe::TagEntry {
        row_scope: crate::tui::probe::RowScope::File,
        display_key: key.to_string(),
        item_key: lofty::tag::ItemKey::Unknown(key.to_string()),
        value: value.clone(),
        original: value,
        is_binary: false,
        is_mixed,
        per_file_originals: per_file_values.clone(),
        per_file_values,
        mb_proposed_value: None,
        mb_proposed_per_file: None,
    });
}

fn bluray_sidecar_track_for_editor_index<'a>(
    sidecar: &'a BluRayMetadataSidecar,
    idx: usize,
) -> Option<&'a BluRayMetadataTrack> {
    let chapter = u32::try_from(idx + 1).ok()?;
    sidecar
        .tracks
        .iter()
        .find(|track| track.source_chapter == Some(chapter))
        .or_else(|| sidecar.tracks.iter().find(|track| track.number == chapter))
        .or_else(|| sidecar.tracks.get(idx))
}

fn bluray_presentation_identity_from_state(
    state: &super::app::MetadataEditorState,
) -> Option<BluRayPresentationIdentity> {
    if let Some(tab) = state.presentation_tabs.get(state.active_tab) {
        if let Some(identity) = bluray_presentation_identity_from_tab(tab) {
            return Some(identity);
        }
    }
    let playlist_number = state.active_surface().bluray_playlist_number?;
    let audio_pid = state.active_surface().bluray_audio_pid?;
    let audio_stream_index = state.active_surface().bluray_audio_stream_index?;
    let angle_number = state.active_surface().bluray_angle_number;
    let track_count = Some(u32::try_from(state.active_surface().paths.len()).ok()?);
    let duration_fingerprint = state.active_surface()
        .bluray_chapter_durations
        .as_deref()
        .and_then(bluray_reliable_chapter_duration_fingerprint_from_secs);
    Some(BluRayPresentationIdentity {
        playlist_number,
        audio_pid,
        audio_stream_index,
        angle_number,
        track_count,
        duration_fingerprint,
        extra: BTreeMap::new(),
    })
}

fn bluray_presentation_identity_from_tab(
    tab: &super::app::PresentationTab,
) -> Option<BluRayPresentationIdentity> {
    let (playlist_number, audio_pid, audio_stream_index, display_angle) = tab.id.blu_ray_parts()?;
    let duration_fingerprint = tab
        .bluray_chapter_durations
        .as_deref()
        .and_then(bluray_reliable_chapter_duration_fingerprint_from_secs);
    Some(BluRayPresentationIdentity {
        playlist_number: tab.bluray_playlist_number.unwrap_or(playlist_number),
        audio_pid: tab.bluray_audio_pid.unwrap_or(audio_pid),
        audio_stream_index: tab.bluray_audio_stream_index.unwrap_or(audio_stream_index),
        angle_number: tab.bluray_angle_number.or(Some(display_angle)),
        track_count: Some(u32::try_from(tab.paths.len()).ok()?),
        duration_fingerprint,
        extra: BTreeMap::new(),
    })
}


pub fn bluray_reliable_chapter_duration_fingerprint_from_secs(durations: &[f64]) -> Option<String> {
    if durations.is_empty() || durations.iter().any(|duration| !(duration.is_finite() && *duration > 0.0)) {
        return None;
    }
    Some(bluray_chapter_duration_fingerprint_from_secs(durations))
}

pub fn bluray_chapter_duration_fingerprint_from_secs(durations: &[f64]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for duration in durations {
        let ms = if duration.is_finite() && *duration > 0.0 {
            (*duration * 1000.0).round().clamp(0.0, u64::MAX as f64) as u64
        } else {
            0
        };
        for byte in ms.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    format!("bluray-ms-v1:{}:{:016x}", durations.len(), hash)
}

fn bluray_editor_key_to_sidecar_key(display_key: &str) -> Option<String> {
    let trimmed = display_key.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(
        dvdv_editor_key_to_sidecar_key(trimmed)
            .map(str::to_string)
            .unwrap_or_else(|| trimmed.to_string()),
    )
}

fn bluray_is_album_level_sidecar_key(key: &str) -> bool {
    matches!(
        key,
        "ALBUM"
            | "ALBUMARTIST"
            | "DATE"
            | "YEAR"
            | "ORIGINALDATE"
            | "RELEASECOUNTRY"
            | "GENRE"
            | "CATALOGNUMBER"
            | "PUBLISHER"
            | "DISCNUMBER"
            | "TOTALTRACKS"
            | "MUSICBRAINZ_ALBUMID"
            | "MUSICBRAINZ_ALBUMARTISTID"
            | "MUSICBRAINZ_RELEASEGROUPID"
    )
}

fn bluray_is_known_track_level_sidecar_key(key: &str) -> bool {
    matches!(
        key,
        "TITLE"
            | "ARTIST"
            | "TRACKNUMBER"
            | "ISRC"
            | "COMPOSER"
            | "PERFORMER"
            | "LYRICIST"
            | "ARRANGER"
            | "COPYRIGHT"
            | "COMMENT"
            | "MUSICBRAINZ_TRACKID"
            | "MUSICBRAINZ_RELEASETRACKID"
            | "MUSICBRAINZ_ARTISTID"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BluRayEditorFieldScope {
    Album,
    Track,
}

fn bluray_custom_field_values_are_track_specific(entry: &super::probe::TagEntry) -> bool {
    if entry.per_file_values.len() <= 1 {
        return false;
    }
    let first = entry.per_file_values.first().map(String::as_str).unwrap_or("");
    entry.per_file_values.iter().skip(1).any(|value| value.as_str() != first)
}

fn bluray_editor_entry_value_changed(entry: &super::probe::TagEntry) -> bool {
    entry.value != entry.original || entry.per_file_values != entry.per_file_originals
}

fn bluray_editor_field_scope(
    key: &str,
    entry: &super::probe::TagEntry,
    existing: Option<&BluRayMetadataSidecar>,
) -> BluRayEditorFieldScope {
    if bluray_is_album_level_sidecar_key(key) {
        return BluRayEditorFieldScope::Album;
    }
    if bluray_is_known_track_level_sidecar_key(key) {
        return BluRayEditorFieldScope::Track;
    }

    let track_specific = bluray_custom_field_values_are_track_specific(entry);
    if track_specific {
        return BluRayEditorFieldScope::Track;
    }

    let existing_album = existing
        .map(|sidecar| sidecar.album.contains_key(key))
        .unwrap_or(false);
    let existing_track = existing
        .map(|sidecar| sidecar.tracks.iter().any(|track| track.tags.contains_key(key)))
        .unwrap_or(false);

    // Unknown/custom fields have no intrinsic sidecar scope. Preserve the
    // previous scope for idempotent saves when the editor entry is unchanged,
    // but let an explicit edit that collapses values to one value move the key
    // to album scope. Edits that make per-file values differ move to track
    // scope above. Known album and known track keys return above and never use
    // this inference path.
    if existing_track
        && !existing_album
        && (!bluray_editor_entry_value_changed(entry) || entry.is_mixed)
    {
        BluRayEditorFieldScope::Track
    } else {
        BluRayEditorFieldScope::Album
    }
}

pub fn bluray_presentation_id(identity: &BluRayPresentationIdentity) -> String {
    match identity.angle_number {
        Some(angle) => format!(
            "playlist{:05}-pid0x{:04x}-stream{}-angle{}",
            identity.playlist_number, identity.audio_pid, identity.audio_stream_index, angle
        ),
        None => format!(
            "playlist{:05}-pid0x{:04x}-stream{}",
            identity.playlist_number, identity.audio_pid, identity.audio_stream_index
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BluRayIdentityMismatchReason {
    MissingStoredIdentity,
    MissingCurrentIdentity,
    PlaylistNumber { stored: u32, current: u32 },
    AudioPid { stored: u16, current: u16 },
    AudioStreamIndex { stored: u8, current: u8 },
    AngleNumber { stored: Option<u8>, current: Option<u8> },
    TrackCount { stored: u32, current: u32 },
    DurationFingerprint { stored: String, current: String },
}

pub fn bluray_presentation_identity_mismatch_reasons(
    stored: Option<&BluRayPresentationIdentity>,
    current: Option<&BluRayPresentationIdentity>,
) -> Vec<BluRayIdentityMismatchReason> {
    let Some(stored) = stored else {
        return vec![BluRayIdentityMismatchReason::MissingStoredIdentity];
    };
    let Some(current) = current else {
        return vec![BluRayIdentityMismatchReason::MissingCurrentIdentity];
    };

    let mut reasons = Vec::new();
    if stored.playlist_number != current.playlist_number {
        reasons.push(BluRayIdentityMismatchReason::PlaylistNumber {
            stored: stored.playlist_number,
            current: current.playlist_number,
        });
    }
    if stored.audio_pid != current.audio_pid {
        reasons.push(BluRayIdentityMismatchReason::AudioPid {
            stored: stored.audio_pid,
            current: current.audio_pid,
        });
    }
    if stored.audio_stream_index != current.audio_stream_index {
        reasons.push(BluRayIdentityMismatchReason::AudioStreamIndex {
            stored: stored.audio_stream_index,
            current: current.audio_stream_index,
        });
    }
    if stored.angle_number != current.angle_number {
        reasons.push(BluRayIdentityMismatchReason::AngleNumber {
            stored: stored.angle_number,
            current: current.angle_number,
        });
    }
    if let Some((stored, current)) = stored.track_count.zip(current.track_count) {
        if stored != current {
            reasons.push(BluRayIdentityMismatchReason::TrackCount { stored, current });
        }
    }
    if let Some((stored, current)) = stored
        .duration_fingerprint
        .as_deref()
        .zip(current.duration_fingerprint.as_deref())
    {
        if stored != current {
            reasons.push(BluRayIdentityMismatchReason::DurationFingerprint {
                stored: stored.to_string(),
                current: current.to_string(),
            });
        }
    }
    reasons
}

pub fn bluray_presentation_identity_compatible(
    stored: Option<&BluRayPresentationIdentity>,
    current: Option<&BluRayPresentationIdentity>,
) -> bool {
    bluray_presentation_identity_mismatch_reasons(stored, current).is_empty()
}

fn bluray_identity_reasons_have_stable_mismatch(reasons: &[BluRayIdentityMismatchReason]) -> bool {
    reasons
        .iter()
        .any(|reason| !matches!(reason, BluRayIdentityMismatchReason::DurationFingerprint { .. }))
}

fn bluray_identity_reasons_have_duration_fingerprint_mismatch(
    reasons: &[BluRayIdentityMismatchReason],
) -> bool {
    reasons
        .iter()
        .any(|reason| matches!(reason, BluRayIdentityMismatchReason::DurationFingerprint { .. }))
}

fn bluray_identity_reasons_have_angle_mismatch_without_other_stable_mismatch(
    reasons: &[BluRayIdentityMismatchReason],
) -> bool {
    reasons
        .iter()
        .any(|reason| matches!(reason, BluRayIdentityMismatchReason::AngleNumber { .. }))
        && reasons.iter().all(|reason| {
            matches!(
                reason,
                BluRayIdentityMismatchReason::AngleNumber { .. }
                    | BluRayIdentityMismatchReason::DurationFingerprint { .. }
            )
        })
}

pub fn bluray_presentation_identity_matches_stable_or_reliable_duration(
    stored: Option<&BluRayPresentationIdentity>,
    current: Option<&BluRayPresentationIdentity>,
) -> bool {
    let reasons = bluray_presentation_identity_mismatch_reasons(stored, current);
    !bluray_identity_reasons_have_stable_mismatch(&reasons)
        && !bluray_identity_reasons_have_duration_fingerprint_mismatch(&reasons)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BluRaySidecarMatchWarning {
    MissingPresentationIdentity { sidecar_id: String },
    DurationFingerprintUnavailable { sidecar_id: String },
    DurationFingerprintMismatch { sidecar_id: String },
    AngleNumberMismatch { sidecar_id: String },
    Ambiguous { first_id: String, duplicate_id: String },
}

#[derive(Debug)]
pub struct BluRaySidecarMatchReport<'a> {
    pub selected: Option<&'a BluRayMetadataSidecar>,
    pub warnings: Vec<BluRaySidecarMatchWarning>,
}

pub fn find_unique_matching_bluray_metadata_sidecar<'a>(
    sidecars: &'a [BluRayMetadataSidecar],
    current: &BluRayPresentationIdentity,
    warn_on_missing_current_fingerprint: bool,
) -> BluRaySidecarMatchReport<'a> {
    let mut selected = None;
    let mut warnings = Vec::new();

    for sidecar in sidecars {
        if !bluray_metadata_sidecar_kind_supported(&sidecar.source.sidecar_kind) {
            continue;
        }
        let stored = sidecar.source.presentation.as_ref();
        let sidecar_id = || bluray_metadata_sidecar_debug_id(sidecar);
        let Some(stored) = stored else {
            warnings.push(BluRaySidecarMatchWarning::MissingPresentationIdentity {
                sidecar_id: sidecar_id(),
            });
            continue;
        };

        let reasons = bluray_presentation_identity_mismatch_reasons(Some(stored), Some(current));
        if bluray_identity_reasons_have_angle_mismatch_without_other_stable_mismatch(&reasons) {
            warnings.push(BluRaySidecarMatchWarning::AngleNumberMismatch {
                sidecar_id: sidecar_id(),
            });
        }
        if bluray_identity_reasons_have_stable_mismatch(&reasons) {
            continue;
        }

        if bluray_identity_reasons_have_duration_fingerprint_mismatch(&reasons) {
            warnings.push(BluRaySidecarMatchWarning::DurationFingerprintMismatch {
                sidecar_id: sidecar_id(),
            });
            continue;
        }

        if warn_on_missing_current_fingerprint
            && stored.duration_fingerprint.is_some()
            && current.duration_fingerprint.is_none()
        {
            warnings.push(BluRaySidecarMatchWarning::DurationFingerprintUnavailable {
                sidecar_id: sidecar_id(),
            });
        }

        if let Some(first) = selected {
            warnings.push(BluRaySidecarMatchWarning::Ambiguous {
                first_id: bluray_metadata_sidecar_debug_id(first),
                duplicate_id: sidecar_id(),
            });
            return BluRaySidecarMatchReport {
                selected: None,
                warnings,
            };
        }
        selected = Some(sidecar);
    }

    BluRaySidecarMatchReport { selected, warnings }
}

pub fn log_bluray_sidecar_match_warnings(context: &str, warnings: &[BluRaySidecarMatchWarning]) {
    for warning in warnings {
        match warning {
            BluRaySidecarMatchWarning::MissingPresentationIdentity { sidecar_id } => {
                log::warn!(
                    "Blu-ray metadata sidecar {context}: skipped {sidecar_id}; presentation identity is missing"
                );
            }
            BluRaySidecarMatchWarning::DurationFingerprintUnavailable { sidecar_id } => {
                log::warn!(
                    "Blu-ray metadata sidecar {context}: matched {sidecar_id} by stable identity without current chapter-duration fingerprint"
                );
            }
            BluRaySidecarMatchWarning::DurationFingerprintMismatch { sidecar_id } => {
                log::warn!(
                    "Blu-ray metadata sidecar {context}: skipped {sidecar_id}; reliable duration fingerprint changed"
                );
            }
            BluRaySidecarMatchWarning::AngleNumberMismatch { sidecar_id } => {
                log::warn!(
                    "Blu-ray metadata sidecar {context}: skipped {sidecar_id}; angle identity did not match"
                );
            }
            BluRaySidecarMatchWarning::Ambiguous { first_id, duplicate_id } => {
                log::warn!(
                    "Blu-ray metadata sidecar {context}: skipped ambiguous match; duplicate stable identity: {first_id}, {duplicate_id}"
                );
            }
        }
    }
}


fn bluray_plural(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

pub fn bluray_sidecar_preload_status_note(report: &BluRaySidecarPreloadReport) -> Option<String> {
    if report.sidecar_count == 0 {
        return None;
    }

    let degraded_matches = report
        .warnings
        .iter()
        .filter(|warning| {
            matches!(
                warning,
                BluRaySidecarMatchWarning::DurationFingerprintUnavailable { .. }
            )
        })
        .count();
    let skipped_matches = report
        .warnings
        .iter()
        .filter(|warning| {
            matches!(
                warning,
                BluRaySidecarMatchWarning::MissingPresentationIdentity { .. }
                    | BluRaySidecarMatchWarning::DurationFingerprintMismatch { .. }
                    | BluRaySidecarMatchWarning::AngleNumberMismatch { .. }
                    | BluRaySidecarMatchWarning::Ambiguous { .. }
            )
        })
        .count();

    if report.applied_presentations == 0 {
        if skipped_matches > 0 {
            return Some(format!(
                "existing sidecar ignored: no deterministic Blu-ray presentation match; {} skipped",
                bluray_plural(skipped_matches, "sidecar entry", "sidecar entries")
            ));
        }
        return Some("existing sidecar ignored: presentation identity did not match".to_string());
    }

    if degraded_matches == 0 && skipped_matches == 0 {
        return None;
    }

    let mut parts = vec![format!(
        "sidecar preload: {} applied",
        bluray_plural(report.applied_presentations, "presentation", "presentations")
    )];
    if degraded_matches > 0 {
        parts.push(format!(
            "{} matched without duration fingerprint",
            bluray_plural(degraded_matches, "presentation", "presentations")
        ));
    }
    if skipped_matches > 0 {
        parts.push(format!(
            "{} skipped",
            bluray_plural(skipped_matches, "sidecar entry", "sidecar entries")
        ));
    }
    Some(parts.join("; "))
}

pub fn bluray_metadata_sidecar_kind_supported(kind: &str) -> bool {
    let kind = kind.trim();
    kind.is_empty() || kind == BLURAY_METADATA_FORMAT || kind == "blu_ray" || kind == "bluray"
}

pub fn bluray_metadata_sidecar_debug_id(sidecar: &BluRayMetadataSidecar) -> String {
    sidecar
        .source
        .presentation
        .as_ref()
        .map(bluray_presentation_id)
        .unwrap_or_else(|| "missing-presentation-identity".to_string())
}

pub fn bluray_metadata_tag_value<'a>(
    tags: &'a BTreeMap<String, String>,
    keys: &[&str],
) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        tags.get(*key)
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BluRayAlbumTagOverlay {
    pub album_title: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BluRayTrackTagOverlay {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub performer: Option<String>,
}

pub fn bluray_album_tag_overlay(sidecar: &BluRayMetadataSidecar) -> BluRayAlbumTagOverlay {
    BluRayAlbumTagOverlay {
        album_title: bluray_metadata_tag_value(&sidecar.album, &["ALBUM"]).map(str::to_string),
        album_artist: bluray_metadata_tag_value(&sidecar.album, &["ALBUMARTIST"]).map(str::to_string),
        genre: bluray_metadata_tag_value(&sidecar.album, &["GENRE"]).map(str::to_string),
        year: bluray_metadata_tag_value(&sidecar.album, &["DATE", "YEAR"]).map(str::to_string),
    }
}

pub fn bluray_track_tag_overlay(track: &BluRayMetadataTrack) -> BluRayTrackTagOverlay {
    BluRayTrackTagOverlay {
        title: bluray_metadata_tag_value(&track.tags, &["TITLE"]).map(str::to_string),
        artist: bluray_metadata_tag_value(&track.tags, &["ARTIST"]).map(str::to_string),
        performer: bluray_metadata_tag_value(&track.tags, &["PERFORMER"]).map(str::to_string),
    }
}

pub fn bluray_track_overlay_performer_value(overlay: &BluRayTrackTagOverlay) -> Option<&str> {
    overlay.artist.as_deref().or(overlay.performer.as_deref())
}

pub fn bluray_track_tag_overlay_for_authored_chapter(
    sidecar: &BluRayMetadataSidecar,
    authored_chapter: u32,
    allow_legacy_track_number_fallback: bool,
) -> Option<BluRayTrackTagOverlay> {
    bluray_metadata_sidecar_track_by_authored_chapter(
        sidecar,
        authored_chapter,
        allow_legacy_track_number_fallback,
    )
    .map(bluray_track_tag_overlay)
}

pub fn bluray_metadata_sidecar_track_authored_chapter(
    track: &BluRayMetadataTrack,
) -> u32 {
    track.source_chapter.unwrap_or(track.number)
}

pub fn bluray_metadata_sidecar_track_by_authored_chapter<'a>(
    sidecar: &'a BluRayMetadataSidecar,
    authored_chapter: u32,
    allow_legacy_track_number_fallback: bool,
) -> Option<&'a BluRayMetadataTrack> {
    sidecar
        .tracks
        .iter()
        .find(|track| track.source_chapter == Some(authored_chapter))
        .or_else(|| {
            allow_legacy_track_number_fallback
                .then(|| sidecar.tracks.iter().find(|track| track.number == authored_chapter))
                .flatten()
        })
}

fn bluray_metadata_sidecar_candidate_paths(source: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    let looks_like_iso = source
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("iso"));
    if source.is_file() || looks_like_iso {
        let parent = source.parent().unwrap_or_else(|| Path::new("."));
        if let Some(stem) = source.file_stem().and_then(|value| value.to_str()) {
            push_unique_path(&mut candidates, parent.join(format!("{stem}.bluray.metadata.toml")));
        }
    }

    if source.is_dir() {
        if let Some(root) = crate::disc::bluray_utils::bluray_directory_root(source) {
            push_unique_path(&mut candidates, root.join(BLURAY_METADATA_SIDECAR_NAME));
        }
        push_unique_path(&mut candidates, source.join(BLURAY_METADATA_SIDECAR_NAME));
    }

    if source
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("BDMV"))
    {
        if let Some(parent) = source.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            push_unique_path(&mut candidates, parent.join(BLURAY_METADATA_SIDECAR_NAME));
        }
    }

    if candidates.is_empty() {
        if let Some(parent) = source.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            push_unique_path(&mut candidates, parent.join(BLURAY_METADATA_SIDECAR_NAME));
        }
        push_unique_path(&mut candidates, source.join(BLURAY_METADATA_SIDECAR_NAME));
    }

    candidates
}

fn push_unique_path(candidates: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !candidates.iter().any(|existing| existing == &candidate) {
        candidates.push(candidate);
    }
}

fn parse_bluray_metadata_toml_presentation(
    path: &Path,
    schema_version: u32,
    presentation_table: &Table,
) -> Result<BluRayMetadataSidecar, String> {
    let source_table = presentation_table.get("source").and_then(Item::as_table);
    let source_path = source_table
        .and_then(|source| source.get("path"))
        .and_then(Item::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| path.to_path_buf());
    let sidecar_kind = source_table
        .and_then(|source| source.get("sidecar_kind"))
        .and_then(Item::as_str)
        .unwrap_or(BLURAY_METADATA_FORMAT)
        .to_string();
    let presentation = bluray_toml_source_identity(source_table);
    let source_extra = source_table
        .map(|table| {
            bluray_toml_table_extra_json(
                table,
                &[
                    "path",
                    "sidecar_kind",
                    "presentation",
                    "playlist_number",
                    "playlist",
                    "audio_pid",
                    "pid",
                    "audio_stream_index",
                    "audio_stream",
                    "angle_number",
                    "angle",
                    "track_count",
                    "chapter_count",
                    "duration_fingerprint",
                ],
            )
        })
        .unwrap_or_default();
    let album = bluray_toml_album_table_to_map(presentation_table.get("album").and_then(Item::as_table));
    let tracks = bluray_toml_tracks_to_vec(presentation_table.get("tracks").and_then(Item::as_array_of_tables));
    let extra = bluray_toml_table_extra_json(presentation_table, &["id", "source", "album", "tracks"]);

    Ok(BluRayMetadataSidecar {
        schema_version,
        source: BluRayMetadataSource {
            path: source_path,
            sidecar_kind,
            presentation,
            extra: source_extra,
        },
        album,
        tracks,
        extra,
    })
}

fn bluray_toml_source_identity(source: Option<&Table>) -> Option<BluRayPresentationIdentity> {
    let source = source?;
    let presentation = source
        .get("presentation")
        .and_then(Item::as_table)
        .unwrap_or(source);
    Some(BluRayPresentationIdentity {
        playlist_number: item_integer_u32(presentation, &["playlist_number", "playlist"])?,
        audio_pid: item_integer_u16_or_hex(presentation, &["audio_pid", "pid"])?,
        audio_stream_index: item_integer_u8(presentation, &["audio_stream_index", "audio_stream"])?,
        angle_number: item_integer_u8(presentation, &["angle_number", "angle"]),
        track_count: item_integer_u32(presentation, &["track_count", "chapter_count"]),
        duration_fingerprint: item_string(presentation, &["duration_fingerprint"]),
        extra: bluray_toml_table_extra_json(
            presentation,
            &[
                "playlist_number",
                "playlist",
                "audio_pid",
                "pid",
                "audio_stream_index",
                "audio_stream",
                "angle_number",
                "angle",
                "track_count",
                "chapter_count",
                "duration_fingerprint",
            ],
        ),
    })
}

fn bluray_toml_album_table_to_map(album_table: Option<&Table>) -> BTreeMap<String, String> {
    let mut album = BTreeMap::new();
    let Some(album_table) = album_table else {
        return album;
    };
    for (key, item) in album_table.iter() {
        if key == "extra" {
            continue;
        }
        let internal_key = bluray_toml_album_key_to_internal(key)
            .map(str::to_string)
            .unwrap_or_else(|| key.to_string());
        if let Some(value) = toml_item_to_string(item) {
            album.insert(internal_key, value);
        }
    }
    if let Some(extra) = album_table.get("extra").and_then(Item::as_table) {
        for (key, item) in extra.iter() {
            if let Some(value) = toml_item_to_string(item) {
                album.insert(key.to_string(), value);
            }
        }
    }
    album
}

fn bluray_toml_tracks_to_vec(track_tables: Option<&ArrayOfTables>) -> Vec<BluRayMetadataTrack> {
    let mut tracks = Vec::new();
    let Some(track_tables) = track_tables else {
        return tracks;
    };
    for table in track_tables.iter() {
        let number = table
            .get("number")
            .and_then(Item::as_integer)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or_else(|| u32::try_from(tracks.len() + 1).unwrap_or(u32::MAX));
        let label = table
            .get("label")
            .and_then(Item::as_str)
            .map(str::to_string);
        let source_chapter = table
            .get("source_chapter")
            .and_then(Item::as_integer)
            .and_then(|v| u32::try_from(v).ok());
        let mut tags = BTreeMap::new();
        if let Some(tags_table) = table.get("tags").and_then(Item::as_table) {
            bluray_toml_tags_table_to_map(tags_table, &mut tags, BLURAY_TRACK_PRIMARY_TOML_KEYS);
        }
        for (key, item) in table.iter() {
            if matches!(key, "number" | "label" | "source_chapter" | "tags" | "extra") {
                continue;
            }
            let Some(internal_key) = bluray_toml_track_key_to_internal(key) else {
                continue;
            };
            if let Some(value) = toml_item_to_string(item) {
                tags.insert(internal_key.to_string(), value);
            }
        }
        let mut extra = bluray_toml_table_extra_json(
            table,
            &["number", "label", "source_chapter", "tags", "extra", "title", "artist", "performer", "TITLE", "ARTIST", "PERFORMER"],
        );
        if let Some(extra_table) = table.get("extra").and_then(Item::as_table) {
            extra.extend(bluray_toml_table_extra_json(extra_table, &[]));
        }
        let label = label
            .or_else(|| tags.get("TITLE").cloned())
            .unwrap_or_else(|| format!("{:02}", number));
        tracks.push(BluRayMetadataTrack {
            number,
            label,
            source_chapter,
            tags,
            extra,
        });
    }
    tracks
}

fn bluray_toml_tags_table_to_map(
    table: &Table,
    out: &mut BTreeMap<String, String>,
    primary_keys: &[(&'static str, &'static str)],
) {
    for (key, item) in table.iter() {
        if key == "extra" {
            continue;
        }
        let internal_key = bluray_toml_key_to_internal(key, primary_keys).unwrap_or(key);
        if let Some(value) = toml_item_to_string(item) {
            out.insert(internal_key.to_string(), value);
        }
    }
    if let Some(extra) = table.get("extra").and_then(Item::as_table) {
        for (key, item) in extra.iter() {
            if let Some(value) = toml_item_to_string(item) {
                out.insert(key.to_string(), value);
            }
        }
    }
}

fn bluray_sidecars_to_toml_string_for_path(
    path: &Path,
    sidecars: &[BluRayMetadataSidecar],
) -> Result<String, String> {
    let mut doc = bluray_toml_document_for_preserving_save(path)?;
    doc["schema_version"] = value(i64::from(BLURAY_METADATA_SIDECAR_SCHEMA_VERSION));
    doc["format"] = value(BLURAY_METADATA_FORMAT);
    for sidecar in sidecars {
        bluray_upsert_toml_presentation(&mut doc, sidecar);
    }
    Ok(doc.to_string())
}

fn bluray_toml_document_for_preserving_save(path: &Path) -> Result<DocumentMut, String> {
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    let payload = fs::read_to_string(path)
        .map_err(|e| format!("read existing Blu-ray TOML sidecar {}: {e}", path.display()))?;
    payload
        .parse::<DocumentMut>()
        .map_err(|e| format!("parse existing Blu-ray TOML sidecar {}: {e}", path.display()))
}

fn bluray_upsert_toml_presentation(doc: &mut DocumentMut, sidecar: &BluRayMetadataSidecar) {
    let target = sidecar.source.presentation.as_ref();
    let mut updated = false;
    let existing = doc.get("presentations").and_then(Item::as_array_of_tables).cloned();
    let mut presentations = ArrayOfTables::new();
    if let Some(existing) = existing {
        for table in existing.iter() {
            let mut table = table.clone();
            if !updated && bluray_toml_presentation_table_matches_target(&table, target) {
                bluray_write_presentation_table(&mut table, sidecar);
                updated = true;
            }
            presentations.push(table);
        }
    }
    if !updated {
        let mut table = Table::new();
        bluray_write_presentation_table(&mut table, sidecar);
        presentations.push(table);
    }
    doc["presentations"] = Item::ArrayOfTables(presentations);
}

fn bluray_toml_presentation_table_matches_target(
    table: &Table,
    target: Option<&BluRayPresentationIdentity>,
) -> bool {
    let stored = table
        .get("source")
        .and_then(Item::as_table)
        .and_then(|source| bluray_toml_source_identity(Some(source)));
    bluray_presentation_identity_matches_stable_or_reliable_duration(stored.as_ref(), target)
}

fn bluray_write_presentation_table(table: &mut Table, sidecar: &BluRayMetadataSidecar) {
    if let Some(identity) = sidecar.source.presentation.as_ref() {
        table.insert("id", value(bluray_presentation_id(identity)));
    }
    insert_json_extras(
        table,
        &sidecar.extra,
        &["id", "source", "album", "tracks"],
    );
    bluray_write_source_subtable(table, &sidecar.source);
    bluray_write_album_subtable(table, &sidecar.album);
    bluray_write_track_subtables(table, &sidecar.tracks);
}

fn bluray_write_source_subtable(table: &mut Table, source: &BluRayMetadataSource) {
    if !table.get("source").map_or(false, Item::is_table) {
        table.insert("source", Item::Table(Table::new()));
    }
    let source_table = table
        .get_mut("source")
        .and_then(Item::as_table_mut)
        .expect("presentations.source table");
    source_table.insert("path", value(source.path.to_string_lossy().as_ref()));
    source_table.insert(
        "sidecar_kind",
        value(if source.sidecar_kind.trim().is_empty() {
            BLURAY_METADATA_FORMAT
        } else {
            source.sidecar_kind.as_str()
        }),
    );
    insert_json_extras(
        source_table,
        &source.extra,
        &[
            "path",
            "sidecar_kind",
            "presentation",
            "playlist_number",
            "playlist",
            "audio_pid",
            "pid",
            "audio_stream_index",
            "audio_stream",
            "angle_number",
            "angle",
            "track_count",
            "chapter_count",
            "duration_fingerprint",
        ],
    );
    if let Some(identity) = source.presentation.as_ref() {
        if !source_table.get("presentation").map_or(false, Item::is_table) {
            source_table.insert("presentation", Item::Table(Table::new()));
        }
        let presentation = source_table
            .get_mut("presentation")
            .and_then(Item::as_table_mut)
            .expect("presentations.source.presentation table");
        presentation.insert("playlist_number", value(i64::from(identity.playlist_number)));
        presentation.insert("audio_pid", value(i64::from(identity.audio_pid)));
        presentation.insert("audio_stream_index", value(i64::from(identity.audio_stream_index)));
        set_toml_optional_i64(&mut *presentation, "angle_number", identity.angle_number.map(i64::from));
        set_toml_optional_i64(&mut *presentation, "track_count", identity.track_count.map(i64::from));
        set_toml_optional_string(
            &mut *presentation,
            "duration_fingerprint",
            identity.duration_fingerprint.as_deref(),
        );
        insert_json_extras(
            presentation,
            &identity.extra,
            &[
                "playlist_number",
                "playlist",
                "audio_pid",
                "pid",
                "audio_stream_index",
                "audio_stream",
                "angle_number",
                "angle",
                "track_count",
                "chapter_count",
                "duration_fingerprint",
            ],
        );
    } else {
        source_table.remove("presentation");
    }
}

fn bluray_write_album_subtable(table: &mut Table, album: &BTreeMap<String, String>) {
    if !table.get("album").map_or(false, Item::is_table) {
        table.insert("album", Item::Table(Table::new()));
    }
    let album_table = table
        .get_mut("album")
        .and_then(Item::as_table_mut)
        .expect("presentations.album table");
    let stale_album_keys: Vec<String> = album_table
        .iter()
        .filter_map(|(key, _)| {
            let is_primary_alias = bluray_toml_album_key_to_internal(key).is_some();
            let is_direct_custom = key != "extra" && !is_primary_alias;
            (key != "extra" && (is_primary_alias || is_direct_custom)).then(|| key.to_string())
        })
        .collect();
    for key in stale_album_keys {
        album_table.remove(&key);
    }
    for (internal, toml_key) in BLURAY_ALBUM_PRIMARY_TOML_KEYS {
        set_toml_optional_string(album_table, toml_key, album.get(*internal).map(String::as_str));
    }
    if !album_table.get("extra").map_or(false, Item::is_table) {
        album_table.insert("extra", Item::Table(Table::new()));
    }
    let remove_extra = {
        let extra = album_table
            .get_mut("extra")
            .and_then(Item::as_table_mut)
            .expect("presentations.album.extra table");
        let wanted_extra_keys: std::collections::BTreeSet<&str> = album
            .keys()
            .filter(|key| !BLURAY_ALBUM_PRIMARY_TOML_KEYS.iter().any(|(internal, _)| internal == key))
            .map(String::as_str)
            .collect();
        let stale_extra_keys: Vec<String> = extra
            .iter()
            .filter_map(|(key, _)| (!wanted_extra_keys.contains(key)).then(|| key.to_string()))
            .collect();
        for key in stale_extra_keys {
            extra.remove(&key);
        }
        for (key, value) in album {
            if !BLURAY_ALBUM_PRIMARY_TOML_KEYS.iter().any(|(internal, _)| internal == key) {
                extra.insert(key, toml_edit::value(value.as_str()));
            }
        }
        extra.iter().next().is_none()
    };
    if remove_extra {
        album_table.remove("extra");
    }
}

fn bluray_write_track_subtables(table: &mut Table, tracks: &[BluRayMetadataTrack]) {
    let mut existing_by_identity: BTreeMap<(Option<u32>, u32), Table> = BTreeMap::new();
    if let Some(existing) = table.get("tracks").and_then(Item::as_array_of_tables) {
        for existing_track in existing.iter() {
            let number = existing_track
                .get("number")
                .and_then(Item::as_integer)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0);
            let source_chapter = existing_track
                .get("source_chapter")
                .and_then(Item::as_integer)
                .and_then(|value| u32::try_from(value).ok());
            existing_by_identity.insert((source_chapter, number), existing_track.clone());
        }
    }
    let mut tracks_array = ArrayOfTables::new();
    for track in tracks {
        let key = (track.source_chapter, track.number);
        let mut track_table = existing_by_identity.remove(&key).unwrap_or_else(Table::new);
        track_table.insert("number", value(i64::from(track.number)));
        track_table.insert("label", value(track.label.as_str()));
        set_toml_optional_i64(&mut track_table, "source_chapter", track.source_chapter.map(i64::from));
        insert_json_extras(
            &mut track_table,
            &track.extra,
            &["number", "label", "source_chapter", "tags"],
        );
        if !track_table.get("tags").map_or(false, Item::is_table) {
            track_table.insert("tags", Item::Table(Table::new()));
        }
        let tags = track_table
            .get_mut("tags")
            .and_then(Item::as_table_mut)
            .expect("presentations.tracks.tags table");
        let stale_tag_keys: Vec<String> = tags
            .iter()
            .filter_map(|(key, _)| {
                let is_primary_alias = bluray_toml_track_key_to_internal(key).is_some();
                let is_direct_custom = key != "extra" && !is_primary_alias;
                (key != "extra" && (is_primary_alias || is_direct_custom)).then(|| key.to_string())
            })
            .collect();
        for key in stale_tag_keys {
            tags.remove(&key);
        }
        for (internal, toml_key) in BLURAY_TRACK_PRIMARY_TOML_KEYS {
            set_toml_optional_string(tags, toml_key, track.tags.get(*internal).map(String::as_str));
        }
        if !tags.get("extra").map_or(false, Item::is_table) {
            tags.insert("extra", Item::Table(Table::new()));
        }
        let remove_extra_tags = {
            let extra_tags = tags
                .get_mut("extra")
                .and_then(Item::as_table_mut)
                .expect("presentations.tracks.tags.extra table");
            let wanted_extra_tag_keys: std::collections::BTreeSet<&str> = track
                .tags
                .keys()
                .filter(|key| !BLURAY_TRACK_PRIMARY_TOML_KEYS.iter().any(|(internal, _)| internal == key))
                .map(String::as_str)
                .collect();
            let stale_extra_tag_keys: Vec<String> = extra_tags
                .iter()
                .filter_map(|(key, _)| (!wanted_extra_tag_keys.contains(key)).then(|| key.to_string()))
                .collect();
            for key in stale_extra_tag_keys {
                extra_tags.remove(&key);
            }
            for (key, value) in &track.tags {
                if !BLURAY_TRACK_PRIMARY_TOML_KEYS.iter().any(|(internal, _)| internal == key) {
                    extra_tags.insert(key, toml_edit::value(value.as_str()));
                }
            }
            extra_tags.iter().next().is_none()
        };
        if remove_extra_tags {
            tags.remove("extra");
        }
        tracks_array.push(track_table);
    }
    table.insert("tracks", Item::ArrayOfTables(tracks_array));
}

fn insert_json_extras(table: &mut Table, extras: &BTreeMap<String, serde_json::Value>, reserved: &[&str]) {
    for (key, value) in extras {
        if reserved.iter().any(|reserved| key.eq_ignore_ascii_case(reserved)) {
            continue;
        }
        table.insert(key, json_value_to_toml_item(value));
    }
}

fn json_value_to_toml_item(json: &serde_json::Value) -> Item {
    match json {
        serde_json::Value::Null => value(""),
        serde_json::Value::Bool(flag) => value(*flag),
        serde_json::Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                value(integer)
            } else if let Some(float) = number.as_f64() {
                value(float)
            } else {
                value(number.to_string())
            }
        }
        serde_json::Value::String(text) => value(text.as_str()),
        serde_json::Value::Array(values) => {
            let mut array = Array::new();
            for value in values {
                array.push(json_value_to_toml_value(value));
            }
            Item::Value(Value::Array(array))
        }
        serde_json::Value::Object(values) => {
            let mut table = InlineTable::new();
            for (key, value) in values {
                table.insert(key, json_value_to_toml_value(value));
            }
            Item::Value(Value::InlineTable(table))
        }
    }
}

fn json_value_to_toml_value(value: &serde_json::Value) -> Value {
    match json_value_to_toml_item(value) {
        Item::Value(value) => value,
        _ => Value::from(value.to_string()),
    }
}

fn item_integer_u32(table: &Table, keys: &[&str]) -> Option<u32> {
    keys.iter()
        .find_map(|key| table.get(*key))
        .and_then(Item::as_integer)
        .and_then(|value| u32::try_from(value).ok())
}

fn item_integer_u16_or_hex(table: &Table, keys: &[&str]) -> Option<u16> {
    keys.iter().find_map(|key| table.get(*key)).and_then(|item| {
        item.as_integer()
            .and_then(|value| u16::try_from(value).ok())
            .or_else(|| {
                item.as_str().and_then(|value| {
                    let trimmed = value.trim();
                    let hex = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X"));
                    match hex {
                        Some(hex) => u16::from_str_radix(hex, 16).ok(),
                        None => trimmed.parse::<u16>().ok(),
                    }
                })
            })
    })
}

fn item_integer_u8(table: &Table, keys: &[&str]) -> Option<u8> {
    keys.iter()
        .find_map(|key| table.get(*key))
        .and_then(Item::as_integer)
        .and_then(|value| u8::try_from(value).ok())
}

fn item_string(table: &Table, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| table.get(*key))
        .and_then(Item::as_str)
        .map(str::to_string)
}

const BLURAY_ALBUM_PRIMARY_TOML_KEYS: &[(&str, &str)] = &[
    ("ALBUM", "album"),
    ("ALBUMARTIST", "album_artist"),
    ("GENRE", "genre"),
    ("DATE", "date"),
    ("YEAR", "year"),
];
const BLURAY_TRACK_PRIMARY_TOML_KEYS: &[(&str, &str)] = &[
    ("TITLE", "title"),
    ("ARTIST", "artist"),
    ("PERFORMER", "performer"),
];
fn bluray_toml_album_key_to_internal(key: &str) -> Option<&'static str> {
    bluray_toml_key_to_internal(key, BLURAY_ALBUM_PRIMARY_TOML_KEYS)
}
fn bluray_toml_track_key_to_internal(key: &str) -> Option<&'static str> {
    bluray_toml_key_to_internal(key, BLURAY_TRACK_PRIMARY_TOML_KEYS)
}
fn bluray_toml_key_to_internal(key: &str, table: &[(&'static str, &'static str)]) -> Option<&'static str> {
    table.iter().find_map(|(internal, toml_key)| {
        (key == *internal || key.eq_ignore_ascii_case(internal) || key == *toml_key || key.eq_ignore_ascii_case(toml_key))
            .then_some(*internal)
    })
}

pub fn dvdv_metadata_sidecar_from_state(
    source: &Path,
    state: &super::app::MetadataEditorState,
) -> Result<DvdVideoMetadataSidecar, String> {
    dvdv_metadata_sidecar_from_state_preserving(source, state, None)
}

fn dvdv_metadata_sidecar_from_state_preserving(
    source: &Path,
    state: &super::app::MetadataEditorState,
    existing: Option<&DvdVideoMetadataSidecar>,
) -> Result<DvdVideoMetadataSidecar, String> {
    let n_tracks = state.active_surface().paths.len();
    if n_tracks == 0 { return Err("DVD-Video metadata editor has no tracks".to_string()); }
    let presentation = dvdv_presentation_identity_from_state(state);
    let existing = existing.filter(|sidecar| dvdv_existing_sidecar_can_merge(sidecar, presentation.as_ref()));
    let mut album = existing.map(|sidecar| sidecar.album.clone()).unwrap_or_default();
    let existing_tracks_by_number: BTreeMap<usize, &DvdVideoMetadataTrack> = existing
        .map(|sidecar| sidecar.tracks.iter().map(|track| (track.number, track)).collect())
        .unwrap_or_default();
    let mut tracks: Vec<DvdVideoMetadataTrack> = (0..n_tracks).map(|idx| {
        let number = idx + 1;
        let existing_track = existing_tracks_by_number.get(&number).copied();
        let label = state.active_surface().file_labels.get(idx).cloned().unwrap_or_else(|| {
            existing_track.map(|track| track.label.clone()).unwrap_or_else(|| format!("{:02}", number))
        });
        let (source_title, source_chapter) = dvdv_track_source_from_state(state, idx, presentation.as_ref(), existing_track, &label);
        DvdVideoMetadataTrack {
            number, label, source_title, source_chapter,
            tags: existing_track.map(|track| track.tags.clone()).unwrap_or_default(),
            extra: existing_track.map(|track| track.extra.clone()).unwrap_or_default(),
        }
    }).collect();
    for (entry_idx, entry) in state.active_surface().entries.iter().enumerate() {
        let Some(sidecar_key) = dvdv_editor_key_to_sidecar_key(&entry.display_key) else { continue; };
        if dvdv_is_album_level_sidecar_key(sidecar_key) {
            if entry.is_mixed && !state.active_surface().deleted.contains(&entry_idx) {
                return Err(format!("album-level field {} has mixed values; cannot save DVD-Video sidecar", entry.display_key));
            }
            album.remove(sidecar_key);
            if !state.active_surface().deleted.contains(&entry_idx) {
                let value = entry.value.trim();
                if !value.is_empty() { album.insert(sidecar_key.to_string(), value.to_string()); }
            }
        } else {
            for (idx, track) in tracks.iter_mut().enumerate() {
                track.tags.remove(sidecar_key);
                if state.active_surface().deleted.contains(&entry_idx) { continue; }
                let value = entry.per_file_values.get(idx).map(|value| value.trim()).unwrap_or_default();
                if !value.is_empty() { track.tags.insert(sidecar_key.to_string(), value.to_string()); }
            }
        }
    }
    Ok(DvdVideoMetadataSidecar {
        schema_version: DVDV_METADATA_SIDECAR_SCHEMA_VERSION,
        source: DvdVideoMetadataSource {
            path: source.to_path_buf(), sidecar_kind: "dvd_video".to_string(), presentation,
            extra: existing.map(|sidecar| sidecar.source.extra.clone()).unwrap_or_default(),
        },
        album,
        tracks,
        extra: existing.map(|sidecar| sidecar.extra.clone()).unwrap_or_default(),
    })
}

fn dvdv_track_source_from_state(
    state: &super::app::MetadataEditorState,
    idx: usize,
    presentation: Option<&DvdVideoPresentationIdentity>,
    existing_track: Option<&DvdVideoMetadataTrack>,
    label: &str,
) -> (Option<u8>, Option<u16>) {
    if let (Some(presentation), Some(chapter)) = (
        presentation,
        state.active_surface().dvdv_source_chapters.as_ref().and_then(|chapters| chapters.get(idx)).copied(),
    ) {
        return (Some(presentation.title_number), Some(chapter));
    }
    if let Some(track) = existing_track.and_then(|track| track.source_title.zip(track.source_chapter)) {
        return (Some(track.0), Some(track.1));
    }
    if let Some((title, chapter)) = dvdv_track_source_from_label(label) {
        return (Some(title), Some(chapter));
    }
    (
        presentation.map(|presentation| presentation.title_number),
        Some((idx + 1).min(u16::MAX as usize) as u16),
    )
}

fn dvdv_track_source_from_label(label: &str) -> Option<(u8, u16)> {
    // Display labels are usually prefixed by the output ordinal, e.g.
    // `01 Title 7 Chapter 1`; parse the semantic title/chapter phrase only.
    let lower = label.to_ascii_lowercase();
    let title_pos = lower.find("title")?;
    let chapter_rel = lower[title_pos..].find("chapter")?;
    let chapter_pos = title_pos + chapter_rel;
    let title = parse_first_u16(&lower[title_pos + "title".len()..chapter_pos])
        .and_then(|value| u8::try_from(value).ok())?;
    let chapter = parse_first_u16(&lower[chapter_pos + "chapter".len()..])?;
    Some((title, chapter))
}

fn parse_first_u16(value: &str) -> Option<u16> {
    value
        .split(|ch: char| !ch.is_ascii_digit())
        .find(|part| !part.is_empty())
        .and_then(|part| part.parse::<u16>().ok())
}

fn dvdv_existing_sidecar_can_merge(
    sidecar: &DvdVideoMetadataSidecar,
    presentation: Option<&DvdVideoPresentationIdentity>,
) -> bool {
    sidecar.source.sidecar_kind == "dvd_video" && dvdv_presentation_identity_compatible(sidecar.source.presentation.as_ref(), presentation)
}

pub fn dvdv_presentation_identity_compatible(
    stored: Option<&DvdVideoPresentationIdentity>,
    current: Option<&DvdVideoPresentationIdentity>,
) -> bool {
    match (stored, current) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(stored), Some(current)) => {
            stored.vts_number == current.vts_number
                && stored.title_number == current.title_number
                && stored.audio_stream_index == current.audio_stream_index
                && dvdv_sparse_angle_identity_compatible(stored.angle_number, current.angle_number)
                && stored.track_count.zip(current.track_count).map_or(true, |(stored, current)| stored == current)
                && stored.duration_fingerprint.as_deref().zip(current.duration_fingerprint.as_deref()).map_or(true, |(stored, current)| stored == current)
        }
    }
}

fn dvdv_sparse_angle_identity_compatible(stored: Option<u8>, current: Option<u8>) -> bool {
    match (stored, current) {
        (None, None) => true,
        (Some(stored), Some(current)) => stored == current,
        // A sparse current identity means a single-angle selected title. Accept
        // an explicit angle-1 document, but never an angle-specific alternate.
        (Some(1), None) => true,
        (Some(_), None) => false,
        // An angle-less sidecar presentation must not apply to a multi-angle
        // selected title, whose current identity carries `Some(angle)`.
        (None, Some(_)) => false,
    }
}

fn dvdv_presentation_identity_from_state(state: &super::app::MetadataEditorState) -> Option<DvdVideoPresentationIdentity> {
    let presentation_id = state.presentation_tabs.get(state.active_tab).map(|tab| &tab.id)?;
    let (vts_number, title_number, audio_stream_index) = presentation_id.dvd_video_parts()?;
    let track_count = Some(state.active_surface().paths.len());
    let duration_fingerprint = state.active_surface().dvdv_track_durations.as_deref().filter(|durations| !durations.is_empty()).map(dvdv_track_duration_fingerprint_from_secs);
    let angle_number = match (state.active_surface().dvdv_title_angle_count, state.active_surface().dvdv_angle_number) {
        (Some(count), Some(angle)) if count > 1 => Some(angle),
        _ => None,
    };
    Some(DvdVideoPresentationIdentity {
        vts_number, title_number, audio_stream_index,
        angle_number,
        track_count,
        duration_fingerprint,
    })
}

pub fn dvdv_track_duration_fingerprint_from_secs(durations: &[f64]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for duration in durations {
        let ms = dvdv_duration_ms(*duration);
        for byte in ms.to_le_bytes() { hash ^= u64::from(byte); hash = hash.wrapping_mul(0x100000001b3); }
    }
    format!("dvdv-ms-v1:{}:{:016x}", durations.len(), hash)
}

fn dvdv_duration_ms(duration_secs: f64) -> u64 {
    if duration_secs.is_finite() && duration_secs > 0.0 { (duration_secs * 1000.0).round().clamp(0.0, u64::MAX as f64) as u64 } else { 0 }
}

fn write_dvdv_metadata_sidecar_atomic(path: &Path, sidecar: &DvdVideoMetadataSidecar) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| format!("DVD-Video sidecar path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("create DVD-Video sidecar directory {}: {e}", parent.display()))?;
    let payload = dvdv_sidecar_to_toml_string(path, sidecar)?;
    let tmp = unique_sidecar_temp_path(path);
    let write_result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new().create_new(true).write(true).open(&tmp)
            .map_err(|e| format!("create temporary DVD-Video sidecar {}: {e}", tmp.display()))?;
        file.write_all(payload.as_bytes()).map_err(|e| format!("write temporary DVD-Video sidecar {}: {e}", tmp.display()))?;
        if !payload.ends_with('\n') { file.write_all(b"\n").map_err(|e| format!("finish temporary DVD-Video sidecar {}: {e}", tmp.display()))?; }
        file.sync_all().map_err(|e| format!("sync temporary DVD-Video sidecar {}: {e}", tmp.display()))?;
        drop(file);
        atomic_replace_file(&tmp, path).map_err(|e| format!("atomically publish DVD-Video TOML sidecar {}: {e}", path.display()))?;
        if let Ok(dir) = fs::File::open(parent) { let _ = dir.sync_all(); }
        Ok(())
    })();
    if write_result.is_err() { let _ = fs::remove_file(&tmp); }
    write_result
}

fn dvdv_sidecar_to_toml_string(path: &Path, sidecar: &DvdVideoMetadataSidecar) -> Result<String, String> {
    let mut doc = dvdv_toml_document_for_multi_presentation_save(path)?;
    doc["schema_version"] = value(i64::from(DVDV_METADATA_SIDECAR_SCHEMA_VERSION));
    doc["format"] = value(DVDV_METADATA_FORMAT);
    // The multi-presentation schema owns all presentation-specific data under
    // [[presentations]]. Do not keep the v12 top-level single-presentation
    // tables as write targets.
    doc.remove("source");
    doc.remove("album");
    doc.remove("tracks");
    dvdv_upsert_toml_presentation(&mut doc, sidecar);
    Ok(doc.to_string())
}

fn dvdv_toml_document_for_multi_presentation_save(path: &Path) -> Result<DocumentMut, String> {
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    let payload = fs::read_to_string(path)
        .map_err(|e| format!("read existing DVD-Video TOML sidecar {}: {e}", path.display()))?;
    payload.parse::<DocumentMut>()
        .map_err(|e| format!("parse existing DVD-Video TOML sidecar {}: {e}", path.display()))
}

fn dvdv_upsert_toml_presentation(doc: &mut DocumentMut, sidecar: &DvdVideoMetadataSidecar) {
    let target = sidecar.source.presentation.as_ref();
    let mut updated = false;
    let existing = doc.get("presentations").and_then(Item::as_array_of_tables).cloned();
    let mut presentations = ArrayOfTables::new();
    if let Some(existing) = existing {
        for table in existing.iter() {
            let mut table = table.clone();
            if !updated && dvdv_toml_presentation_table_matches_target(&table, target) {
                dvdv_write_presentation_table(&mut table, sidecar);
                updated = true;
            }
            presentations.push(table);
        }
    }
    if !updated {
        let mut table = Table::new();
        dvdv_write_presentation_table(&mut table, sidecar);
        presentations.push(table);
    }
    doc["presentations"] = Item::ArrayOfTables(presentations);
}

fn dvdv_write_presentation_table(table: &mut Table, sidecar: &DvdVideoMetadataSidecar) {
    if let Some(identity) = sidecar.source.presentation.as_ref() {
        table.insert("id", value(dvdv_presentation_id(identity)));
    }
    dvdv_write_source_subtable(table, sidecar.source.presentation.as_ref());
    dvdv_write_album_subtable(table, &sidecar.album);
    dvdv_write_track_subtables(table, &sidecar.tracks);
}

fn dvdv_write_source_subtable(table: &mut Table, identity: Option<&DvdVideoPresentationIdentity>) {
    if !table.get("source").map_or(false, Item::is_table) {
        table.insert("source", Item::Table(Table::new()));
    }
    let source = table.get_mut("source").and_then(Item::as_table_mut).expect("presentations.source table");
    if let Some(identity) = identity {
        source.insert("vts", value(i64::from(identity.vts_number)));
        source.insert("title", value(i64::from(identity.title_number)));
        source.insert("audio_stream", value(i64::from(identity.audio_stream_index)));
        set_toml_optional_i64(source, "angle", identity.angle_number.map(i64::from));
        set_toml_optional_i64(source, "track_count", identity.track_count.map(|value| value as i64));
        set_toml_optional_string(source, "duration_fingerprint", identity.duration_fingerprint.as_deref());
    }
}

fn dvdv_write_album_subtable(table: &mut Table, album: &BTreeMap<String, String>) {
    if !table.get("album").map_or(false, Item::is_table) {
        table.insert("album", Item::Table(Table::new()));
    }
    let album_table = table.get_mut("album").and_then(Item::as_table_mut).expect("presentations.album table");
    for (internal, toml_key) in DVDV_ALBUM_PRIMARY_TOML_KEYS {
        set_toml_optional_string(album_table, toml_key, album.get(*internal).map(String::as_str));
    }
    if !album_table.get("extra").map_or(false, Item::is_table) {
        album_table.insert("extra", Item::Table(Table::new()));
    }
    let extra = album_table.get_mut("extra").and_then(Item::as_table_mut).expect("presentations.album.extra table");
    for (key, value) in album {
        if !DVDV_ALBUM_PRIMARY_TOML_KEYS.iter().any(|(internal, _)| internal == key) {
            extra.insert(key, toml_edit::value(value.as_str()));
        }
    }
}

fn dvdv_write_track_subtables(table: &mut Table, tracks: &[DvdVideoMetadataTrack]) {
    let mut existing_by_identity: BTreeMap<(Option<u8>, Option<u16>, usize), Table> = BTreeMap::new();
    if let Some(existing) = table.get("tracks").and_then(Item::as_array_of_tables) {
        for existing_track in existing.iter() {
            let number = existing_track.get("number").and_then(Item::as_integer)
                .and_then(|value| usize::try_from(value).ok()).unwrap_or(0);
            let source_title = existing_track.get("source_title").and_then(Item::as_integer).and_then(|value| u8::try_from(value).ok());
            let source_chapter = existing_track.get("source_chapter").and_then(Item::as_integer).and_then(|value| u16::try_from(value).ok());
            existing_by_identity.insert((source_title, source_chapter, number), existing_track.clone());
        }
    }
    let mut tracks_array = ArrayOfTables::new();
    for track in tracks {
        let key = (track.source_title, track.source_chapter, track.number);
        let mut track_table = existing_by_identity.remove(&key).unwrap_or_else(Table::new);
        track_table.insert("number", value(track.number as i64));
        set_toml_optional_i64(&mut track_table, "source_title", track.source_title.map(i64::from));
        set_toml_optional_i64(&mut track_table, "source_chapter", track.source_chapter.map(i64::from));
        for (internal, toml_key) in DVDV_TRACK_PRIMARY_TOML_KEYS {
            set_toml_optional_string(&mut track_table, toml_key, track.tags.get(*internal).map(String::as_str));
        }
        if !track_table.get("extra").map_or(false, Item::is_table) {
            track_table.insert("extra", Item::Table(Table::new()));
        }
        let extra = track_table.get_mut("extra").and_then(Item::as_table_mut).expect("presentations.tracks.extra table");
        for (key, value) in &track.tags {
            if !DVDV_TRACK_PRIMARY_TOML_KEYS.iter().any(|(internal, _)| internal == key) {
                extra.insert(key, toml_edit::value(value.as_str()));
            }
        }
        tracks_array.push(track_table);
    }
    table.insert("tracks", Item::ArrayOfTables(tracks_array));
}

fn set_toml_optional_string(table: &mut Table, key: &str, value: Option<&str>) {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => { table.insert(key, toml_edit::value(value)); }
        None => { table.remove(key); }
    }
}
fn set_toml_optional_i64(table: &mut Table, key: &str, value: Option<i64>) {
    match value { Some(value) => { table.insert(key, toml_edit::value(value)); } None => { table.remove(key); } }
}

const DVDV_ALBUM_PRIMARY_TOML_KEYS: &[(&str, &str)] = &[
    ("ARTIST", "artist"), ("ALBUMARTIST", "album_artist"), ("ALBUM", "album"), ("GENRE", "genre"),
    ("DATE", "date"), ("DISCNUMBER", "disc_number"), ("TOTALTRACKS", "total_tracks"),
    ("MUSICBRAINZ_ALBUMID", "musicbrainz_albumid"),
    ("MUSICBRAINZ_ALBUMARTISTID", "musicbrainz_albumartistid"),
    ("MUSICBRAINZ_RELEASEGROUPID", "musicbrainz_releasegroupid"),
];
const DVDV_TRACK_PRIMARY_TOML_KEYS: &[(&str, &str)] = &[
    ("TITLE", "title"), ("ARTIST", "artist"), ("ALBUMARTIST", "album_artist"), ("GENRE", "genre"),
    ("DATE", "date"), ("TRACKNUMBER", "track_number"), ("DISCNUMBER", "disc_number"),
    ("ISRC", "isrc"), ("COMPOSER", "composer"), ("PERFORMER", "performer"),
    ("PUBLISHER", "publisher"), ("COPYRIGHT", "copyright"), ("COMMENT", "comment"),
    ("MUSICBRAINZ_TRACKID", "musicbrainz_trackid"),
    ("MUSICBRAINZ_RELEASETRACKID", "musicbrainz_releasetrackid"),
    ("MUSICBRAINZ_ARTISTID", "musicbrainz_artistid"),
];
fn dvdv_toml_album_key_to_internal(key: &str) -> Option<&'static str> { dvdv_toml_key_to_internal(key, DVDV_ALBUM_PRIMARY_TOML_KEYS) }
fn dvdv_toml_track_key_to_internal(key: &str) -> Option<&'static str> { dvdv_toml_key_to_internal(key, DVDV_TRACK_PRIMARY_TOML_KEYS) }
fn dvdv_toml_key_to_internal(key: &str, table: &[(&'static str, &'static str)]) -> Option<&'static str> {
    table.iter().find_map(|(internal, toml_key)| (*toml_key == key).then_some(*internal))
}
fn dvdv_toml_extra_key_to_internal(key: &str) -> String {
    // Normalize known MusicBrainz tag keys to uppercase Vorbis comment convention.
    match key.to_ascii_lowercase().as_str() {
        "musicbrainz_albumid" => "MUSICBRAINZ_ALBUMID".to_string(),
        "musicbrainz_albumartistid" => "MUSICBRAINZ_ALBUMARTISTID".to_string(),
        "musicbrainz_artistid" => "MUSICBRAINZ_ARTISTID".to_string(),
        "musicbrainz_releasegroupid" => "MUSICBRAINZ_RELEASEGROUPID".to_string(),
        "musicbrainz_releasetrackid" => "MUSICBRAINZ_RELEASETRACKID".to_string(),
        "musicbrainz_trackid" => "MUSICBRAINZ_TRACKID".to_string(),
        _ => key.to_string(),
    }
}

fn atomic_replace_file(src: &Path, dst: &Path) -> io::Result<()> { atomic_replace_file_impl(src, dst) }
#[cfg(unix)]
fn atomic_replace_file_impl(src: &Path, dst: &Path) -> io::Result<()> { fs::rename(src, dst) }
#[cfg(windows)]
fn atomic_replace_file_impl(src: &Path, dst: &Path) -> io::Result<()> {
    match fs::rename(src, dst) { Ok(()) => return Ok(()), Err(first_err) if first_err.kind() == io::ErrorKind::AlreadyExists => {}, Err(first_err) if dst.exists() => { let _ = first_err; }, Err(first_err) => return Err(first_err), }
    let backup = unique_replace_backup_path(dst);
    fs::rename(dst, &backup)?;
    match fs::rename(src, dst) {
        Ok(()) => { let _ = fs::remove_file(&backup); Ok(()) }
        Err(promote_err) => {
            let restore_result = fs::rename(&backup, dst);
            if let Err(restore_err) = restore_result { return Err(io::Error::new(promote_err.kind(), format!("failed to publish replacement '{}': {promote_err}; also failed to restore previous sidecar from '{}': {restore_err}", dst.display(), backup.display()))); }
            Err(promote_err)
        }
    }
}
#[cfg(windows)]
fn unique_replace_backup_path(dst: &Path) -> PathBuf {
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let file_name = dst.file_name().and_then(|name| name.to_str()).unwrap_or("dvdvideo-metadata");
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or(0);
    parent.join(format!(".{file_name}.replace-backup.{}.{}.tmp", std::process::id(), nanos))
}
#[cfg(not(any(unix, windows)))]
fn atomic_replace_file_impl(src: &Path, dst: &Path) -> io::Result<()> { fs::rename(src, dst) }
fn unique_sidecar_temp_path(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or(0);
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("dvdvideo-metadata");
    path.with_file_name(format!(".{file_name}.{}.{}.tmp", std::process::id(), nanos + u128::from(counter)))
}

fn dvdv_editor_key_to_sidecar_key(display_key: &str) -> Option<&'static str> {
    match display_key.to_ascii_uppercase().as_str() {
        "ALBUM" => Some("ALBUM"),
        "ALBUMARTIST" | "ALBUM ARTIST" => Some("ALBUMARTIST"),
        "DATE" | "YEAR" => Some("DATE"),
        "ORIGINALDATE" | "ORIGINAL DATE" => Some("ORIGINALDATE"),
        "RELEASECOUNTRY" | "RELEASE COUNTRY" => Some("RELEASECOUNTRY"),
        "CATALOGNUMBER" | "CATALOG NUMBER" | "CATNO" => Some("CATALOGNUMBER"),
        "GENRE" => Some("GENRE"),
        "PUBLISHER" | "LABEL" => Some("PUBLISHER"),
        "DISCNUMBER" | "DISC NUMBER" => Some("DISCNUMBER"),
        "TRACKNUMBER" | "TRACK NUMBER" => Some("TRACKNUMBER"),
        "TITLE" => Some("TITLE"),
        "ARTIST" => Some("ARTIST"),
        "PERFORMER" => Some("PERFORMER"),
        "COMPOSER" => Some("COMPOSER"),
        "LYRICIST" => Some("LYRICIST"),
        "ARRANGER" => Some("ARRANGER"),
        "ISRC" => Some("ISRC"),
        "COPYRIGHT" => Some("COPYRIGHT"),
        "COMMENT" | "DESCRIPTION" => Some("COMMENT"),
        "MUSICBRAINZ_TRACKID" | "MUSICBRAINZ RECORDING ID" => Some("MUSICBRAINZ_TRACKID"),
        "MUSICBRAINZ_RELEASETRACKID" | "MUSICBRAINZ TRACK ID" => {
            Some("MUSICBRAINZ_RELEASETRACKID")
        }
        "MUSICBRAINZ_ARTISTID" | "MUSICBRAINZ ARTIST ID" => Some("MUSICBRAINZ_ARTISTID"),
        "MUSICBRAINZ_ALBUMID" | "MUSICBRAINZ RELEASE ID" => Some("MUSICBRAINZ_ALBUMID"),
        "MUSICBRAINZ_ALBUMARTISTID" | "MUSICBRAINZ ALBUM ARTIST ID" => {
            Some("MUSICBRAINZ_ALBUMARTISTID")
        }
        "MUSICBRAINZ_RELEASEGROUPID" | "MUSICBRAINZ RELEASE GROUP ID" => {
            Some("MUSICBRAINZ_RELEASEGROUPID")
        }
        _ => None,
    }
}

fn dvdv_is_album_level_sidecar_key(key: &str) -> bool {
    matches!(
        key,
        "ALBUM"
            | "ALBUMARTIST"
            | "DATE"
            | "ORIGINALDATE"
            | "RELEASECOUNTRY"
            | "CATALOGNUMBER"
            | "GENRE"
            | "PUBLISHER"
            | "DISCNUMBER"
            | "MUSICBRAINZ_ALBUMID"
            | "MUSICBRAINZ_ALBUMARTISTID"
            | "MUSICBRAINZ_RELEASEGROUPID"
    )
}

/// Common spawn point for the unified `:tags-mb` TOC flow. Browse audio-file
/// selection, SACD, DVD-Audio, and DVD-Video editors compute their own sectors
/// and `toc_string`, then call this to do the cache check, status, and async
/// `lookup_release_by_toc` fire. The result re-enters via `MbOutcome::Toc` and
/// routes through the shared handler.
///
/// `paths` is the audio paths (or ISO replicated per-track for SACD).
/// `editor_park` is true when an editor is sitting in
/// `active_overlay` and should be populated in place. `fallback_seed`
/// is `Some(...)` for SACD, DVD-Audio, and DVD-Video editors where TOC misses
/// are common enough to justify the `search_releases_by_query` fallback hop.
pub(super) fn spawn_tags_mb_toc_lookup(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    mut operation_id: super::message::TagsMbOperationId,
    candidates: Vec<super::musicbrainz::TocCandidate>,
    paths: Vec<std::path::PathBuf>,
    editor_park: bool,
    fallback_seed: Option<SacdMbSeed>,
    editor_session: Option<super::message::MetadataEditorSessionGuard>,
) {
    if operation_id.is_assigned() {
        if !super::event_loop::tags_mb_operation_is_current_phase(
            app,
            operation_id,
            super::app::TagsMbOperationPhase::Lookup,
        ) {
            return;
        }
    } else {
        operation_id = match super::event_loop::begin_tags_mb_lookup_operation(app, editor_park) {
            Ok(operation_id) => operation_id,
            Err(error) => {
                app.set_status(error);
                return;
            }
        };
    }
    // Pre-fetch the cache body for every cascade stage here on the UI
    // thread; the spawned lookup stays database-free.
    let cached: Vec<Option<String>> = candidates
        .iter()
        .map(|candidate| {
            super::musicbrainz::build_mb_toc(&candidate.sectors)
                .and_then(|toc| app.db.get_cached_mb_response(&toc))
        })
        .collect();
    let n_cached = if cached.first().is_some_and(Option::is_some) {
        "cached"
    } else {
        "fetching"
    };
    let n_tracks = candidates
        .first()
        .map(|c| c.sectors.len().saturating_sub(1))
        .unwrap_or(0);
    let n_stages = candidates.len();
    app.set_status(if n_stages > 1 {
        format!(
            ":tags-mb: {} disc TOC ({} tracks, {} stub-drop stages)…",
            n_cached, n_tracks, n_stages,
        )
    } else {
        format!(":tags-mb: {} disc TOC ({} tracks)…", n_cached, n_tracks)
    });

    let tx = tx.clone();
    let ctx = super::message::TagsMbContext {
        operation_id,
        paths,
        editor_park,
        fallback_seed,
        editor_session,
    };

    tokio::spawn(async move {
        let outcome =
            super::musicbrainz::lookup_release_by_toc_cascading(&candidates, cached).await;
        let _ = tx
            .send(AppMessage::TagsFromMbComplete {
                outcome: super::message::MbOutcome::Toc { outcome },
                ctx,
            })
            .await;
    });
}

fn dispatch_regular_file_editor_tags_mb_toc(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    state_owned: Box<super::app::MetadataEditorState>,
) -> bool {
    dispatch_regular_file_editor_tags_mb_toc_from_taken(
        app,
        tx,
        super::event_loop::TakenMetadataEditor {
            state: state_owned,
            slot: super::event_loop::MetadataEditorRestoreSlot::Active,
        },
        super::message::TagsMbOperationId::UNASSIGNED,
    )
}

fn dispatch_regular_file_editor_tags_mb_toc_from_taken(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    taken: super::event_loop::TakenMetadataEditor,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    // File editor: state.active_surface().paths is the audio file set. Use the
    // same TOC derivation the Browse path uses — first try AccurateRip-style
    // offsets in the parent dir, fall back to per-file sample counts. The
    // editor is restored through the slot-aware event-loop helper on every
    // path so a stale async completion cannot overwrite a newer picker/modal.
    let editor_session = metadata_editor_session_guard(&taken.state);
    let paths = taken.state.active_surface().paths.clone();
    let Some(first_path) = paths.first().cloned() else {
        super::event_loop::restore_taken_metadata_editor(app, taken);
        if operation_id.is_assigned() {
            super::event_loop::finish_tags_mb_operation_if_current(app, operation_id);
        }
        app.set_status(":tags-mb: editor has no paths".to_string());
        return true;
    };
    let dir = first_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    let sectors = match super::accuraterip::find_toc_offsets(&dir) {
        Some(s) => s,
        None => match super::accuraterip::collect_sample_counts(&paths) {
            Ok((sample_counts, sample_rate)) => {
                let samples_per_frame = (sample_rate / 75) as u64;
                let mut sectors = Vec::with_capacity(sample_counts.len() + 1);
                let mut frame: u64 = 150;
                for &count in &sample_counts {
                    sectors.push(frame as u32);
                    frame += count / samples_per_frame;
                }
                sectors.push(frame as u32);
                sectors
            }
            Err(e) => {
                super::event_loop::restore_taken_metadata_editor(app, taken);
                if operation_id.is_assigned() {
                    super::event_loop::finish_tags_mb_operation_if_current(app, operation_id);
                }
                app.set_status(format!(":tags-mb: {}", e));
                return true;
            }
        },
    };

    if super::musicbrainz::build_mb_toc(&sectors).is_none() {
        super::event_loop::restore_taken_metadata_editor(app, taken);
        if operation_id.is_assigned() {
            super::event_loop::finish_tags_mb_operation_if_current(app, operation_id);
        }
        app.set_status(":tags-mb: TOC too short".to_string());
        return true;
    }

    super::event_loop::restore_taken_metadata_editor(app, taken);
    spawn_tags_mb_toc_lookup(
        app,
        tx,
        operation_id,
        vec![super::musicbrainz::TocCandidate::exact(sectors)],
        paths,
        /* editor_park */ true,
        None,
        Some(editor_session),
    );
    true
}

fn spawn_in_editor_split_cue_musicbrainz_info_collection(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    mut request: InEditorSplitCueMusicBrainzInfoRequest,
) {
    if request.operation_id.is_assigned() {
        if !super::event_loop::tags_mb_operation_is_current_phase(
            app,
            request.operation_id,
            super::app::TagsMbOperationPhase::Discovery,
        ) {
            return;
        }
    } else {
        request.operation_id = match super::event_loop::begin_tags_mb_prelookup_operation(
            app,
            true,
            super::app::TagsMbOperationPhase::Discovery,
        ) {
            Ok(operation_id) => operation_id,
            Err(error) => {
                app.set_status(error);
                return;
            }
        };
    }
    let label = match request.source {
        InEditorSplitCueMusicBrainzSource::UnifiedAlbum => "unified CUE album",
        InEditorSplitCueMusicBrainzSource::PresentationTabs => "split CUE editor",
        InEditorSplitCueMusicBrainzSource::SingleSourceFolder => "source folder",
    };
    app.set_status(format!(":tags-mb: discovering {label} CUE parts…"));
    let tx = tx.clone();
    tokio::spawn(async move {
        let sources = request.sources.clone();
        let audio_paths = request.audio_paths.clone();
        let result = task::spawn_blocking(move || {
            collect_single_image_cue_infos_for_sources(&sources, &audio_paths)
        })
        .await
        .map_err(|err| format!("split-CUE discovery task failed: {err}"));
        let _ = tx
            .send(AppMessage::InEditorSplitCueMusicBrainzInfoComplete {
                request: Box::new(request),
                result,
            })
            .await;
    });
}

pub(super) fn handle_in_editor_split_cue_musicbrainz_info_complete(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    request: InEditorSplitCueMusicBrainzInfoRequest,
    result: Result<Vec<super::cue_parser::SingleImageInfo>, String>,
) {
    if !super::event_loop::tags_mb_operation_is_current_phase(
        app,
        request.operation_id,
        super::app::TagsMbOperationPhase::Discovery,
    ) {
        return;
    }

    let Some(taken) = super::event_loop::take_metadata_editor_with_restore_slot(app) else {
        if super::event_loop::finish_tags_mb_operation_if_current(app, request.operation_id) {
            app.set_status(
                ":tags-mb: editor closed during split-CUE discovery; rerun".to_string(),
            );
        }
        return;
    };
    if !super::event_loop::metadata_editor_matches_session_guard(
        &taken.state,
        request.editor_session,
    ) {
        super::event_loop::restore_taken_metadata_editor(app, taken);
        if super::event_loop::finish_tags_mb_operation_if_current(app, request.operation_id) {
            app.set_status(
                ":tags-mb: metadata editor changed during split-CUE discovery; rerun"
                    .to_string(),
            );
        }
        return;
    }

    let infos = match result {
        Ok(infos) => infos,
        Err(err) => {
            super::event_loop::restore_taken_metadata_editor(app, taken);
            if super::event_loop::finish_tags_mb_operation_if_current(app, request.operation_id) {
                app.set_status(format!(":tags-mb: {err}"));
            }
            return;
        }
    };

    if !super::event_loop::transition_tags_mb_operation_phase(
        app,
        request.operation_id,
        super::app::TagsMbOperationPhase::Discovery,
        super::app::TagsMbOperationPhase::Grouping,
    ) {
        super::event_loop::restore_taken_metadata_editor(app, taken);
        return;
    }

    match request.source {
        InEditorSplitCueMusicBrainzSource::UnifiedAlbum
        | InEditorSplitCueMusicBrainzSource::PresentationTabs => {
            super::event_loop::restore_taken_metadata_editor(app, taken);
            if dispatch_split_cue_musicbrainz_concat_or_text_fallback(
                app,
                tx,
                request.operation_id,
                &infos,
                true,
                request.active_audio_path,
                Some(request.editor_session),
            ) {
                return;
            }
            if super::event_loop::finish_tags_mb_operation_if_current(app, request.operation_id) {
                let kind = match request.source {
                    InEditorSplitCueMusicBrainzSource::UnifiedAlbum => "unified CUE album",
                    _ => "split CUE editor",
                };
                app.set_status(format!(
                    ":tags-mb: {kind} has no concatenated TOC or album/artist seed"
                ));
            }
        }
        InEditorSplitCueMusicBrainzSource::SingleSourceFolder => {
            let Some(active_audio_path) = request.active_audio_path else {
                super::event_loop::restore_taken_metadata_editor(app, taken);
                if super::event_loop::finish_tags_mb_operation_if_current(
                    app,
                    request.operation_id,
                ) {
                    app.set_status(":tags-mb: editor has no active audio path".to_string());
                }
                return;
            };
            let same_folder_infos: Vec<_> = infos
                .into_iter()
                .filter(|info| {
                    info.cue_path.parent() == request.sources.first().map(PathBuf::as_path)
                })
                .collect();
            let contains_active = same_folder_infos
                .iter()
                .any(|info| same_path_for_split_cue(&info.audio_path, &active_audio_path));
            if same_folder_infos.len() < 2 || !contains_active {
                if !super::event_loop::transition_tags_mb_operation_phase(
                    app,
                    request.operation_id,
                    super::app::TagsMbOperationPhase::Grouping,
                    super::app::TagsMbOperationPhase::Lookup,
                ) {
                    super::event_loop::restore_taken_metadata_editor(app, taken);
                    return;
                }
                dispatch_regular_file_editor_tags_mb_toc_from_taken(
                    app,
                    tx,
                    taken,
                    request.operation_id,
                );
                return;
            }
            if taken.state.any_presentation_dirty() {
                super::event_loop::restore_taken_metadata_editor(app, taken);
                if super::event_loop::finish_tags_mb_operation_if_current(
                    app,
                    request.operation_id,
                ) {
                    app.set_status(
                        ":tags-mb: source folder contains multiple CUE surfaces; save or revert editor changes before running split-CUE MusicBrainz lookup"
                            .to_string(),
                    );
                }
                return;
            }
            super::event_loop::restore_taken_metadata_editor(app, taken);
            if dispatch_split_cue_musicbrainz_concat_or_text_fallback(
                app,
                tx,
                request.operation_id,
                &same_folder_infos,
                true,
                Some(active_audio_path),
                Some(request.editor_session),
            ) {
                return;
            }
            if super::event_loop::finish_tags_mb_operation_if_current(app, request.operation_id) {
                app.set_status(
                    ":tags-mb: split CUE source folder has no concatenated TOC or album/artist seed"
                        .to_string(),
                );
            }
        }
    }
}


/// Dispatch `:tags-mb` when a metadata editor is open. Handles both
/// SACD ISOs (TOC from per-area durations) and regular file editors
/// (TOC from accuraterip helpers on `state.active_surface().paths`). Returns
/// `Some(true)` when the dispatch fired (caller short-circuits) or
/// `None` when no editor is in scope (caller falls through to the
/// Browse path).
///
/// `direct_seed = Some(...)` means the user supplied explicit args
/// (`:tags-mb --catno X miles davis`); the dispatch then skips TOC
/// entirely and spawns a text search using the supplied seed. The
/// editor still gets parked and populated in place as for any
/// in-editor dispatch.
///
/// **Parking-slot invariant:** the editor must be left in
/// `active_overlay`, never in `pending_metadata_editor`, before this
/// returns. The CommandInput Enter and ContextMenu wrappers
/// auto-restore `pending → active` only when `active == None`, so
/// leaving the editor parked would cause it to be drained before
/// our async result arrives.
fn try_dispatch_in_editor_tags_mb(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    direct_seed: Option<SacdMbSeed>,
) -> Option<bool> {
    use super::app::ActiveOverlay;

    let (state_owned, restore_slot): (
        Box<super::app::MetadataEditorState>,
        super::event_loop::MetadataEditorRestoreSlot,
    ) = if let Some(parked) = app.pending_metadata_editor.take() {
        (parked, super::event_loop::MetadataEditorRestoreSlot::Pending)
    } else if matches!(app.active_overlay, ActiveOverlay::MetadataEditor(_)) {
        let prev = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
        if let ActiveOverlay::MetadataEditor(s) = prev {
            (s, super::event_loop::MetadataEditorRestoreSlot::Active)
        } else {
            unreachable!()
        }
    } else {
        return None;
    };

    if state_owned.model.tags_mb_in_flight {
        super::event_loop::restore_taken_metadata_editor(
            app,
            super::event_loop::TakenMetadataEditor {
                state: state_owned,
                slot: restore_slot,
            },
        );
        app.set_status(
            ":tags-mb: a MusicBrainz lookup is already in flight for this editor".to_string(),
        );
        return Some(true);
    }

    let editor_session = metadata_editor_session_guard(&state_owned);

    // Direct-args path: skip TOC, fire a text search using the
    // user-supplied seed. Editor goes into `active_overlay` so the
    // result handler can populate it in place (same parking-slot
    // invariant as the TOC path).
    if let Some(seed) = direct_seed {
        let paths = metadata_editor_tags_mb_context_paths(&state_owned);
        app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
        let ctx = super::message::TagsMbContext {
            operation_id: super::message::TagsMbOperationId::UNASSIGNED,
            paths,
            editor_park: true,
            fallback_seed: None,
            editor_session: Some(editor_session),
        };
        super::event_loop::spawn_tags_mb_text_search(
            app,
            tx,
            seed,
            ctx,
            super::event_loop::TextSearchMode::DirectRequest,
        );
        return Some(true);
    }

    // Compute sectors + fallback eligibility per editor kind. SACD
    // editors derive geometry from their stashed per-area durations
    // and enable the text-search fallback (TOC misses are common for
    // SACD-only releases). Regular file editors derive geometry from
    // the audio files via the same accuraterip helpers the Browse
    // path uses, and disable the fallback (file-level TOCs are
    // sample-exact; fallback rarely helps).
    let (sectors, fallback_seed, synthesized_toc) = if let Some(area_kind) = state_owned.active_surface().sacd_area_kind {
        let durations = {
            let surface = state_owned.active_surface();
            match area_kind {
                super::sacd::AreaKind::Stereo => surface.sacd_stereo_durations.clone(),
                super::sacd::AreaKind::MultiChannel => surface.sacd_multi_channel_durations.clone(),
            }
        };
        let Some(durations) = durations.filter(|d| !d.is_empty()) else {
            app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
            app.set_status(
                ":tags-mb: no per-track durations for this SACD area — \
                 ISO TRL sectors may be malformed"
                    .to_string(),
            );
            return Some(true);
        };
        let sectors = sacd_durations_to_sectors(&durations);
        let seed = seed_sacd_mb_query(&state_owned);
        (sectors, seed, true)
    } else if super::keybindings::metadata_editor_is_dvda_source(&state_owned) {
        let paths = state_owned.active_surface().paths.clone();
        let Some(first_path) = paths.first().cloned() else {
            app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
            app.set_status(":tags-mb: DVD-Audio editor has no source".to_string());
            return Some(true);
        };
        if !paths.iter().all(|p| p == &first_path) {
            app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
            app.set_status(":tags-mb: DVD-Audio editor paths do not share one source".to_string());
            return Some(true);
        }
        let group_nr = super::keybindings::dvda_group_from_editor_state(&state_owned);
        let sectors = match dvda_source_to_cd_sectors(&first_path, group_nr) {
            Ok(sectors) => sectors,
            Err(e) => {
                app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
                app.set_status(format!(":tags-mb: {}", e));
                return Some(true);
            }
        };
        let seed = seed_sacd_mb_query(&state_owned);
        (sectors, seed, true)
    } else if super::keybindings::metadata_editor_is_bluray_source(&state_owned) {
        let paths = state_owned.active_surface().paths.clone();
        let Some(first_path) = paths.first().cloned() else {
            app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
            app.set_status(":tags-mb: Blu-ray editor has no source".to_string());
            return Some(true);
        };
        if !paths.iter().all(|p| p == &first_path) {
            app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
            app.set_status(":tags-mb: Blu-ray editor paths do not share one source".to_string());
            return Some(true);
        }
        let seed = seed_sacd_mb_query(&state_owned);
        let chapter_durations = state_owned.active_surface().bluray_chapter_durations.clone();
        let sectors = match chapter_durations.as_deref() {
            Some(durations) => match bluray_editor_durations_to_cd_sectors(durations) {
                Ok(sectors) => sectors,
                Err(err) => {
                    let ctx_paths = paths.clone();
                    app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
                    if let Some(seed) = seed {
                        app.set_status(format!(
                            ":tags-mb: Blu-ray synthetic TOC skipped: {}; running MusicBrainz text search from editor tags",
                            err
                        ));
                        let ctx = super::message::TagsMbContext {
                            operation_id: super::message::TagsMbOperationId::UNASSIGNED,
                            paths: ctx_paths,
                            editor_park: true,
                            fallback_seed: None,
                            editor_session: Some(editor_session),
                        };
                        super::event_loop::spawn_tags_mb_text_search(
                            app,
                            tx,
                            seed,
                            ctx,
                            super::event_loop::TextSearchMode::DvdvTocSkippedInvalidDurations,
                        );
                    } else {
                        app.set_status(format!(
                            ":tags-mb: {}; no seeded album/artist/catalog/year tags are available for text-search fallback",
                            err
                        ));
                    }
                    return Some(true);
                }
            },
            None => {
                let ctx_paths = paths.clone();
                app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
                if let Some(seed) = seed {
                    app.set_status(
                        ":tags-mb: Blu-ray synthetic TOC skipped: no chapter durations are available; running MusicBrainz text search from editor tags".to_string(),
                    );
                    let ctx = super::message::TagsMbContext {
                        operation_id: super::message::TagsMbOperationId::UNASSIGNED,
                        paths: ctx_paths,
                        editor_park: true,
                        fallback_seed: None,
                        editor_session: Some(editor_session),
                    };
                    super::event_loop::spawn_tags_mb_text_search(
                        app,
                        tx,
                        seed,
                        ctx,
                        super::event_loop::TextSearchMode::DvdvTocSkippedInvalidDurations,
                    );
                } else {
                    app.set_status(
                        ":tags-mb: Blu-ray editor has no chapter durations for TOC lookup and no seeded album/artist/catalog/year tags for text-search fallback".to_string(),
                    );
                }
                return Some(true);
            }
        };
        (sectors, seed, true)
    } else if super::keybindings::metadata_editor_is_dvdv_source(&state_owned) {
        let paths = state_owned.active_surface().paths.clone();
        let Some(first_path) = paths.first().cloned() else {
            app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
            app.set_status(":tags-mb: DVD-Video editor has no source".to_string());
            return Some(true);
        };
        if !paths.iter().all(|p| p == &first_path) {
            app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
            app.set_status(":tags-mb: DVD-Video editor paths do not share one source".to_string());
            return Some(true);
        }
        let seed = seed_sacd_mb_query(&state_owned);
        let track_durations = state_owned.active_surface().dvdv_track_durations.clone();
        let sectors = match track_durations.as_deref() {
            Some(durations) => match dvdv_editor_durations_to_cd_sectors(durations) {
                Ok(sectors) => sectors,
                Err(err) => {
                    let ctx_paths = paths.clone();
                    app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
                    if let Some(seed) = seed {
                        let ctx = super::message::TagsMbContext {
                            operation_id: super::message::TagsMbOperationId::UNASSIGNED,
                            paths: ctx_paths,
                            editor_park: true,
                            fallback_seed: None,
                            editor_session: Some(editor_session),
                        };
                        super::event_loop::spawn_tags_mb_text_search(
                            app,
                            tx,
                            seed,
                            ctx,
                            super::event_loop::TextSearchMode::DvdvTocSkippedInvalidDurations,
                        );
                    } else {
                        app.set_status(format!(
                            ":tags-mb: {}; add artist/album/catalog/year or reopen the editor after DVD-Video durations are available",
                            err
                        ));
                    }
                    return Some(true);
                }
            },
            None => match dvdv_source_to_cd_sectors(&first_path) {
                Ok(sectors) => sectors,
                Err(e) => {
                    app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
                    app.set_status(format!(":tags-mb: {}", e));
                    return Some(true);
                }
            },
        };
        (sectors, seed, true)
    } else if let Some(request) = in_editor_split_cue_mb_request_from_unified_album(&state_owned) {
        app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
        spawn_in_editor_split_cue_musicbrainz_info_collection(app, tx, request);
        return Some(true);
    } else if let Some(request) = in_editor_split_cue_mb_request_from_metadata_tabs(&state_owned) {
        app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
        spawn_in_editor_split_cue_musicbrainz_info_collection(app, tx, request);
        return Some(true);
    } else if let Some(request) =
        in_editor_split_cue_mb_request_from_single_editor_source_folder(&state_owned)
    {
        app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
        spawn_in_editor_split_cue_musicbrainz_info_collection(app, tx, request);
        return Some(true);
    } else {
        return Some(dispatch_regular_file_editor_tags_mb_toc(app, tx, state_owned));
    };

    if super::musicbrainz::build_mb_toc(&sectors).is_none() {
        app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);
        app.set_status(":tags-mb: TOC too short".to_string());
        return Some(true);
    }
    // Synthesized TOCs (SACD/DVD-A/DVD-V/Blu-ray durations) get the
    // stub-drop cascade; real rip geometry stays a single exact candidate.
    let candidates = if synthesized_toc {
        super::musicbrainz::toc_candidates_from_sectors(&sectors)
    } else {
        vec![super::musicbrainz::TocCandidate::exact(sectors)]
    };
    let paths = state_owned.active_surface().paths.clone();

    // Editor MUST go back into active_overlay (not pending) before
    // the spawn — see the parking-slot invariant in this function's
    // docstring.
    app.active_overlay = ActiveOverlay::MetadataEditor(state_owned);

    spawn_tags_mb_toc_lookup(
        app,
        tx,
        super::message::TagsMbOperationId::UNASSIGNED,
        candidates,
        paths,
        /* editor_park */ true,
        fallback_seed,
        Some(editor_session),
    );

    Some(true)
}

/// Public entry point for metadata editing from context_menu.rs.
pub fn execute_edit_metadata_pub(app: &mut AppState, field: crate::tui::probe::MetadataField) {
    execute_edit_metadata(app, field);
}

/// Execute `:e title` / `:edit artist` etc. — open a TextEdit overlay
/// for the selected audio file's metadata tag on the Browse screen.
fn execute_edit_metadata(app: &mut AppState, field: crate::tui::probe::MetadataField) {
    // Validate: must be on Browse with a selected audio file.
    let entry = match app.browse.selected_entry() {
        Some(e) => e,
        None => {
            app.set_status("edit: no file selected");
            return;
        }
    };
    if !entry.is_audio() {
        app.set_status("edit: selected entry is not an audio file");
        return;
    }
    let path = entry.path.clone();

    // Race check: refuse if the file is currently being converted.
    let is_processing = app.items_snapshot.iter().any(|item| {
        item.input_path == path
            && matches!(
                item.status,
                crate::convert::ConversionStatus::Processing { .. }
            )
    });
    if is_processing {
        app.set_status(format!(
            "cannot edit: {} is currently being converted",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        return;
    }

    // Pre-fill with the current value from probe cache (if available).
    let current_value = app
        .browse
        .entries
        .iter()
        .find(|e| e.path == path)
        .and_then(|e| app.browse.valid_probe_arc_for_entry(e))
        .map(|cached| {
            let m = &cached.metadata;
            match field {
                crate::tui::probe::MetadataField::Title => m.title.clone().unwrap_or_default(),
                crate::tui::probe::MetadataField::Artist => m.artist.clone().unwrap_or_default(),
                crate::tui::probe::MetadataField::Album => m.album.clone().unwrap_or_default(),
                crate::tui::probe::MetadataField::Genre => m.genre.clone().unwrap_or_default(),
                crate::tui::probe::MetadataField::Year => m.year.clone().unwrap_or_default(),
            }
        })
        .unwrap_or_default();

    app.active_overlay = ActiveOverlay::TextEdit {
        input: super::text_input::TextInputState::new(current_value),
        target: TextEditTarget::BrowseMetadata { path, field },
        label: format!("edit {}", field.label()),
    };
}

/// Execute `:expand` / `:x` — open the BatchList expand overlay.
/// Only valid when the Convert source is a multi-file Batch.
fn execute_expand(app: &mut AppState) {
    if app.current_screen != AppScreen::Convert {
        app.set_status(":expand only works on the Convert screen");
        return;
    }
    if !app.convert.source.mode.is_batch() {
        app.set_status(":expand: no batch loaded (use :queue from Browse)");
        return;
    }
    app.active_overlay = ActiveOverlay::BatchList { scroll: 0 };
}

/// Execute a `:rename` command for the browse screen.
/// - With no argument: opens a TextEdit overlay seeded with the current name
///   so the user can edit and press Enter to commit.
/// - With an argument: commits the rename directly.
fn execute_rename(app: &mut AppState, new_name: &str, tx: &mpsc::Sender<AppMessage>) {
    if app.current_screen != AppScreen::Browse {
        app.set_status(":rename only works on the browse screen");
        return;
    }

    // Capture the currently selected entry's path (must be a real entry,
    // not the ".." parent pseudo-entry).
    let (original_path, current_name) = match app.browse.selected_entry() {
        Some(e) if !matches!(e.kind, crate::convert::classify::EntryKind::ParentDir) => {
            (e.path.clone(), e.name.clone())
        }
        Some(_) => {
            app.set_status("rename: cannot rename parent directory (..)");
            return;
        }
        None => {
            app.set_status("rename: no selection");
            return;
        }
    };

    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        // No arg → open the overlay seeded with the current name.
        app.active_overlay = ActiveOverlay::TextEdit {
            input: super::text_input::TextInputState::new(current_name),
            target: TextEditTarget::BrowseRename(original_path),
            label: "rename".to_string(),
        };
    } else {
        // Arg provided → commit directly.
        super::keybindings::commit_browse_rename(app, original_path, trimmed, tx);
    }
}

/// Execute a :set command
fn execute_set(app: &mut AppState, key: &str, value: &str) {
    if key.is_empty() {
        app.set_status("Usage: :set <key> <value>  (format, rate, depth, dither, rg)");
        return;
    }
    if value.is_empty() {
        // Show current value
        match key {
            "f" | "format" => {
                app.set_status(format!(
                    "format = {}",
                    app.convert.format.format.selected_label()
                ));
            }
            "r" | "rate" => {
                app.set_status(format!(
                    "rate = {}",
                    app.convert.format.sample_rate.selected_label()
                ));
            }
            "d" | "depth" => {
                app.set_status(format!(
                    "depth = {}",
                    app.convert.format.bit_depth.selected_label()
                ));
            }
            "dither" => {
                app.set_status(format!(
                    "dither = {}",
                    app.convert.format.dither.selected_label()
                ));
            }
            "rg" | "replaygain" => {
                app.set_status(format!(
                    "replaygain = {}",
                    app.convert.format.replaygain.selected_label()
                ));
            }
            _ => {
                app.set_status(format!("Unknown setting: {}", key));
            }
        }
        return;
    }

    match key {
        "f" | "format" => {
            let fmt = match value.to_lowercase().as_str() {
                "flac" => Some(AudioFormat::Flac),
                "opus" => Some(AudioFormat::Opus),
                "aac" => Some(AudioFormat::Aac),
                "mp3" => Some(AudioFormat::Mp3),
                "alac" => Some(AudioFormat::Alac),
                "wav" => Some(AudioFormat::Wav),
                "wavpack" | "wv" => Some(AudioFormat::WavPack),
                "aiff" => Some(AudioFormat::Aiff),
                _ => None,
            };
            if let Some(f) = fmt {
                // Route through the same transition logic as the keyboard and
                // mouse paths (rate restore, auto dither/resampler, DSD-source
                // cascade) so :set format cannot drift from them.
                let before_format = *app.convert.format.format.selected_value();
                let before_depth = *app.convert.format.bit_depth.selected_value();
                let before_dsd = app.convert.format.is_dsd_selected();
                let source_bits = app.convert.current_source_bit_depth();
                let source_rate = app.convert.current_source_sample_rate();
                app.convert.format.format.select_value(&f);
                app.convert.format.after_user_selection(
                    super::app::FormatField::Format,
                    before_format,
                    before_depth,
                    source_bits,
                    source_rate,
                );
                super::format_interactions::cascade_dsd_source_to_pcm(
                    &mut app.convert,
                    before_dsd,
                );
                app.preset.mark_modified();
                app.set_status(format!("format = {}", f.name()));
            } else {
                app.set_status(format!(
                    "Unknown format: {}. Try: flac, opus, aac, mp3, alac, wav, wavpack, aiff",
                    value
                ));
            }
        }
        "r" | "rate" => {
            let rate: Option<u32> = match value {
                "44.1" | "44100" => Some(44_100),
                "48" | "48000" => Some(48_000),
                "88.2" | "88200" => Some(88_200),
                "96" | "96000" => Some(96_000),
                "176.4" | "176400" => Some(176_400),
                "192" | "192000" => Some(192_000),
                "352.8" | "352800" => Some(352_800),
                "384" | "384000" => Some(384_000),
                "705.6" | "705600" => Some(705_600),
                "768" | "768000" => Some(768_000),
                _ => None,
            };
            if let Some(r) = rate {
                if app.convert.format.sample_rate.select_value(&r) {
                    app.preset.mark_modified();
                    app.set_status(format!("rate = {} kHz", value));
                } else {
                    app.set_status(format!(
                        "rate {} is not available for {}",
                        value,
                        app.convert.format.format.selected_label()
                    ));
                }
            } else {
                app.set_status(format!(
                    "Unknown rate: {}. Try: 44.1, 48, 88.2, 96, 176.4, 192, 352.8, 384, 705.6, 768",
                    value
                ));
            }
        }
        "d" | "depth" => {
            let depth = match value.to_lowercase().as_str() {
                "16" => Some(BitDepthChoice::Int16),
                "24" => Some(BitDepthChoice::Int24),
                "32" => Some(BitDepthChoice::Int32),
                "32f" | "f32" | "float32" => Some(BitDepthChoice::Float32),
                "64f" | "f64" | "float64" => Some(BitDepthChoice::Float64),
                _ => None,
            };
            if let Some(d) = depth {
                if app.convert.format.bit_depth.select_value(&d) {
                    app.preset.mark_modified();
                    app.set_status(format!("depth = {}", value));
                } else {
                    app.set_status(format!(
                        "depth {} is not available for {}",
                        value,
                        app.convert.format.format.selected_label()
                    ));
                }
            } else {
                app.set_status(format!(
                    "Unknown depth: {}. Try: 16, 24, 32, 32f, 64f",
                    value
                ));
            }
        }
        "dither" => {
            let dt = match value.to_lowercase().as_str() {
                "tpdf" => Some(DitherType::TPDF),
                "none" | "off" => Some(DitherType::None),
                "shaped" | "shibata" => Some(DitherType::Shibata),
                _ => None,
            };
            if let Some(d) = dt {
                if app.convert.format.dither.select_value(&d) {
                    app.preset.mark_modified();
                    app.set_status(format!("dither = {}", value));
                } else {
                    app.set_status(format!(
                        "dither {} is not available for {}",
                        value,
                        app.convert.format.format.selected_label()
                    ));
                }
            } else {
                app.set_status(format!(
                    "Unknown dither: {}. Try: tpdf, none, shaped",
                    value
                ));
            }
        }
        "rg" | "replaygain" => {
            let rg = match value.to_lowercase().as_str() {
                "album" => Some(ReplayGainChoice::Album),
                "track" => Some(ReplayGainChoice::Track),
                "both" => Some(ReplayGainChoice::Both),
                "off" | "none" => Some(ReplayGainChoice::Off),
                _ => None,
            };
            if let Some(r) = rg {
                if app.convert.format.replaygain.select_value(&r) {
                    app.preset.mark_modified();
                    app.set_status(format!("replaygain = {}", value));
                } else {
                    app.set_status(format!(
                        "replaygain {} is not available for {}",
                        value,
                        app.convert.format.format.selected_label()
                    ));
                }
            } else {
                app.set_status(format!(
                    "Unknown rg mode: {}. Try: album, track, both, off",
                    value
                ));
            }
        }
        _ => {
            app.set_status(format!(
                "Unknown setting: {}. Try: format, rate, depth, dither, rg",
                key
            ));
        }
    }
}

/// Detect a uniform AR offset from the SQLite cache for a set of track files.
///
/// Returns:
/// - `Some(n)` if every track has a fresh cache entry, all are verified, and
///   they share a single offset value `n` (which may be 0 — meaning AR
///   confirmed offset 0). The caller can use `n` directly.
/// - `None` if at least one track has no cache entry, results are stale, any
///   track is not verified, or offsets disagree across tracks. The caller
///   should run `:ar` to resolve before proceeding.
fn detect_ar_offset_from_cache(db: &crate::db::Database, paths: &[PathBuf]) -> Option<i32> {
    let mut common_offset: Option<i32> = None;

    for path in paths {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta
            .modified()
            .map(crate::db::systemtime_to_unix)
            .unwrap_or(0);
        let size = meta.len();
        let path_str = path.display().to_string();

        let cached = db.get_cached_ar(&path_str, mtime, size)?;

        for t in &cached {
            if t.status != super::accuraterip::ArTrackStatus::Verified {
                return None;
            }
            // `offset = None` means AR did not record an offset for this
            // track — treat as indeterminate and re-run.
            let off = t.offset?;
            match common_offset {
                Some(prev) if prev != off => return None, // mixed offsets
                None => common_offset = Some(off),
                _ => {} // same offset, continue
            }
        }
    }

    common_offset
}

/// Expand ~ to home directory.
fn expand_path(path: &str) -> String {
    if path.starts_with('~') {
        if let Ok(home) = std::env::var("HOME") {
            return path.replacen('~', &home, 1);
        }
    }
    path.to_string()
}

/// Build a list of proposed changes by comparing CUE sheet metadata
/// against the current tags in the metadata editor.
/// CUE fields that can be imported, mapped to their tag display keys.
const CUE_IMPORTABLE_FIELDS: &[&str] = &["TITLE", "ARTIST", "ALBUM", "TRACKNUMBER"];

/// Check if a tag field has corresponding CUE data.
pub fn is_cue_importable(field: &str) -> bool {
    let upper = field.to_ascii_uppercase();
    CUE_IMPORTABLE_FIELDS.iter().any(|&f| f == upper) || upper == "PERFORMER"
}

#[allow(dead_code)] // Legacy CUE diff flow; will be removed in cleanup.
fn build_cue_diff(
    state: &super::app::MetadataEditorState,
    sheet: &super::cue_parser::CueSheet,
    field_filter: Option<&str>,
) -> Vec<super::app::CueImportChange> {
    let mut changes = Vec::new();

    // Map the filter to the CUE field it corresponds to.
    // ARTIST and PERFORMER both map to CUE performer.
    let filter_upper = field_filter.map(|f| f.to_ascii_uppercase());

    for (i, path) in state.active_surface().paths.iter().enumerate() {
        let stem = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let filename = path
            .file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| stem.clone());

        let track = sheet
            .tracks
            .iter()
            .find(|t| t.file.as_deref() == Some(stem.as_str()))
            .or_else(|| sheet.tracks.get(i));

        let mut proposed: Vec<(&str, String)> = Vec::new();

        if let Some(t) = track {
            if let Some(ref title) = t.title {
                proposed.push(("TITLE", title.clone()));
            }
            if let Some(ref performer) = t.performer {
                proposed.push(("ARTIST", performer.clone()));
            }
            if t.number > 0 {
                proposed.push(("TRACKNUMBER", t.number.to_string()));
            }
        }
        if let Some(ref album) = sheet.title {
            proposed.push(("ALBUM", album.clone()));
        }

        for (field, new_value) in proposed {
            // Apply field filter if set.
            if let Some(ref filter) = filter_upper {
                let field_upper = field.to_ascii_uppercase();
                // PERFORMER filter matches ARTIST CUE data.
                let matches =
                    field_upper == *filter || (filter == "PERFORMER" && field_upper == "ARTIST");
                if !matches {
                    continue;
                }
            }

            let old_value = state.active_surface()
                .entries
                .iter()
                .find(|e| e.display_key.eq_ignore_ascii_case(field))
                .map(|e| e.per_file_values.get(i).cloned().unwrap_or_default())
                .unwrap_or_default();

            if old_value != new_value {
                changes.push(super::app::CueImportChange {
                    file_index: i,
                    filename: filename.clone(),
                    field: field.to_string(),
                    old_value,
                    new_value,
                });
            }
        }
    }

    changes
}

/// Apply a set of CUE import changes to a metadata editor state.
pub fn apply_cue_changes(
    state: &mut super::app::MetadataEditorState,
    changes: &[super::app::CueImportChange],
) {
    let n = state.active_surface().paths.len();

    for change in changes {
        // Find or create the entry for this field.
        let idx = match state.active_surface()
            .entries
            .iter()
            .position(|e| e.display_key.eq_ignore_ascii_case(&change.field))
        {
            Some(i) => i,
            None => {
                let item_key = match change.field.to_ascii_uppercase().as_str() {
                    "TITLE" => lofty::tag::ItemKey::TrackTitle,
                    "ARTIST" => lofty::tag::ItemKey::TrackArtist,
                    "ALBUM" => lofty::tag::ItemKey::AlbumTitle,
                    "TRACKNUMBER" => lofty::tag::ItemKey::TrackNumber,
                    _ => lofty::tag::ItemKey::Unknown(change.field.to_ascii_uppercase()),
                };
                state.active_surface_mut().entries.push(super::probe::TagEntry {
                    row_scope: crate::tui::probe::RowScope::File,
                    display_key: change.field.to_ascii_uppercase(),
                    item_key,
                    value: String::new(),
                    original: String::new(),
                    is_binary: false,
                    is_mixed: false,
                    per_file_values: vec![String::new(); n],
                    per_file_originals: vec![String::new(); n],
                    mb_proposed_value: None,
                    mb_proposed_per_file: None,
                });
                state.active_surface().entries.len() - 1
            }
        };

        if change.file_index < n {
            state.active_surface_mut().entries[idx].per_file_values[change.file_index] = change.new_value.clone();
        }
    }

    // Update merged display values and mixed state.
    for entry in &mut state.active_surface_mut().entries {
        let all_same = entry.per_file_values.windows(2).all(|w| w[0] == w[1]);
        entry.is_mixed = !all_same && n > 1;
        entry.value = if entry.is_mixed {
            "<multiple values>".to_string()
        } else {
            entry.per_file_values.first().cloned().unwrap_or_default()
        };
    }

    state.active_surface_mut().dirty = true;
}

#[cfg(test)]
mod completion_tests {
    use super::*;
    use crate::tui::text_input::TextInputState;

    fn test_source_options() -> crate::convert::pipeline::SourceOptions {
        crate::convert::pipeline::SourceOptions {
            archive_password: None,
            sacd_area: None,
            dvda_group: None,
            dvda_group_selection: crate::convert::pipeline::DvdaGroupSelection::Default,
            dvda_assume_decrypted: false,
            dvda_downmix_policy: crate::convert::pipeline::DvdaDownmixPolicy::Auto,
            dvdv_vts: None,
            dvdv_title: None,
            dvdv_audio_stream: None,
            dvdv_angle: None,
            bluray_playlist: None,
            bluray_audio_pid: None,
            bluray_audio_stream: None,
            bluray_angle: None,
            cue_sidecar: crate::convert::pipeline::CueSidecarPolicy::PreferSidecar,
            track_selection: crate::convert::pipeline::TrackSelection::All,
        }
    }

    fn dvda_multitrack_mode(group: u8) -> SourceMode {
        SourceMode::MultiTrack {
            path: std::path::PathBuf::from("/tmp/disc.iso"),
            info: None,
            metadata: crate::tui::probe::SourceMetadata::default(),
            tracks: vec![crate::tui::app::MultiTrackEntry {
                number: 1,
                title: Some("Track 1".to_string()),
                performer: None,
                duration_display: None,
            }],
            area_label: Some(format!("Group {group}")),
            album_title: None,
            album_artist: None,
            probe_notice: None,
            scroll: 0,
            cursor: 0,
            selected: vec![true],
            archive_preview: None,
            disc_contents: None,
            selected_presentation_id: Some(crate::disc::PresentationId::DvdAudioGroup(group)),
        }
    }

    #[test]
    fn multitrack_commit_state_overwrites_prebuilt_default_dvda_group() {
        let mode = dvda_multitrack_mode(3);
        let mut source = test_source_options();

        apply_multitrack_convert_state_to_source_options(&mode, &mut source, None);

        assert_eq!(
            source.effective_dvda_group_selection(),
            crate::convert::pipeline::DvdaGroupSelection::Group(3)
        );
        assert!(matches!(
            source.dvda_downmix_policy,
            crate::convert::pipeline::DvdaDownmixPolicy::Auto
        ));
        assert!(matches!(
            source.track_selection,
            crate::convert::pipeline::TrackSelection::All
        ));
    }

    #[test]
    fn multitrack_commit_state_resets_stale_track_subset_when_all_tracks_selected() {
        let mode = dvda_multitrack_mode(3);
        let mut source = test_source_options();
        source.track_selection = crate::convert::pipeline::TrackSelection::Set(
            std::collections::BTreeSet::from([1]),
        );

        apply_multitrack_convert_state_to_source_options(&mode, &mut source, None);

        assert!(matches!(
            source.track_selection,
            crate::convert::pipeline::TrackSelection::All
        ));
        assert_eq!(
            source.effective_dvda_group_selection(),
            crate::convert::pipeline::DvdaGroupSelection::Group(3)
        );
    }

    #[test]
    fn multitrack_commit_state_preserves_track_subset_and_dvda_group() {
        let mode = dvda_multitrack_mode(3);
        let mut source = test_source_options();
        let selected = std::collections::BTreeSet::from([2, 4]);

        apply_multitrack_convert_state_to_source_options(&mode, &mut source, Some(&selected));

        assert_eq!(
            source.effective_dvda_group_selection(),
            crate::convert::pipeline::DvdaGroupSelection::Group(3)
        );
        match source.track_selection {
            crate::convert::pipeline::TrackSelection::Set(actual) => assert_eq!(actual, selected),
            other => panic!("expected selected track set, got {other:?}"),
        }
    }

    // ── compute_completion: command-name completion ──

    #[test]
    fn command_completion_prefix_q() {
        // Typing "q" should match multiple q-prefixed commands.
        let got = compute_completion("q", 1).expect("should have candidates");
        assert_eq!(got.prefix_start, 0);
        assert!(got.candidates.contains(&"q".to_string()));
        assert!(got.candidates.contains(&"quit".to_string()));
        assert!(got.candidates.contains(&"queue".to_string()));
        assert!(got.candidates.contains(&"queue!".to_string()));
    }

    #[test]
    fn command_completion_prefix_commit_uppercase() {
        // Case-sensitive: "Comm" matches "Commit" but not "commit".
        let got = compute_completion("Comm", 4).expect("should have candidates");
        assert_eq!(got.candidates, vec!["Commit".to_string()]);
    }

    #[test]
    fn command_completion_prefix_com_lowercase() {
        // "com" matches "commit" but not "Commit" (case-sensitive).
        let got = compute_completion("com", 3).expect("should have candidates");
        assert!(got.candidates.contains(&"commit".to_string()));
        assert!(!got.candidates.contains(&"Commit".to_string()));
    }

    #[test]
    fn command_completion_prefix_con_matches_convert_and_context() {
        let got = compute_completion("con", 3).expect("should have candidates");
        assert_eq!(
            got.candidates,
            vec!["convert".to_string(), "context".to_string()]
        );
    }

    #[test]
    fn command_completion_empty_prefix() {
        // Empty input matches all commands.
        let got = compute_completion("", 0).expect("should have candidates");
        assert_eq!(got.candidates.len(), COMMAND_NAMES.len());
    }

    #[test]
    fn command_completion_no_match() {
        // Gibberish matches nothing.
        let got = compute_completion("xyzzy", 5);
        assert!(got.is_none());
    }

    #[test]
    fn command_completion_uppercase_q_no_match() {
        // Case-sensitive: "Q" doesn't match "quit" (lowercase).
        let got = compute_completion("Q", 1);
        assert!(got.is_none());
    }

    // ── compute_completion: preset-arg completion ──

    #[test]
    fn preset_completion_only_for_preset_taking_commands() {
        // "set foo" is NOT a preset-taking command, so no completion
        // after the space.
        let got = compute_completion("set foo", 7);
        assert!(got.is_none());
    }

    #[test]
    fn preset_completion_for_non_preset_command() {
        // "presets foo" (plural, which lists presets) is NOT in
        // PRESET_TAKING_COMMANDS, so no arg completion.
        let got = compute_completion("presets foo", 11);
        assert!(got.is_none());
    }

    // Note: preset completion positive tests can't be unit-tested here
    // without setting up a temp preset directory. The parsing/dispatch
    // logic is covered by the structural tests above.

    // ── apply_completion_to_input ──

    #[test]
    fn apply_replaces_prefix_and_updates_cursor() {
        let mut input = TextInputState::new("q".to_string());
        input.cursor = 1;
        let state = CompletionState {
            candidates: vec!["quit".to_string()],
            cursor: 0,
            prefix_start: 0,
        };
        apply_completion_to_input(&mut input, &state);
        assert_eq!(input.text, "quit");
        assert_eq!(input.cursor, 4);
    }

    #[test]
    fn apply_preserves_text_before_prefix() {
        let mut input = TextInputState::new("queue fo".to_string());
        input.cursor = 8;
        let state = CompletionState {
            candidates: vec!["foobar2k".to_string()],
            cursor: 0,
            prefix_start: 6,
        };
        apply_completion_to_input(&mut input, &state);
        assert_eq!(input.text, "queue foobar2k");
        assert_eq!(input.cursor, 14);
    }

    #[test]
    fn apply_then_cycle_replaces_previous_candidate() {
        // Simulate: type "q" → Tab → first candidate → Tab → second candidate.
        // After first apply, input.cursor is at the end of the first candidate;
        // the second apply should replace the previous candidate entirely.
        let mut input = TextInputState::new("q".to_string());
        input.cursor = 1;
        let mut state = CompletionState {
            candidates: vec!["quit".to_string(), "queue".to_string()],
            cursor: 0,
            prefix_start: 0,
        };
        apply_completion_to_input(&mut input, &state);
        assert_eq!(input.text, "quit");
        assert_eq!(input.cursor, 4);

        // Cycle forward
        cycle_completion(&mut input, &mut state, 1);
        assert_eq!(input.text, "queue");
        assert_eq!(input.cursor, 5);

        // Cycle forward again — wraps back to "quit"
        cycle_completion(&mut input, &mut state, 1);
        assert_eq!(input.text, "quit");
        assert_eq!(input.cursor, 4);
    }

    #[test]
    fn cycle_backward_wraps() {
        let mut input = TextInputState::new("".to_string());
        input.cursor = 0;
        let mut state = CompletionState {
            candidates: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            cursor: 0,
            prefix_start: 0,
        };
        apply_completion_to_input(&mut input, &state);
        assert_eq!(input.text, "a");

        cycle_completion(&mut input, &mut state, -1);
        assert_eq!(input.text, "c");

        cycle_completion(&mut input, &mut state, -1);
        assert_eq!(input.text, "b");

        cycle_completion(&mut input, &mut state, -1);
        assert_eq!(input.text, "a");
    }
}

#[cfg(test)]
mod dvdv_sidecar_idempotency_tests {
    use super::*;
    use crate::tui::app::MetadataEditorState;
    use crate::tui::probe::TagEntry;
    use lofty::tag::ItemKey;

    fn entry(key: &str, value: &str, per_file_values: Vec<&str>) -> TagEntry {
        TagEntry {
            row_scope: crate::tui::probe::RowScope::File,
            display_key: key.to_string(),
            item_key: ItemKey::Unknown(key.to_string()),
            value: value.to_string(),
            original: String::new(),
            is_binary: false,
            is_mixed: false,
            per_file_values: per_file_values.into_iter().map(str::to_string).collect(),
            per_file_originals: Vec::new(),
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        }
    }

    fn state(entries: Vec<TagEntry>) -> MetadataEditorState {
        MetadataEditorState::for_files(
            vec![PathBuf::from("/tmp/track1.wav"), PathBuf::from("/tmp/track2.wav")],
            entries,
            vec!["01".to_string(), "02".to_string()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        )
    }


    #[test]
    fn dvdv_sidecar_struct_roundtrip_preserves_presentation_and_extensions() {
        let sidecar = DvdVideoMetadataSidecar {
            schema_version: 2,
            source: DvdVideoMetadataSource {
                path: PathBuf::from("/tmp/concert.iso"),
                sidecar_kind: "dvd_video".to_string(),
                presentation: Some(DvdVideoPresentationIdentity {
                    vts_number: 3,
                    title_number: 7,
                    audio_stream_index: 1,
                    angle_number: Some(2),
                    track_count: Some(1),
                    duration_fingerprint: Some("dvdv-ms-v1:1:abc".to_string()),
                }),
                extra: BTreeMap::from([(
                    "source_vendor_extension".to_string(),
                    serde_json::json!({"v": 1}),
                )]),
            },
            album: BTreeMap::from([
                ("ALBUM".to_string(), "Concert Film".to_string()),
                ("MUSICBRAINZ_ALBUMID".to_string(), "release-id".to_string()),
            ]),
            tracks: vec![DvdVideoMetadataTrack {
                number: 1,
                label: "VTS 3 Title 7 Chapter 1".to_string(),
                source_title: Some(3),
                source_chapter: Some(1),
                tags: BTreeMap::from([
                    ("TITLE".to_string(), "Opening".to_string()),
                    ("MUSICBRAINZ_TRACKID".to_string(), "recording-id".to_string()),
                ]),
                extra: BTreeMap::from([(
                    "track_vendor_extension".to_string(),
                    serde_json::json!(["keep"]),
                )]),
            }],
            extra: BTreeMap::from([(
                "top_vendor_extension".to_string(),
                serde_json::json!(true),
            )]),
        };

        let encoded = serde_json::to_string_pretty(&sidecar).expect("serialize DVD-Video sidecar struct");
        let parsed: DvdVideoMetadataSidecar =
            serde_json::from_str(&encoded).expect("parse DVD-Video sidecar struct");

        assert_eq!(parsed.schema_version, 2);
        assert_eq!(
            parsed.source.presentation,
            Some(DvdVideoPresentationIdentity {
                vts_number: 3,
                title_number: 7,
                audio_stream_index: 1,
                angle_number: Some(2),
                track_count: Some(1),
                duration_fingerprint: Some("dvdv-ms-v1:1:abc".to_string()),
            })
        );
        assert_eq!(parsed.album.get("MUSICBRAINZ_ALBUMID").map(String::as_str), Some("release-id"));
        assert_eq!(parsed.tracks[0].tags.get("MUSICBRAINZ_TRACKID").map(String::as_str), Some("recording-id"));
        assert_eq!(
            parsed.source.extra.get("source_vendor_extension"),
            Some(&serde_json::json!({"v": 1}))
        );
        assert_eq!(
            parsed.tracks[0].extra.get("track_vendor_extension"),
            Some(&serde_json::json!(["keep"]))
        );
        assert_eq!(parsed.extra.get("top_vendor_extension"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn dvdv_sidecar_save_preserves_unknown_existing_data() {
        let existing = DvdVideoMetadataSidecar {
            schema_version: 2,
            source: DvdVideoMetadataSource {
                path: PathBuf::from("/tmp/concert.iso"),
                sidecar_kind: "dvd_video".to_string(),
                presentation: None,
                extra: BTreeMap::from([(
                    "source_vendor_extension".to_string(),
                    serde_json::json!("keep-source"),
                )]),
            },
            album: BTreeMap::from([
                ("ALBUM".to_string(), "Old Album".to_string()),
                ("OBSCURE_ALBUM_KEY".to_string(), "keep-album".to_string()),
            ]),
            tracks: vec![
                DvdVideoMetadataTrack {
                    number: 1,
                    label: "old 01".to_string(),
                    source_title: Some(1),
                    source_chapter: Some(1),
                    tags: BTreeMap::from([
                        ("TITLE".to_string(), "Old Title".to_string()),
                        ("MUSICBRAINZ_TRACKID".to_string(), "recording-1".to_string()),
                        ("OBSCURE_TRACK_KEY".to_string(), "keep-track".to_string()),
                    ]),
                    extra: BTreeMap::from([(
                        "track_vendor_extension".to_string(),
                        serde_json::json!({"keep": true}),
                    )]),
                },
            ],
            extra: BTreeMap::from([(
                "top_vendor_extension".to_string(),
                serde_json::json!(42),
            )]),
        };
        let state = state(vec![
            entry("ALBUM", "New Album", vec!["New Album", "New Album"]),
            entry("TITLE", "", vec!["New Title", ""]),
        ]);

        let sidecar = dvdv_metadata_sidecar_from_state_preserving(
            Path::new("/tmp/concert.iso"),
            &state,
            Some(&existing),
        )
        .expect("sidecar should save");

        assert_eq!(sidecar.album.get("ALBUM").map(String::as_str), Some("New Album"));
        assert_eq!(
            sidecar.album.get("OBSCURE_ALBUM_KEY").map(String::as_str),
            Some("keep-album")
        );
        assert_eq!(
            sidecar.tracks[0].tags.get("TITLE").map(String::as_str),
            Some("New Title")
        );
        assert_eq!(
            sidecar.tracks[0].tags.get("MUSICBRAINZ_TRACKID").map(String::as_str),
            Some("recording-1")
        );
        assert_eq!(
            sidecar.tracks[0].tags.get("OBSCURE_TRACK_KEY").map(String::as_str),
            Some("keep-track")
        );
        assert_eq!(
            sidecar.source.extra.get("source_vendor_extension"),
            Some(&serde_json::json!("keep-source"))
        );
        assert_eq!(
            sidecar.tracks[0].extra.get("track_vendor_extension"),
            Some(&serde_json::json!({"keep": true}))
        );
        assert_eq!(sidecar.extra.get("top_vendor_extension"), Some(&serde_json::json!(42)));
        assert!(!sidecar.tracks[1].tags.contains_key("TITLE"));
    }

    #[test]
    fn dvdv_sidecar_save_load_save_is_semantically_idempotent() {
        let existing = DvdVideoMetadataSidecar {
            schema_version: 3,
            source: DvdVideoMetadataSource {
                path: PathBuf::from("/tmp/concert.iso"),
                sidecar_kind: "dvd_video".to_string(),
                presentation: None,
                extra: BTreeMap::from([("source_ext".to_string(), serde_json::json!({"keep": true}))]),
            },
            album: BTreeMap::from([
                ("ALBUM".to_string(), "Old Album".to_string()),
                ("UNKNOWN_ALBUM".to_string(), "keep".to_string()),
            ]),
            tracks: vec![DvdVideoMetadataTrack {
                number: 1,
                label: "01".to_string(),
                source_title: None,
                source_chapter: None,
                tags: BTreeMap::from([
                    ("TITLE".to_string(), "Old Title".to_string()),
                    ("UNKNOWN_TRACK".to_string(), "keep".to_string()),
                ]),
                extra: BTreeMap::from([("track_ext".to_string(), serde_json::json!(7))]),
            }],
            extra: BTreeMap::from([("top_ext".to_string(), serde_json::json!("keep"))]),
        };
        let state = state(vec![
            entry("ALBUM", "New Album", vec!["New Album", "New Album"]),
            entry("TITLE", "", vec!["New Title", ""]),
        ]);

        let once = dvdv_metadata_sidecar_from_state_preserving(
            Path::new("/tmp/concert.iso"),
            &state,
            Some(&existing),
        )
        .expect("first save");
        let twice = dvdv_metadata_sidecar_from_state_preserving(
            Path::new("/tmp/concert.iso"),
            &state,
            Some(&once),
        )
        .expect("second save");

        assert_eq!(
            serde_json::to_value(&once).expect("first JSON"),
            serde_json::to_value(&twice).expect("second JSON")
        );
    }

    #[test]
    fn dvdv_toml_unknown_inline_album_and_track_keys_do_not_copy_to_extra() {
        let path = std::env::temp_dir().join(format!(
            "tonepoet-dvdv-unknown-inline-{}.toml",
            std::process::id()
        ));
        let original = r#"schema_version = 1
format = "tonepoet-dvdvideo-metadata"

[[presentations]]
id = "vts1-title1-stream0"

[presentations.source]
vts = 1
title = 1
audio_stream = 0
track_count = 1

[presentations.album]
album = "Old Album"
custom_user_field = "keep-album"

[[presentations.tracks]]
number = 1
title = "Old Title"
custom_track_field = "keep-track"
"#;
        std::fs::write(&path, original).expect("write TOML fixture");

        let parsed = parse_dvdv_metadata_sidecar(&path).expect("parse TOML sidecar");
        assert_eq!(parsed.album.get("ALBUM").map(String::as_str), Some("Old Album"));
        assert!(!parsed.album.contains_key("CUSTOM_USER_FIELD"));
        assert_eq!(parsed.tracks[0].tags.get("TITLE").map(String::as_str), Some("Old Title"));
        assert!(!parsed.tracks[0].tags.contains_key("CUSTOM_TRACK_FIELD"));

        let rewritten = dvdv_sidecar_to_toml_string(&path, &parsed).expect("rewrite TOML sidecar");
        assert!(rewritten.contains("custom_user_field = \"keep-album\""));
        assert!(rewritten.contains("custom_track_field = \"keep-track\""));
        assert!(!rewritten.contains("CUSTOM_USER_FIELD"));
        assert!(!rewritten.contains("CUSTOM_TRACK_FIELD"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dvdv_sidecar_save_uses_state_source_chapters_before_display_label() {
        let mut state = state(vec![entry("TITLE", "", vec!["Song One", "Song Two"])]);
        state.active_surface_mut().file_labels = vec![
            "01 Title 7 Chapter 1".to_string(),
            "02 Title 7 Chapter 2".to_string(),
        ];
        state.active_surface_mut().dvdv_source_chapters = Some(vec![1, 2]);
        let mut tab = crate::tui::app::PresentationTab::from_editor_state(
            crate::disc::model::PresentationId::DvdVideoTitle {
                vts_number: 3,
                title_number: 7,
                audio_stream_index: 1,
            },
            "Title 7",
            &state,
        );
        tab.dirty = true;
        state.set_presentation_surfaces(vec![tab], 0);

        let sidecar = dvdv_metadata_sidecar_from_state_preserving(
            Path::new("/tmp/concert.iso"),
            &state,
            None,
        )
        .expect("save sidecar");

        assert_eq!(sidecar.tracks[0].source_title, Some(7));
        assert_eq!(sidecar.tracks[0].source_chapter, Some(1));
        assert_eq!(sidecar.tracks[1].source_title, Some(7));
        assert_eq!(sidecar.tracks[1].source_chapter, Some(2));
    }

    #[test]
    fn dvdv_track_source_label_parser_ignores_leading_output_ordinal() {
        assert_eq!(dvdv_track_source_from_label("01 Title 7 Chapter 1"), Some((7, 1)));
        assert_eq!(dvdv_track_source_from_label("02 Title 7 Chapter 2 (03:12)"), Some((7, 2)));
        assert_eq!(dvdv_track_source_from_label("01 07 01"), None);
    }

    #[test]
    fn dvdv_editor_preload_empty_single_presentation_state_matches_identity_sidecar_by_shape() {
        let dir = std::env::temp_dir().join(format!(
            "tonepoet-dvdv-preload-empty-tabs-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("make temp dir");
        let source = dir.join("concert.iso");
        std::fs::write(&source, b"dvdv fixture").expect("write source fixture");
        let sidecar_path = dvdv_metadata_sidecar_path_for_source(&source).expect("sidecar path");
        let durations = vec![1.0, 2.0];
        let fingerprint = dvdv_track_duration_fingerprint_from_secs(&durations);
        let sidecar = DvdVideoMetadataSidecar {
            schema_version: DVDV_METADATA_SIDECAR_SCHEMA_VERSION,
            source: DvdVideoMetadataSource {
                path: source.clone(),
                sidecar_kind: "dvd_video".to_string(),
                presentation: Some(DvdVideoPresentationIdentity {
                    vts_number: 1,
                    title_number: 1,
                    audio_stream_index: 0,
                    angle_number: None,
                    track_count: Some(2),
                    duration_fingerprint: Some(fingerprint),
                }),
                extra: BTreeMap::new(),
            },
            album: BTreeMap::from([
                ("ALBUM".to_string(), "Concert Film".to_string()),
                ("MOOD".to_string(), "Electric".to_string()),
            ]),
            tracks: vec![
                DvdVideoMetadataTrack {
                    number: 1,
                    label: "Opening".to_string(),
                    source_title: Some(1),
                    source_chapter: Some(1),
                    tags: BTreeMap::from([
                        ("TITLE".to_string(), "Opening".to_string()),
                        ("WORK".to_string(), "Concert Film".to_string()),
                    ]),
                    extra: BTreeMap::new(),
                },
                DvdVideoMetadataTrack {
                    number: 2,
                    label: "Finale".to_string(),
                    source_title: Some(1),
                    source_chapter: Some(2),
                    tags: BTreeMap::from([
                        ("TITLE".to_string(), "Finale".to_string()),
                        ("WORK".to_string(), "Concert Film".to_string()),
                    ]),
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        };
        let toml = dvdv_sidecar_to_toml_string(&sidecar_path, &sidecar).expect("serialize sidecar");
        std::fs::write(&sidecar_path, toml).expect("write sidecar");

        let mut state = state(vec![
            entry("ALBUM", "", vec!["", ""]),
            entry("TITLE", "", vec!["", ""]),
        ]);
        state.presentation_tabs = Vec::new();
        state.active_surface_mut().dvdv_track_durations = Some(durations);
        state.active_surface_mut().dvdv_source_chapters = Some(vec![1, 2]);

        assert!(preload_dvdv_metadata_editor_state_from_sidecar(&source, &mut state)
            .expect("preload sidecar"));

        let album = state.active_surface().entries.iter().find(|entry| entry.display_key == "ALBUM").unwrap();
        assert_eq!(album.value, "Concert Film");
        assert_eq!(album.per_file_values, vec!["Concert Film".to_string(), "Concert Film".to_string()]);
        let mood = state.active_surface().entries.iter().find(|entry| entry.display_key == "MOOD").unwrap();
        assert_eq!(mood.value, "Electric");
        assert_eq!(mood.per_file_values, vec!["Electric".to_string(), "Electric".to_string()]);
        let work = state.active_surface().entries.iter().find(|entry| entry.display_key == "WORK").unwrap();
        assert_eq!(work.value, "Concert Film");
        assert_eq!(work.per_file_values, vec!["Concert Film".to_string(), "Concert Film".to_string()]);
        let title = state.active_surface().entries.iter().find(|entry| entry.display_key == "TITLE").unwrap();
        assert_eq!(title.value, "<multiple values>");
        assert_eq!(title.per_file_values, vec!["Opening".to_string(), "Finale".to_string()]);
        assert!(!state.active_surface().dirty);
        assert!(state.active_surface().deleted.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dvdv_editor_preload_empty_state_rejects_ambiguous_identity_sidecars() {
        let mut sidecars = vec![
            DvdVideoMetadataSidecar {
                schema_version: DVDV_METADATA_SIDECAR_SCHEMA_VERSION,
                source: DvdVideoMetadataSource {
                    path: PathBuf::from("/tmp/concert.iso"),
                    sidecar_kind: "dvd_video".to_string(),
                    presentation: Some(DvdVideoPresentationIdentity {
                        vts_number: 1,
                        title_number: 1,
                        audio_stream_index: 0,
                        angle_number: None,
                        track_count: Some(2),
                        duration_fingerprint: None,
                    }),
                    extra: BTreeMap::new(),
                },
                album: BTreeMap::new(),
                tracks: Vec::new(),
                extra: BTreeMap::new(),
            },
            DvdVideoMetadataSidecar {
                schema_version: DVDV_METADATA_SIDECAR_SCHEMA_VERSION,
                source: DvdVideoMetadataSource {
                    path: PathBuf::from("/tmp/concert.iso"),
                    sidecar_kind: "dvd_video".to_string(),
                    presentation: Some(DvdVideoPresentationIdentity {
                        vts_number: 2,
                        title_number: 1,
                        audio_stream_index: 0,
                        angle_number: None,
                        track_count: Some(2),
                        duration_fingerprint: None,
                    }),
                    extra: BTreeMap::new(),
                },
                album: BTreeMap::new(),
                tracks: Vec::new(),
                extra: BTreeMap::new(),
            },
        ];
        let shape = DvdvEditorPresentationShape {
            track_count: 2,
            duration_fingerprint: None,
            angle_number: None,
        };
        assert!(dvdv_matching_sidecar_for_editor(&sidecars, None, &shape).is_none());

        sidecars[1].source.presentation.as_mut().unwrap().track_count = Some(3);
        assert_eq!(
            dvdv_matching_sidecar_for_editor(&sidecars, None, &shape)
                .and_then(|sidecar| sidecar.source.presentation.as_ref())
                .map(|identity| identity.vts_number),
            Some(1)
        );
    }

    #[test]
    fn dvdv_editor_preload_rejects_duplicate_full_identity_matches() {
        let identity = DvdVideoPresentationIdentity {
            vts_number: 1,
            title_number: 1,
            audio_stream_index: 0,
            angle_number: None,
            track_count: Some(2),
            duration_fingerprint: None,
        };
        let sidecars = vec![
            DvdVideoMetadataSidecar {
                schema_version: DVDV_METADATA_SIDECAR_SCHEMA_VERSION,
                source: DvdVideoMetadataSource {
                    path: PathBuf::from("/tmp/concert.iso"),
                    sidecar_kind: "dvd_video".to_string(),
                    presentation: Some(identity.clone()),
                    extra: BTreeMap::new(),
                },
                album: BTreeMap::from([("ALBUM".to_string(), "First".to_string())]),
                tracks: Vec::new(),
                extra: BTreeMap::new(),
            },
            DvdVideoMetadataSidecar {
                schema_version: DVDV_METADATA_SIDECAR_SCHEMA_VERSION,
                source: DvdVideoMetadataSource {
                    path: PathBuf::from("/tmp/concert.iso"),
                    sidecar_kind: "dvd_video".to_string(),
                    presentation: Some(identity.clone()),
                    extra: BTreeMap::new(),
                },
                album: BTreeMap::from([("ALBUM".to_string(), "Second".to_string())]),
                tracks: Vec::new(),
                extra: BTreeMap::new(),
            },
        ];
        let shape = DvdvEditorPresentationShape {
            track_count: 2,
            duration_fingerprint: None,
            angle_number: None,
        };

        assert!(dvdv_matching_sidecar_for_editor(&sidecars, Some(&identity), &shape).is_none());
    }

    #[test]
    fn dvdv_editor_preload_accepts_selected_presentation_id_for_empty_tabs() {
        let dir = std::env::temp_dir().join(format!(
            "tonepoet-dvdv-preload-selected-id-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("make temp dir");
        let source = dir.join("concert.iso");
        std::fs::write(&source, b"dvdv fixture").expect("write source fixture");
        let sidecar_path = dvdv_metadata_sidecar_path_for_source(&source).expect("sidecar path");
        let sidecar = DvdVideoMetadataSidecar {
            schema_version: DVDV_METADATA_SIDECAR_SCHEMA_VERSION,
            source: DvdVideoMetadataSource {
                path: source.clone(),
                sidecar_kind: "dvd_video".to_string(),
                presentation: Some(DvdVideoPresentationIdentity {
                    vts_number: 7,
                    title_number: 3,
                    audio_stream_index: 1,
                    angle_number: None,
                    track_count: Some(2),
                    duration_fingerprint: None,
                }),
                extra: BTreeMap::new(),
            },
            album: BTreeMap::from([("ALBUM".to_string(), "Selected Presentation".to_string())]),
            tracks: vec![
                DvdVideoMetadataTrack {
                    number: 1,
                    label: "First".to_string(),
                    source_title: Some(3),
                    source_chapter: Some(1),
                    tags: BTreeMap::from([("TITLE".to_string(), "First".to_string())]),
                    extra: BTreeMap::new(),
                },
                DvdVideoMetadataTrack {
                    number: 2,
                    label: "Second".to_string(),
                    source_title: Some(3),
                    source_chapter: Some(2),
                    tags: BTreeMap::from([("TITLE".to_string(), "Second".to_string())]),
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        };
        let toml = dvdv_sidecar_to_toml_string(&sidecar_path, &sidecar).expect("serialize sidecar");
        std::fs::write(&sidecar_path, toml).expect("write sidecar");

        let mut state = state(vec![
            entry("ALBUM", "", vec!["", ""]),
            entry("TITLE", "", vec!["", ""]),
        ]);
        state.presentation_tabs = Vec::new();
        state.active_surface_mut().dvdv_source_chapters = Some(vec![1, 2]);

        assert!(preload_dvdv_metadata_editor_state_from_sidecar_with_presentation_id(
            &source,
            &mut state,
            Some(crate::disc::model::PresentationId::DvdVideoTitle {
                vts_number: 7,
                title_number: 3,
                audio_stream_index: 1,
            }),
        )
        .expect("preload sidecar"));

        let album = state.active_surface().entries.iter().find(|entry| entry.display_key == "ALBUM").unwrap();
        assert_eq!(album.value, "Selected Presentation");
        let title = state.active_surface().entries.iter().find(|entry| entry.display_key == "TITLE").unwrap();
        assert_eq!(title.per_file_values, vec!["First".to_string(), "Second".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dvdv_sparse_angle_identity_writes_minimal_single_angle_toml() {
        let mut state = state(vec![entry("ALBUM", "Single Angle", vec!["Single Angle", "Single Angle"])]);
        state.active_surface_mut().dvdv_title_angle_count = Some(1);
        let mut tab = crate::tui::app::PresentationTab::from_editor_state(
            crate::disc::model::PresentationId::DvdVideoTitle {
                vts_number: 1,
                title_number: 1,
                audio_stream_index: 0,
            },
            "Title 1",
            &state,
        );
        tab.dirty = true;
        state.set_presentation_surfaces(vec![tab], 0);

        let sidecar = dvdv_metadata_sidecar_from_state_preserving(
            Path::new("/tmp/concert.iso"),
            &state,
            None,
        ).expect("single-angle sidecar");
        assert_eq!(sidecar.source.presentation.as_ref().unwrap().angle_number, None);
        assert_eq!(dvdv_presentation_id(sidecar.source.presentation.as_ref().unwrap()), "vts1-title1-stream0");
        let toml = dvdv_sidecar_to_toml_string(Path::new("/tmp/nonexistent-dvdv-single-angle.toml"), &sidecar)
            .expect("single-angle TOML");
        assert!(toml.contains("id = \"vts1-title1-stream0\""));
        assert!(!toml.contains("angle = 1"));
        assert!(!toml.contains("-angle1"));
    }

    #[test]
    fn dvdv_sparse_angle_identity_writes_multi_angle_toml() {
        let mut state = state(vec![entry("ALBUM", "Angle 2", vec!["Angle 2", "Angle 2"])]);
        state.active_surface_mut().dvdv_angle_number = Some(2);
        state.active_surface_mut().dvdv_title_angle_count = Some(2);
        let mut tab = crate::tui::app::PresentationTab::from_editor_state(
            crate::disc::model::PresentationId::DvdVideoTitle {
                vts_number: 1,
                title_number: 1,
                audio_stream_index: 0,
            },
            "Title 1 Angle 2",
            &state,
        );
        tab.dirty = true;
        state.set_presentation_surfaces(vec![tab], 0);

        let sidecar = dvdv_metadata_sidecar_from_state_preserving(
            Path::new("/tmp/concert.iso"),
            &state,
            None,
        ).expect("multi-angle sidecar");
        assert_eq!(sidecar.source.presentation.as_ref().unwrap().angle_number, Some(2));
        assert_eq!(dvdv_presentation_id(sidecar.source.presentation.as_ref().unwrap()), "vts1-title1-stream0-angle2");
        let toml = dvdv_sidecar_to_toml_string(Path::new("/tmp/nonexistent-dvdv-angle2.toml"), &sidecar)
            .expect("multi-angle TOML");
        assert!(toml.contains("id = \"vts1-title1-stream0-angle2\""));
        assert!(toml.contains("angle = 2"));
    }

    #[test]
    fn dvdv_sparse_angle_identity_matching_rules_are_safe() {
        let single = DvdVideoPresentationIdentity {
            vts_number: 1,
            title_number: 1,
            audio_stream_index: 0,
            angle_number: None,
            track_count: Some(2),
            duration_fingerprint: None,
        };
        let angle1 = DvdVideoPresentationIdentity { angle_number: Some(1), ..single.clone() };
        let angle2 = DvdVideoPresentationIdentity { angle_number: Some(2), ..single.clone() };
        assert!(dvdv_presentation_identity_compatible(Some(&single), Some(&single)));
        assert!(dvdv_presentation_identity_compatible(Some(&angle1), Some(&angle1)));
        assert!(dvdv_presentation_identity_compatible(Some(&angle2), Some(&angle2)));
        assert!(!dvdv_presentation_identity_compatible(Some(&single), Some(&angle1)));
        assert!(!dvdv_presentation_identity_compatible(Some(&angle1), Some(&angle2)));
        assert!(dvdv_presentation_identity_compatible(Some(&angle1), Some(&single)));
        assert!(!dvdv_presentation_identity_compatible(Some(&angle2), Some(&single)));
    }

    #[test]
    fn dvdv_multi_angle_save_updates_one_angle_without_altering_sibling() {
        let path = std::env::temp_dir().join(format!(
            "tonepoet-dvdv-angle-sparse-{}.toml",
            std::process::id()
        ));
        let initial = r#"schema_version = 1
format = "tonepoet-dvdvideo-metadata"

[[presentations]]
id = "vts1-title1-stream0-angle1"
[presentations.source]
vts = 1
title = 1
audio_stream = 0
angle = 1
track_count = 1
[presentations.album]
album = "Angle 1"
[[presentations.tracks]]
number = 1
title = "Angle 1 Song"

[[presentations]]
id = "vts1-title1-stream0-angle2"
[presentations.source]
vts = 1
title = 1
audio_stream = 0
angle = 2
track_count = 1
[presentations.album]
album = "Old Angle 2"
[[presentations.tracks]]
number = 1
title = "Old Angle 2 Song"
"#;
        std::fs::write(&path, initial).expect("write angle fixture");
        let sidecar = DvdVideoMetadataSidecar {
            schema_version: DVDV_METADATA_SIDECAR_SCHEMA_VERSION,
            source: DvdVideoMetadataSource {
                path: path.clone(),
                sidecar_kind: "dvd_video".to_string(),
                presentation: Some(DvdVideoPresentationIdentity {
                    vts_number: 1,
                    title_number: 1,
                    audio_stream_index: 0,
                    angle_number: Some(2),
                    track_count: Some(1),
                    duration_fingerprint: None,
                }),
                extra: BTreeMap::new(),
            },
            album: BTreeMap::from([("ALBUM".to_string(), "New Angle 2".to_string())]),
            tracks: vec![DvdVideoMetadataTrack {
                number: 1,
                label: "01 Title 1 Chapter 1".to_string(),
                source_title: Some(1),
                source_chapter: Some(1),
                tags: BTreeMap::from([("TITLE".to_string(), "New Angle 2 Song".to_string())]),
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        };
        let rewritten = dvdv_sidecar_to_toml_string(&path, &sidecar).expect("rewrite angle 2");
        assert!(rewritten.contains("album = \"Angle 1\""));
        assert!(rewritten.contains("title = \"Angle 1 Song\""));
        assert!(rewritten.contains("album = \"New Angle 2\""));
        assert!(rewritten.contains("title = \"New Angle 2 Song\""));
        assert!(!rewritten.contains("Old Angle 2 Song"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dvdv_duration_fingerprint_is_stable_and_duration_sensitive() {
        let first = dvdv_track_duration_fingerprint_from_secs(&[240.0, 210.25]);
        let same = dvdv_track_duration_fingerprint_from_secs(&[240.0, 210.25]);
        let different = dvdv_track_duration_fingerprint_from_secs(&[240.0, 210.26]);

        assert_eq!(first, same);
        assert_ne!(first, different);
        assert!(first.starts_with("dvdv-ms-v1:2:"));
    }

    #[test]
    fn dvdv_toml_writer_preserves_unrelated_presentations_without_leaking() {
        let path = std::env::temp_dir().join(format!(
            "tonepoet-dvdv-cross-presentation-{}.toml",
            std::process::id()
        ));
        let original = r#"schema_version = 1
format = "tonepoet-dvdvideo-metadata"

[[presentations]]
id = "vts1-title1-stream0"
keep_presentation_a_comment = "belongs-to-presentation-a"

[presentations.source]
vts = 1
title = 1
audio_stream = 0
track_count = 1
source_note = "belongs-to-presentation-a"

[presentations.album]
album = "Presentation A"
custom_user_field = "keep-a-only"

[presentations.album.extra]
STALE_ALBUM_EXTRA = "keep-a-only"

[[presentations.tracks]]
number = 1
source_title = 1
source_chapter = 1
title = "Presentation A Track"
custom_track_field = "keep-a-only"

[presentations.tracks.extra]
STALE_TRACK_EXTRA = "keep-a-only"
"#;
        std::fs::write(&path, original).expect("write TOML fixture");

        let sidecar = DvdVideoMetadataSidecar {
            schema_version: DVDV_METADATA_SIDECAR_SCHEMA_VERSION,
            source: DvdVideoMetadataSource {
                path: path.clone(),
                sidecar_kind: "dvd_video".to_string(),
                presentation: Some(DvdVideoPresentationIdentity {
                    vts_number: 2,
                    title_number: 4,
                    audio_stream_index: 1,
                    angle_number: None,
                    track_count: Some(1),
                    duration_fingerprint: Some("dvdv-ms-v1:1:0000000000000001".to_string()),
                }),
                extra: BTreeMap::new(),
            },
            album: BTreeMap::from([("ALBUM".to_string(), "Presentation B".to_string())]),
            tracks: vec![DvdVideoMetadataTrack {
                number: 1,
                label: "01 Title 4 Chapter 1".to_string(),
                source_title: Some(4),
                source_chapter: Some(1),
                tags: BTreeMap::from([("TITLE".to_string(), "Presentation B Track".to_string())]),
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        };

        let rewritten = dvdv_sidecar_to_toml_string(&path, &sidecar).expect("rewrite TOML sidecar");
        assert!(rewritten.contains("id = \"vts1-title1-stream0\""));
        assert!(rewritten.contains("id = \"vts2-title4-stream1\""));
        assert!(rewritten.contains("album = \"Presentation A\""));
        assert!(rewritten.contains("custom_user_field = \"keep-a-only\""));
        assert!(rewritten.contains("album = \"Presentation B\""));
        assert!(rewritten.contains("title = \"Presentation B Track\""));
        let b_section = rewritten.split("id = \"vts2-title4-stream1\"").nth(1).expect("presentation B section");
        assert!(!b_section.contains("STALE_ALBUM_EXTRA"));
        assert!(!b_section.contains("custom_track_field = \"keep-a-only\""));
        assert!(!b_section.contains("STALE_TRACK_EXTRA"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dvdv_sidecar_save_does_not_merge_alternate_presentation_data() {
        let existing = DvdVideoMetadataSidecar {
            schema_version: 2,
            source: DvdVideoMetadataSource {
                path: PathBuf::from("/tmp/concert.iso"),
                sidecar_kind: "dvd_video".to_string(),
                presentation: Some(DvdVideoPresentationIdentity {
                    vts_number: 1,
                    title_number: 1,
                    audio_stream_index: 0,
                    angle_number: None,
                    track_count: None,
                    duration_fingerprint: None,
                }),
                extra: BTreeMap::from([(
                    "source_vendor_extension".to_string(),
                    serde_json::json!("wrong-presentation"),
                )]),
            },
            album: BTreeMap::from([(
                "OBSCURE_ALBUM_KEY".to_string(),
                "do-not-merge".to_string(),
            )]),
            tracks: vec![DvdVideoMetadataTrack {
                number: 1,
                label: "01".to_string(),
                source_title: None,
                source_chapter: None,
                tags: BTreeMap::from([(
                    "OBSCURE_TRACK_KEY".to_string(),
                    "do-not-merge".to_string(),
                )]),
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::from([(
                "top_vendor_extension".to_string(),
                serde_json::json!("wrong-presentation"),
            )]),
        };
        let state = state(vec![entry("ALBUM", "Selected Program", vec!["Selected Program", "Selected Program"])]);
        assert!(!dvdv_existing_sidecar_can_merge(&existing, None));

        let sidecar = dvdv_metadata_sidecar_from_state_preserving(
            Path::new("/tmp/concert.iso"),
            &state,
            Some(&existing),
        )
        .expect("sidecar should save");

        assert_eq!(sidecar.source.presentation, None);
        assert!(!sidecar.album.contains_key("OBSCURE_ALBUM_KEY"));
        assert!(!sidecar.tracks[0].tags.contains_key("OBSCURE_TRACK_KEY"));
        assert!(!sidecar.source.extra.contains_key("source_vendor_extension"));
        assert!(!sidecar.extra.contains_key("top_vendor_extension"));
    }


    #[test]
    fn dvdv_multi_presentation_toml_parses_two_presentations() {
        let path = std::env::temp_dir().join(format!(
            "tonepoet-dvdv-multi-parse-{}.toml",
            std::process::id()
        ));
        let fixture = r#"schema_version = 1
format = "tonepoet-dvdvideo-metadata"

[[presentations]]
id = "vts1-title1-stream0"
[presentations.source]
vts = 1
title = 1
audio_stream = 0
track_count = 1
[presentations.album]
album = "Main"
[[presentations.tracks]]
number = 1
source_title = 1
source_chapter = 1
title = "Main Song"

[[presentations]]
id = "vts2-title1-stream0"
[presentations.source]
vts = 2
title = 1
audio_stream = 0
track_count = 1
[presentations.album]
album = "Bonus"
[[presentations.tracks]]
number = 1
source_title = 1
source_chapter = 1
title = "Bonus Song"
"#;
        std::fs::write(&path, fixture).expect("write multi-presentation TOML fixture");

        let sidecars = parse_dvdv_metadata_sidecar_presentations(&path).expect("parse presentations");

        assert_eq!(sidecars.len(), 2);
        assert_eq!(sidecars[0].album.get("ALBUM").map(String::as_str), Some("Main"));
        assert_eq!(sidecars[1].album.get("ALBUM").map(String::as_str), Some("Bonus"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dvdv_multi_presentation_save_updates_one_entry_without_duplication() {
        let path = std::env::temp_dir().join(format!(
            "tonepoet-dvdv-multi-update-{}.toml",
            std::process::id()
        ));
        let initial = r#"schema_version = 1
format = "tonepoet-dvdvideo-metadata"

[[presentations]]
id = "vts1-title1-stream0"
keep_comment = "presentation-a"
[presentations.source]
vts = 1
title = 1
audio_stream = 0
track_count = 1
[presentations.album]
album = "Old Main"
custom_user_field = "keep-a"
[[presentations.tracks]]
number = 1
source_title = 1
source_chapter = 1
title = "Old Main Song"

[[presentations]]
id = "vts2-title1-stream0"
[presentations.source]
vts = 2
title = 1
audio_stream = 0
track_count = 1
[presentations.album]
album = "Bonus"
[[presentations.tracks]]
number = 1
source_title = 1
source_chapter = 1
title = "Bonus Song"
"#;
        std::fs::write(&path, initial).expect("write multi-presentation TOML fixture");
        let sidecar = DvdVideoMetadataSidecar {
            schema_version: DVDV_METADATA_SIDECAR_SCHEMA_VERSION,
            source: DvdVideoMetadataSource {
                path: path.clone(),
                sidecar_kind: "dvd_video".to_string(),
                presentation: Some(DvdVideoPresentationIdentity {
                    vts_number: 1,
                    title_number: 1,
                    audio_stream_index: 0,
                    angle_number: None,
                    track_count: Some(1),
                    duration_fingerprint: None,
                }),
                extra: BTreeMap::new(),
            },
            album: BTreeMap::from([
                ("ALBUM".to_string(), "New Main".to_string()),
                ("custom_album_key".to_string(), "keep-main".to_string()),
            ]),
            tracks: vec![DvdVideoMetadataTrack {
                number: 1,
                label: "01".to_string(),
                source_title: Some(1),
                source_chapter: Some(1),
                tags: BTreeMap::from([("TITLE".to_string(), "New Main Song".to_string())]),
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        };

        let rewritten_once = dvdv_sidecar_to_toml_string(&path, &sidecar).expect("rewrite once");
        std::fs::write(&path, &rewritten_once).expect("write rewritten TOML");
        let rewritten_twice = dvdv_sidecar_to_toml_string(&path, &sidecar).expect("rewrite twice");

        assert_eq!(rewritten_twice.matches("[[presentations]]").count(), 2);
        assert!(rewritten_twice.contains("album = \"New Main\""));
        assert!(rewritten_twice.contains("album = \"Bonus\""));
        assert!(rewritten_twice.contains("custom_user_field = \"keep-a\""));
        assert!(!rewritten_twice.contains("Old Main Song"));
        let _ = std::fs::remove_file(&path);
    }

}

#[cfg(test)]
mod bluray_sidecar_tests {
    use super::*;
    use crate::tui::app::{MetadataEditorState, PresentationTab};
    use crate::tui::probe::TagEntry;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "tonepoet-bluray-sidecar-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    fn minimal_sidecar_text(album: &str) -> String {
        format!(
            r#"schema_version = 1
format = "tonepoet-bluray-metadata"

[[presentations]]
[presentations.source]
path = "/fixtures/Concert.iso"
sidecar_kind = "tonepoet-bluray-metadata"
[presentations.source.presentation]
playlist_number = 12
audio_pid = 4352
audio_stream_index = 0
angle_number = 1
track_count = 2
duration_fingerprint = "fp-a"

[presentations.album]
ALBUM = "{}"
ALBUMARTIST = "Artist"
GENRE = "Classical"
DATE = "1972"

[[presentations.tracks]]
number = 1
label = "Opening"
source_chapter = 1
[presentations.tracks.tags]
TITLE = "Opening"
ARTIST = "Soloist"
"#,
            album
        )
    }

    fn identity(fingerprint: Option<&str>) -> BluRayPresentationIdentity {
        BluRayPresentationIdentity {
            playlist_number: 12,
            audio_pid: 0x1100,
            audio_stream_index: 0,
            angle_number: Some(1),
            track_count: Some(2),
            duration_fingerprint: fingerprint.map(str::to_string),
            extra: BTreeMap::new(),
        }
    }

    fn sidecar_with_fingerprint(fingerprint: Option<&str>) -> BluRayMetadataSidecar {
        BluRayMetadataSidecar {
            schema_version: BLURAY_METADATA_SIDECAR_SCHEMA_VERSION,
            source: BluRayMetadataSource {
                path: PathBuf::from("/fixtures/Concert.iso"),
                sidecar_kind: BLURAY_METADATA_FORMAT.to_string(),
                presentation: Some(identity(fingerprint)),
                extra: BTreeMap::new(),
            },
            album: BTreeMap::from([("ALBUM".to_string(), "Album".to_string())]),
            tracks: vec![BluRayMetadataTrack {
                number: 1,
                label: "Opening".to_string(),
                source_chapter: Some(1),
                tags: BTreeMap::from([("TITLE".to_string(), "Opening".to_string())]),
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        }
    }

    fn sidecar_for_playlist(playlist_number: u32, album: &str) -> BluRayMetadataSidecar {
        let mut sidecar = sidecar_with_fingerprint(None);
        sidecar.source.presentation.as_mut().unwrap().playlist_number = playlist_number;
        sidecar.album.insert("ALBUM".to_string(), album.to_string());
        sidecar
    }

    fn sidecar_for_stream(audio_stream_index: u8, album: &str) -> BluRayMetadataSidecar {
        let mut sidecar = sidecar_with_fingerprint(None);
        let identity = sidecar.source.presentation.as_mut().unwrap();
        identity.audio_pid = 0x1100 + u16::from(audio_stream_index);
        identity.audio_stream_index = audio_stream_index;
        identity.duration_fingerprint = None;
        sidecar.album.insert("ALBUM".to_string(), album.to_string());
        sidecar.album.insert("MOOD".to_string(), format!("{album} Mood"));
        sidecar.tracks = vec![
            BluRayMetadataTrack {
                number: 1,
                label: format!("{album} Chapter 1"),
                source_chapter: Some(1),
                tags: BTreeMap::from([
                    ("TITLE".to_string(), format!("{album} One")),
                    ("TAKE_NOTE".to_string(), format!("{album} Note One")),
                ]),
                extra: BTreeMap::new(),
            },
            BluRayMetadataTrack {
                number: 2,
                label: format!("{album} Chapter 2"),
                source_chapter: Some(2),
                tags: BTreeMap::from([
                    ("TITLE".to_string(), format!("{album} Two")),
                    ("TAKE_NOTE".to_string(), format!("{album} Note Two")),
                ]),
                extra: BTreeMap::new(),
            },
        ];
        sidecar
    }

    fn entry_value<'a>(entries: &'a [TagEntry], key: &str) -> Option<&'a str> {
        entries
            .iter()
            .find(|entry| entry.display_key == key)
            .map(|entry| entry.value.as_str())
    }

    fn bluray_save_tab(
        source: &Path,
        playlist_number: u32,
        audio_stream_index: u8,
        album: &str,
        title_prefix: &str,
        dirty: bool,
    ) -> PresentationTab {
        let paths = vec![source.to_path_buf(), source.to_path_buf()];
        let title_one = format!("{title_prefix} 1");
        let title_two = format!("{title_prefix} 2");
        let entries = vec![
            bluray_tag_entry("ALBUM", album, vec![album, album]),
            bluray_tag_entry(
                "TITLE",
                "<multiple values>",
                vec![title_one.as_str(), title_two.as_str()],
            ),
        ];
        let mut tab = PresentationTab::new(
            crate::disc::model::PresentationId::try_blu_ray_title(
                playlist_number,
                0x1100 + u16::from(audio_stream_index),
                audio_stream_index,
                1,
            )
            .expect("valid Blu-ray presentation id"),
            format!("Playlist {playlist_number}"),
            paths,
            entries,
            vec!["Chapter 1".to_string(), "Chapter 2".to_string()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        );
        tab.dirty = dirty;
        tab.bluray_playlist_number = Some(playlist_number);
        tab.bluray_audio_pid = Some(0x1100 + u16::from(audio_stream_index));
        tab.bluray_audio_stream_index = Some(audio_stream_index);
        tab.bluray_angle_number = Some(1);
        tab.bluray_chapter_durations = Some(vec![90.0, 91.0]);
        tab
    }


    fn bluray_state_with_tabs(tabs: Vec<PresentationTab>) -> MetadataEditorState {
        MetadataEditorState::for_disc_presentations(tabs, 0)
    }


    #[test]
    fn bluray_toml_parser_reads_identity_tags_and_extension_fields() {
        let text = r#"schema_version = 1
format = "tonepoet-bluray-metadata"

[[presentations]]
keep_top = { vendor = "yes" }
[presentations.source]
path = "/fixtures/Concert.iso"
sidecar_kind = "tonepoet-bluray-metadata"
source_extension = ["keep"]
[presentations.source.presentation]
playlist_number = 12
audio_pid = 4352
audio_stream_index = 0
angle_number = 1
track_count = 2
duration_fingerprint = "fp-a"
presentation_extension = "keep-presentation"
[presentations.album]
ALBUM = "Sidecar Album"
ALBUMARTIST = "Sidecar Artist"
GENRE = "Classical"
DATE = "1972"
[presentations.album.extra]
MUSICBRAINZ_RELEASEGROUPID = "rgid"
[[presentations.tracks]]
number = 1
label = "Opening"
source_chapter = 1
track_extension = 9
[presentations.tracks.tags]
TITLE = "Opening"
ARTIST = "Soloist"
[presentations.tracks.tags.extra]
MUSICBRAINZ_TRACKID = "recording-id"
"#;

        let parsed = parse_bluray_metadata_sidecar_presentations(text, Path::new("/tmp/Concert.bluray.metadata.toml"))
            .expect("parse Blu-ray sidecar");

        assert_eq!(parsed.len(), 1);
        let sidecar = &parsed[0];
        let mut expected_identity = identity(Some("fp-a"));
        expected_identity.extra.insert(
            "presentation_extension".to_string(),
            serde_json::json!("keep-presentation"),
        );
        assert_eq!(sidecar.source.presentation, Some(expected_identity));
        assert_eq!(sidecar.album.get("ALBUM").map(String::as_str), Some("Sidecar Album"));
        assert_eq!(sidecar.album.get("MUSICBRAINZ_RELEASEGROUPID").map(String::as_str), Some("rgid"));
        assert_eq!(sidecar.tracks[0].source_chapter, Some(1));
        assert_eq!(sidecar.tracks[0].tags.get("TITLE").map(String::as_str), Some("Opening"));
        assert_eq!(sidecar.tracks[0].tags.get("MUSICBRAINZ_TRACKID").map(String::as_str), Some("recording-id"));
        assert_eq!(sidecar.extra.get("keep_top"), Some(&serde_json::json!({"vendor": "yes"})));
        assert_eq!(sidecar.source.extra.get("source_extension"), Some(&serde_json::json!(["keep"])));
        assert_eq!(
            sidecar.source.presentation.as_ref().and_then(|identity| identity.extra.get("presentation_extension")),
            Some(&serde_json::json!("keep-presentation"))
        );
        assert_eq!(sidecar.tracks[0].extra.get("track_extension"), Some(&serde_json::json!(9)));
    }

    #[test]
    fn bluray_sidecar_discovery_finds_iso_root_and_bdmv_paths() {
        let root = unique_dir("discovery");
        std::fs::create_dir_all(root.join("BDMV")).expect("create BDMV");
        let root_sidecar = root.join(BLURAY_METADATA_SIDECAR_NAME);
        std::fs::write(&root_sidecar, minimal_sidecar_text("Directory Album"))
            .expect("write directory sidecar");

        let (found_root, parsed_root) = load_bluray_metadata_sidecar_presentations(&root)
            .expect("load root sidecar")
            .expect("root sidecar present");
        assert_eq!(found_root, root_sidecar);
        assert_eq!(parsed_root[0].album.get("ALBUM").map(String::as_str), Some("Directory Album"));

        let (found_bdmv, _) = load_bluray_metadata_sidecar_presentations(&root.join("BDMV"))
            .expect("load BDMV sidecar")
            .expect("BDMV sidecar present");
        assert_eq!(found_bdmv, root.join(BLURAY_METADATA_SIDECAR_NAME));

        let iso = root.join("Concert.iso");
        let iso_sidecar = root.join("Concert.bluray.metadata.toml");
        std::fs::write(&iso_sidecar, minimal_sidecar_text("ISO Album"))
            .expect("write ISO sidecar");
        let (found_iso, parsed_iso) = load_bluray_metadata_sidecar_presentations(&iso)
            .expect("load ISO sidecar")
            .expect("ISO sidecar present");
        assert_eq!(found_iso, iso_sidecar);
        assert_eq!(parsed_iso[0].album.get("ALBUM").map(String::as_str), Some("ISO Album"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bluray_shared_matcher_reports_ambiguity_and_fingerprint_problems() {
        let current = identity(Some("fp-current"));
        let stale = vec![sidecar_with_fingerprint(Some("fp-stale"))];
        let stale_report = find_unique_matching_bluray_metadata_sidecar(&stale, &current, true);
        assert!(stale_report.selected.is_none());
        assert!(matches!(
            stale_report.warnings.as_slice(),
            [BluRaySidecarMatchWarning::DurationFingerprintMismatch { .. }]
        ));

        let current_without_fingerprint = identity(None);
        let unavailable_report = find_unique_matching_bluray_metadata_sidecar(
            &stale,
            &current_without_fingerprint,
            true,
        );
        assert!(unavailable_report.selected.is_some());
        assert!(matches!(
            unavailable_report.warnings.as_slice(),
            [BluRaySidecarMatchWarning::DurationFingerprintUnavailable { .. }]
        ));

        let first = sidecar_with_fingerprint(None);
        let mut second = sidecar_with_fingerprint(None);
        second.album.insert("ALBUM".to_string(), "Duplicate".to_string());
        let duplicates = vec![first, second];
        let ambiguous_report = find_unique_matching_bluray_metadata_sidecar(&duplicates, &identity(None), true);
        assert!(ambiguous_report.selected.is_none());
        assert!(matches!(
            ambiguous_report.warnings.as_slice(),
            [BluRaySidecarMatchWarning::Ambiguous { .. }]
        ));

        let mut anonymous = sidecar_with_fingerprint(None);
        anonymous.source.presentation = None;
        let anonymous_sidecars = [anonymous];
        let anonymous_report = find_unique_matching_bluray_metadata_sidecar(&anonymous_sidecars, &identity(None), true);
        assert!(anonymous_report.selected.is_none());
        assert!(matches!(
            anonymous_report.warnings.as_slice(),
            [BluRaySidecarMatchWarning::MissingPresentationIdentity { .. }]
        ));
    }

    #[test]
    fn bluray_angle_identity_is_strict_and_visible_in_match_report() {
        let current = identity(None);
        let mut missing_angle = sidecar_with_fingerprint(None);
        missing_angle.source.presentation.as_mut().unwrap().angle_number = None;

        let missing_angle_sidecars = [missing_angle];
        let report = find_unique_matching_bluray_metadata_sidecar(&missing_angle_sidecars, &current, true);
        assert!(report.selected.is_none());
        assert!(matches!(
            report.warnings.as_slice(),
            [BluRaySidecarMatchWarning::AngleNumberMismatch { .. }]
        ));
    }

    #[test]
    fn bluray_preload_report_surfaces_degraded_and_skipped_matches() {
        let mut state = bluray_editor_state_for_custom_fields(Vec::new());
        state.active_surface_mut().bluray_chapter_durations = None;
        let mut stale_but_stable = sidecar_with_fingerprint(Some("fp-from-earlier-map"));
        stale_but_stable.album.insert("ALBUM".to_string(), "Recovered".to_string());
        let mut missing_angle = sidecar_with_fingerprint(None);
        missing_angle.source.presentation.as_mut().unwrap().angle_number = None;
        missing_angle.album.insert("ALBUM".to_string(), "Wrong Angle".to_string());

        let report = preload_bluray_metadata_editor_state_from_sidecars_with_report(
            &mut state,
            &[stale_but_stable, missing_angle],
        );

        assert_eq!(report.applied_presentations, 1);
        assert_eq!(entry_value(&state.active_surface().entries, "ALBUM"), Some("Recovered"));
        assert!(report
            .warnings
            .iter()
            .any(|warning| matches!(warning, BluRaySidecarMatchWarning::DurationFingerprintUnavailable { .. })));
        assert!(report
            .warnings
            .iter()
            .any(|warning| matches!(warning, BluRaySidecarMatchWarning::AngleNumberMismatch { .. })));
        let note = bluray_sidecar_preload_status_note(&report).expect("visible status note");
        assert!(note.contains("matched without duration fingerprint"));
        assert!(note.contains("skipped"));
    }

    #[test]
    fn bluray_duration_fingerprints_are_only_built_from_reliable_positive_durations() {
        assert!(bluray_reliable_chapter_duration_fingerprint_from_secs(&[]).is_none());
        assert!(bluray_reliable_chapter_duration_fingerprint_from_secs(&[90.0, 0.0]).is_none());
        assert!(bluray_reliable_chapter_duration_fingerprint_from_secs(&[90.0, f64::NAN]).is_none());
        assert!(bluray_reliable_chapter_duration_fingerprint_from_secs(&[90.0, 91.25]).is_some());
    }

    fn bluray_tag_entry(key: &str, value: &str, per_file_values: Vec<&str>) -> TagEntry {
        TagEntry {
            row_scope: crate::tui::probe::RowScope::File,
            display_key: key.to_string(),
            item_key: lofty::tag::ItemKey::Unknown(key.to_string()),
            value: value.to_string(),
            original: value.to_string(),
            is_binary: false,
            is_mixed: per_file_values.windows(2).any(|w| w[0] != w[1]),
            per_file_originals: per_file_values.iter().map(|value| (*value).to_string()).collect(),
            per_file_values: per_file_values.iter().map(|value| (*value).to_string()).collect(),
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        }
    }

    fn bluray_editor_state_for_custom_fields(entries: Vec<TagEntry>) -> MetadataEditorState {
        let mut state = MetadataEditorState::for_files(
            vec![
                PathBuf::from("/fixtures/Concert.iso"),
                PathBuf::from("/fixtures/Concert.iso"),
            ],
            entries,
            vec!["Chapter 1".to_string(), "Chapter 2".to_string()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        );
        state.active_surface_mut().dirty = true;
        state.active_surface_mut().bluray_playlist_number = Some(12);
        state.active_surface_mut().bluray_audio_pid = Some(0x1100);
        state.active_surface_mut().bluray_audio_stream_index = Some(0);
        state.active_surface_mut().bluray_angle_number = Some(1);
        state
    }


    fn sidecar_with_custom_fields() -> BluRayMetadataSidecar {
        let mut sidecar = sidecar_with_fingerprint(Some("fp-a"));
        sidecar.source.presentation.as_mut().unwrap().track_count = Some(2);
        sidecar.album.insert("MOOD".to_string(), "Old Mood".to_string());
        sidecar.tracks = vec![
            BluRayMetadataTrack {
                number: 1,
                label: "Chapter 1".to_string(),
                source_chapter: Some(1),
                tags: BTreeMap::from([
                    ("TITLE".to_string(), "Opening".to_string()),
                    ("TAKE_NOTE".to_string(), "Old One".to_string()),
                ]),
                extra: BTreeMap::new(),
            },
            BluRayMetadataTrack {
                number: 2,
                label: "Chapter 2".to_string(),
                source_chapter: Some(2),
                tags: BTreeMap::from([
                    ("TITLE".to_string(), "Finale".to_string()),
                    ("TAKE_NOTE".to_string(), "Old Two".to_string()),
                ]),
                extra: BTreeMap::new(),
            },
        ];
        sidecar
    }

    #[test]
    fn bluray_preload_from_parsed_sidecars_applies_all_tabs_and_syncs_active_tab() {
        let source = Path::new("/fixtures/Concert.iso");
        let mut state = bluray_state_with_tabs(vec![
            bluray_save_tab(source, 12, 0, "Mapped Stream 1", "Mapped One", false),
            bluray_save_tab(source, 12, 1, "Mapped Stream 2", "Mapped Two", false),
        ]);
        assert!(state.switch_presentation_tab(1));

        let sidecars = vec![
            sidecar_for_stream(0, "Saved Stream 1"),
            sidecar_for_stream(1, "Saved Stream 2"),
        ];
        assert!(preload_bluray_metadata_editor_state_from_sidecars(
            &mut state,
            &sidecars,
        ));

        assert_eq!(state.presentation_tabs.len(), 2);
        assert_eq!(state.presentation_tabs[0].label, "Saved Stream 1");
        assert_eq!(state.presentation_tabs[1].label, "Saved Stream 2");
        assert_eq!(entry_value(&state.presentation_tabs[0].entries, "ALBUM"), Some("Saved Stream 1"));
        assert_eq!(entry_value(&state.presentation_tabs[1].entries, "ALBUM"), Some("Saved Stream 2"));
        assert_eq!(entry_value(&state.presentation_tabs[0].entries, "MOOD"), Some("Saved Stream 1 Mood"));
        assert_eq!(entry_value(&state.presentation_tabs[1].entries, "MOOD"), Some("Saved Stream 2 Mood"));
        assert_eq!(state.active_surface().file_labels, vec!["Saved Stream 2 Chapter 1", "Saved Stream 2 Chapter 2"]);
        assert_eq!(entry_value(&state.active_surface().entries, "ALBUM"), Some("Saved Stream 2"));
        assert!(!state.active_surface().dirty);
        assert!(state.presentation_tabs.iter().all(|tab| !tab.dirty));
    }

    #[test]
    fn bluray_preload_single_active_state_uses_same_sidecar_application_path() {
        let source = Path::new("/fixtures/Concert.iso");
        let mut state = bluray_state_with_tabs(vec![bluray_save_tab(
            source,
            12,
            0,
            "Mapped Stream",
            "Mapped",
            false,
        )]);
        let active = state.presentation_tabs.remove(0);
        state.active_surface_mut().paths = active.paths;
        state.active_surface_mut().entries = active.entries;
        state.active_surface_mut().file_labels = active.file_labels;
        state.active_surface_mut().deleted = active.deleted;
        state.active_surface_mut().dirty = true;
        state.active_surface_mut().bluray_playlist_number = active.bluray_playlist_number;
        state.active_surface_mut().bluray_audio_pid = active.bluray_audio_pid;
        state.active_surface_mut().bluray_audio_stream_index = active.bluray_audio_stream_index;
        state.active_surface_mut().bluray_angle_number = active.bluray_angle_number;
        state.active_surface_mut().bluray_chapter_durations = active.bluray_chapter_durations;

        let sidecars = vec![sidecar_for_stream(0, "Saved Active")];
        assert!(preload_bluray_metadata_editor_state_from_sidecars(
            &mut state,
            &sidecars,
        ));

        assert!(state.presentation_tabs.is_empty());
        assert_eq!(entry_value(&state.active_surface().entries, "ALBUM"), Some("Saved Active"));
        assert_eq!(entry_value(&state.active_surface().entries, "MOOD"), Some("Saved Active Mood"));
        assert_eq!(state.active_surface().file_labels, vec!["Saved Active Chapter 1", "Saved Active Chapter 2"]);
        assert!(!state.active_surface().dirty);
    }

    #[test]
    fn bluray_save_then_reopen_preloads_album_track_and_custom_metadata() {
        let root = unique_dir("save-reopen-preload");
        std::fs::create_dir_all(&root).expect("create sidecar dir");
        let source = root.as_path();
        let mut state = bluray_state_with_tabs(vec![bluray_save_tab(
            source,
            12,
            0,
            "Saved Concert",
            "Saved Track",
            true,
        )]);
        state.active_surface_mut().entries.push(bluray_tag_entry(
            "MOOD",
            "After Hours",
            vec!["After Hours", "After Hours"],
        ));
        state.active_surface_mut().entries.push(bluray_tag_entry(
            "TAKE_NOTE",
            "<multiple values>",
            vec!["First Take", "Second Take"],
        ));

        let outcome = save_bluray_metadata_sidecar_dirty_presentations_from_state(source, &state)
            .expect("save dirty Blu-ray state");
        assert_eq!(outcome.saved_presentations, 1);

        let mut reopened = bluray_state_with_tabs(vec![bluray_save_tab(
            source,
            12,
            0,
            "Mapped Concert",
            "Mapped Track",
            false,
        )]);
        assert!(preload_bluray_metadata_editor_state_from_sidecar(source, &mut reopened)
            .expect("preload saved sidecar"));

        assert_eq!(entry_value(&reopened.active_surface().entries, "ALBUM"), Some("Saved Concert"));
        assert_eq!(entry_value(&reopened.active_surface().entries, "MOOD"), Some("After Hours"));
        let take_note = reopened
            .active_surface()
            .entries
            .iter()
            .find(|entry| entry.display_key == "TAKE_NOTE")
            .expect("custom track field should reload");
        assert_eq!(take_note.per_file_values, vec!["First Take", "Second Take"]);
        let title = reopened
            .active_surface()
            .entries
            .iter()
            .find(|entry| entry.display_key == "TITLE")
            .expect("title should reload");
        assert_eq!(title.per_file_values, vec!["Saved Track 1", "Saved Track 2"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bluray_custom_album_and_track_fields_can_be_edited_saved_and_reloaded() {
        let existing = sidecar_with_custom_fields();
        let mut entries = Vec::new();
        bluray_apply_sidecar_to_editor_fields(&mut entries, 2, &existing);

        let mood = entries
            .iter_mut()
            .find(|entry| entry.display_key == "MOOD")
            .expect("custom album field should preload");
        mood.value = "Edited Mood".to_string();
        mood.per_file_values = vec!["Edited Mood".to_string(), "Edited Mood".to_string()];
        mood.is_mixed = false;

        let take_note = entries
            .iter_mut()
            .find(|entry| entry.display_key == "TAKE_NOTE")
            .expect("custom track field should preload");
        take_note.value = "<multiple values>".to_string();
        take_note.per_file_values = vec!["Edited One".to_string(), "Edited Two".to_string()];
        take_note.is_mixed = true;

        let state = bluray_editor_state_for_custom_fields(entries);
        let saved = bluray_metadata_sidecar_from_state_preserving(
            Path::new("/fixtures/Concert.iso"),
            &state,
            Some(&existing),
        )
        .expect("save Blu-ray editor state");

        assert_eq!(saved.album.get("MOOD").map(String::as_str), Some("Edited Mood"));
        assert_eq!(
            saved.tracks[0].tags.get("TAKE_NOTE").map(String::as_str),
            Some("Edited One")
        );
        assert_eq!(
            saved.tracks[1].tags.get("TAKE_NOTE").map(String::as_str),
            Some("Edited Two")
        );

        let mut reloaded = Vec::new();
        bluray_apply_sidecar_to_editor_fields(&mut reloaded, 2, &saved);
        assert_eq!(
            reloaded
                .iter()
                .find(|entry| entry.display_key == "MOOD")
                .map(|entry| entry.value.as_str()),
            Some("Edited Mood")
        );
        assert_eq!(
            reloaded
                .iter()
                .find(|entry| entry.display_key == "TAKE_NOTE")
                .map(|entry| entry.per_file_values.clone()),
            Some(vec!["Edited One".to_string(), "Edited Two".to_string()])
        );
    }

    #[test]
    fn bluray_same_artist_across_all_tracks_stays_track_scoped() {
        let state = bluray_editor_state_for_custom_fields(vec![bluray_tag_entry(
            "ARTIST",
            "Same Artist",
            vec!["Same Artist", "Same Artist"],
        )]);

        let saved = bluray_metadata_sidecar_from_state_preserving(
            Path::new("/fixtures/Concert.iso"),
            &state,
            None,
        )
        .expect("save Blu-ray editor state");

        assert!(!saved.album.contains_key("ARTIST"));
        assert_eq!(
            saved.tracks[0].tags.get("ARTIST").map(String::as_str),
            Some("Same Artist")
        );
        assert_eq!(
            saved.tracks[1].tags.get("ARTIST").map(String::as_str),
            Some("Same Artist")
        );
    }

    #[test]
    fn bluray_single_track_title_stays_track_scoped() {
        let mut state = bluray_editor_state_for_custom_fields(vec![bluray_tag_entry(
            "TITLE",
            "Only Title",
            vec!["Only Title"],
        )]);
        state.active_surface_mut().paths.truncate(1);
        state.active_surface_mut().file_labels.truncate(1);
        state.active_surface_mut().bluray_chapter_durations = Some(vec![180.0]);

        let saved = bluray_metadata_sidecar_from_state_preserving(
            Path::new("/fixtures/Concert.iso"),
            &state,
            None,
        )
        .expect("save Blu-ray editor state");

        assert!(!saved.album.contains_key("TITLE"));
        assert_eq!(saved.tracks.len(), 1);
        assert_eq!(
            saved.tracks[0].tags.get("TITLE").map(String::as_str),
            Some("Only Title")
        );
    }

    #[test]
    fn bluray_same_performer_and_musicbrainz_trackid_stay_track_scoped() {
        let state = bluray_editor_state_for_custom_fields(vec![
            bluray_tag_entry(
                "PERFORMER",
                "Same Performer",
                vec!["Same Performer", "Same Performer"],
            ),
            bluray_tag_entry(
                "MUSICBRAINZ_TRACKID",
                "same-recording-id",
                vec!["same-recording-id", "same-recording-id"],
            ),
        ]);

        let saved = bluray_metadata_sidecar_from_state_preserving(
            Path::new("/fixtures/Concert.iso"),
            &state,
            None,
        )
        .expect("save Blu-ray editor state");

        assert!(!saved.album.contains_key("PERFORMER"));
        assert!(!saved.album.contains_key("MUSICBRAINZ_TRACKID"));
        for track in &saved.tracks {
            assert_eq!(
                track.tags.get("PERFORMER").map(String::as_str),
                Some("Same Performer")
            );
            assert_eq!(
                track.tags.get("MUSICBRAINZ_TRACKID").map(String::as_str),
                Some("same-recording-id")
            );
        }
    }

    #[test]
    fn bluray_unknown_custom_fields_still_use_flexible_scope_inference() {
        let uniform = bluray_editor_state_for_custom_fields(vec![bluray_tag_entry(
            "CUSTOM_SCOPE_NOTE",
            "Albumish",
            vec!["Albumish", "Albumish"],
        )]);
        let saved_uniform = bluray_metadata_sidecar_from_state_preserving(
            Path::new("/fixtures/Concert.iso"),
            &uniform,
            None,
        )
        .expect("save uniform custom field");
        assert_eq!(
            saved_uniform.album.get("CUSTOM_SCOPE_NOTE").map(String::as_str),
            Some("Albumish")
        );
        assert!(saved_uniform
            .tracks
            .iter()
            .all(|track| !track.tags.contains_key("CUSTOM_SCOPE_NOTE")));

        let track_specific = bluray_editor_state_for_custom_fields(vec![bluray_tag_entry(
            "CUSTOM_SCOPE_NOTE",
            "<multiple values>",
            vec!["Left", "Right"],
        )]);
        let saved_track_specific = bluray_metadata_sidecar_from_state_preserving(
            Path::new("/fixtures/Concert.iso"),
            &track_specific,
            None,
        )
        .expect("save track-specific custom field");
        assert!(!saved_track_specific.album.contains_key("CUSTOM_SCOPE_NOTE"));
        assert_eq!(
            saved_track_specific.tracks[0]
                .tags
                .get("CUSTOM_SCOPE_NOTE")
                .map(String::as_str),
            Some("Left")
        );
        assert_eq!(
            saved_track_specific.tracks[1]
                .tags
                .get("CUSTOM_SCOPE_NOTE")
                .map(String::as_str),
            Some("Right")
        );
    }

    #[test]
    fn bluray_custom_field_can_move_from_album_to_track_scope() {
        let mut existing = sidecar_with_custom_fields();
        existing.album.insert("LOCATION_NOTE".to_string(), "Same Room".to_string());
        let entries = vec![bluray_tag_entry(
            "LOCATION_NOTE",
            "<multiple values>",
            vec!["Front Hall", "Back Hall"],
        )];
        let state = bluray_editor_state_for_custom_fields(entries);

        let saved = bluray_metadata_sidecar_from_state_preserving(
            Path::new("/fixtures/Concert.iso"),
            &state,
            Some(&existing),
        )
        .expect("save Blu-ray editor state");

        assert!(!saved.album.contains_key("LOCATION_NOTE"));
        assert_eq!(
            saved.tracks[0].tags.get("LOCATION_NOTE").map(String::as_str),
            Some("Front Hall")
        );
        assert_eq!(
            saved.tracks[1].tags.get("LOCATION_NOTE").map(String::as_str),
            Some("Back Hall")
        );
    }

    #[test]
    fn bluray_custom_field_can_move_from_track_to_album_scope_when_collapsed() {
        let existing = sidecar_with_custom_fields();
        let mut entry = bluray_tag_entry("TAKE_NOTE", "One Note", vec!["One Note", "One Note"]);
        entry.original = "<multiple values>".to_string();
        entry.per_file_originals = vec!["Old One".to_string(), "Old Two".to_string()];
        entry.is_mixed = false;
        let state = bluray_editor_state_for_custom_fields(vec![entry]);

        let saved = bluray_metadata_sidecar_from_state_preserving(
            Path::new("/fixtures/Concert.iso"),
            &state,
            Some(&existing),
        )
        .expect("save Blu-ray editor state");

        assert_eq!(saved.album.get("TAKE_NOTE").map(String::as_str), Some("One Note"));
        assert!(!saved.tracks[0].tags.contains_key("TAKE_NOTE"));
        assert!(!saved.tracks[1].tags.contains_key("TAKE_NOTE"));
    }

    #[test]
    fn bluray_unchanged_track_scoped_custom_field_stays_track_scoped() {
        let existing = sidecar_with_custom_fields();
        let mut entries = Vec::new();
        bluray_apply_sidecar_to_editor_fields(&mut entries, 2, &existing);
        let state = bluray_editor_state_for_custom_fields(entries);

        let saved = bluray_metadata_sidecar_from_state_preserving(
            Path::new("/fixtures/Concert.iso"),
            &state,
            Some(&existing),
        )
        .expect("save Blu-ray editor state");

        assert!(!saved.album.contains_key("TAKE_NOTE"));
        assert_eq!(
            saved.tracks[0].tags.get("TAKE_NOTE").map(String::as_str),
            Some("Old One")
        );
        assert_eq!(
            saved.tracks[1].tags.get("TAKE_NOTE").map(String::as_str),
            Some("Old Two")
        );
    }

    #[test]
    fn bluray_deleted_or_blank_custom_fields_are_omitted_on_save() {
        let existing = sidecar_with_custom_fields();
        let entries = vec![
            bluray_tag_entry("MOOD", "", vec!["", ""]),
            bluray_tag_entry("TAKE_NOTE", "<multiple values>", vec!["", "Kept Two"]),
        ];
        let mut state = bluray_editor_state_for_custom_fields(entries);
        state.active_surface_mut().deleted.push(0);

        let saved = bluray_metadata_sidecar_from_state_preserving(
            Path::new("/fixtures/Concert.iso"),
            &state,
            Some(&existing),
        )
        .expect("save Blu-ray editor state");

        assert!(!saved.album.contains_key("MOOD"));
        assert!(!saved.tracks[0].tags.contains_key("TAKE_NOTE"));
        assert_eq!(
            saved.tracks[1].tags.get("TAKE_NOTE").map(String::as_str),
            Some("Kept Two")
        );
    }

    #[test]
    fn bluray_custom_field_deletions_remove_stale_toml_keys() {
        let root = unique_dir("custom-delete-toml");
        std::fs::create_dir_all(&root).expect("create sidecar dir");
        let path = root.join(BLURAY_METADATA_SIDECAR_NAME);
        let initial = r#"schema_version = 1
format = "tonepoet-bluray-metadata"

[[presentations]]
[presentations.source]
path = "/fixtures/Concert.iso"
sidecar_kind = "tonepoet-bluray-metadata"
[presentations.source.presentation]
playlist_number = 12
audio_pid = 4352
audio_stream_index = 0
angle_number = 1
track_count = 2
duration_fingerprint = "fp-a"
[presentations.album]
ALBUM = "Old Album"
MOOD = "Old Mood"
[presentations.album.extra]
VENUE = "Old Venue"

[[presentations.tracks]]
number = 1
source_chapter = 1
label = "Chapter 1"
[presentations.tracks.tags]
TITLE = "Opening"
TAKE_NOTE = "Old One"
[presentations.tracks.tags.extra]
CUT_NOTE = "Old Cut One"

[[presentations.tracks]]
number = 2
source_chapter = 2
label = "Chapter 2"
[presentations.tracks.tags]
TITLE = "Finale"
TAKE_NOTE = "Old Two"
[presentations.tracks.tags.extra]
CUT_NOTE = "Old Cut Two"
"#;
        std::fs::write(&path, initial).expect("write initial TOML");
        let mut parsed = parse_bluray_metadata_sidecar_presentations(initial, &path)
            .expect("parse initial sidecar");
        let existing = parsed.remove(0);
        let mut entries = Vec::new();
        bluray_apply_sidecar_to_editor_fields(&mut entries, 2, &existing);

        for entry in &mut entries {
            match entry.display_key.as_str() {
                "MOOD" | "VENUE" => {
                    entry.value.clear();
                    entry.per_file_values = vec![String::new(), String::new()];
                    entry.is_mixed = false;
                }
                "TAKE_NOTE" => {
                    entry.value = "<multiple values>".to_string();
                    entry.per_file_values = vec![String::new(), String::new()];
                    entry.is_mixed = true;
                }
                "CUT_NOTE" => {
                    entry.value = "<multiple values>".to_string();
                    entry.per_file_values = vec!["Kept Cut One".to_string(), "Kept Cut Two".to_string()];
                    entry.is_mixed = true;
                }
                _ => {}
            }
        }
        let mut state = bluray_editor_state_for_custom_fields(entries);
        state.active_surface_mut().bluray_chapter_durations = None;
        let saved = bluray_metadata_sidecar_from_state_preserving(
            Path::new("/fixtures/Concert.iso"),
            &state,
            Some(&existing),
        )
        .expect("save Blu-ray editor state");

        save_bluray_metadata_sidecar(&path, &[saved]).expect("rewrite sidecar");
        let rewritten = std::fs::read_to_string(&path).expect("read rewritten TOML");
        assert!(!rewritten.contains("MOOD"));
        assert!(!rewritten.contains("VENUE"));
        assert!(!rewritten.contains("TAKE_NOTE"));
        assert!(rewritten.contains("CUT_NOTE"));
        assert!(rewritten.contains("Kept Cut One"));
        assert!(rewritten.contains("Kept Cut Two"));

        let mut reparsed_sidecars = load_bluray_metadata_sidecar_presentations(&root)
            .expect("load rewritten sidecar")
            .expect("sidecar present")
            .1;
        let reparsed = reparsed_sidecars.remove(0);
        assert!(!reparsed.album.contains_key("MOOD"));
        assert!(!reparsed.album.contains_key("VENUE"));
        assert!(!reparsed.tracks[0].tags.contains_key("TAKE_NOTE"));
        assert_eq!(
            reparsed.tracks[0].tags.get("CUT_NOTE").map(String::as_str),
            Some("Kept Cut One")
        );
        assert_eq!(
            reparsed.tracks[1].tags.get("CUT_NOTE").map(String::as_str),
            Some("Kept Cut Two")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bluray_preserving_save_updates_target_without_dropping_siblings() {
        let root = unique_dir("preserve");
        std::fs::create_dir_all(&root).expect("create sidecar dir");
        let path = root.join("tonepoet.bluray.metadata.toml");
        let initial = r#"schema_version = 1
format = "tonepoet-bluray-metadata"

[[presentations]]
keep_presentation = "main"
[presentations.source]
path = "/fixtures/Concert.iso"
sidecar_kind = "tonepoet-bluray-metadata"
[presentations.source.presentation]
playlist_number = 12
audio_pid = 4352
audio_stream_index = 0
angle_number = 1
track_count = 1
presentation_extension = "keep-identity"
[presentations.album]
ALBUM = "Old Main"
custom_album_key = "keep-main"
[[presentations.tracks]]
number = 1
source_chapter = 1
label = "Old Main Song"
[presentations.tracks.tags]
TITLE = "Old Main Song"
[presentations.tracks.extra]
review_note = "keep-track"

[[presentations]]
[presentations.source.presentation]
playlist_number = 99
audio_pid = 4352
audio_stream_index = 0
[presentations.album]
ALBUM = "Sibling"
"#;
        std::fs::write(&path, initial).expect("write initial TOML");
        let sidecar = BluRayMetadataSidecar {
            schema_version: BLURAY_METADATA_SIDECAR_SCHEMA_VERSION,
            source: BluRayMetadataSource {
                path: PathBuf::from("/fixtures/Concert.iso"),
                sidecar_kind: BLURAY_METADATA_FORMAT.to_string(),
                presentation: Some(BluRayPresentationIdentity {
                    playlist_number: 12,
                    audio_pid: 0x1100,
                    audio_stream_index: 0,
                    angle_number: Some(1),
                    track_count: Some(1),
                    duration_fingerprint: None,
                    extra: BTreeMap::new(),
                }),
                extra: BTreeMap::new(),
            },
            album: BTreeMap::from([
                ("ALBUM".to_string(), "New Main".to_string()),
                ("custom_album_key".to_string(), "keep-main".to_string()),
            ]),
            tracks: vec![BluRayMetadataTrack {
                number: 1,
                label: "New Main Song".to_string(),
                source_chapter: Some(1),
                tags: BTreeMap::from([("TITLE".to_string(), "New Main Song".to_string())]),
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        };

        save_bluray_metadata_sidecar(&path, &[sidecar]).expect("save preserving Blu-ray sidecar");
        let rewritten = std::fs::read_to_string(&path).expect("read rewritten TOML");

        assert_eq!(rewritten.matches("[[presentations]]").count(), 2);
        assert!(rewritten.contains("ALBUM = \"New Main\"") || rewritten.contains("album = \"New Main\""));
        assert!(rewritten.contains("ALBUM = \"Sibling\"") || rewritten.contains("album = \"Sibling\""));
        assert!(rewritten.contains("keep_presentation = \"main\""));
        assert!(rewritten.contains("custom_album_key = \"keep-main\""));
        assert!(rewritten.contains("review_note = \"keep-track\""));
        assert!(!rewritten.contains("Old Main Song"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bluray_dirty_multi_presentation_save_updates_all_dirty_tabs_once() {
        let root = unique_dir("multi-tab-save");
        std::fs::create_dir_all(&root).expect("create sidecar dir");
        let path = root.join(BLURAY_METADATA_SIDECAR_NAME);
        save_bluray_metadata_sidecar(
            &path,
            &[
                sidecar_for_playlist(12, "Old Main"),
                sidecar_for_playlist(14, "Clean Sibling"),
            ],
        )
        .expect("write initial sidecar");

        let source = root.as_path();
        let state = bluray_state_with_tabs(vec![
            bluray_save_tab(source, 12, 0, "New Main", "Main", true),
            bluray_save_tab(source, 13, 1, "New Added", "Added", true),
            bluray_save_tab(source, 14, 0, "Ignored Clean", "Clean", false),
        ]);

        let outcome = save_bluray_metadata_sidecar_dirty_presentations_from_state(source, &state)
            .expect("save dirty Blu-ray presentations");
        assert_eq!(outcome.saved_presentations, 2);
        assert_eq!(outcome.saved_tab_indices, vec![0, 1]);
        assert_eq!(outcome.updated_presentations, 1);
        assert_eq!(outcome.added_presentations, 1);
        assert_eq!(outcome.skipped_clean_presentations, 1);
        assert_eq!(outcome.missing_identity_presentations, 0);

        let (_, saved) = load_bluray_metadata_sidecar_presentations(source)
            .expect("load saved sidecar")
            .expect("sidecar present");
        assert_eq!(saved.len(), 3);
        let albums: BTreeMap<u32, &str> = saved
            .iter()
            .filter_map(|sidecar| {
                Some((
                    sidecar.source.presentation.as_ref()?.playlist_number,
                    sidecar.album.get("ALBUM")?.as_str(),
                ))
            })
            .collect();
        assert_eq!(albums.get(&12).copied(), Some("New Main"));
        assert_eq!(albums.get(&13).copied(), Some("New Added"));
        assert_eq!(albums.get(&14).copied(), Some("Clean Sibling"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bluray_dirty_save_skips_tabs_without_sufficient_identity_without_writing() {
        let root = unique_dir("missing-identity-save");
        std::fs::create_dir_all(&root).expect("create sidecar dir");
        let mut tab = bluray_save_tab(root.as_path(), 12, 0, "No Identity", "No Identity", true);
        tab.id = crate::disc::model::PresentationId::DvdAudioGroup(1);
        tab.bluray_playlist_number = None;
        tab.bluray_audio_pid = None;
        tab.bluray_audio_stream_index = None;
        tab.bluray_angle_number = None;

        let state = bluray_state_with_tabs(vec![tab]);
        let outcome = save_bluray_metadata_sidecar_dirty_presentations_from_state(
            root.as_path(),
            &state,
        )
        .expect("missing identity should be a reported skip, not a write error");
        assert_eq!(outcome.saved_presentations, 0);
        assert_eq!(outcome.missing_identity_presentations, 1);
        assert!(!root.join(BLURAY_METADATA_SIDECAR_NAME).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bluray_synthetic_toc_uses_positive_chapter_durations() {
        let sectors = bluray_editor_durations_to_cd_sectors(&[1.0, 2.0])
            .expect("positive Blu-ray chapter durations should build a TOC");
        assert_eq!(sectors, vec![150, 225, 375]);
    }

    #[test]
    fn bluray_synthetic_toc_reports_missing_or_invalid_chapter_durations() {
        let err = bluray_editor_durations_to_cd_sectors(&[90.0, 0.0])
            .expect_err("zero duration sentinel must not produce a false TOC");
        assert!(err.contains("Blu-ray editor chapter 2"));
        assert!(err.contains("missing or invalid duration"));

        let err = bluray_editor_durations_to_cd_sectors(&[])
            .expect_err("empty durations must not produce a false TOC");
        assert!(err.contains("no chapter durations"));
    }

    #[test]
    fn bluray_discovery_missing_sidecar_returns_none_and_malformed_toml_errors() {
        let root = unique_dir("missing-malformed");
        std::fs::create_dir_all(root.join("BDMV")).expect("create BDMV");

        assert!(
            load_bluray_metadata_sidecar_presentations(&root)
                .expect("missing sidecar is not an error")
                .is_none()
        );

        let path = root.join(BLURAY_METADATA_SIDECAR_NAME);
        std::fs::write(&path, "schema_version = [\n").expect("write malformed TOML");
        let err = load_bluray_metadata_sidecar_presentations(&root)
            .expect_err("malformed TOML should be reported");
        assert!(err.contains("parse Blu-ray TOML sidecar"));
        assert!(err.contains(path.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bluray_identity_matching_reports_structured_mismatch_reasons() {
        let current = identity(Some("fp-a"));
        assert!(bluray_presentation_identity_compatible(Some(&current), Some(&current)));

        let mut stored = current.clone();
        stored.playlist_number = 99;
        assert!(matches!(
            bluray_presentation_identity_mismatch_reasons(Some(&stored), Some(&current)).as_slice(),
            [BluRayIdentityMismatchReason::PlaylistNumber { .. }]
        ));

        let mut stored = current.clone();
        stored.audio_pid = 0x1101;
        assert!(matches!(
            bluray_presentation_identity_mismatch_reasons(Some(&stored), Some(&current)).as_slice(),
            [BluRayIdentityMismatchReason::AudioPid { .. }]
        ));

        let mut stored = current.clone();
        stored.audio_stream_index = 1;
        assert!(matches!(
            bluray_presentation_identity_mismatch_reasons(Some(&stored), Some(&current)).as_slice(),
            [BluRayIdentityMismatchReason::AudioStreamIndex { .. }]
        ));

        let mut stored = current.clone();
        stored.angle_number = Some(2);
        assert!(matches!(
            bluray_presentation_identity_mismatch_reasons(Some(&stored), Some(&current)).as_slice(),
            [BluRayIdentityMismatchReason::AngleNumber { .. }]
        ));

        let mut stored = current.clone();
        stored.angle_number = None;
        assert!(matches!(
            bluray_presentation_identity_mismatch_reasons(Some(&stored), Some(&current)).as_slice(),
            [BluRayIdentityMismatchReason::AngleNumber { .. }]
        ));
        assert!(!bluray_presentation_identity_compatible(Some(&stored), Some(&current)));

        let mut stored = current.clone();
        stored.track_count = Some(99);
        assert!(matches!(
            bluray_presentation_identity_mismatch_reasons(Some(&stored), Some(&current)).as_slice(),
            [BluRayIdentityMismatchReason::TrackCount { .. }]
        ));

        let mut stored = current.clone();
        stored.duration_fingerprint = Some("fp-b".to_string());
        assert!(matches!(
            bluray_presentation_identity_mismatch_reasons(Some(&stored), Some(&current)).as_slice(),
            [BluRayIdentityMismatchReason::DurationFingerprint { .. }]
        ));
    }

    #[test]
    fn bluray_save_is_deterministic_and_preserves_multiple_presentations() {
        let root = unique_dir("deterministic-save");
        std::fs::create_dir_all(&root).expect("create sidecar dir");
        let path = root.join(BLURAY_METADATA_SIDECAR_NAME);
        let first = minimal_sidecar_text("Album One");
        let second = minimal_sidecar_text("Album Two")
            .replace("playlist_number = 12", "playlist_number = 13")
            .replace("duration_fingerprint = \"fp-a\"", "duration_fingerprint = \"fp-b\"");
        let text = format!("{}\n{}", first, second.replace("schema_version = 1\nformat = \"tonepoet-bluray-metadata\"\n\n", ""));
        let parsed = parse_bluray_metadata_sidecar_presentations(&text, &path)
            .expect("parse multiple presentations");
        assert_eq!(parsed.len(), 2);

        save_bluray_metadata_sidecar(&path, &parsed).expect("first save");
        let saved_once = std::fs::read_to_string(&path).expect("read first save");
        let reparsed = load_bluray_metadata_sidecar_presentations(&root)
            .expect("load saved sidecar")
            .expect("sidecar present")
            .1;
        save_bluray_metadata_sidecar(&path, &reparsed).expect("second save");
        let saved_twice = std::fs::read_to_string(&path).expect("read second save");

        assert_eq!(saved_once, saved_twice);
        assert_eq!(saved_once.matches("[[presentations]]").count(), 2);
        assert!(saved_once.contains("Album One"));
        assert!(saved_once.contains("Album Two"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bluray_shared_tag_helpers_apply_deterministic_precedence_and_safe_track_fallback() {
        let mut sidecar = sidecar_with_fingerprint(None);
        sidecar.album.insert("YEAR".to_string(), "1971".to_string());
        sidecar.album.insert("DATE".to_string(), "1972".to_string());
        let album_overlay = bluray_album_tag_overlay(&sidecar);
        assert_eq!(album_overlay.year.as_deref(), Some("1972"));

        sidecar.tracks = vec![BluRayMetadataTrack {
            number: 1,
            label: "Legacy".to_string(),
            source_chapter: None,
            tags: BTreeMap::from([
                ("TITLE".to_string(), "Legacy One".to_string()),
                ("ARTIST".to_string(), "Artist wins".to_string()),
                ("PERFORMER".to_string(), "Performer fallback".to_string()),
            ]),
            extra: BTreeMap::new(),
        }];
        let overlay = bluray_track_tag_overlay_for_authored_chapter(&sidecar, 1, true)
            .expect("legacy fallback allowed");
        assert_eq!(overlay.title.as_deref(), Some("Legacy One"));
        assert_eq!(bluray_track_overlay_performer_value(&overlay), Some("Artist wins"));
        assert!(bluray_track_tag_overlay_for_authored_chapter(&sidecar, 1, false).is_none());
    }
}

#[cfg(test)]
mod sacd_seed_tests {
    use super::*;
    use crate::tui::app::MetadataEditorState;
    use crate::tui::probe::TagEntry;

    fn editor_with(entries: Vec<(&str, &str, bool)>) -> MetadataEditorState {
        // Each entry: (display_key, value, is_mixed). per_file_values
        // shape doesn't matter for seed extraction — it only reads
        // `value` and `is_mixed`/empty checks via `value`.
        use lofty::tag::ItemKey;
        let entries: Vec<TagEntry> = entries
            .into_iter()
            .map(|(k, v, mixed)| TagEntry {
                row_scope: crate::tui::probe::RowScope::File,
                display_key: k.to_string(),
                item_key: ItemKey::Unknown(k.to_string()),
                value: v.to_string(),
                original: String::new(),
                is_binary: false,
                is_mixed: mixed,
                per_file_values: vec![v.to_string()],
                per_file_originals: vec![v.to_string()],
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            })
            .collect();
        MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/tmp/x.iso")],
            entries,
            vec!["01".to_string()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        )
    }


    #[test]
    fn seed_returns_none_when_all_fields_empty() {
        let state = editor_with(vec![]);
        assert!(seed_sacd_mb_query(&state).is_none());
    }

    #[test]
    fn seed_returns_none_when_only_other_keys_present() {
        // TITLE / TRACKNUMBER aren't seed candidates.
        let state = editor_with(vec![
            ("TITLE", "Some Title", false),
            ("TRACKNUMBER", "01", false),
        ]);
        assert!(seed_sacd_mb_query(&state).is_none());
    }

    #[test]
    fn seed_prefers_albumartist_over_artist() {
        let state = editor_with(vec![
            ("ALBUMARTIST", "Miles Davis", false),
            ("ARTIST", "Miles Davis Sextet", false),
            ("ALBUM", "Kind of Blue", false),
        ]);
        let seed = seed_sacd_mb_query(&state).expect("seed");
        assert_eq!(seed.artist, "Miles Davis");
        assert_eq!(seed.album, "Kind of Blue");
        assert_eq!(seed.catalog, None);
        assert_eq!(seed.year, None);
    }

    #[test]
    fn seed_falls_back_to_artist_when_albumartist_missing() {
        let state = editor_with(vec![
            ("ARTIST", "Thelonious Monk", false),
            ("ALBUM", "Solo Monk", false),
        ]);
        let seed = seed_sacd_mb_query(&state).expect("seed");
        assert_eq!(seed.artist, "Thelonious Monk");
    }

    #[test]
    fn seed_skips_mixed_per_track_artist() {
        // ARTIST is per-track and divergent → can't seed an
        // album-level query; ALBUMARTIST missing → artist empty.
        let state = editor_with(vec![
            ("ARTIST", "<multiple values>", true),
            ("ALBUM", "Various Compilation", false),
        ]);
        let seed = seed_sacd_mb_query(&state).expect("seed");
        assert_eq!(seed.artist, "");
        assert_eq!(seed.album, "Various Compilation");
    }

    #[test]
    fn seed_passes_catalog_and_year_when_present() {
        let state = editor_with(vec![
            ("ALBUMARTIST", "Thelonious Monk", false),
            ("ALBUM", "Solo Monk", false),
            ("CATALOGNUMBER", "SRGS 4520", false),
            ("DATE", "1965", false),
        ]);
        let seed = seed_sacd_mb_query(&state).expect("seed");
        assert_eq!(seed.catalog.as_deref(), Some("SRGS 4520"));
        assert_eq!(seed.year.as_deref(), Some("1965"));
    }

    #[test]
    fn seed_trims_whitespace_in_field_values() {
        // ScarletBook strings sometimes carry trailing whitespace
        // from padding in the binary header; the seed should strip
        // those before they reach Lucene escape.
        let state = editor_with(vec![("ALBUM", "  Padded Album  ", false)]);
        let seed = seed_sacd_mb_query(&state).expect("seed");
        assert_eq!(seed.album, "Padded Album");
    }

    #[test]
    fn seed_treats_only_catalog_as_sufficient() {
        // MB search with only `catno:` is valid and useful (some
        // pressings have nothing else reliable).
        let state = editor_with(vec![("CATALOGNUMBER", "SRGS 4520", false)]);
        let seed = seed_sacd_mb_query(&state).expect("seed");
        assert_eq!(seed.artist, "");
        assert_eq!(seed.album, "");
        assert_eq!(seed.catalog.as_deref(), Some("SRGS 4520"));
    }

    #[test]
    fn seed_empty_value_is_treated_as_absent() {
        // An entry that exists but has empty value (e.g., user
        // cleared the row) shouldn't make catalog Some("").
        let state = editor_with(vec![("ALBUM", "X", false), ("CATALOGNUMBER", "", false)]);
        let seed = seed_sacd_mb_query(&state).expect("seed");
        assert_eq!(seed.catalog, None);
    }
}

#[cfg(test)]
mod sacd_toc_tests {
    use super::{dvdv_editor_durations_to_cd_sectors, durations_to_cd_sectors, sacd_durations_to_sectors};

    #[test]
    fn three_track_disc_offsets_and_leadout() {
        // 4:00, 3:30, 5:00 = 240s / 210s / 300s at 75 fps.
        // 240*75 = 18000, 210*75 = 15750, 300*75 = 22500.
        // offsets: 150, 18150, 33900;  leadout: 56400.
        let sectors = sacd_durations_to_sectors(&[240.0, 210.0, 300.0]);
        assert_eq!(sectors, vec![150, 18150, 33900, 56400]);
    }

    #[test]
    fn single_track_disc() {
        let sectors = sacd_durations_to_sectors(&[60.0]);
        // offsets: [150], leadout: 150 + 60*75 = 4650.
        assert_eq!(sectors, vec![150, 4650]);
    }

    #[test]
    fn empty_input_yields_pre_gap_only() {
        // Defensive: zero tracks produces only the pre-gap entry.
        // The dispatch path refuses this case before calling, but
        // we don't want to panic if it slips through.
        let sectors = sacd_durations_to_sectors(&[]);
        assert_eq!(sectors, vec![150]);
    }

    #[test]
    fn fractional_seconds_round_to_nearest_frame() {
        // 4:00.74 frame = 240 + 74/75 sec → 18074 frames exact.
        // Verify rounding handles sub-frame fractions cleanly.
        let sectors = sacd_durations_to_sectors(&[240.0 + 74.0 / 75.0]);
        assert_eq!(sectors, vec![150, 150 + 18074]);
    }

    #[test]
    fn dvdv_duration_toc_uses_musicbrainz_cd_sector_geometry() {
        let sectors = durations_to_cd_sectors([240.0, 210.0, 300.0]);
        let toc = crate::tui::musicbrainz::build_mb_toc(&sectors).expect("toc");
        assert_eq!(sectors, vec![150, 18150, 33900, 56400]);
        assert_eq!(toc, "1+3+56400+150+18150+33900");
    }

    #[test]
    fn dvdv_editor_toc_rejects_missing_zero_or_nonfinite_durations() {
        assert!(dvdv_editor_durations_to_cd_sectors(&[]).is_err());
        assert!(dvdv_editor_durations_to_cd_sectors(&[240.0, 0.0, 210.0]).is_err());
        assert!(dvdv_editor_durations_to_cd_sectors(&[240.0, f64::NAN]).is_err());
        assert!(dvdv_editor_durations_to_cd_sectors(&[240.0, f64::INFINITY]).is_err());
    }

    #[test]
    fn dvdv_editor_toc_accepts_only_positive_finite_durations() {
        let sectors = dvdv_editor_durations_to_cd_sectors(&[240.0, 210.0]).expect("sectors");
        assert_eq!(sectors, vec![150, 18150, 33900]);
    }

    #[test]
    fn build_mb_toc_consumes_the_sector_form() {
        // Round-trip with the existing `build_mb_toc` to confirm the
        // output shape is the one MB expects: "1 N leadout off1 off2 …"
        // ("+"-joined, since the helper is also used to build URLs).
        let sectors = sacd_durations_to_sectors(&[240.0, 210.0, 300.0]);
        let toc = crate::tui::musicbrainz::build_mb_toc(&sectors).expect("toc");
        assert_eq!(toc, "1+3+56400+150+18150+33900");
    }

    #[test]
    fn long_compilation_does_not_overflow() {
        // 24 hours of audio = ~6.5M frames, well within u32.
        // 100 tracks of 14:24 each = 86400 seconds total.
        let dur = 864.0; // 14:24 each
        let durs = vec![dur; 100];
        let sectors = sacd_durations_to_sectors(&durs);
        assert_eq!(sectors.len(), 101);
        // Final leadout = 150 + 100 * 14:24 in frames.
        let expected_leadout = 150 + 100 * ((dur * 75.0).round() as u32);
        assert_eq!(*sectors.last().unwrap(), expected_leadout);
    }
}

#[cfg(test)]
mod tags_mb_args_tests {
    use super::*;

    fn parse(s: &str) -> Command {
        super::parse_tags_mb_args(s)
    }

    fn tokenize(s: &str) -> Result<Vec<String>, String> {
        super::tokenize_tags_mb_args(s)
    }

    // ── tokenizer ──────────────────────────────────────────────

    #[test]
    fn tokenize_plain_whitespace_split() {
        assert_eq!(tokenize("a b c").unwrap(), vec!["a", "b", "c"]);
    }

    #[test]
    fn tokenize_double_quoted_preserves_internal_spaces() {
        assert_eq!(
            tokenize(r#"--catno "SRGS 4520" miles davis"#).unwrap(),
            vec!["--catno", "SRGS 4520", "miles", "davis"],
        );
    }

    #[test]
    fn tokenize_unterminated_quote_errors() {
        assert!(tokenize(r#"--catno "SRGS 4520"#).is_err());
    }

    #[test]
    fn tokenize_empty_input_is_empty_vec() {
        assert!(tokenize("").unwrap().is_empty());
        assert!(tokenize("   ").unwrap().is_empty());
    }

    #[test]
    fn tokenize_multiple_consecutive_spaces_collapse() {
        assert_eq!(tokenize("a   b").unwrap(), vec!["a", "b"]);
    }

    // ── parser: bare form ─────────────────────────────────────

    #[test]
    fn empty_args_parses_to_all_none() {
        match parse("") {
            Command::TagsFromMb { query, catno, year } => {
                assert_eq!(query, None);
                assert_eq!(catno, None);
                assert_eq!(year, None);
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    #[test]
    fn whitespace_only_args_parses_to_all_none() {
        match parse("   ") {
            Command::TagsFromMb { query, catno, year } => {
                assert!(query.is_none() && catno.is_none() && year.is_none());
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    // ── parser: free-form text ─────────────────────────────────

    #[test]
    fn free_form_text_only() {
        match parse("miles davis kind of blue") {
            Command::TagsFromMb { query, catno, year } => {
                assert_eq!(query.as_deref(), Some("miles davis kind of blue"));
                assert_eq!(catno, None);
                assert_eq!(year, None);
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    // ── parser: --catno ────────────────────────────────────────

    #[test]
    fn catno_only_with_quoted_value() {
        match parse(r#"--catno "SRGS 4520""#) {
            Command::TagsFromMb { query, catno, year } => {
                assert_eq!(catno.as_deref(), Some("SRGS 4520"));
                assert_eq!(query, None);
                assert_eq!(year, None);
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    #[test]
    fn catno_with_bare_value_no_quotes() {
        match parse("--catno SRGS-4520") {
            Command::TagsFromMb { catno, .. } => {
                assert_eq!(catno.as_deref(), Some("SRGS-4520"));
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    #[test]
    fn catno_missing_value_errors() {
        assert!(matches!(parse("--catno"), Command::Unknown(_)));
    }

    #[test]
    fn catno_duplicate_errors() {
        assert!(matches!(parse("--catno X --catno Y"), Command::Unknown(_)));
    }

    // ── parser: --year ─────────────────────────────────────────

    #[test]
    fn year_only() {
        match parse("--year 1971") {
            Command::TagsFromMb { year, .. } => {
                assert_eq!(year.as_deref(), Some("1971"));
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    #[test]
    fn year_missing_value_errors() {
        assert!(matches!(parse("--year"), Command::Unknown(_)));
    }

    // ── parser: combined / interleaved ────────────────────────

    #[test]
    fn all_three_in_canonical_order() {
        match parse(r#"--catno "ESGA 509" --year 1971 carole king tapestry"#) {
            Command::TagsFromMb { query, catno, year } => {
                assert_eq!(catno.as_deref(), Some("ESGA 509"));
                assert_eq!(year.as_deref(), Some("1971"));
                assert_eq!(query.as_deref(), Some("carole king tapestry"));
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    #[test]
    fn flags_after_free_text() {
        match parse("miles davis --year 1959 --catno CL-1355") {
            Command::TagsFromMb { query, catno, year } => {
                assert_eq!(query.as_deref(), Some("miles davis"));
                assert_eq!(catno.as_deref(), Some("CL-1355"));
                assert_eq!(year.as_deref(), Some("1959"));
            }
            c => panic!("expected TagsFromMb, got {:?}", c),
        }
    }

    // ── parser: error cases ────────────────────────────────────

    #[test]
    fn unknown_flag_errors() {
        assert!(matches!(parse("--frobnicate X"), Command::Unknown(_)));
    }

    #[test]
    fn unterminated_quote_errors() {
        assert!(matches!(
            parse(r#"--catno "SRGS 4520"#),
            Command::Unknown(_)
        ));
    }
}


#[cfg(test)]
mod execute_queue_state_consistency_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::tui::app::{AppScreen, SourceMode};
    use crate::convert::classify::EntryKind;
    use crate::tui::browse::BrowseEntry;
    use tokio::sync::mpsc;

    #[test]
    fn execute_queue_uses_async_folder_expansion_before_source_publication() {
        let source = include_str!("command.rs");
        let execute_queue_start = source
            .find("fn execute_queue_with_post_load(")
            .expect("shared queue implementation should exist");
        let library_arm_start = source[execute_queue_start..]
            .find("AppScreen::Library =>")
            .map(|offset| execute_queue_start + offset)
            .expect("Browse arm should be followed by Library arm");
        let browse_arm = &source[execute_queue_start..library_arm_start];

        let candidate_check = browse_arm
            .find("browse_selection_contains_regular_audio_folder_for_convert(app, &raw_selection)")
            .expect("queue should cheaply detect regular folder candidates on the raw selection");
        let async_start = browse_arm
            .find("start_browse_convert_folder_expansion(")
            .expect("regular folder expansion must start a background job");
        let collect = browse_arm
            .find("let selection = app.browse.collect_selection_for_queue();")
            .expect("queue should collect the browse selection once");
        let immediate_finish = browse_arm
            .find("finish_browse_queue_review_after_expansion(app, tx, preset, selection, 0)")
            .expect("non-folder selections should still publish immediately");
        let post_load = browse_arm
            .find("apply_browse_convert_post_load_action(app, tx, post_load)")
            .expect("non-folder selections should run the same post-load continuation");

        // The raw-selection candidate check must run before
        // collect_selection_for_queue(): that collection expands directories
        // with a synchronous recursive walk on the reducer path.
        assert!(candidate_check < async_start);
        assert!(async_start < collect);
        assert!(collect < immediate_finish);
        assert!(immediate_finish < post_load);
        assert!(
            !browse_arm.contains("expand_regular_filesystem_audio_folders_for_convert_blocking"),
            "execute_queue must not recursively expand folders on the reducer path"
        );
    }

    #[test]
    fn async_convert_review_completion_preserves_post_load_commit_continuation() {
        let source = include_str!("command.rs");
        let target_start = source
            .find("ConvertReview {\n        preset: Option<String>,\n        post_load: BrowseConvertPostLoad,")
            .expect("ConvertReview target should carry post-load continuation");
        let handler_start = source
            .find("pub(crate) fn handle_browse_convert_expansion_complete(")
            .expect("expansion completion handler should exist");
        let handler_body = &source[handler_start..];
        let finish_call = handler_body
            .find("finish_browse_queue_review_after_expansion(")
            .expect("fresh ConvertReview completions should publish via queue review helper");
        let continuation = handler_body
            .find("apply_browse_convert_post_load_action(app, tx, post_load)")
            .expect("fresh ConvertReview completions should resume post-load commit/start");

        assert!(target_start < handler_start);
        assert!(finish_call < continuation);
    }

    #[test]
    fn browse_queue_commit_preserves_synthetic_artifact_on_ownership_failure() {
        let source = include_str!("command.rs");
        let start = source
            .find("fn queue_browse_convert_paths_for_processing(")
            .expect("Browse queue commit helper should exist");
        let end = source[start..]
            .find("pub(crate) fn install_browse_convert_source_paths")
            .map(|offset| start + offset)
            .expect("queue helper should precede source installation helper");
        let body = &source[start..end];

        assert!(
            body.contains("ConversionError::SyntheticCueArtifactOwnershipFailed"),
            "Browse queue commit must distinguish registry failure from ordinary admission failure"
        );
        assert!(
            !body.contains("register_synthetic_cue_artifact(&item_id"),
            "Browse queue commit must not repeat registration after manager ready-admission already registered ownership"
        );
        let failure_arm = body
            .split("ConversionError::SyntheticCueArtifactOwnershipFailed")
            .nth(1)
            .expect("ownership failure arm should be present")
            .split("Err(err) =>")
            .next()
            .expect("ownership failure arm should precede ordinary error arm");
        assert!(
            failure_arm.contains("artifact preserved"),
            "ownership failure status/log must tell the user the artifact was preserved"
        );
        assert!(
            !failure_arm.contains("cleanup_synthetic_cue_artifact"),
            "ownership failure must not clean caller-owned synthetic artifacts"
        );
    }



    #[test]
    fn convert_commit_uses_transaction_result_without_post_commit_path_inspection() {
        let source = include_str!("command.rs");
        let start = source
            .find("fn execute_commit_with_source_options_transform(")
            .expect("commit helper should exist");
        let end = source[start..]
            .find("fn execute_go(")
            .map(|offset| start + offset)
            .expect("commit helper should precede go helper");
        let body = &source[start..end];

        assert!(
            body.contains("let transaction = app.manager.commit_batch_with_cue_artifacts("),
            "Convert commit must use the manager transaction result"
        );
        assert!(
            body.contains("|item| {") && body.contains("item.pipeline_request = Some(PipelineRequest"),
            "Convert commit must configure admitted items inside the manager transaction closure"
        );
        assert!(
            !body.contains("blocking_write()"),
            "Convert commit must not call Tokio blocking lock methods from the async TUI event loop"
        );
        assert!(
            body.contains("artifacts_remaining_caller_owned"),
            "Convert commit must consume exact caller-owned artifact state from the transaction"
        );
        assert!(
            !body.contains("synthetic_cue_artifact_paths_owned_by_manager"),
            "Convert commit must not reconstruct synthetic ownership with a post-commit path inspection"
        );
        assert!(
            !body.contains("register_synthetic_cue_artifacts_for_current_queue_nonblocking"),
            "Convert commit must not run a second path-based registration pass"
        );
        assert!(
            !body.contains("pre_commit_item_ids_for_batch"),
            "Convert commit must not rely on pre-commit path snapshots for rollback"
        );
        assert!(
            !body.contains("item_ids_for_paths(&batch)"),
            "Convert commit rollback must not infer admitted ids from paths"
        );
    }


    #[test]
    fn browse_queue_completion_carries_expansion_cue_metadata_after_freshness_checks() {
        let source = include_str!("command.rs");
        let handler_start = source
            .find("pub(crate) fn handle_browse_convert_expansion_complete(")
            .expect("expansion completion handler should exist");
        let handler_body = &source[handler_start..];
        let generation_guard = handler_body
            .find("if generation != app.probe_generation")
            .expect("completion must generation-check stale jobs");
        let selection_guard = handler_body
            .find("browse_convert_expansion_selection_still_current")
            .expect("completion must selection-check stale jobs");
        let finish_call = handler_body
            .find("finish_browse_queue_review_after_expansion(")
            .expect("fresh ConvertReview completions should publish via queue review helper");

        assert!(generation_guard < finish_call);
        assert!(selection_guard < finish_call);

        let finish_start = source
            .find("fn finish_browse_queue_review_after_expansion(")
            .expect("queue review finish helper should exist");
        // Bound the scan to this function (up to the next top-level `fn `):
        // later functions — notably execute_queue_with_post_load's synchronous
        // non-folder path — legitimately collect the Browse selection.
        let finish_tail = &source[finish_start..];
        let finish_end = finish_tail[1..]
            .find("\nfn ")
            .map(|offset| offset + 1)
            .unwrap_or(finish_tail.len());
        let finish_body = &finish_tail[..finish_end];
        assert!(
            !finish_body.contains("let selection = app.browse.collect_selection_for_queue();"),
            "fresh async completions must not recompute queue semantics through Browse"
        );
        let destructure = finish_body
            .find("let QueueExpansionResult { paths, mut cue_artifact_audio, synthetic_cue_artifacts, expansion_errors } = queue;")
            .expect("CUE metadata should come from the expansion result");
        let retain = finish_body
            .find("cue_artifact_audio.retain")
            .expect("CUE metadata must be trimmed to expanded paths");
        // Warnings pass through to install_browse_convert_source_paths so
        // they fold into the final status instead of being overwritten.
        let publish = finish_body
            .find("QueueExpansionResult { paths, cue_artifact_audio, synthetic_cue_artifacts, expansion_errors }")
            .expect("CUE metadata should be published with source paths");

        assert!(destructure < retain);
        assert!(retain < publish);
    }

    #[test]
    fn async_browse_folder_expansion_uses_one_global_queue_plan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("album");
        std::fs::create_dir_all(&album).expect("album dir");
        let cue = album.join("album.cue");
        let image = album.join("album.flac");
        std::fs::write(&image, b"not real flac").expect("image fixture");
        std::fs::write(
            &cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:12:00
"#,
        )
        .expect("cue fixture");

        let expansion = expand_regular_filesystem_audio_folders_for_convert_blocking(
            false,
            vec![album, image.clone()],
            tokio_util::sync::CancellationToken::new(),
        );

        assert!(!expansion.cancelled);
        assert!(expansion.expansion_errors.is_empty());
        assert_eq!(expansion.queue.paths, vec![cue]);
        assert!(!expansion.queue.paths.contains(&image));
    }

    #[test]
    fn compatibility_folder_adapter_rejects_owned_synthetic_cue_artifacts() {
        fn synthetic_artifact_dirs() -> std::collections::BTreeSet<std::path::PathBuf> {
            let root = std::env::temp_dir().join("tonepoet-synthetic-cue-albums");
            std::fs::read_dir(root)
                .ok()
                .into_iter()
                .flat_map(|entries| entries.flatten())
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("artifact-"))
                })
                .collect()
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("album");
        std::fs::create_dir_all(&album).expect("album dir");
        for (stem, title) in [("side_a", "Album Side A"), ("side_b", "Album Side B")] {
            std::fs::write(album.join(format!("{stem}.flac")), b"not real flac").expect("audio fixture");
            std::fs::write(
                album.join(format!("{stem}.cue")),
                format!(
                    r#"TITLE "{title}"
FILE "{stem}.flac" WAVE
  TRACK 01 AUDIO
    TITLE "{stem} track 1"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "{stem} track 2"
    INDEX 01 00:30:00
"#
                ),
            )
            .expect("cue fixture");
        }

        let before = synthetic_artifact_dirs();
        let err = regular_filesystem_audio_folder_paths_for_convert_blocking(false, &album)
            .expect_err("legacy adapter must not discard ownership of synthetic CUE artifacts");
        let after = synthetic_artifact_dirs();

        assert!(
            err.contains("ownership-bearing queue expansion API"),
            "adapter should direct callers to the lifecycle-owning API: {err}"
        );
        assert_eq!(
            before, after,
            "adapter must clean any transient synthetic CUE artifacts before returning an error"
        );
    }

    #[test]
    fn async_browse_folder_expansion_explicit_cue_suppresses_discovered_audio() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("album");
        std::fs::create_dir_all(&album).expect("album dir");
        let cue = album.join("album.cue");
        let image = album.join("album.flac");
        std::fs::write(&image, b"not real flac").expect("image fixture");
        std::fs::write(
            &cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:12:00
"#,
        )
        .expect("cue fixture");

        let expansion = expand_regular_filesystem_audio_folders_for_convert_blocking(
            false,
            vec![album, cue.clone()],
            tokio_util::sync::CancellationToken::new(),
        );

        assert!(!expansion.cancelled);
        assert!(expansion.expansion_errors.is_empty());
        assert_eq!(expansion.queue.paths, vec![cue]);
        assert!(!expansion.queue.paths.contains(&image));
    }

    #[cfg(unix)]
    #[test]
    fn async_browse_folder_expansion_deduplicates_canonical_equivalent_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("album");
        std::fs::create_dir_all(&album).expect("album dir");
        let track = album.join("track.flac");
        let link = temp.path().join("track-link.flac");
        std::fs::write(&track, b"not real flac").expect("track fixture");
        std::os::unix::fs::symlink(&track, &link).expect("symlink fixture");

        let expansion = expand_regular_filesystem_audio_folders_for_convert_blocking(
            false,
            vec![album, link.clone()],
            tokio_util::sync::CancellationToken::new(),
        );

        assert!(!expansion.cancelled);
        assert!(expansion.expansion_errors.is_empty());
        assert_eq!(expansion.queue.paths.len(), 1);
        assert!(crate::convert::queue_expansion::path_list_contains_queue_identity(
            &expansion.queue.paths,
            &track,
        ));
        assert!(crate::convert::queue_expansion::path_list_contains_queue_identity(
            &expansion.queue.paths,
            &link,
        ));
    }

    #[test]
    fn async_browse_folder_expansion_preserves_cue_artifact_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("album");
        std::fs::create_dir_all(&album).expect("album dir");
        let cue = album.join("album.cue");
        let image = album.join("album.flac");
        std::fs::write(&image, b"not real flac").expect("image fixture");
        std::fs::write(
            &cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .expect("cue fixture");

        let expansion = expand_regular_filesystem_audio_folders_for_convert_blocking(
            false,
            vec![album],
            tokio_util::sync::CancellationToken::new(),
        );

        assert!(!expansion.cancelled);
        assert!(expansion.expansion_errors.is_empty());
        assert_eq!(expansion.queue.paths, vec![image.clone()]);
        assert!(expansion.queue.cue_artifact_audio.contains(&image));
        assert!(matches!(
            crate::convert::queue_expansion::cue_sidecar_override_for_commit_path(
                &image,
                &expansion.queue.cue_artifact_audio,
            ),
            Some(crate::convert::pipeline::CueSidecarPolicy::EmbeddedOnly),
        ));
    }

    // Entering the Convert screen kicks off an async source probe
    // (tokio::spawn), so this test needs a runtime.
    #[tokio::test]
    async fn async_browse_queue_review_completion_preserves_stale_notice_after_final_status() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("album");
        std::fs::create_dir_all(&album).expect("album dir");
        let track = album.join("01 - One.flac");
        std::fs::write(&track, b"fixture").expect("track fixture");

        let (tx, _rx) = mpsc::channel(8);
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = temp.path().to_path_buf();
        app.browse.entries = vec![BrowseEntry::new(
            album.clone(),
            "album".to_string(),
            EntryKind::Directory,
            0,
            None,
        )];
        app.browse.selected_index = 0;

        let request = BrowseConvertExpansionRequest {
            target: BrowseConvertExpansionTarget::ConvertReview {
                preset: None,
                post_load: BrowseConvertPostLoad::ReviewOnly,
            },
            selection_snapshot: vec![album],
            browse_in_archive: false,
            dropped_stale_selection_count: 2,
        };
        let (generation, _cancel) = app.begin_browse_convert_expansion(request.clone());

        handle_browse_convert_expansion_complete(
            &mut app,
            &tx,
            generation,
            request,
            BrowseConvertExpansion {
                queue: QueueExpansionResult {
                    paths: vec![track],
                    cue_artifact_audio: std::collections::HashSet::new(),
                    synthetic_cue_artifacts: std::collections::HashSet::new(),
                    expansion_errors: Vec::new(),
                },
                expanded_folder_count: 1,
                empty_audio_folders: Vec::new(),
                expansion_errors: Vec::new(),
                visited: 1,
                cancelled: false,
            },
        );

        assert_eq!(app.current_screen, AppScreen::Convert);
        let status = app
            .status_message
            .as_ref()
            .map(|(message, _)| message.as_str())
            .unwrap_or_default();
        // The stale notice must survive as a prefix of whichever completion
        // status the review flow writes (probe wording varies by fixture
        // decodability, so pin the shared suffix instead of one branch).
        assert!(status.starts_with("Ignored 2 stale selection marks outside the current directory; "));
        assert!(status.contains("review settings, then :commit or :Commit"));
    }

    #[test]
    fn stale_browse_convert_expansion_completion_does_not_publish_or_commit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album_a = temp.path().join("album-a");
        let album_b = temp.path().join("album-b");
        std::fs::create_dir_all(&album_a).expect("album-a dir");
        std::fs::create_dir_all(&album_b).expect("album-b dir");
        let track_a = album_a.join("01 - A.flac");
        std::fs::write(&track_a, b"fixture").expect("track fixture");

        let (tx, _rx) = mpsc::channel(8);
        let mut config = TonepoetConfig::default();
        config.conversion.default_destination = Some(temp.path().join("out"));
        let mut app = AppState::new_for_test(config);
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = temp.path().to_path_buf();
        app.browse.entries = vec![
            BrowseEntry::new(
                album_a.clone(),
                "album-a".to_string(),
                EntryKind::Directory,
                0,
                None,
            ),
            BrowseEntry::new(
                album_b.clone(),
                "album-b".to_string(),
                EntryKind::Directory,
                0,
                None,
            ),
        ];
        app.browse.selected_index = 0;

        let request = BrowseConvertExpansionRequest {
            target: BrowseConvertExpansionTarget::ConvertReview {
                preset: None,
                post_load: BrowseConvertPostLoad::Commit { start: false },
            },
            selection_snapshot: vec![album_a],
            browse_in_archive: false,
            dropped_stale_selection_count: 0,
        };
        let (generation, _cancel) = app.begin_browse_convert_expansion(request.clone());

        let merge_album = temp.path().join("merge-album");
        std::fs::create_dir_all(&merge_album).expect("merge album dir");
        std::fs::write(merge_album.join("side_a.flac"), b"fixture").expect("side A audio");
        std::fs::write(merge_album.join("side_b.flac"), b"fixture").expect("side B audio");
        std::fs::write(
            merge_album.join("side_a.cue"),
            r#"TITLE "Album Side A"
FILE "side_a.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 00:30:00
"#,
        )
        .expect("side A cue");
        std::fs::write(
            merge_album.join("side_b.cue"),
            r#"TITLE "Album Side B"
FILE "side_b.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 00:30:00
"#,
        )
        .expect("side B cue");
        let stale_expansion = crate::convert::queue_expansion::expand_paths_to_audio_with_metadata(&[
            merge_album.clone(),
        ]);
        assert_eq!(stale_expansion.synthetic_cue_artifacts.len(), 1);
        let stale_artifact = stale_expansion
            .synthetic_cue_artifacts
            .iter()
            .next()
            .expect("synthetic artifact")
            .clone();
        assert!(stale_artifact.exists());

        app.browse.selected_index = 1;
        handle_browse_convert_expansion_complete(
            &mut app,
            &tx,
            generation,
            request,
            BrowseConvertExpansion {
                queue: stale_expansion,
                expanded_folder_count: 1,
                empty_audio_folders: Vec::new(),
                expansion_errors: Vec::new(),
                visited: 1,
                cancelled: false,
            },
        );

        assert!(!stale_artifact.exists(), "discarded stale expansion must clean unowned synthetic artifacts");
        assert!(app.pending_browse_convert_expansion.is_none());
        assert_eq!(app.current_screen, AppScreen::Browse);
        assert!(matches!(app.convert.source.mode, SourceMode::Empty));
        let queued = app.manager.queue.try_read().expect("queue read").all_items().len();
        assert_eq!(queued, 0);
    }
}

#[cfg(test)]
mod command_companion_policy_tests {
    use super::*;

    #[test]
    fn command_companion_policy_uses_projected_custom_values() {
        let mut options = crate::convert::formats::ConversionOptions::default();
        options.copy_auxiliary_files = true;
        options.copy_subdirectories = true;
        options.companion_extensions = vec!["jpg".to_string(), ".PDF".to_string()];
        options.companion_folders = vec!["Scans".to_string(), "Artwork".to_string()];

        let policy = companion_copy_policy_from_conversion_options(&options);

        assert_eq!(policy.extensions, vec![".jpg", ".pdf"]);
        assert_eq!(policy.folders, vec!["Scans", "Artwork"]);
    }

    #[test]
    fn command_companion_policy_preserves_explicit_disabled_values() {
        let mut options = crate::convert::formats::ConversionOptions::default();
        options.copy_auxiliary_files = false;
        options.copy_subdirectories = false;
        options.companion_extensions = vec!["jpg".to_string()];
        options.companion_folders = vec!["Scans".to_string()];

        let policy = companion_copy_policy_from_conversion_options(&options);

        assert!(policy.extensions.is_empty());
        assert!(policy.folders.is_empty());
    }

    #[test]
    fn command_request_patch_paths_assign_companion_policy() {
        let source = include_str!("command.rs");
        let command_path = source
            .find("fn execute_commit_with_source_options_transform(")
            .expect("command commit path should exist");
        let command_body = &source[command_path..];

        let policy_build = command_body
            .find("let companion_policy = companion_copy_policy_from_conversion_options(&options);")
            .expect("commit path must build companion policy from projected options");
        let existing_assignment = command_body
            .find("existing_req.companion = companion_policy.clone();")
            .expect("prebuilt PipelineRequest branch must refresh companion policy");
        let new_request_assignment = command_body
            .find("companion: companion_policy.clone(),")
            .expect("manual PipelineRequest initializer must carry companion policy");

        assert!(policy_build < existing_assignment);
        assert!(policy_build < new_request_assignment);
    }


    #[test]
    fn command_request_builder_uses_canonical_effective_naming_template() {
        let source = include_str!("command.rs");
        let command_path = source
            .find("fn execute_commit_with_source_options_transform(")
            .expect("command commit path should exist");
        let command_body = &source[command_path..];

        let canonical_build = command_body
            .find(r#"let canonical_naming_template = options.effective_naming_template("%NN% - %TITLE%");"#)
            .expect("commit path must build canonical naming template from ConversionOptions");
        let existing_assignment = command_body
            .find("existing_req.naming.template = canonical_naming_template.clone();")
            .expect("prebuilt PipelineRequest branch must refresh canonical naming template");
        let new_request_assignment = command_body
            .find("template: canonical_naming_template.clone(),")
            .expect("manual PipelineRequest initializer must use canonical naming template");

        assert!(canonical_build < existing_assignment);
        assert!(canonical_build < new_request_assignment);
        assert!(
            !command_body.contains(".naming_template\n                                .clone()"),
            "request construction must not bypass ConversionOptions::effective_naming_template"
        );
    }
}

#[cfg(test)]
mod cue_sidecar_override_source_transform_tests {
    use super::*;

    fn source_options_with_transform_applied() -> crate::convert::pipeline::SourceOptions {
        let mut source = crate::convert::pipeline::SourceOptions {
            archive_password: None,
            sacd_area: None,
            dvda_group: None,
            dvda_group_selection: crate::convert::pipeline::DvdaGroupSelection::Default,
            dvda_assume_decrypted: false,
            dvda_downmix_policy: crate::convert::pipeline::DvdaDownmixPolicy::Auto,
            cue_sidecar: crate::convert::pipeline::CueSidecarPolicy::PreferSidecar,
            track_selection: crate::convert::pipeline::TrackSelection::All,
            dvdv_vts: None,
            dvdv_title: None,
            dvdv_audio_stream: None,
            dvdv_angle: None,
            bluray_playlist: None,
            bluray_audio_pid: None,
            bluray_audio_stream: None,
            bluray_angle: None,
        };
        source.cue_sidecar = crate::convert::pipeline::CueSidecarPolicy::PreferEmbedded;
        source
    }

    #[test]
    fn cue_override_is_reapplied_after_source_option_transform() {
        let mut item = crate::convert::ConversionItem::default();
        item.cue_sidecar_override = Some(crate::convert::pipeline::CueSidecarPolicy::EmbeddedOnly);
        let mut source = source_options_with_transform_applied();

        apply_queue_item_cue_sidecar_override_to_source_options(&item, &mut source);

        assert_eq!(
            source.cue_sidecar,
            crate::convert::pipeline::CueSidecarPolicy::EmbeddedOnly
        );
    }
}

#[cfg(test)]
mod dvdv_metadata_editor_sidecar_preload_tests {
    use super::*;

    #[test]
    fn dvdv_editor_preload_matches_tracks_by_source_chapter_before_number() {
        let mut entries = Vec::new();
        let sidecar = DvdVideoMetadataSidecar {
            schema_version: DVDV_METADATA_SIDECAR_SCHEMA_VERSION,
            source: DvdVideoMetadataSource {
                path: PathBuf::from("/tmp/ODD_NUMBERING.ISO.dvdvideo.metadata.toml"),
                sidecar_kind: "dvd_video".to_string(),
                presentation: Some(DvdVideoPresentationIdentity {
                    vts_number: 1,
                    title_number: 1,
                    audio_stream_index: 0,
                    angle_number: None,
                    track_count: Some(2),
                    duration_fingerprint: None,
                }),
                extra: BTreeMap::new(),
            },
            album: BTreeMap::from([("ALBUM".to_string(), "Authored Chapter Fixture".to_string())]),
            tracks: vec![
                DvdVideoMetadataTrack {
                    number: 10,
                    label: "10".to_string(),
                    source_title: Some(1),
                    source_chapter: Some(1),
                    tags: BTreeMap::from([("TITLE".to_string(), "Authored Chapter One".to_string())]),
                    extra: BTreeMap::new(),
                },
                DvdVideoMetadataTrack {
                    number: 20,
                    label: "20".to_string(),
                    source_title: Some(1),
                    source_chapter: Some(2),
                    tags: BTreeMap::from([("TITLE".to_string(), "Authored Chapter Two".to_string())]),
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        };

        dvdv_apply_sidecar_to_editor_fields(&mut entries, 2, Some(&[1, 2]), &sidecar);

        let title = entries
            .iter()
            .find(|entry| entry.display_key == "TITLE")
            .expect("TITLE entry");
        assert_eq!(
            title.per_file_values,
            vec!["Authored Chapter One".to_string(), "Authored Chapter Two".to_string()]
        );
        assert!(title.is_mixed);
        assert_eq!(title.value, "<multiple values>");

        let album = entries
            .iter()
            .find(|entry| entry.display_key == "ALBUM")
            .expect("ALBUM entry");
        assert_eq!(
            album.per_file_values,
            vec!["Authored Chapter Fixture".to_string(), "Authored Chapter Fixture".to_string()]
        );
        assert!(!album.is_mixed);
    }
}

#[cfg(test)]
mod bulk_guard_behavior_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use std::path::PathBuf;

    fn audio_tree(count: usize) -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("album");
        std::fs::create_dir_all(&album).expect("album dir");
        for idx in 0..count {
            std::fs::write(album.join(format!("track-{idx:03}.flac")), b"fixture")
                .expect("audio fixture");
        }
        (temp, album)
    }

    #[test]
    fn bulk_guard_threshold_opens_confirmation_only_above_threshold() {
        let (_small_temp, small_album) = audio_tree(BULK_AUDIO_GUARD_THRESHOLD);
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        assert!(!maybe_confirm_bulk_operation(
            &mut app,
            BulkOperationKind::Analyze,
            BulkGuardCommand::Analyze { force: false },
            &[small_album],
        ));
        assert!(matches!(app.active_overlay, ActiveOverlay::None));

        let (_large_temp, large_album) = audio_tree(BULK_AUDIO_GUARD_THRESHOLD + 1);
        assert!(maybe_confirm_bulk_operation(
            &mut app,
            BulkOperationKind::Analyze,
            BulkGuardCommand::Analyze { force: false },
            &[large_album.clone()],
        ));

        match &app.active_overlay {
            ActiveOverlay::Confirmation {
                message,
                action:
                    ConfirmAction::BulkOperation {
                        operation,
                        command: BulkGuardCommand::Analyze { force: false },
                        paths,
                        count,
                    },
            } => {
                assert_eq!(*operation, BulkOperationKind::Analyze);
                assert_eq!(paths, &vec![large_album]);
                assert_eq!(*count, BULK_AUDIO_GUARD_THRESHOLD + 1);
                assert!(message.contains(&(BULK_AUDIO_GUARD_THRESHOLD + 1).to_string()));
            }
            other => panic!("expected analyze confirmation, got {other:?}"),
        }
    }

    #[test]
    fn frozen_guard_paths_override_later_browse_selection_for_replay() {
        let (_temp, frozen_album) = audio_tree(BULK_AUDIO_GUARD_THRESHOLD + 1);
        let later_path = PathBuf::from("/tmp/later-selection.flac");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.browse.multi_selected = vec![later_path];
        app.bulk_guard_frozen_paths = Some(vec![frozen_album.clone()]);

        assert_eq!(collect_selection_for_file_ops(&app), vec![frozen_album]);
    }

    #[derive(Clone, Copy)]
    enum IncidentEntryKind {
        Directory,
        AudioFile,
    }

    fn incident_entry(path: &PathBuf, kind: IncidentEntryKind) -> crate::tui::browse::BrowseEntry {
        let name = path
            .file_name()
            .expect("fixture path has a filename")
            .to_string_lossy()
            .to_string();
        let entry_kind = match kind {
            IncidentEntryKind::Directory => crate::convert::classify::EntryKind::Directory,
            IncidentEntryKind::AudioFile => crate::convert::classify::EntryKind::AudioFile(
                crate::convert::formats::AudioFormat::Flac,
            ),
        };
        crate::tui::browse::BrowseEntry::new(path.clone(), name, entry_kind, 0, None)
    }

    fn install_browse_listing(
        app: &mut AppState,
        dir: PathBuf,
        paths: &[PathBuf],
        kind: IncidentEntryKind,
    ) {
        app.browse.current_dir = dir;
        app.browse.entries = paths
            .iter()
            .map(|path| incident_entry(path, kind))
            .collect();
        app.browse.selected_index = 0;
        app.browse.visible_height = app.browse.entries.len().max(1);
    }

    fn mark_all_visible(app: &mut AppState) {
        for index in 0..app.browse.entries.len() {
            app.browse
                .toggle_selection_at_index(index)
                .expect("fixture entry should be selectable");
        }
    }

    fn incident_shape_app(
        kind: IncidentEntryKind,
    ) -> (tempfile::TempDir, AppState, Vec<PathBuf>, Vec<PathBuf>) {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir_a = temp.path().join("source-library");
        let dir_b = temp.path().join("converted-output");
        std::fs::create_dir_all(&dir_a).expect("source dir");
        std::fs::create_dir_all(&dir_b).expect("current dir");

        let (paths_a, paths_b) = match kind {
            IncidentEntryKind::Directory => (
                vec![dir_a.join("ccr-source-1"), dir_a.join("ccr-source-2")],
                vec![dir_b.join("ccr-converted-1"), dir_b.join("ccr-converted-2")],
            ),
            IncidentEntryKind::AudioFile => (
                vec![dir_a.join("01 - source.flac"), dir_a.join("02 - source.flac")],
                vec![dir_b.join("01 - converted.flac"), dir_b.join("02 - converted.flac")],
            ),
        };
        for path in paths_a.iter().chain(paths_b.iter()) {
            match kind {
                IncidentEntryKind::Directory => {
                    std::fs::create_dir_all(path).expect("album dir");
                }
                IncidentEntryKind::AudioFile => {
                    std::fs::write(path, b"fixture").expect("audio fixture");
                }
            }
        }

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        install_browse_listing(&mut app, dir_a.clone(), &paths_a, kind);
        mark_all_visible(&mut app);
        assert_eq!(app.browse.multi_selected, paths_a);

        app.browse.navigate_to(dir_b.clone());
        assert!(
            app.browse.multi_selected.is_empty(),
            "directory navigation must clear source-directory marks before the user marks destination entries"
        );
        install_browse_listing(&mut app, dir_b, &paths_b, kind);
        mark_all_visible(&mut app);
        assert_eq!(app.browse.multi_selected, paths_b);

        (temp, app, paths_a, paths_b)
    }

    fn raw_stale_and_current_app() -> (tempfile::TempDir, AppState, Vec<PathBuf>, Vec<PathBuf>) {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir_a = temp.path().join("source-library");
        let dir_b = temp.path().join("converted-output");
        std::fs::create_dir_all(&dir_a).expect("source dir");
        std::fs::create_dir_all(&dir_b).expect("current dir");

        let stale = vec![dir_a.join("ccr-source-1"), dir_a.join("ccr-source-2")];
        let current = vec![dir_b.join("ccr-converted-1"), dir_b.join("ccr-converted-2")];
        for path in stale.iter().chain(current.iter()) {
            std::fs::create_dir_all(path).expect("album dir");
        }

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        install_browse_listing(&mut app, dir_b, &current, IncidentEntryKind::Directory);
        app.browse.multi_selected = vec![
            stale[0].clone(),
            stale[1].clone(),
            current[0].clone(),
            current[1].clone(),
        ];
        app.browse.multi_select_anchor = Some(stale[0].clone());
        (temp, app, stale, current)
    }

    #[test]
    fn context_move_to_incident_shape_captures_only_current_directory_sources() {
        let (_temp, mut app, source_paths, current_paths) =
            incident_shape_app(IncidentEntryKind::Directory);
        let (tx, _rx) = tokio::sync::mpsc::channel(8);

        super::super::context_menu::execute_context_action(
            &mut app,
            super::super::context_menu::ContextAction::MoveTo,
            &tx,
            false,
        );

        match &app.active_overlay {
            ActiveOverlay::FilePicker(state) => match &state.purpose {
                FilePickerPurpose::MoveTo { sources, force } => {
                    assert_eq!(sources, &current_paths);
                    assert!(!force);
                }
                other => panic!("expected MoveTo file picker purpose, got {other:?}"),
            },
            other => panic!("expected destination file picker, got {other:?}"),
        }
        assert!(source_paths.iter().all(|path| path.exists()));
    }

    #[test]
    fn delete_incident_shape_confirms_only_current_directory_sources() {
        let (_temp, mut app, source_paths, current_paths) =
            incident_shape_app(IncidentEntryKind::Directory);
        let (tx, _rx) = tokio::sync::mpsc::channel(8);

        execute_command(&mut app, Command::Delete, &tx);

        match &app.active_overlay {
            ActiveOverlay::Confirmation {
                action: ConfirmAction::DeleteSelection(paths),
                ..
            } => assert_eq!(paths, &current_paths),
            other => panic!("expected delete confirmation, got {other:?}"),
        }
        assert!(source_paths.iter().all(|path| path.exists()));
    }

    #[test]
    fn bulk_rename_incident_shape_opens_with_only_current_directory_audio_sources() {
        let (_temp, mut app, source_paths, current_paths) =
            incident_shape_app(IncidentEntryKind::AudioFile);
        let (tx, _rx) = tokio::sync::mpsc::channel(8);

        execute_command(&mut app, Command::BulkRename, &tx);

        match &app.active_overlay {
            ActiveOverlay::BulkRename(state) => assert_eq!(&state.sources, &current_paths),
            other => panic!("expected bulk rename overlay, got {other:?}"),
        }
        assert!(source_paths.iter().all(|path| path.exists()));
    }

    #[test]
    fn scoped_file_op_selection_prunes_raw_stale_marks_and_reports_count() {
        let (_temp, mut app, stale, current) = raw_stale_and_current_app();

        let selection = collect_selection_for_file_ops_scoped(&mut app);

        assert_eq!(selection.paths, current);
        assert_eq!(selection.dropped_stale_count, stale.len());
        assert_eq!(app.browse.multi_selected, selection.paths);
        assert!(app.browse.multi_select_anchor.is_none());
        surface_stale_selection_notice(&mut app, selection.dropped_stale_count);
        let status = app
            .status_message
            .as_ref()
            .map(|(message, _)| message.as_str())
            .unwrap_or_default();
        assert!(status.contains("Ignored 2 stale selection marks outside the current directory"));
    }

    #[test]
    fn stale_only_marks_are_pruned_before_cursor_fallback_is_decided() {
        let (_temp, mut app, stale, _current) = raw_stale_and_current_app();
        let cursor_path = app.browse.entries[0].path.clone();
        app.browse.multi_selected = stale;
        app.browse.multi_select_anchor = app.browse.multi_selected.first().cloned();
        app.browse.selected_index = 0;

        let selection = collect_selection_for_file_ops_scoped(&mut app);

        assert_eq!(selection.paths, vec![cursor_path]);
        assert_eq!(selection.dropped_stale_count, 2);
        assert!(app.browse.multi_selected.is_empty());
        assert!(app.browse.multi_select_anchor.is_none());
    }

    // Entering the Convert screen kicks off an async source probe
    // (tokio::spawn), so this test needs a runtime.
    #[tokio::test]
    async fn queue_command_preserves_stale_notice_after_final_queue_status() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir_a = temp.path().join("source-library");
        let dir_b = temp.path().join("converted-output");
        std::fs::create_dir_all(&dir_a).expect("source dir");
        std::fs::create_dir_all(&dir_b).expect("current dir");

        let stale = vec![dir_a.join("01 - source.flac"), dir_a.join("02 - source.flac")];
        let current = vec![dir_b.join("01 - converted.flac"), dir_b.join("02 - converted.flac")];
        for path in stale.iter().chain(current.iter()) {
            std::fs::write(path, b"fixture").expect("audio fixture");
        }

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        install_browse_listing(&mut app, dir_b, &current, IncidentEntryKind::AudioFile);
        app.browse.multi_selected = vec![
            stale[0].clone(),
            stale[1].clone(),
            current[0].clone(),
            current[1].clone(),
        ];
        app.browse.multi_select_anchor = Some(stale[0].clone());
        let (tx, _rx) = tokio::sync::mpsc::channel(8);

        execute_command(&mut app, Command::Queue { preset: None }, &tx);

        assert_eq!(app.current_screen, AppScreen::Convert);
        assert_eq!(app.convert.source.mode.all_paths(), current);
        assert_eq!(app.browse.multi_selected, current);
        let status = app
            .status_message
            .as_ref()
            .map(|(message, _)| message.as_str())
            .unwrap_or_default();
        // Same invariant as the async review test: notice prefix + a real
        // completion status, without pinning the probe-dependent wording.
        assert!(status.starts_with("Ignored 2 stale selection marks outside the current directory; "));
        assert!(status.contains("review settings, then :commit or :Commit"));
    }
}


#[cfg(test)]
mod conversion_action_command_parser_tests {
    use super::*;

    #[test]
    fn identity_import_command_preserves_optional_reviewed_source_path() {
        // parse_command receives the input WITHOUT the leading ':'.
        assert!(matches!(
            parse_command("actions-identity-import"),
            Command::ActionsIdentityImport(None)
        ));
        assert!(matches!(
            parse_command("actions-identity-import reviewed identity.json"),
            Command::ActionsIdentityImport(Some(path)) if path == "reviewed identity.json"
        ));
    }
}

#[cfg(test)]
mod split_cue_source_coverage_tests {
    use super::*;

    fn fixture_tool_available(tool: &str) -> bool {
        std::process::Command::new(tool)
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[test]
    fn file_surface_editor_without_presentation_tabs_discovers_split_cue_source_folder() {
        if !fixture_tool_available("ffmpeg") {
            eprintln!("skipping: ffmpeg unavailable");
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("album");
        std::fs::create_dir_all(&album).expect("album dir");
        for stem in ["side_a", "side_b"] {
            let image = album.join(format!("{stem}.flac"));
            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-y",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:sample_rate=44100:duration=2",
                    "-c:a",
                    "flac",
                ])
                .arg(&image)
                .stdin(std::process::Stdio::null())
                .status()
                .expect("ffmpeg fixture");
            assert!(status.success());
            std::fs::write(
                album.join(format!("{stem}.cue")),
                format!(
                    "PERFORMER \"Artist\"\nTITLE \"Album ({stem})\"\nFILE \"{stem}.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:01:00\n"
                ),
            )
            .expect("cue fixture");
        }
        let state = crate::tui::app::MetadataEditorState::for_files(
            vec![album.join("side_a.flac")],
            Vec::new(),
            vec!["01".to_string(), "02".to_string()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        );
        assert!(
            state.presentation_tabs.is_empty(),
            "ordinary file editors are file_surface-backed; they do not create a presentation tab"
        );

        let request = in_editor_split_cue_mb_request_from_single_editor_source_folder(&state)
            .expect("file-surface editor should snapshot its source folder for async split-CUE discovery");
        assert_eq!(request.active_audio_path.as_deref(), Some(album.join("side_a.flac").as_path()));
        assert_eq!(request.sources, vec![album.clone()]);
        assert!(request.audio_paths.is_empty());
        let infos = collect_single_image_cue_infos_for_sources(&request.sources, &request.audio_paths);
        assert_eq!(infos.len(), 2);
        assert!(infos.iter().any(|info| info.audio_path == album.join("side_b.flac")));
    }

    #[test]
    fn common_cue_album_title_strips_side_designators() {
        let titles = vec![
            "The Dark Side Of The Moon (Side A)".to_string(),
            "The Dark Side Of The Moon (Side B)".to_string(),
        ];
        assert_eq!(
            super::common_cue_album_title(&titles).as_deref(),
            Some("The Dark Side Of The Moon")
        );
    }

    #[test]
    fn common_cue_album_title_keeps_single_or_unrelated_titles() {
        assert_eq!(
            super::common_cue_album_title(&["Chronicle".to_string()]).as_deref(),
            Some("Chronicle")
        );
        assert_eq!(
            super::common_cue_album_title(&[
                "Chronicle".to_string(),
                "Willy and the Poor Boys".to_string(),
            ])
            .as_deref(),
            Some("Chronicle"),
            "unrelated part titles fall back to the first title"
        );
    }



    fn split_cue_grouping_fixture(titles: &[Option<&str>]) -> Vec<crate::tui::cue_parser::SingleImageInfo> {
        titles
            .iter()
            .enumerate()
            .map(|(idx, title)| {
                let stem = format!("part_{}", idx + 1);
                crate::tui::cue_parser::SingleImageInfo {
                    audio_path: std::path::PathBuf::from(format!("/tmp/album/{stem}.wv")),
                    cue_path: std::path::PathBuf::from(format!("/tmp/album/{stem}.cue")),
                    sheet: crate::tui::cue_parser::CueSheet {
                        title: title.map(str::to_string),
                        performer: Some("Artist".to_string()),
                        date: None,
                        genre: None,
                        catalog: None,
                        tracks: vec![
                            crate::tui::cue_parser::CueTrack {
                                number: 1,
                                title: Some(format!("{stem} track 1")),
                                performer: None,
                                file: Some(format!("{stem}.wv")),
                                index01_frames: Some(0),
                                index00_frames: None,
                                isrc: None,
                                directives: Vec::new(),
                            },
                            crate::tui::cue_parser::CueTrack {
                                number: 2,
                                title: Some(format!("{stem} track 2")),
                                performer: None,
                                file: Some(format!("{stem}.wv")),
                                index01_frames: Some(4500),
                                index00_frames: None,
                                isrc: None,
                                directives: Vec::new(),
                            },
                        ],
                    },
                    sample_rate: 44_100,
                    // Vary durations per part: identical boundaries would make
                    // the per-cue TOC strings collide (one cache entry, one
                    // "distinct" release), which no real split-side rip does.
                    total_samples: 44_100 * (120 + idx as u64 * 20),
                    track_boundaries: vec![
                        (0, 44_100 * (60 + idx as u64 * 10)),
                        (44_100 * (60 + idx as u64 * 10), 44_100 * (60 + idx as u64 * 10)),
                    ],
                }
            })
            .collect()
    }

    fn mb_cached_body(release_id: &str, title: &str, tracks: usize) -> String {
        let tracks_json = (1..=tracks)
            .map(|idx| format!(r#"{{"position":{idx},"title":"Track {idx}"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"releases":[{{"id":"{release_id}","title":"{title}","artist-credit":[{{"name":"Artist"}}],"media":[{{"track-count":{tracks},"tracks":[{tracks_json}]}}]}}]}}"#
        )
    }

    #[test]
    fn split_cue_grouping_title_rung_merges_shared_prefix() {
        let infos = split_cue_grouping_fixture(&[
            Some("The Dark Side Of The Moon (Side A)"),
            Some("The Dark Side Of The Moon (Side B)"),
        ]);
        let decision = split_cue_title_rung_decision(&infos).expect("title rung decision");
        assert_eq!(decision.reason, SplitCueAlbumGroupingReason::TitleSharedPrefix);
        assert_eq!(decision.groups.len(), 1);
        assert_eq!(decision.groups[0].len(), 2);
    }

    #[test]
    fn split_cue_grouping_title_rung_defers_unrelated_titles_to_toc_rungs() {
        let infos = split_cue_grouping_fixture(&[
            Some("Chronicle"),
            Some("Willy And The Poor Boys"),
        ]);
        assert!(
            split_cue_title_rung_decision(&infos).is_none(),
            "unrelated cue titles are not decisive split evidence; the ladder must continue to concat-TOC, per-CUE TOC, then conservative merge"
        );
    }

    #[test]
    fn split_cue_grouping_title_rung_defers_blank_titles_to_cached_toc_rungs() {
        let infos = split_cue_grouping_fixture(&[None, None]);
        assert!(
            split_cue_title_rung_decision(&infos).is_none(),
            "missing titles are not negative evidence; they must continue to concat/per-CUE TOC rungs"
        );
    }

    #[test]
    fn split_cue_grouping_decision_is_reused_for_same_session_metadata_grouping() {
        let infos = split_cue_grouping_fixture(&[
            Some("Chronicle"),
            Some("Willy And The Poor Boys"),
        ]);
        let key = split_cue_album_grouping_key(&infos);
        let active_audio = infos[1].audio_path.clone();
        let mut app = crate::tui::app::AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.split_cue_album_grouping_cache.insert(
            key.clone(),
            split_cue_album_grouping_decision_split_each(
                &infos,
                SplitCueAlbumGroupingReason::PerCueDistinctTocHits,
            ),
        );

        let reused_group = split_cue_infos_for_cached_or_title_grouping(
            &mut app,
            infos.clone(),
            Some(active_audio.as_path()),
        );
        assert_eq!(
            reused_group.len(),
            1,
            "the session cache, not a fresh title re-evaluation, should drive later same-folder grouping"
        );
        assert_eq!(reused_group[0].audio_path, active_audio);
        assert_eq!(
            app.split_cue_album_grouping_cache.get(&key).map(|decision| decision.reason),
            Some(SplitCueAlbumGroupingReason::PerCueDistinctTocHits),
            "cached TOC-rung decisions must be reused by later editor/dispatch flows"
        );
    }

    #[test]
    fn conversion_grouping_snapshot_is_reducer_safe_and_worker_augments_evidence() {
        let source = include_str!("command.rs");
        let snapshot_start = source
            .find("pub(crate) fn resolved_split_cue_album_grouping_decisions_for_conversion(")
            .expect("snapshot helper should exist");
        // The worker fn sits BETWEEN the snapshot helper and the insert
        // helper in this file; the snapshot span must end where the worker
        // begins or it would (wrongly) contain the worker's discovery calls.
        let snapshot_end = source[snapshot_start..]
            .find("fn augment_split_cue_album_grouping_decisions_for_conversion_blocking(")
            .map(|offset| snapshot_start + offset)
            .expect("worker helper follows snapshot helper");
        let snapshot_body = &source[snapshot_start..snapshot_end];
        assert!(
            snapshot_body.contains("split_cue_album_grouping_cache.values()"),
            "the reducer may snapshot already-resolved decisions"
        );
        assert!(
            !snapshot_body.contains("collect_single_image_cue_infos_for_sources"),
            "the reducer snapshot must not discover/parse/probe CUE groups"
        );
        assert!(
            !snapshot_body.contains("insert_cached_split_cue_grouping_decisions_for_infos"),
            "cache-evidence construction must not run on the reducer path"
        );

        let worker_start = source
            .find("fn augment_split_cue_album_grouping_decisions_for_conversion_blocking(")
            .expect("blocking augmentation helper should exist");
        let worker_end = source[worker_start..]
            .find("fn insert_queue_grouping_decision(")
            .map(|offset| worker_start + offset)
            .expect("insert helper follows worker augmentation helper");
        let worker_body = &source[worker_start..worker_end];
        assert!(
            worker_body.contains("collect_single_image_cue_infos_for_sources"),
            "filesystem discovery and media probing must happen in the blocking worker"
        );
        assert!(
            worker_body.contains("insert_cached_split_cue_grouping_decisions_for_infos_with_cache"),
            "TOC cache evidence must be constructed after worker-side probing"
        );
    }

    #[test]
    fn cached_split_cue_grouping_decision_is_snapshotted_for_conversion_expansion() {
        let infos = split_cue_grouping_fixture(&[
            Some("Chronicle"),
            Some("Willy And The Poor Boys"),
        ]);
        let mut app = crate::tui::app::AppState::new_for_test(crate::config::TonepoetConfig::default());
        let decision = split_cue_album_grouping_decision_split_each(
            &infos,
            SplitCueAlbumGroupingReason::PerCueDistinctTocHits,
        );
        app.split_cue_album_grouping_cache
            .insert(split_cue_album_grouping_key(&infos), decision.clone());

        let snapshot = resolved_split_cue_album_grouping_decisions_for_conversion(
            &app,
            &[PathBuf::from("/tmp/album")],
        );
        let cue_paths: Vec<PathBuf> = infos.iter().map(|info| info.cue_path.clone()).collect();
        let queue_key = split_cue_album_grouping_key_for_queue(&cue_paths);
        assert_eq!(
            snapshot.get(&queue_key).map(|decision| decision.reason),
            Some(SplitCueAlbumGroupingReason::PerCueDistinctTocHits),
            "Browse conversion must carry the same cached decisive split that metadata/GNUDB already resolved"
        );
        assert_eq!(snapshot.get(&queue_key), Some(&decision));
    }


    fn store_cached_mb_body_for_sectors(
        app: &mut crate::tui::app::AppState,
        sectors: &[u32],
        body: &str,
    ) {
        let toc = crate::tui::musicbrainz::build_mb_toc(sectors).expect("toc string");
        app.db.store_mb_response(&toc, body).expect("store cached mb body");
    }

    #[test]
    fn cached_full_ladder_concat_hit_feeds_later_grouping_without_precomputed_decision() {
        let infos = split_cue_grouping_fixture(&[
            Some("Chronicle"),
            Some("Willy And The Poor Boys"),
        ]);
        let mut app = crate::tui::app::AppState::new_for_test(crate::config::TonepoetConfig::default());
        let concat = concat_single_image_cue_infos_to_cd_sectors(&infos).expect("concat toc");
        store_cached_mb_body_for_sectors(
            &mut app,
            &concat,
            &mb_cached_body("merged-release", "Merged", 4),
        );

        let decision = cached_or_title_split_cue_album_grouping_decision(&mut app, &infos)
            .expect("cached concat-TOC rung should decide");
        assert_eq!(decision.reason, SplitCueAlbumGroupingReason::ConcatTocHit);
        assert_eq!(decision.groups.len(), 1);
        assert_eq!(
            app.split_cue_album_grouping_cache
                .get(&split_cue_album_grouping_key(&infos))
                .map(|decision| decision.reason),
            Some(SplitCueAlbumGroupingReason::ConcatTocHit),
            "metadata-editor and GNUDB call sites must not require another flow to precompute the session decision"
        );
    }

    #[test]
    fn cached_full_ladder_per_cue_distinct_hits_feed_active_group_filtering() {
        let infos = split_cue_grouping_fixture(&[
            Some("Chronicle"),
            Some("Willy And The Poor Boys"),
        ]);
        let mut app = crate::tui::app::AppState::new_for_test(crate::config::TonepoetConfig::default());
        let concat = concat_single_image_cue_infos_to_cd_sectors(&infos).expect("concat toc");
        store_cached_mb_body_for_sectors(&mut app, &concat, r#"{"releases":[]}"#);
        for (idx, info) in infos.iter().enumerate() {
            let sectors = single_image_cue_info_to_cd_sectors(info).expect("per cue toc");
            store_cached_mb_body_for_sectors(
                &mut app,
                &sectors,
                &mb_cached_body(&format!("release-{}", idx + 1), "Separate", 2),
            );
        }
        let active_audio = infos[1].audio_path.clone();

        let active_group = split_cue_infos_for_cached_or_title_grouping(
            &mut app,
            infos.clone(),
            Some(active_audio.as_path()),
        );
        assert_eq!(active_group.len(), 1);
        assert_eq!(active_group[0].audio_path, active_audio);
        assert_eq!(
            app.split_cue_album_grouping_cache
                .get(&split_cue_album_grouping_key(&infos))
                .map(|decision| decision.reason),
            Some(SplitCueAlbumGroupingReason::PerCueDistinctTocHits),
            "per-CUE decisive split must be available to metadata/GNUDB grouping from cached MB bodies, not only after :tags-mb computed it"
        );
    }

    #[test]
    fn cached_per_cue_distinct_release_ids_drive_metadata_and_conversion_split_identically() {
        let infos = split_cue_grouping_fixture(&[
            Some("Chronicle"),
            Some("Willy And The Poor Boys"),
        ]);
        let mut app = crate::tui::app::AppState::new_for_test(crate::config::TonepoetConfig::default());
        let concat = concat_single_image_cue_infos_to_cd_sectors(&infos).expect("concat toc");
        store_cached_mb_body_for_sectors(&mut app, &concat, r#"{"releases":[]}"#);
        for (idx, info) in infos.iter().enumerate() {
            let sectors = single_image_cue_info_to_cd_sectors(info).expect("per cue toc");
            store_cached_mb_body_for_sectors(
                &mut app,
                &sectors,
                &mb_cached_body(&format!("distinct-release-{}", idx + 1), "Separate", 2),
            );
        }

        let metadata_decision = cached_or_title_split_cue_album_grouping_decision(&mut app, &infos)
            .expect("metadata ladder uses cached per-cue release ids");
        assert_eq!(
            metadata_decision.reason,
            SplitCueAlbumGroupingReason::PerCueDistinctTocHits,
        );

        let mut conversion_decisions = QueueSplitCueAlbumGroupingDecisions::new();
        insert_cached_split_cue_grouping_decisions_for_infos(&app, &infos, &mut conversion_decisions);
        let queue_key = split_cue_album_grouping_key_for_queue(
            &infos.iter().map(|info| info.cue_path.clone()).collect::<Vec<_>>(),
        );
        let conversion_decision = conversion_decisions
            .get(&queue_key)
            .expect("conversion snapshot carries equivalent cached TOC evidence");
        assert_eq!(conversion_decision.reason, metadata_decision.reason);
        assert_eq!(conversion_decision.groups, metadata_decision.groups);
    }


    #[tokio::test]
    async fn split_cue_grouping_unrelated_titles_still_reach_concat_toc_hit() {
        let infos = split_cue_grouping_fixture(&[
            Some("Chronicle"),
            Some("Willy And The Poor Boys"),
        ]);
        assert!(
            split_cue_title_rung_decision(&infos).is_none(),
            "the title rung must not split unrelated same-folder cues before the TOC rungs run"
        );
        let concat = concat_single_image_cue_infos_to_cd_sectors(&infos).expect("concat toc");
        let candidates = crate::tui::musicbrainz::toc_candidates_from_sectors(&concat);
        let cached = candidates
            .iter()
            .map(|_| Some(mb_cached_body("merged-release", "Merged", 4)))
            .collect();
        let outcome = compute_split_cue_album_grouping_ladder(infos, candidates, cached, Vec::new())
            .await
            .expect("grouping ladder");
        assert_eq!(outcome.decision.reason, SplitCueAlbumGroupingReason::ConcatTocHit);
        assert_eq!(outcome.decision.groups.len(), 1);
    }

    #[tokio::test]
    async fn split_cue_grouping_concat_toc_hit_is_decisive_merge() {
        let infos = split_cue_grouping_fixture(&[None, None]);
        let concat = concat_single_image_cue_infos_to_cd_sectors(&infos).expect("concat toc");
        let candidates = crate::tui::musicbrainz::toc_candidates_from_sectors(&concat);
        let cached = candidates.iter().map(|_| Some(mb_cached_body("merged-release", "Merged", 4))).collect();
        let per_cue = Vec::new();

        let outcome = compute_split_cue_album_grouping_ladder(infos, candidates, cached, per_cue)
            .await
            .expect("grouping ladder");
        assert_eq!(outcome.decision.reason, SplitCueAlbumGroupingReason::ConcatTocHit);
        assert_eq!(outcome.decision.groups.len(), 1);
        assert!(outcome.toc_outcome.expect("toc outcome").releases.len() == 1);
        assert!(
            outcome.cache_writes.is_empty(),
            "cached concat responses must not produce network-derived cache writes in regression tests"
        );
    }

    #[tokio::test]
    async fn split_cue_grouping_concat_toc_hit_wins_before_contradictory_per_cue_hits() {
        let infos = split_cue_grouping_fixture(&[None, None]);
        let concat = concat_single_image_cue_infos_to_cd_sectors(&infos).expect("concat toc");
        let candidates = crate::tui::musicbrainz::toc_candidates_from_sectors(&concat);
        let cached = candidates.iter().map(|_| Some(mb_cached_body("merged-release", "Merged", 4))).collect();
        let per_cue = infos
            .iter()
            .enumerate()
            .map(|(idx, info)| {
                (
                    single_image_cue_info_to_cd_sectors(info).expect("per cue toc"),
                    Some(mb_cached_body(&format!("separate-release-{idx}"), "Separate", 2)),
                )
            })
            .collect();

        let outcome = compute_split_cue_album_grouping_ladder(infos, candidates, cached, per_cue)
            .await
            .expect("grouping ladder");
        assert_eq!(
            outcome.decision.reason,
            SplitCueAlbumGroupingReason::ConcatTocHit,
            "the decisive-positive concat rung must short-circuit later per-CUE split evidence"
        );
        assert_eq!(outcome.decision.groups.len(), 1);
    }

    #[tokio::test]
    async fn split_cue_grouping_per_cue_distinct_hits_are_decisive_split() {
        let infos = split_cue_grouping_fixture(&[None, None]);
        let concat = concat_single_image_cue_infos_to_cd_sectors(&infos).expect("concat toc");
        let candidates = crate::tui::musicbrainz::toc_candidates_from_sectors(&concat);
        let cached = candidates.iter().map(|_| Some(r#"{"releases":[]}"#.to_string())).collect();
        let per_cue = infos
            .iter()
            .enumerate()
            .map(|(idx, info)| {
                (
                    single_image_cue_info_to_cd_sectors(info).expect("per cue toc"),
                    Some(mb_cached_body(&format!("release-{idx}"), "Part", 2)),
                )
            })
            .collect();

        let outcome = compute_split_cue_album_grouping_ladder(infos, candidates, cached, per_cue)
            .await
            .expect("grouping ladder");
        assert_eq!(outcome.decision.reason, SplitCueAlbumGroupingReason::PerCueDistinctTocHits);
        assert_eq!(outcome.decision.groups.len(), 2);
        assert!(outcome.toc_outcome.is_none());
        assert!(
            outcome.cache_writes.is_empty(),
            "per-CUE decisive separation test must be driven entirely by cached/mocked MB bodies"
        );
    }

    #[tokio::test]
    async fn split_cue_grouping_per_cue_same_release_hits_are_not_separation_evidence() {
        let infos = split_cue_grouping_fixture(&[None, None]);
        let concat = concat_single_image_cue_infos_to_cd_sectors(&infos).expect("concat toc");
        let candidates = crate::tui::musicbrainz::toc_candidates_from_sectors(&concat);
        let cached = candidates.iter().map(|_| Some(r#"{"releases":[]}"#.to_string())).collect();
        let per_cue = infos
            .iter()
            .map(|info| {
                (
                    single_image_cue_info_to_cd_sectors(info).expect("per cue toc"),
                    Some(mb_cached_body("same-release", "Same", 2)),
                )
            })
            .collect();

        let outcome = compute_split_cue_album_grouping_ladder(infos, candidates, cached, per_cue)
            .await
            .expect("grouping ladder");
        assert_eq!(
            outcome.decision.reason,
            SplitCueAlbumGroupingReason::AmbiguousMerge,
            "per-CUE hits only prove separateness when every part resolves to a different release"
        );
        assert_eq!(outcome.decision.groups.len(), 1);
        assert!(outcome.toc_outcome.is_none());
    }

    #[tokio::test]
    async fn split_cue_grouping_misses_conservatively_merge() {
        let infos = split_cue_grouping_fixture(&[None, None]);
        let concat = concat_single_image_cue_infos_to_cd_sectors(&infos).expect("concat toc");
        let candidates = crate::tui::musicbrainz::toc_candidates_from_sectors(&concat);
        let cached = candidates.iter().map(|_| Some(r#"{"releases":[]}"#.to_string())).collect();
        let per_cue = infos
            .iter()
            .map(|info| {
                (
                    single_image_cue_info_to_cd_sectors(info).expect("per cue toc"),
                    Some(r#"{"releases":[]}"#.to_string()),
                )
            })
            .collect();

        let outcome = compute_split_cue_album_grouping_ladder(infos, candidates, cached, per_cue)
            .await
            .expect("grouping ladder");
        assert_eq!(outcome.decision.reason, SplitCueAlbumGroupingReason::AmbiguousMerge);
        assert_eq!(outcome.decision.groups.len(), 1);
        assert!(outcome.toc_outcome.is_none());
    }
    #[test]
    fn split_cue_info_collection_keeps_ogg_and_tta_cues_before_plain_audio_grouping() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("split-side-album");
        std::fs::create_dir_all(&album).expect("album dir");

        // SingleImageInfo detection probes the image for sample counts, so
        // the fixtures must be REAL decodable audio; the dev/test shell
        // provides ffmpeg, which encodes both Vorbis and TTA. The second
        // track's INDEX must fall inside the 2-second image.
        for (stem, ext, title) in [
            ("side_a", "ogg", "Side A"),
            ("side_b", "tta", "Side B"),
        ] {
            let audio = album.join(format!("{stem}.{ext}"));
            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
                    "-ar", "44100",
                ])
                .arg(&audio)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("spawn ffmpeg for audio fixture");
            assert!(status.success(), "ffmpeg fixture encode failed for {ext}");
            std::fs::write(
                album.join(format!("{stem}.cue")),
                format!(
                    "PERFORMER \"Artist\"\nTITLE \"Album\"\nFILE \"{stem}.{ext}\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"{title} One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"{title} Two\"\n    INDEX 01 00:01:00\n"
                ),
            )
            .expect("cue fixture");
        }

        let audio_paths = crate::convert::queue_expansion::expand_paths_to_all_audio(&[album.clone()]);
        assert_eq!(audio_paths.len(), 2, "OGG and TTA must both be audio sources");

        let cue_infos = collect_single_image_cue_infos_for_sources(&[album.clone()], &audio_paths);

        assert_eq!(cue_infos.len(), 2, "split-side Browse tagging must stay CUE-shaped");
        assert_eq!(cue_infos[0].cue_path, album.join("side_a.cue"));
        assert_eq!(cue_infos[0].audio_path, album.join("side_a.ogg"));
        assert_eq!(cue_infos[0].sheet.tracks.len(), 2);
        assert_eq!(cue_infos[1].cue_path, album.join("side_b.cue"));
        assert_eq!(cue_infos[1].audio_path, album.join("side_b.tta"));
        assert_eq!(cue_infos[1].sheet.tracks.len(), 2);
    }
}

#[cfg(test)]
mod in_editor_tags_mb_reducer_safety_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::tui::app::{ActiveOverlay, AppState, MetadataEditorState, MetadataTechnicalDetails};
    use tokio::sync::mpsc;

    fn tx() -> mpsc::Sender<AppMessage> {
        let (tx, _rx) = mpsc::channel(4);
        tx
    }

    fn editor_for_path(path: std::path::PathBuf) -> Box<MetadataEditorState> {
        Box::new(MetadataEditorState::for_files(
            vec![path],
            Vec::new(),
            vec!["track.flac".to_string()],
            MetadataTechnicalDetails::default(),
        ))
    }

    fn session_guard_for(
        state: &MetadataEditorState,
    ) -> super::super::message::MetadataEditorSessionGuard {
        let details = &state.active_surface().technical_details;
        super::super::message::MetadataEditorSessionGuard {
            session_id: details.session_id,
            save_generation: details.save_generation,
            editor_generation: state.model.editor_save_generation,
        }
    }

    #[test]
    fn stale_split_cue_discovery_completion_after_reopen_is_ignored() {
        let path = std::path::PathBuf::from("/tmp/reopened-album/track.flac");
        let old_editor = editor_for_path(path.clone());
        let old_guard = session_guard_for(&old_editor);
        let reopened_editor = editor_for_path(path.clone());
        let reopened_guard = session_guard_for(&reopened_editor);
        assert_ne!(old_guard.session_id, reopened_guard.session_id);

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::MetadataEditor(reopened_editor);
        let operation_id = super::super::event_loop::begin_tags_mb_prelookup_operation(
            &mut app,
            true,
            crate::tui::app::TagsMbOperationPhase::Discovery,
        )
        .expect("operation");
        let request = InEditorSplitCueMusicBrainzInfoRequest {
            operation_id,
            source: InEditorSplitCueMusicBrainzSource::UnifiedAlbum,
            sources: vec![path.clone()],
            audio_paths: vec![path.clone()],
            active_audio_path: Some(path),
            editor_session: old_guard,
        };

        handle_in_editor_split_cue_musicbrainz_info_complete(
            &mut app,
            &tx(),
            request,
            Ok(Vec::new()),
        );

        match &app.active_overlay {
            ActiveOverlay::MetadataEditor(state) => assert_eq!(
                session_guard_for(state).session_id,
                reopened_guard.session_id,
                "stale discovery completion must preserve the current reopened editor"
            ),
            other => panic!("expected reopened editor to remain active, got {:?}", other),
        }
        assert!(app.pending_metadata_editor.is_none());
        let status = app.status_message.as_ref().map(|(msg, _)| msg.as_str());
        assert!(
            status
                .unwrap_or_default()
                .contains("metadata editor changed during split-CUE discovery"),
            "stale completion should report the session change, got {:?}",
            status
        );
    }

    #[test]
    fn in_editor_tags_mb_split_cue_discovery_and_dispatch_are_session_guarded() {
        let source = include_str!("command.rs");
        let request_start = source
            .find("pub struct InEditorSplitCueMusicBrainzInfoRequest")
            .expect("in-editor split-CUE MB request should exist");
        let request_end = source[request_start..]
            .find("pub(crate) fn split_cue_album_grouping_key_from_paths")
            .map(|offset| request_start + offset)
            .expect("request struct should precede grouping helpers");
        let request_body = &source[request_start..request_end];
        assert!(
            request_body.contains("editor_session: super::message::MetadataEditorSessionGuard"),
            "split-CUE discovery requests must carry the originating editor session"
        );

        let completion_start = source
            .find("pub(super) fn handle_in_editor_split_cue_musicbrainz_info_complete(")
            .expect("split-CUE MB info completion handler should exist");
        let completion_end = source[completion_start..]
            .find("/// Dispatch `:tags-mb` when a metadata editor is open")
            .map(|offset| completion_start + offset)
            .expect("completion handler should precede :tags-mb dispatcher");
        let completion_body = &source[completion_start..completion_end];
        assert!(
            completion_body.contains("super::event_loop::metadata_editor_matches_session_guard(")
                && completion_body.contains("&taken.state")
                && completion_body.contains("request.editor_session"),
            "stale split-CUE discovery completions must be rejected through the editor-level shared session guard"
        );
        assert!(
            completion_body.contains("super::event_loop::take_metadata_editor_with_restore_slot(app)"),
            "split-CUE discovery completions must remember whether the editor came from active or pending"
        );
        assert!(
            completion_body.contains("super::event_loop::restore_taken_metadata_editor(app, taken)"),
            "split-CUE discovery rejection paths must restore without overwriting a newer active overlay"
        );
        assert!(
            !completion_body.contains("ActiveOverlay::MetadataEditor(state_owned)"),
            "split-CUE discovery completion must not directly overwrite active_overlay on rejection"
        );
        assert!(
            completion_body.contains("Some(request.editor_session)"),
            "accepted split-CUE discovery must propagate the session guard into TOC/text dispatch"
        );
    }

    #[test]
    fn in_editor_tags_mb_split_cue_discovery_is_worker_only() {
        let source = include_str!("command.rs");
        let dispatch_start = source
            .find("fn try_dispatch_in_editor_tags_mb(")
            .expect("in-editor :tags-mb dispatcher should exist");
        let dispatch_end = source[dispatch_start..]
            .find("/// Public entry point for metadata editing from context_menu.rs.")
            .map(|offset| dispatch_start + offset)
            .expect("metadata entry point should follow the dispatcher");
        let dispatch_body = &source[dispatch_start..dispatch_end];

        assert!(
            dispatch_body.contains("in_editor_split_cue_mb_request_from_unified_album"),
            "unified-album :tags-mb must route through the async split-CUE discovery request"
        );
        assert!(
            dispatch_body.contains("in_editor_split_cue_mb_request_from_metadata_tabs"),
            "legacy tabbed split-CUE :tags-mb must route through the async split-CUE discovery request"
        );
        assert!(
            dispatch_body.contains("in_editor_split_cue_mb_request_from_single_editor_source_folder"),
            "single-editor source-folder :tags-mb must route through the async split-CUE discovery request"
        );
        assert!(
            !dispatch_body.contains("collect_single_image_cue_infos_for_sources("),
            "the in-editor :tags-mb reducer path must not discover or parse CUE files"
        );

        let worker_start = source
            .find("fn spawn_in_editor_split_cue_musicbrainz_info_collection(")
            .expect("blocking split-CUE discovery spawn helper should exist");
        let worker_end = source[worker_start..]
            .find("pub(super) fn handle_in_editor_split_cue_musicbrainz_info_complete(")
            .map(|offset| worker_start + offset)
            .expect("completion handler should follow the spawn helper");
        let worker_body = &source[worker_start..worker_end];
        assert!(
            worker_body.contains("task::spawn_blocking"),
            "split-CUE discovery must run on a blocking worker"
        );
        assert!(
            worker_body.contains("collect_single_image_cue_infos_for_sources(&sources, &audio_paths)"),
            "the blocking worker should own the CUE discovery/parsing call"
        );
    }

    #[test]
    fn second_tags_mb_dispatch_keeps_picker_and_parked_editor_latched() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let path = std::path::PathBuf::from("/tmp/album/track.flac");
        let mut editor = editor_for_path(path.clone());
        editor.model.tags_mb_in_flight = true;
        app.pending_metadata_editor = Some(editor);
        app.active_overlay = ActiveOverlay::MbSelect(Box::new(
            crate::tui::app::MbSelectState::new(
                vec![crate::tui::musicbrainz::MbRelease {
                    release_id: "release-1".to_string(),
                    title: "Release".to_string(),
                    ..Default::default()
                }],
                vec![path],
            ),
        ));

        assert_eq!(try_dispatch_in_editor_tags_mb(&mut app, &tx(), None), Some(true));
        assert!(
            matches!(app.active_overlay, ActiveOverlay::MbSelect(_)),
            "refusing a duplicate dispatch must not replace the active picker"
        );
        assert!(
            app.pending_metadata_editor
                .as_ref()
                .is_some_and(|state| state.model.tags_mb_in_flight),
            "the parked editor must remain latched until picker accept or cancel"
        );
        assert!(app
            .status_message
            .as_ref()
            .is_some_and(|(message, _)| message.contains("already in flight")));
    }

}

#[cfg(test)]
mod convert_commit_synthetic_artifact_lifecycle_tests {
    #[test]
    fn convert_commit_transaction_replaces_ownership_inspection_uncertainty_path() {
        let source = include_str!("command.rs");
        let commit_start = source
            .find("fn execute_commit_with_source_options_transform(")
            .expect("convert commit function should exist");
        let commit_end = source[commit_start..]
            .find("/// Execute a `:go` / `:start` command")
            .map(|offset| commit_start + offset)
            .expect("start command should follow convert commit");
        let commit_body = &source[commit_start..commit_end];

        assert!(
            commit_body.contains("commit_batch_with_cue_artifacts"),
            "commit must receive an authoritative queue/artifact transaction"
        );
        assert!(
            !commit_body.contains("quarantine_synthetic_cue_artifacts_without_cleanup"),
            "commit must not use ambiguous post-admission quarantine as normal control flow"
        );
        assert!(
            !commit_body.contains("release_synthetic_cue_artifacts_without_cleanup"),
            "commit must not detach artifacts into ownerless limbo after admission"
        );
        assert!(
            !commit_body.contains("synthetic_cue_artifact_paths_owned_by_manager"),
            "commit must not inspect manager ownership by path after queue side effects"
        );
    }
}
