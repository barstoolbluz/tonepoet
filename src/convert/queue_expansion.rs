//! Conversion-domain queue expansion and CUE artifact policy.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use once_cell::sync::OnceCell;

use crate::convert::classify::{classify_file, is_audio_file_path, is_cue_sheet_path, EntryKind};
use crate::convert::source_admission::is_direct_queue_source_path;
use crate::convert::pipeline::CueSidecarPolicy;
use crate::convert::split_cue_album::{
    common_cue_album_title, decide_with_toc_evidence, grouping_key_from_paths,
    SplitCueAlbumGroupingDecision, SplitCueAlbumGroupingReason,
};


/// Resolved split-CUE album grouping decisions supplied by a higher layer that
/// already ran the authoritative title/TOC ladder. Keys are order-independent,
/// canonicalized CUE member sets from `split_cue_album::grouping_key_from_paths`;
/// values are the exact decision conversion must honor.
pub type QueueSplitCueAlbumGroupingDecisions =
    BTreeMap<Vec<PathBuf>, SplitCueAlbumGroupingDecision>;

#[must_use]
pub fn split_cue_album_grouping_key_for_queue(paths: &[PathBuf]) -> Vec<PathBuf> {
    grouping_key_from_paths(paths)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueExpansionResult {
    /// Paths to queue for conversion, in deterministic browse order.
    pub paths: Vec<PathBuf>,
    /// Audio paths whose sibling sidecar CUE was already classified as a
    /// metadata artifact during queue expansion. Downstream conversion must
    /// skip sidecar CUE discovery for these paths while still honoring
    /// embedded CUESHEET tags.
    pub cue_artifact_audio: HashSet<PathBuf>,
    /// Synthetic CUE files created for merged split-CUE albums. These are
    /// transient queue inputs and must be owned by the Convert source state
    /// until commit, then by the conversion manager until the corresponding
    /// queue item reaches a terminal state or is removed.
    pub synthetic_cue_artifacts: HashSet<PathBuf>,
    /// Fatal queue-planning errors. When this is non-empty the queue planner
    /// has deliberately failed closed: no paths are staged, so callers cannot
    /// silently fall back to side-specific CUE jobs or raw audio conversion.
    pub expansion_errors: Vec<String>,
}

impl QueueExpansionResult {
    #[must_use]
    pub fn into_paths(self) -> Vec<PathBuf> {
        self.paths
    }

    #[must_use]
    pub fn first_error(&self) -> Option<&str> {
        self.expansion_errors.first().map(String::as_str)
    }
}

impl std::ops::Deref for QueueExpansionResult {
    type Target = [PathBuf];

    fn deref(&self) -> &Self::Target {
        &self.paths
    }
}

impl IntoIterator for QueueExpansionResult {
    type Item = PathBuf;
    type IntoIter = std::vec::IntoIter<PathBuf>;

    fn into_iter(self) -> Self::IntoIter {
        self.paths.into_iter()
    }
}

/// Count audio files reachable from `paths`, stopping once `limit` is exceeded.
///
/// This is intended for UI guards before expensive bulk operations. It walks
/// ordinary directories recursively, counts only files classified as audio,
/// ignores unreadable directories best-effort, and never follows directory
/// symlinks. Directory entries are inspected as they stream from `read_dir`;
/// only subdirectories are pushed for later traversal, and the walk returns as
/// soon as the bounded count exceeds `limit`. Returning `limit + 1` means "more
/// than `limit`" without enumerating the rest of a huge directory.
pub fn count_audio_files_bounded(paths: &[PathBuf], limit: usize) -> usize {
    let mut count = 0usize;
    let mut stack: Vec<PathBuf> = paths.to_vec();

    while let Some(path) = stack.pop() {
        if count > limit {
            return count;
        }

        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };

        if metadata.is_dir() {
            let Ok(read_dir) = fs::read_dir(&path) else {
                continue;
            };

            for child in read_dir.flatten() {
                let child_path = child.path();
                let Ok(child_metadata) = fs::symlink_metadata(&child_path) else {
                    continue;
                };

                if child_metadata.is_dir() {
                    stack.push(child_path);
                } else if child_metadata.is_file() || child_metadata.file_type().is_symlink() {
                    if matches!(classify_file(&child_path), EntryKind::AudioFile(_)) {
                        count = count.saturating_add(1);
                        if count > limit {
                            return count;
                        }
                    }
                }
            }
        } else if metadata.is_file() || metadata.file_type().is_symlink() {
            if matches!(classify_file(&path), EntryKind::AudioFile(_)) {
                count = count.saturating_add(1);
                if count > limit {
                    return count;
                }
            }
        }
    }

    count
}

/// Expands files/directories to queueable paths using the historical
/// `Vec<PathBuf>` API. This adapter cannot transfer ownership of transient
/// synthetic CUE artifacts, so it cleans and omits them; queue-building callers
/// that may materialize merged split-CUE albums must use
/// `expand_paths_to_audio_with_metadata()` and register returned artifacts.
pub fn expand_paths_to_audio(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut expansion = expand_paths_to_audio_with_metadata(paths);
    if !expansion.synthetic_cue_artifacts.is_empty() {
        // The legacy Vec-only API cannot transfer ownership of transient
        // synthetic CUE artifacts to a queue manager. Do not leak them or
        // return paths to caller-unowned temp files; queue-building callers
        // must use `expand_paths_to_audio_with_metadata()` instead.
        let artifacts = std::mem::take(&mut expansion.synthetic_cue_artifacts);
        expansion.paths.retain(|path| !artifacts.contains(path));
        cleanup_synthetic_cue_artifacts(&artifacts);
    }
    expansion.into_paths()
}

/// Expands files/directories to every audio file they contain, with no CUE
/// suppression. This is the collector for metadata and analysis surfaces
/// (metadata editor, tag lookups, verification, comparison): tags and audio
/// data live in the audio files, so a single-image album must yield its
/// image file here even though queue expansion would suppress it in favor of
/// the CUE. Deterministic: per-level sorted walk order (files before
/// subdirectories), deduplicated by canonical path, symlinks skipped
/// (matching the queue walk policy).
pub fn expand_paths_to_all_audio(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        collect_all_audio(path, &mut out, &mut seen);
    }
    out
}

fn collect_all_audio(path: &Path, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    if path.is_dir() {
        let Ok(read) = fs::read_dir(path) else {
            return;
        };
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for entry in read.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let child = entry.path();
            if file_type.is_dir() {
                dirs.push(child);
            } else {
                files.push(child);
            }
        }
        dirs.sort();
        files.sort();
        for file in files {
            push_audio_file(file, out, seen);
        }
        for dir in dirs {
            collect_all_audio(&dir, out, seen);
        }
    } else {
        push_audio_file(path.to_path_buf(), out, seen);
    }
}

fn push_audio_file(path: PathBuf, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    if !matches!(
        crate::convert::classify::classify_file(&path),
        crate::convert::classify::EntryKind::AudioFile(_)
    ) {
        return;
    }
    if seen.insert(queue_path_key(&path)) {
        out.push(path);
    }
}

/// Expands files/directories for conversion queue construction and carries
/// sidecar-CUE suppression metadata alongside the path list. Queue-building
/// callers must use this result; non-queue callers should use
/// `expand_paths_to_audio()` above to preserve the old API contract.
pub fn expand_paths_to_audio_with_metadata(paths: &[PathBuf]) -> QueueExpansionResult {
    expand_paths_to_audio_with_metadata_using_grouping_decisions(
        paths,
        &QueueSplitCueAlbumGroupingDecisions::new(),
    )
}

/// Expands files/directories for conversion using caller-supplied authoritative
/// split-CUE grouping decisions. TUI conversion must use this when it has a
/// cached or freshly resolved metadata/GNUDB/MB ladder result so conversion
/// cannot recompute a weaker title-only approximation and disagree with the
/// metadata surface.
pub fn expand_paths_to_audio_with_metadata_using_grouping_decisions(
    paths: &[PathBuf],
    grouping_decisions: &QueueSplitCueAlbumGroupingDecisions,
) -> QueueExpansionResult {
    let mut plan = QueueExpansionPlan::default();
    for path in paths {
        collect_queue_candidates(path, &mut plan);
    }
    plan.into_queue_paths_with_grouping_decisions(grouping_decisions)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueExpansionLimitedError {
    pub message: String,
    pub visited: usize,
    pub cancelled: bool,
}

impl QueueExpansionLimitedError {
    fn cancelled(visited: usize) -> Self {
        Self {
            message: "folder expansion cancelled".to_string(),
            visited,
            cancelled: true,
        }
    }

    fn failed(message: String, visited: usize) -> Self {
        Self {
            message,
            visited,
            cancelled: false,
        }
    }
}

/// Expands paths with the same queue/CUE semantics as
/// `expand_paths_to_audio_with_metadata`, while allowing UI workers to enforce
/// cancellation and a visited-entry cap. This exists so Browse async folder
/// expansion can use the canonical queue planner instead of maintaining an
/// audio-only walk with divergent CUE behavior.
pub fn expand_paths_to_audio_with_metadata_limited<F>(
    paths: &[PathBuf],
    max_visited: usize,
    is_cancelled: F,
) -> Result<(QueueExpansionResult, usize), QueueExpansionLimitedError>
where
    F: FnMut() -> bool,
{
    expand_paths_to_audio_with_metadata_limited_using_grouping_decisions(
        paths,
        max_visited,
        is_cancelled,
        &QueueSplitCueAlbumGroupingDecisions::new(),
    )
}

/// Bounded expansion variant that honors authoritative split-CUE decisions
/// supplied by a caller that already ran the metadata/GNUDB/MB ladder.
pub fn expand_paths_to_audio_with_metadata_limited_using_grouping_decisions<F>(
    paths: &[PathBuf],
    max_visited: usize,
    is_cancelled: F,
    grouping_decisions: &QueueSplitCueAlbumGroupingDecisions,
) -> Result<(QueueExpansionResult, usize), QueueExpansionLimitedError>
where
    F: FnMut() -> bool,
{
    expand_paths_to_audio_with_preserved_disc_roots_limited_using_grouping_decisions(
        paths,
        &[],
        max_visited,
        is_cancelled,
        grouping_decisions,
    )
}

/// Expands paths with canonical queue/CUE semantics while preserving selected
/// disc/source roots as opaque queue items. This is the bounded counterpart to
/// `expand_paths_to_audio_with_preserved_disc_roots()`: callers still submit
/// the complete selection in one pass so CUE suppression can be decided after
/// all candidates, including explicitly selected files, have been collected.
pub fn expand_paths_to_audio_with_preserved_disc_roots_limited<F>(
    paths: &[PathBuf],
    preserved_disc_roots: &[PathBuf],
    max_visited: usize,
    is_cancelled: F,
) -> Result<(QueueExpansionResult, usize), QueueExpansionLimitedError>
where
    F: FnMut() -> bool,
{
    expand_paths_to_audio_with_preserved_disc_roots_limited_using_grouping_decisions(
        paths,
        preserved_disc_roots,
        max_visited,
        is_cancelled,
        &QueueSplitCueAlbumGroupingDecisions::new(),
    )
}

/// Bounded preserved-root expansion that honors already-resolved split-CUE
/// grouping decisions from the metadata/GNUDB/MB ladder.
pub fn expand_paths_to_audio_with_preserved_disc_roots_limited_using_grouping_decisions<F>(
    paths: &[PathBuf],
    preserved_disc_roots: &[PathBuf],
    max_visited: usize,
    mut is_cancelled: F,
    grouping_decisions: &QueueSplitCueAlbumGroupingDecisions,
) -> Result<(QueueExpansionResult, usize), QueueExpansionLimitedError>
where
    F: FnMut() -> bool,
{
    let preserved_disc_root_keys: HashSet<PathBuf> = preserved_disc_roots
        .iter()
        .map(|path| queue_path_key(path))
        .collect();
    let mut state = LimitedQueueExpansionState {
        max_visited,
        visited: 0,
    };
    let mut plan = QueueExpansionPlan::default();

    for path in paths {
        if is_cancelled() {
            return Err(QueueExpansionLimitedError::cancelled(state.visited));
        }
        if preserved_disc_root_keys.contains(&queue_path_key(path)) {
            state.visit(path)?;
            plan.add_disc_root(path.clone());
        } else {
            collect_queue_candidates_limited(path, &mut plan, &mut state, &mut is_cancelled)?;
        }
    }

    let queue = plan.into_queue_paths_with_grouping_decisions(grouping_decisions);
    // Expansion warnings are carried alongside usable paths, matching the
    // unlimited API. Fail closed only when no queueable path survived, and
    // reclaim every transient artifact before dropping the result.
    if queue.paths.is_empty() {
        if let Some(message) = queue.first_error() {
            cleanup_synthetic_cue_artifacts(&queue.synthetic_cue_artifacts);
            return Err(QueueExpansionLimitedError::failed(
                message.to_string(),
                state.visited,
            ));
        }
    }

    Ok((queue, state.visited))
}

pub(crate) fn expand_paths_to_audio_with_preserved_disc_roots(
    paths: &[PathBuf],
    preserved_disc_roots: &[PathBuf],
) -> QueueExpansionResult {
    expand_paths_to_audio_with_preserved_disc_roots_using_grouping_decisions(
        paths,
        preserved_disc_roots,
        &QueueSplitCueAlbumGroupingDecisions::new(),
    )
}

pub(crate) fn expand_paths_to_audio_with_preserved_disc_roots_using_grouping_decisions(
    paths: &[PathBuf],
    preserved_disc_roots: &[PathBuf],
    grouping_decisions: &QueueSplitCueAlbumGroupingDecisions,
) -> QueueExpansionResult {
    let preserved_disc_root_keys: HashSet<PathBuf> = preserved_disc_roots
        .iter()
        .map(|path| queue_path_key(path))
        .collect();
    let mut plan = QueueExpansionPlan::default();
    for path in paths {
        if preserved_disc_root_keys.contains(&queue_path_key(path)) {
            plan.add_disc_root(path.clone());
        } else {
            collect_queue_candidates(path, &mut plan);
        }
    }
    plan.into_queue_paths_with_grouping_decisions(grouping_decisions)
}

/// Directory/file expansion plan for conversion queue inputs.
///
/// Build the whole candidate set before deciding what to queue. A split-source
/// CUE discovered late in a directory walk can suppress audio discovered earlier,
/// so queue decisions must happen after collection to stay idempotent.
#[derive(Default)]
struct QueueExpansionPlan {
    disc_roots: Vec<PathBuf>,
    disc_root_keys: HashSet<PathBuf>,
    cue_sheets: Vec<CueQueueCandidate>,
    queueable_non_cue: Vec<PathBuf>,
    queueable_non_cue_keys: HashSet<PathBuf>,
}

#[derive(Debug, Clone)]
struct CueQueueCandidate {
    path: PathBuf,
    path_key: PathBuf,
    explicit: bool,
}

impl QueueExpansionPlan {
    fn add_disc_root(&mut self, path: PathBuf) {
        push_unique_path_with_keys(&mut self.disc_roots, &mut self.disc_root_keys, path);
    }

    fn add_explicit_file(&mut self, path: PathBuf) {
        self.add_file(path, true);
    }

    fn add_discovered_file(&mut self, path: PathBuf) {
        self.add_file(path, false);
    }

    fn add_file(&mut self, path: PathBuf, explicit: bool) {
        if is_cue_sheet_path(&path) {
            self.add_cue_sheet(path, explicit);
        } else if is_direct_queue_source_path(&path) {
            push_unique_path_with_keys(
                &mut self.queueable_non_cue,
                &mut self.queueable_non_cue_keys,
                path,
            );
        }
    }

    fn add_cue_sheet(&mut self, path: PathBuf, explicit: bool) {
        let path_key = queue_path_key(&path);
        if let Some(existing) = self
            .cue_sheets
            .iter_mut()
            .find(|existing| existing.path_key == path_key)
        {
            existing.explicit |= explicit;
            return;
        }

        self.cue_sheets.push(CueQueueCandidate {
            path,
            path_key,
            explicit,
        });
    }

    fn into_queue_paths_with_grouping_decisions(
        self,
        grouping_decisions: &QueueSplitCueAlbumGroupingDecisions,
    ) -> QueueExpansionResult {
        let QueueExpansionPlan {
            disc_roots,
            disc_root_keys,
            cue_sheets,
            queueable_non_cue,
            queueable_non_cue_keys: _,
        } = self;

        let mut result = Vec::new();
        let mut result_keys = HashSet::new();
        let mut suppressed_audio_keys = HashSet::new();
        let mut cue_artifact_audio_keys = HashSet::new();

        for disc_root in disc_roots {
            push_unique_path_with_keys(&mut result, &mut result_keys, disc_root);
        }

        let (grouped_cue_keys, synthetic_album_errors, synthetic_album_warnings, mut synthetic_cue_artifacts) =
            push_synthetic_cue_album_groups_for_queue(
                &cue_sheets,
                &queueable_non_cue,
                &disc_root_keys,
                grouping_decisions,
                &mut result,
                &mut result_keys,
                &mut suppressed_audio_keys,
            );

        let mut expansion_errors = synthetic_album_errors;
        expansion_errors.extend(synthetic_album_warnings);

        for cue in cue_sheets {
            if grouped_cue_keys.contains(&cue.path_key) {
                continue;
            }
            if path_key_is_under_any_root(&queue_path_key(&cue.path), &disc_root_keys) {
                continue;
            }
            match cue_queue_decision_for_path(&cue.path) {
                Ok(CueQueueDecision::SplitSource { referenced_audio }) => {
                    push_unique_path_with_keys(&mut result, &mut result_keys, cue.path);
                    for path in referenced_audio {
                        suppressed_audio_keys.insert(queue_path_key(&path));
                    }
                }
                Ok(CueQueueDecision::MetadataArtifact { referenced_audio }) => {
                    if cue.explicit {
                        push_unique_path_with_keys(&mut result, &mut result_keys, cue.path);
                    } else {
                        for path in referenced_audio {
                            cue_artifact_audio_keys.insert(queue_path_key(&path));
                        }
                    }
                }
                Err(err) => {
                    log::warn!(
                        "CUE {} is not safe to queue from folder expansion; suppressing it and marking sibling audio to skip sidecar CUE detection: {}",
                        cue.path.display(),
                        err
                    );
                    if cue.explicit {
                        push_unique_path_with_keys(&mut result, &mut result_keys, cue.path);
                    } else {
                        mark_sibling_audio_as_cue_artifacts(
                            &cue.path,
                            &queueable_non_cue,
                            &mut cue_artifact_audio_keys,
                        );
                    }
                }
            }
        }

        let mut cue_artifact_audio = HashSet::new();
        for path in queueable_non_cue {
            // Suppression applies to explicitly selected audio too when the
            // same expansion also contains an explicit split-source CUE that
            // references it. The explicit CUE selection is honored, and the
            // referenced audio is omitted by design to avoid converting the
            // same source twice through both the CUE materializer and the
            // raw audio-file path.
            let path_key = queue_path_key(&path);
            if path_key_is_under_any_root(&path_key, &disc_root_keys) {
                continue;
            }
            if is_audio_file_path(&path) && suppressed_audio_keys.contains(&path_key) {
                continue;
            }
            if is_audio_file_path(&path) && cue_artifact_audio_keys.contains(&path_key) {
                cue_artifact_audio.insert(path.clone());
            }
            push_unique_path_with_keys(&mut result, &mut result_keys, path);
        }

        let orphaned_synthetic_artifacts = synthetic_cue_artifacts
            .iter()
            .filter(|path| !result.iter().any(|queued| queue_path_key(queued) == queue_path_key(path)))
            .cloned()
            .collect::<HashSet<_>>();
        cleanup_synthetic_cue_artifacts(&orphaned_synthetic_artifacts);
        synthetic_cue_artifacts.retain(|path| {
            result.iter().any(|queued| queue_path_key(queued) == queue_path_key(path))
        });

        QueueExpansionResult {
            paths: result,
            cue_artifact_audio,
            synthetic_cue_artifacts,
            expansion_errors,
        }
    }
}


#[derive(Debug, Clone)]
struct SyntheticCueAlbumPart {
    cue_path: PathBuf,
    cue_key: PathBuf,
    sheet: crate::convert::cue_parser::CueSheet,
    referenced_audio: Vec<PathBuf>,
}

fn push_synthetic_cue_album_groups_for_queue(
    cue_sheets: &[CueQueueCandidate],
    queueable_non_cue: &[PathBuf],
    disc_root_keys: &HashSet<PathBuf>,
    grouping_decisions: &QueueSplitCueAlbumGroupingDecisions,
    result: &mut Vec<PathBuf>,
    result_keys: &mut HashSet<PathBuf>,
    suppressed_audio_keys: &mut HashSet<PathBuf>,
) -> (HashSet<PathBuf>, Vec<String>, Vec<String>, HashSet<PathBuf>) {
    let mut by_parent: BTreeMap<PathBuf, Vec<CueQueueCandidate>> = BTreeMap::new();
    for cue in cue_sheets {
        if cue.explicit || path_key_is_under_any_root(&cue.path_key, disc_root_keys) {
            continue;
        }
        let Some(parent) = cue.path.parent().map(queue_path_key) else {
            continue;
        };
        by_parent.entry(parent).or_default().push(cue.clone());
    }

    let mut grouped = HashSet::new();
    let mut fatal_errors = Vec::new();
    let mut nonfatal_errors = Vec::new();
    let mut synthetic_cue_artifacts = HashSet::new();
    for (parent, mut candidates) in by_parent {
        if candidates.len() < 2 {
            continue;
        }
        candidates.sort_by(|a, b| deterministic_path_sort_key(&a.path).cmp(&deterministic_path_sort_key(&b.path)));

        let mut parts = Vec::new();
        let mut parent_failed_closed = false;
        for cue in &candidates {
            let sheet = match crate::convert::cue_parser::parse_cue_file(&cue.path) {
                Ok(sheet) => sheet,
                Err(err) => {
                    fatal_errors.push(format!(
                        "Cannot queue merged CUE album for {}: failed to parse {}: {}. Nothing was staged for this group; fix the CUE or select a single .cue explicitly.",
                        parent.display(),
                        cue.path.display(),
                        err
                    ));
                    parent_failed_closed = true;
                    break;
                }
            };
            let referenced_audio = match cue_queue_decision_for_path(&cue.path) {
                Ok(CueQueueDecision::SplitSource { referenced_audio }) => referenced_audio,
                Ok(CueQueueDecision::MetadataArtifact { .. }) => continue,
                Err(err) => {
                    fatal_errors.push(format!(
                        "Cannot queue merged CUE album for {}: failed to analyze {}: {}. Nothing was staged for this group; fix the CUE or select a single .cue explicitly.",
                        parent.display(),
                        cue.path.display(),
                        err
                    ));
                    parent_failed_closed = true;
                    break;
                }
            };
            if referenced_audio.is_empty() || sheet.tracks.is_empty() {
                continue;
            }
            parts.push(SyntheticCueAlbumPart {
                cue_path: cue.path.clone(),
                cue_key: cue.path_key.clone(),
                sheet,
                referenced_audio,
            });
        }
        if parent_failed_closed {
            for cue in &candidates {
                grouped.insert(cue.path_key.clone());
            }
            suppress_parent_audio_for_failed_synthetic_group(&parent, queueable_non_cue, suppressed_audio_keys);
            continue;
        }
        if parts.len() < 2 {
            continue;
        }
        // Two cues referencing the SAME image are alternate track layouts of
        // one rip, not album parts; merging would duplicate audio in the
        // synthetic sheet. Such folders keep the legacy per-cue queue path.
        {
            let mut seen_images: HashSet<PathBuf> = HashSet::new();
            let mut overlapping = false;
            for part in &parts {
                for audio in &part.referenced_audio {
                    if !seen_images.insert(queue_path_key(audio)) {
                        overlapping = true;
                    }
                }
            }
            if overlapping {
                continue;
            }
        }

        parts.sort_by(|a, b| deterministic_path_sort_key(&a.cue_path).cmp(&deterministic_path_sort_key(&b.cue_path)));
        let cue_paths: Vec<PathBuf> = parts.iter().map(|part| part.cue_path.clone()).collect();
        let titles: Vec<String> = parts
            .iter()
            .map(|part| part.sheet.title.clone().unwrap_or_default())
            .collect();
        let decision_key = grouping_key_from_paths(&cue_paths);
        let Some(decision) = grouping_decisions
            .get(&decision_key)
            .cloned()
            .or_else(|| decide_with_toc_evidence(&cue_paths, &titles, None, None))
        else {
            continue;
        };
        if matches!(decision.reason, SplitCueAlbumGroupingReason::PerCueDistinctTocHits) {
            continue;
        }

        for group in decision.groups {
            if group.len() < 2 {
                continue;
            }
            let group_keys: HashSet<PathBuf> = group.into_iter().collect();
            let mut group_parts: Vec<SyntheticCueAlbumPart> = parts
                .iter()
                .filter(|part| group_keys.contains(&part.cue_key))
                .cloned()
                .collect();
            if group_parts.len() < 2 {
                continue;
            }
            group_parts.sort_by(|a, b| deterministic_path_sort_key(&a.cue_path).cmp(&deterministic_path_sort_key(&b.cue_path)));
            let total_tracks: usize = group_parts.iter().map(|part| part.sheet.tracks.len()).sum();
            if total_tracks == 0 {
                fatal_errors.push(format!(
                    "Cannot queue merged CUE album for {}: the merged CUE group has no tracks. Nothing was staged for this group.",
                    parent.display()
                ));
                block_failed_synthetic_group(&group_parts, &mut grouped, suppressed_audio_keys);
                continue;
            }
            if total_tracks > 99 {
                fatal_errors.push(format!(
                    "Cannot queue merged CUE album for {}: the merged CUE group has {} tracks, but CUE syntax supports at most 99. Nothing was staged for this group; split the selection or edit the cues.",
                    parent.display(),
                    total_tracks
                ));
                block_failed_synthetic_group(&group_parts, &mut grouped, suppressed_audio_keys);
                continue;
            }
            if let Some(path_with_quote) = first_resolved_member_audio_path_with_quote(&group_parts) {
                nonfatal_errors.push(format!(
                    "Cannot merge CUE album for {}: member image path contains a double quote that CUE FILE syntax cannot round-trip exactly: {}. Falling back to per-CUE queue items.",
                    parent.display(),
                    path_with_quote.display()
                ));
                continue;
            }
            let text = match generate_queue_synthetic_cue_album(&group_parts) {
                Ok(text) => text,
                Err(err) => {
                    fatal_errors.push(format!(
                        "Cannot queue merged CUE album for {}: failed to generate the synthetic CUE: {}. Nothing was staged for this group.",
                        parent.display(),
                        err
                    ));
                    block_failed_synthetic_group(&group_parts, &mut grouped, suppressed_audio_keys);
                    continue;
                }
            };
            let path = match write_queue_synthetic_cue_album(&group_parts, &text) {
                Ok(path) => path,
                Err(err) => {
                    fatal_errors.push(format!(
                        "Cannot queue merged CUE album for {}: failed to stage the synthetic CUE: {}. Nothing was staged for this group.",
                        parent.display(),
                        err
                    ));
                    block_failed_synthetic_group(&group_parts, &mut grouped, suppressed_audio_keys);
                    continue;
                }
            };
            synthetic_cue_artifacts.insert(path.clone());
            push_unique_path_with_keys(result, result_keys, path);
            for part in &group_parts {
                grouped.insert(part.cue_key.clone());
                for audio in &part.referenced_audio {
                    suppressed_audio_keys.insert(queue_path_key(audio));
                }
            }
        }
    }

    (grouped, fatal_errors, nonfatal_errors, synthetic_cue_artifacts)
}

fn block_failed_synthetic_group(
    group_parts: &[SyntheticCueAlbumPart],
    grouped: &mut HashSet<PathBuf>,
    suppressed_audio_keys: &mut HashSet<PathBuf>,
) {
    for part in group_parts {
        grouped.insert(part.cue_key.clone());
        for audio in &part.referenced_audio {
            suppressed_audio_keys.insert(queue_path_key(audio));
        }
    }
}

fn suppress_parent_audio_for_failed_synthetic_group(
    parent_key: &Path,
    queueable_non_cue: &[PathBuf],
    suppressed_audio_keys: &mut HashSet<PathBuf>,
) {
    for path in queueable_non_cue {
        if !is_audio_file_path(path) {
            continue;
        }
        let Some(parent) = path.parent().map(queue_path_key) else {
            continue;
        };
        if parent == parent_key {
            suppressed_audio_keys.insert(queue_path_key(path));
        }
    }
}

fn first_resolved_member_audio_path_with_quote(parts: &[SyntheticCueAlbumPart]) -> Option<PathBuf> {
    for part in parts {
        for track in &part.sheet.tracks {
            let Some(file_ref) = track.file.as_deref() else {
                continue;
            };
            let Some(parent) = part.cue_path.parent() else {
                continue;
            };
            let resolved = match resolve_cue_file_reference_for_queue(parent, file_ref) {
                CueReferenceResolution::Resolved(path) => path,
                CueReferenceResolution::Missing | CueReferenceResolution::Ambiguous(_) => continue,
            };
            if resolved.display().to_string().contains('"') {
                return Some(resolved);
            }
        }
    }
    None
}

fn read_embedded_cuesheet_text_for_queue(path: &Path) -> Option<String> {
    use lofty::prelude::*;

    let tagged = lofty::read_from_path(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    for item in tag.items() {
        if let lofty::tag::ItemKey::Unknown(key) = item.key() {
            if key.eq_ignore_ascii_case("CUESHEET") {
                if let Some(text) = item.value().text() {
                    return Some(text.to_string());
                }
            }
        }
    }
    tag.get_string(&lofty::tag::ItemKey::Unknown("CUESHEET".to_string()))
        .map(|value| value.to_string())
}

fn unique_member_audio_paths_for_synthetic_parts(parts: &[SyntheticCueAlbumPart]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for part in parts {
        for audio in &part.referenced_audio {
            if seen.insert(queue_path_key(audio)) {
                out.push(audio.clone());
            }
        }
    }
    out
}

fn push_unique_embedded_cue_base_dir(
    bases: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    base: PathBuf,
) {
    if seen.insert(queue_path_key(&base)) {
        bases.push(base);
    }
}

fn common_existing_parent_dir(paths: &[PathBuf]) -> Option<PathBuf> {
    let parents: Vec<PathBuf> = paths
        .iter()
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect();
    let first = parents.first()?;
    first
        .ancestors()
        .find(|candidate| {
            candidate.parent().is_some()
                && parents.iter().all(|parent| parent.starts_with(candidate))
        })
        .map(Path::to_path_buf)
}

fn embedded_cue_base_dirs_for_synthetic_parts(parts: &[SyntheticCueAlbumPart]) -> Vec<PathBuf> {
    let member_audio = unique_member_audio_paths_for_synthetic_parts(parts);
    let mut bases = Vec::new();
    let mut seen = HashSet::new();

    for part in parts {
        if let Some(parent) = part.cue_path.parent() {
            push_unique_embedded_cue_base_dir(&mut bases, &mut seen, parent.to_path_buf());
        }
    }
    if let Some(common) = common_existing_parent_dir(&member_audio) {
        push_unique_embedded_cue_base_dir(&mut bases, &mut seen, common);
    }
    for audio in member_audio {
        if let Some(parent) = audio.parent() {
            push_unique_embedded_cue_base_dir(&mut bases, &mut seen, parent.to_path_buf());
        }
    }

    bases
}

fn resolved_file_order_for_parsed_cue(
    parent: &Path,
    parsed: &crate::convert::cue_parser::CueSheet,
) -> Option<Vec<PathBuf>> {
    let mut resolved = Vec::new();
    let mut last_key: Option<PathBuf> = None;
    for track in &parsed.tracks {
        let file_ref = track.file.as_deref()?;
        let path = match resolve_cue_file_reference_for_queue(parent, file_ref) {
            CueReferenceResolution::Resolved(path) => path,
            CueReferenceResolution::Missing | CueReferenceResolution::Ambiguous(_) => return None,
        };
        let key = queue_path_key(&path);
        if last_key.as_ref() != Some(&key) {
            resolved.push(path);
            last_key = Some(key);
        }
    }
    Some(resolved)
}

fn rewrite_embedded_cue_file_lines_to_absolute_paths(
    text: &str,
    file_order: &[PathBuf],
) -> Option<String> {
    let mut next_file = 0usize;
    let mut out = String::new();
    for line in text.trim().lines() {
        let leading = line.len().saturating_sub(line.trim_start().len());
        let trimmed = line.trim_start();
        if trimmed
            .get(..4)
            .map(|prefix| prefix.eq_ignore_ascii_case("FILE"))
            .unwrap_or(false)
        {
            let after = trimmed[4..].chars().next();
            if !after.map(|ch| ch.is_whitespace()).unwrap_or(false) {
                out.push_str(line);
                out.push('\n');
                continue;
            }
            let path = file_order.get(next_file)?;
            if path.display().to_string().contains('"') {
                return None;
            }
            out.push_str(&line[..leading]);
            out.push_str(&format!(
                "FILE \"{}\" {}\n",
                quote_cue_value(&path.display().to_string()),
                cue_file_type_for_queue(path)
            ));
            next_file += 1;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    (next_file == file_order.len()).then_some(out)
}

fn authoritative_embedded_cuesheet_for_member_audio(
    member_audio: &[PathBuf],
    base_dir: &Path,
) -> Option<String> {
    if member_audio.len() < 2 {
        return None;
    }

    let mut embedded_texts = Vec::with_capacity(member_audio.len());
    for audio in member_audio {
        let text = read_embedded_cuesheet_text_for_queue(audio)?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        embedded_texts.push(trimmed.to_string());
    }
    let first = embedded_texts.first()?.clone();
    if embedded_texts.iter().any(|text| text != &first) {
        return None;
    }

    let parsed = crate::convert::cue_parser::parse_cue(&first);
    if parsed.tracks.is_empty() {
        return None;
    }
    let file_order = resolved_file_order_for_parsed_cue(base_dir, &parsed)?;
    if file_order.len() != member_audio.len() {
        return None;
    }

    let expected: HashSet<PathBuf> = member_audio.iter().map(|path| queue_path_key(path)).collect();
    let actual: HashSet<PathBuf> = file_order.iter().map(|path| queue_path_key(path)).collect();
    if actual != expected {
        return None;
    }

    if parsed.tracks.iter().any(|track| track.index01_frames.is_none() || track.file.is_none()) {
        return None;
    }

    // Structural validation the materializer will later enforce on the
    // synthetic sheet: adopting an embedded sheet that fails it would turn a
    // plan-time sidecar fallback into a convert-time group failure.
    // TRACK numbers must be unique, non-zero, and continuous from 01, and
    // INDEX 01 must strictly increase within each FILE.
    let mut seen_track_numbers = HashSet::new();
    for (position, track) in parsed.tracks.iter().enumerate() {
        if track.number == 0
            || track.number as usize != position + 1
            || !seen_track_numbers.insert(track.number)
        {
            return None;
        }
    }
    let resolved_tracks: Vec<(u32, PathBuf, u32)> = parsed
        .tracks
        .iter()
        .filter_map(|track| {
            let file_ref = track.file.as_deref()?;
            let resolved = match resolve_cue_file_reference_for_queue(base_dir, file_ref) {
                CueReferenceResolution::Resolved(path) => path,
                _ => return None,
            };
            Some((track.number, resolved, track.index01_frames?))
        })
        .collect();
    if resolved_tracks.len() != parsed.tracks.len()
        || validate_queue_cue_index_order(&resolved_tracks).is_err()
    {
        return None;
    }

    rewrite_embedded_cue_file_lines_to_absolute_paths(&first, &file_order)
}

#[cfg(test)]
pub(crate) fn planner_authoritative_embedded_cuesheet_accepts_for_test(
    member_audio: &[PathBuf],
    base_dir: &Path,
) -> bool {
    authoritative_embedded_cuesheet_for_member_audio(member_audio, base_dir).is_some()
}

fn authoritative_embedded_cuesheet_for_member_audio_with_base_dirs(
    member_audio: &[PathBuf],
    base_dirs: &[PathBuf],
) -> Option<String> {
    for base_dir in base_dirs {
        if let Some(text) = authoritative_embedded_cuesheet_for_member_audio(member_audio, base_dir) {
            return Some(text);
        }
    }
    None
}

fn authoritative_embedded_cuesheet_for_synthetic_parts(parts: &[SyntheticCueAlbumPart]) -> Option<String> {
    if parts.len() < 2 {
        return None;
    }
    let member_audio = unique_member_audio_paths_for_synthetic_parts(parts);
    if member_audio.len() != parts.len() || member_audio.is_empty() {
        return None;
    }
    let base_dirs = embedded_cue_base_dirs_for_synthetic_parts(parts);
    authoritative_embedded_cuesheet_for_member_audio_with_base_dirs(&member_audio, &base_dirs)
}

fn generate_queue_synthetic_cue_album(parts: &[SyntheticCueAlbumPart]) -> Result<String, String> {
    if let Some(authoritative) = authoritative_embedded_cuesheet_for_synthetic_parts(parts) {
        return Ok(authoritative);
    }

    let titles: Vec<String> = parts
        .iter()
        .map(|part| part.sheet.title.clone().unwrap_or_default())
        .collect();
    let title = common_cue_album_title(&titles)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Merged CUE album".to_string());
    let performer = first_non_empty(parts.iter().filter_map(|part| part.sheet.performer.as_deref()));
    let date = first_non_empty(parts.iter().filter_map(|part| part.sheet.date.as_deref()));
    let genre = first_non_empty(parts.iter().filter_map(|part| part.sheet.genre.as_deref()));
    let catalog = first_non_empty(parts.iter().filter_map(|part| part.sheet.catalog.as_deref()));

    let mut out = String::new();
    if let Some(catalog) = catalog {
        out.push_str(&format!("CATALOG {}\n", catalog.trim()));
    }
    if let Some(performer) = performer {
        out.push_str(&format!("PERFORMER \"{}\"\n", quote_cue_value(performer.trim())));
    }
    out.push_str(&format!("TITLE \"{}\"\n", quote_cue_value(&title)));
    if let Some(date) = date {
        out.push_str(&format!("REM DATE {}\n", quote_cue_value(date.trim())));
    }
    if let Some(genre) = genre {
        out.push_str(&format!("REM GENRE \"{}\"\n", quote_cue_value(genre.trim())));
    }

    let mut next_track = 1usize;
    let mut last_audio_key: Option<PathBuf> = None;
    for part in parts {
        for track in &part.sheet.tracks {
            let file_ref = track.file.as_deref().ok_or_else(|| {
                format!("track {} in {} has no FILE reference", track.number, part.cue_path.display())
            })?;
            let parent = part.cue_path.parent().ok_or_else(|| "CUE path has no parent directory".to_string())?;
            let resolved = match resolve_cue_file_reference_for_queue(parent, file_ref) {
                CueReferenceResolution::Resolved(path) => path,
                CueReferenceResolution::Missing => return Err(format!("FILE reference {:?} not found", file_ref)),
                CueReferenceResolution::Ambiguous(paths) => {
                    return Err(format!("FILE reference {:?} ambiguous: {}", file_ref, format_candidate_paths_for_log(&paths)))
                }
            };
            let audio_key = queue_path_key(&resolved);
            if last_audio_key.as_ref() != Some(&audio_key) {
                out.push_str(&format!(
                    "FILE \"{}\" {}\n",
                    quote_cue_value(&resolved.display().to_string()),
                    cue_file_type_for_queue(&resolved)
                ));
                last_audio_key = Some(audio_key);
            }
            out.push_str(&format!("  TRACK {:02} AUDIO\n", next_track));
            if let Some(isrc) = track.isrc.as_deref().filter(|value| !value.trim().is_empty()) {
                out.push_str(&format!("    ISRC {}\n", isrc.trim()));
            }
            if let Some(title) = track.title.as_deref().filter(|value| !value.trim().is_empty()) {
                out.push_str(&format!("    TITLE \"{}\"\n", quote_cue_value(title.trim())));
            }
            // Emit the parse-normal form: parse_cue inherits the album
            // PERFORMER into performer-less tracks, so byte-stable
            // round-trips require emitting that inherited value here too.
            let track_performer = track
                .performer
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| performer.filter(|value| !value.trim().is_empty()));
            if let Some(track_performer) = track_performer {
                out.push_str(&format!("    PERFORMER \"{}\"\n", quote_cue_value(track_performer.trim())));
            }
            if let Some(frames) = track.index00_frames {
                out.push_str(&format!("    INDEX 00 {}\n", cue_timestamp(frames)));
            }
            let frames = track.index01_frames.ok_or_else(|| {
                format!("track {} in {} has no INDEX 01", track.number, part.cue_path.display())
            })?;
            out.push_str(&format!("    INDEX 01 {}\n", cue_timestamp(frames)));
            next_track += 1;
        }
    }
    Ok(out)
}

const SYNTHETIC_CUE_ALBUM_DIR: &str = "tonepoet-synthetic-cue-albums";
const SYNTHETIC_CUE_ALBUM_PROCESS_PREFIX: &str = "process-";
const SYNTHETIC_CUE_ALBUM_PROCESS_LOCK: &str = ".owner.lock";
const SYNTHETIC_CUE_ALBUM_PROCESS_PID: &str = ".owner.pid";
const SYNTHETIC_CUE_ALBUM_ARTIFACT_PREFIX: &str = "artifact-";
const SYNTHETIC_CUE_ALBUM_FILE: &str = "album.cue";
const SYNTHETIC_CUE_ALBUM_TMP: &str = "album.cue.tmp";
const SYNTHETIC_CUE_ALBUM_SCAVENGE_AFTER_SECS: u64 = 24 * 60 * 60;

struct SyntheticCueAlbumProcessRoot {
    path: PathBuf,
    #[allow(dead_code)]
    lock_file: std::fs::File,
}

static SYNTHETIC_CUE_ALBUM_PROCESS_ROOT: OnceCell<SyntheticCueAlbumProcessRoot> = OnceCell::new();

fn synthetic_cue_album_root() -> PathBuf {
    std::env::temp_dir().join(SYNTHETIC_CUE_ALBUM_DIR)
}

fn synthetic_cue_album_process_root() -> Result<PathBuf, String> {
    SYNTHETIC_CUE_ALBUM_PROCESS_ROOT
        .get_or_try_init(|| {
            let root = synthetic_cue_album_root();
            fs::create_dir_all(&root)
                .map_err(|err| format!("failed to create synthetic CUE root: {err}"))?;
            sync_directory_best_effort(&root);

            let pid = std::process::id();
            for attempt in 0..128u32 {
                let nonce = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or_default();
                let process_root = root.join(format!(
                    "{SYNTHETIC_CUE_ALBUM_PROCESS_PREFIX}{pid}-{nonce:x}-{attempt}"
                ));
                match fs::create_dir(&process_root) {
                    Ok(()) => {
                        let lock_path = process_root.join(SYNTHETIC_CUE_ALBUM_PROCESS_LOCK);
                        let lock_file = OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create_new(true)
                            .open(&lock_path)
                            .map_err(|err| format!("failed to create synthetic CUE owner lock: {err}"))?;
                        lock_file
                            .lock_exclusive()
                            .map_err(|err| format!("failed to lock synthetic CUE owner root: {err}"))?;
                        let pid_path = process_root.join(SYNTHETIC_CUE_ALBUM_PROCESS_PID);
                        write_and_sync_file(&pid_path, std::process::id().to_string().as_bytes())
                            .map_err(|err| format!("failed to write synthetic CUE owner pid: {err}"))?;
                        sync_directory_best_effort(&process_root);
                        sync_directory_best_effort(&root);
                        return Ok(SyntheticCueAlbumProcessRoot {
                            path: process_root,
                            lock_file,
                        });
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(err) => {
                        return Err(format!("failed to create synthetic CUE owner root: {err}"));
                    }
                }
            }
            Err("failed to allocate a unique synthetic CUE owner root".to_string())
        })
        .map(|root| root.path.clone())
}

fn write_queue_synthetic_cue_album(parts: &[SyntheticCueAlbumPart], text: &str) -> Result<PathBuf, String> {
    let process_root = synthetic_cue_album_process_root()?;

    let identity_hash = deterministic_synthetic_cue_album_hash(parts, text);
    let artifact_dir = create_unique_synthetic_cue_album_dir(&process_root, identity_hash)?;
    let tmp_path = artifact_dir.join(SYNTHETIC_CUE_ALBUM_TMP);
    let final_path = artifact_dir.join(SYNTHETIC_CUE_ALBUM_FILE);

    if let Err(err) = write_and_sync_file(&tmp_path, text.as_bytes()) {
        let _ = fs::remove_dir_all(&artifact_dir);
        return Err(format!("failed to write synthetic CUE: {err}"));
    }
    if let Err(err) = fs::rename(&tmp_path, &final_path) {
        let _ = fs::remove_file(&tmp_path);
        let _ = fs::remove_dir_all(&artifact_dir);
        return Err(format!("failed to publish synthetic CUE: {err}"));
    }
    sync_directory_best_effort(&artifact_dir);
    sync_directory_best_effort(&process_root);
    sync_directory_best_effort(&synthetic_cue_album_root());

    Ok(final_path)
}

fn deterministic_synthetic_cue_album_hash(parts: &[SyntheticCueAlbumPart], text: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for part in parts {
        fnv1a_update(&mut hash, queue_path_key(&part.cue_path).to_string_lossy().as_bytes());
        fnv1a_update(&mut hash, &[0]);
    }
    fnv1a_update(&mut hash, text.as_bytes());
    hash
}

fn fnv1a_update(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

fn create_unique_synthetic_cue_album_dir(root: &Path, identity_hash: u64) -> Result<PathBuf, String> {
    let pid = std::process::id();
    for attempt in 0..128u32 {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let dir = root.join(format!(
            "{SYNTHETIC_CUE_ALBUM_ARTIFACT_PREFIX}{identity_hash:016x}-{pid}-{nonce:x}-{attempt}"
        ));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(format!("failed to create synthetic CUE artifact directory: {err}")),
        }
    }
    Err("failed to allocate a unique synthetic CUE artifact directory".to_string())
}

fn write_and_sync_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn sync_directory_best_effort(path: &Path) {
    if let Ok(dir) = std::fs::File::open(path) {
        let _ = dir.sync_all();
    }
}

pub fn is_synthetic_cue_album_artifact(path: &Path) -> bool {
    if path.file_name().and_then(|name| name.to_str()) != Some(SYNTHETIC_CUE_ALBUM_FILE) {
        return false;
    }
    let Some(artifact_dir) = path.parent() else {
        return false;
    };
    if !artifact_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(SYNTHETIC_CUE_ALBUM_ARTIFACT_PREFIX))
    {
        return false;
    }
    let Some(process_root) = artifact_dir.parent() else {
        return false;
    };
    process_root
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(SYNTHETIC_CUE_ALBUM_PROCESS_PREFIX))
        && process_root
            .parent()
            .is_some_and(|root| root == synthetic_cue_album_root())
}

pub fn cleanup_synthetic_cue_artifact(path: &Path) {
    if !is_synthetic_cue_album_artifact(path) {
        return;
    }
    if let Some(artifact_dir) = path.parent() {
        let process_root = artifact_dir.parent().map(Path::to_path_buf);
        let _ = fs::remove_dir_all(artifact_dir);
        if let Some(process_root) = process_root {
            sync_directory_best_effort(&process_root);
        }
        sync_directory_best_effort(&synthetic_cue_album_root());
    }
}

pub fn cleanup_synthetic_cue_artifacts(paths: &HashSet<PathBuf>) {
    for path in paths {
        cleanup_synthetic_cue_artifact(path);
    }
}

pub fn scavenge_stale_synthetic_cue_album_artifacts() {
    scavenge_synthetic_cue_album_artifacts_older_than(Duration::from_secs(SYNTHETIC_CUE_ALBUM_SCAVENGE_AFTER_SECS));
}

fn scavenge_synthetic_cue_album_artifacts_older_than(max_age: Duration) {
    let root = synthetic_cue_album_root();
    let Ok(read_dir) = fs::read_dir(&root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in read_dir.flatten() {
        let process_root = entry.path();
        let name_matches = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(SYNTHETIC_CUE_ALBUM_PROCESS_PREFIX));
        if !name_matches || !process_root.is_dir() {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let old_enough = now
            .duration_since(modified)
            .map(|age| age >= max_age)
            .unwrap_or(false);
        if !old_enough {
            continue;
        }

        try_remove_abandoned_synthetic_cue_process_root(&process_root);
    }
    sync_directory_best_effort(&root);
}

fn try_remove_abandoned_synthetic_cue_process_root(process_root: &Path) {
    if process_root_owner_is_live(process_root) {
        return;
    }

    let lock_path = process_root.join(SYNTHETIC_CUE_ALBUM_PROCESS_LOCK);
    let Ok(lock_file) = OpenOptions::new().read(true).write(true).open(&lock_path) else {
        // A malformed root without a lock is not safely attributable to an
        // abandoned owner. Leave it for explicit cleanup rather than risk
        // deleting another process's live conversion input.
        return;
    };

    match lock_file.try_lock_exclusive() {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
            // A live owner still holds this process-root lock. Age alone is
            // not proof of abandonment; do not delete queued/paused work.
            return;
        }
        Err(_) => return,
    }

    if process_root_owner_is_live(process_root) {
        let _ = lock_file.unlock();
        return;
    }

    // Windows commonly rejects `remove_dir_all` when any file under the tree
    // is still open. Once the exclusive lock proves there is no live owner,
    // release and drop the handle before removing the abandoned process root.
    // New live owners never attach to an existing process root; each process
    // creates a fresh unique root. A racing scavenger can only race this same
    // best-effort deletion, which remains harmless and idempotent.
    let _ = lock_file.unlock();
    drop(lock_file);

    let _ = fs::remove_dir_all(process_root);
}

fn process_root_owner_is_live(process_root: &Path) -> bool {
    let pid_path = process_root.join(SYNTHETIC_CUE_ALBUM_PROCESS_PID);
    let Ok(text) = fs::read_to_string(pid_path) else {
        return false;
    };
    let Ok(pid) = text.trim().parse::<u32>() else {
        return false;
    };
    process_id_is_live(pid)
}

fn process_id_is_live(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }

    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(not(unix))]
    {
        false
    }
}

fn first_non_empty<'a>(values: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    values.map(str::trim).find(|value| !value.is_empty())
}

fn quote_cue_value(value: &str) -> String {
    value.replace('"', "'")
}

fn cue_timestamp(frames: u32) -> String {
    let minutes = frames / (60 * 75);
    let seconds = (frames / 75) % 60;
    let frame = frames % 75;
    format!("{minutes:02}:{seconds:02}:{frame:02}")
}

fn cue_file_type_for_queue(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());
    match ext.as_deref() {
        Some("flac") => "FLAC",
        Some("wv") => "WAVE",
        Some("aif") | Some("aiff") => "AIFF",
        Some("mp3") => "MP3",
        _ => "WAVE",
    }
}

fn path_key_is_under_any_root(path_key: &Path, root_keys: &HashSet<PathBuf>) -> bool {
    root_keys.iter().any(|root_key| path_key.starts_with(root_key))
}

fn mark_sibling_audio_as_cue_artifacts(
    cue_path: &Path,
    queueable_non_cue: &[PathBuf],
    cue_artifact_audio_keys: &mut HashSet<PathBuf>,
) {
    let Some(cue_parent) = cue_path.parent().map(queue_path_key) else {
        return;
    };

    for path in queueable_non_cue {
        if !is_audio_file_path(path) {
            continue;
        }
        let Some(audio_parent) = path.parent().map(queue_path_key) else {
            continue;
        };
        if audio_parent == cue_parent {
            cue_artifact_audio_keys.insert(queue_path_key(path));
        }
    }
}

fn collect_queue_candidates(path: &Path, plan: &mut QueueExpansionPlan) {
    if path.is_dir() {
        collect_queue_candidates_recursive(path, plan);
    } else {
        plan.add_explicit_file(path.to_path_buf());
    }
}

/// Recursively collect candidate queue inputs without deciding suppression.
/// Symlinks are skipped to avoid loops, matching the browse stats walk policy.
fn collect_queue_candidates_recursive(dir: &Path, plan: &mut QueueExpansionPlan) {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };

    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in read.flatten() {
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            dirs.push(path);
        } else {
            files.push(path);
        }
    }

    dirs.sort();
    files.sort();

    for file in files {
        plan.add_discovered_file(file);
    }
    for child in dirs {
        collect_queue_candidates_recursive(&child, plan);
    }
}

struct LimitedQueueExpansionState {
    max_visited: usize,
    visited: usize,
}

impl LimitedQueueExpansionState {
    fn visit(&mut self, path: &Path) -> Result<(), QueueExpansionLimitedError> {
        self.visited = self.visited.saturating_add(1);
        if self.visited > self.max_visited {
            return Err(QueueExpansionLimitedError::failed(
                format!(
                    "folder expansion for {} exceeded {} entries; narrow the selection or queue files directly",
                    path.display(),
                    self.max_visited
                ),
                self.visited,
            ));
        }
        Ok(())
    }
}

fn collect_queue_candidates_limited<F>(
    path: &Path,
    plan: &mut QueueExpansionPlan,
    state: &mut LimitedQueueExpansionState,
    is_cancelled: &mut F,
) -> Result<(), QueueExpansionLimitedError>
where
    F: FnMut() -> bool,
{
    if is_cancelled() {
        return Err(QueueExpansionLimitedError::cancelled(state.visited));
    }
    state.visit(path)?;
    if path.is_dir() {
        collect_queue_candidates_recursive_limited(path, path, plan, state, is_cancelled)
    } else {
        plan.add_explicit_file(path.to_path_buf());
        Ok(())
    }
}

fn collect_queue_candidates_recursive_limited<F>(
    root: &Path,
    dir: &Path,
    plan: &mut QueueExpansionPlan,
    state: &mut LimitedQueueExpansionState,
    is_cancelled: &mut F,
) -> Result<(), QueueExpansionLimitedError>
where
    F: FnMut() -> bool,
{
    if is_cancelled() {
        return Err(QueueExpansionLimitedError::cancelled(state.visited));
    }

    let read = fs::read_dir(dir).map_err(|err| {
        QueueExpansionLimitedError::failed(
            format!(
                "folder expansion for {} could not fully scan the tree: {}",
                root.display(),
                err
            ),
            state.visited,
        )
    })?;

    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in read {
        if is_cancelled() {
            return Err(QueueExpansionLimitedError::cancelled(state.visited));
        }
        let entry = entry.map_err(|err| {
            QueueExpansionLimitedError::failed(
                format!(
                    "folder expansion for {} could not fully scan the tree: {}",
                    root.display(),
                    err
                ),
                state.visited,
            )
        })?;
        let path = entry.path();
        state.visit(&path)?;
        let file_type = entry.file_type().map_err(|err| {
            QueueExpansionLimitedError::failed(
                format!(
                    "folder expansion for {} could not fully scan the tree: {}",
                    root.display(),
                    err
                ),
                state.visited,
            )
        })?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            dirs.push(path);
        } else {
            files.push(path);
        }
    }

    dirs.sort();
    files.sort();

    for file in files {
        plan.add_discovered_file(file);
    }
    for child in dirs {
        collect_queue_candidates_recursive_limited(root, &child, plan, state, is_cancelled)?;
    }

    Ok(())
}

#[derive(Debug)]
enum CueQueueDecision {
    /// The CUE provides track boundaries that are not represented by the
    /// referenced audio file set alone. Queue the CUE and suppress every audio
    /// file it references so the materializer is the single source of tracks.
    SplitSource { referenced_audio: Vec<PathBuf> },
    /// The CUE points one-to-one at already-split tracks. For folder expansion,
    /// queue the audio files and suppress the CUE as a metadata artifact. This
    /// also covers a one-track image CUE: with no split points to materialize,
    /// the image file itself is the queueable source and the CUE is metadata.
    MetadataArtifact { referenced_audio: Vec<PathBuf> },
}

#[derive(Debug)]
struct CueQueueAnalysis {
    referenced_audio: Vec<PathBuf>,
    track_count_by_audio_key: BTreeMap<PathBuf, usize>,
}

fn cue_queue_decision_for_path(cue_path: &Path) -> Result<CueQueueDecision, String> {
    let analysis = analyze_cue_for_queue(cue_path)?;

    // A CUE is a split source as soon as it provides split points for at least
    // one referenced audio file. Mixed layouts can also reference one-track
    // bonus files; once the CUE is a split source, suppress every referenced
    // audio file so the materializer owns the complete track index and the
    // queue never double-converts the one-track references.
    let has_split_source = analysis
        .track_count_by_audio_key
        .values()
        .any(|track_count| *track_count > 1);

    if has_split_source {
        Ok(CueQueueDecision::SplitSource {
            referenced_audio: analysis.referenced_audio,
        })
    } else {
        Ok(CueQueueDecision::MetadataArtifact {
            referenced_audio: analysis.referenced_audio,
        })
    }
}

/// Return audio paths that queue expansion should suppress for a CUE.
///
/// This is deliberately a suppression helper, not a generic "materializable"
/// query: metadata-artifact CUEs return an empty list, while split-source CUEs
/// return every referenced audio path. In mixed layouts that includes one-track
/// referenced files, because once the CUE provides split points for any audio
/// file, the materializer owns the complete CUE track index and raw audio paths
/// must not be queued separately.
#[cfg(test)]
pub(crate) fn cue_referenced_audio_paths_to_suppress_for_queue(
    cue_path: &Path,
) -> Result<Vec<PathBuf>, String> {
    match cue_queue_decision_for_path(cue_path)? {
        CueQueueDecision::SplitSource { referenced_audio } => Ok(referenced_audio),
        CueQueueDecision::MetadataArtifact { .. } => Ok(Vec::new()),
    }
}

fn analyze_cue_for_queue(cue_path: &Path) -> Result<CueQueueAnalysis, String> {
    let sheet = crate::convert::cue_parser::parse_cue_file(cue_path)
        .map_err(|err| format!("failed to parse CUE: {err}"))?;
    let parent = cue_path
        .parent()
        .ok_or_else(|| "CUE path has no parent directory".to_string())?;
    let parent_key = queue_path_key(parent);

    if sheet.tracks.is_empty() {
        return Err("CUE sheet has no tracks".to_string());
    }

    let mut referenced_audio = Vec::new();
    let mut referenced_audio_keys = HashSet::new();
    let mut resolved_tracks = Vec::with_capacity(sheet.tracks.len());
    let mut track_count_by_audio_key = BTreeMap::new();
    for track in &sheet.tracks {
        let index01 = track
            .index01_frames
            .ok_or_else(|| format!("track {} has no INDEX 01", track.number))?;
        let file_ref = track
            .file
            .as_deref()
            .ok_or_else(|| format!("track {} has no FILE reference", track.number))?;

        let resolved = match resolve_cue_file_reference_for_queue(parent, file_ref) {
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

        if !is_audio_file_path(&resolved) {
            return Err(format!(
                "track {} FILE reference {:?} did not resolve to a supported audio file: {}",
                track.number,
                file_ref,
                resolved.display()
            ));
        }

        // Folder expansion intentionally accepts only CUE references to audio
        // in the exact same directory as the CUE. Some valid CUE layouts keep
        // the image under a child directory, but the queue heuristic chooses a
        // conservative boundary here: cross-directory references are treated as
        // unsafe metadata artifacts so a folder conversion does not unexpectedly
        // materialize audio outside the CUE's sibling file set. Explicit CUE
        // selection is still honored by `into_queue_paths()`.
        if !is_same_directory_key_for_queue(&parent_key, &resolved) {
            return Err(format!(
                "track {} FILE reference {:?} resolved outside the CUE directory: {}",
                track.number,
                file_ref,
                resolved.display()
            ));
        }

        let resolved_key = queue_path_key(&resolved);
        if referenced_audio_keys.insert(resolved_key.clone()) {
            referenced_audio.push(resolved.clone());
        }
        *track_count_by_audio_key.entry(resolved_key).or_insert(0) += 1;
        resolved_tracks.push((track.number, resolved, index01));
    }

    validate_queue_cue_index_order(&resolved_tracks)?;

    Ok(CueQueueAnalysis {
        referenced_audio,
        track_count_by_audio_key,
    })
}

fn validate_queue_cue_index_order(resolved_tracks: &[(u32, PathBuf, u32)]) -> Result<(), String> {
    let mut previous_by_file: BTreeMap<PathBuf, (u32, u32)> = BTreeMap::new();
    for (track_number, path, index01) in resolved_tracks {
        let key = queue_path_key(path);
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
pub(crate) enum CueReferenceResolution {
    Resolved(PathBuf),
    Missing,
    Ambiguous(Vec<PathBuf>),
}

pub(crate) fn resolve_cue_file_reference_for_queue(parent: &Path, file_ref: &str) -> CueReferenceResolution {
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
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };

    let mut candidates: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_audio_file_path(path) && matches_reference(path))
        .collect();
    candidates.sort_by_key(|path| deterministic_path_sort_key(path));
    let mut seen = HashSet::new();
    candidates.retain(|path| seen.insert(queue_path_key(path)));
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

/// Stable key for queue path comparisons.
///
/// Existing files are canonicalized once before set/map operations so queue
/// expansion avoids repeated filesystem lookups in inner loops. The fallback
/// preserves the old behavior for paths that cannot be canonicalized.
pub(crate) fn queue_path_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Sort paths deterministically and deduplicate them by the same
/// canonicalize-with-fallback identity used by queue expansion. Use this for
/// UI snapshots and adapter boundaries only; the queue planner itself preserves
/// its collect-then-decide traversal order.
pub(crate) fn sort_dedup_paths_by_queue_identity(paths: &mut Vec<PathBuf>) {
    paths.sort();
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(queue_path_key(path)));
}

/// Return true when `candidate` has the same queue identity as any path in
/// `paths`. This intentionally uses `queue_path_key()` instead of raw PathBuf
/// equality so symlink/canonical-equivalent paths do not lose CUE metadata at
/// TUI adapter boundaries.
pub(crate) fn path_list_contains_queue_identity(paths: &[PathBuf], candidate: &Path) -> bool {
    let candidate_key = queue_path_key(candidate);
    paths
        .iter()
        .any(|path| queue_path_key(path) == candidate_key)
}

fn format_candidate_paths_for_log(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn push_unique_path_with_keys(
    paths: &mut Vec<PathBuf>,
    keys: &mut HashSet<PathBuf>,
    candidate: PathBuf,
) {
    if keys.insert(queue_path_key(&candidate)) {
        paths.push(candidate);
    }
}

#[cfg(test)]
pub(crate) fn path_list_contains(paths: &[PathBuf], candidate: &Path) -> bool {
    paths
        .iter()
        .any(|existing| same_path_for_queue(existing, candidate))
}

#[cfg(test)]
fn same_path_for_queue(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn is_same_directory_key_for_queue(left_key: &Path, right_file: &Path) -> bool {
    let Some(right) = right_file.parent() else {
        return false;
    };

    queue_path_key(right) == left_key
}




/// Map queue-expansion CUE-artifact metadata onto the sidecar policy for one
/// queued path. Shared by CLI and TUI queue construction so both front ends
/// apply identical CUE semantics.
pub fn cue_sidecar_override_for_commit_path(
    path: &Path,
    cue_artifact_audio: &HashSet<PathBuf>,
) -> Option<CueSidecarPolicy> {
    cue_artifact_audio
        .iter()
        .any(|candidate| same_queue_identity(candidate, path))
        .then_some(CueSidecarPolicy::EmbeddedOnly)
}

fn same_queue_identity(left: &Path, right: &Path) -> bool {
    queue_path_key(left) == queue_path_key(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_queue_split_cue_part(
        dir: &Path,
        stem: &str,
        title: &str,
        tracks: usize,
        header: &str,
        bom: bool,
        crlf: bool,
    ) -> (PathBuf, PathBuf) {
        let audio = dir.join(format!("{stem}.flac"));
        let cue = dir.join(format!("{stem}.cue"));
        std::fs::write(&audio, b"not real flac").expect("audio fixture");
        let mut body = String::new();
        body.push_str(header);
        body.push_str(&format!("TITLE \"{title}\"\n"));
        body.push_str(&format!("FILE \"{stem}.flac\" WAVE\n"));
        for idx in 0..tracks {
            let seconds = idx * 31;
            body.push_str(&format!(
                "  TRACK {:02} AUDIO\n    TITLE \"{stem} track {}\"\n    INDEX 01 {:02}:{:02}:00\n",
                idx + 1,
                idx + 1,
                seconds / 60,
                seconds % 60,
            ));
        }
        if crlf {
            body = body.replace('\n', "\r\n");
        }
        let mut bytes = Vec::new();
        if bom {
            bytes.extend_from_slice(&[0xef, 0xbb, 0xbf]);
        }
        bytes.extend_from_slice(body.as_bytes());
        std::fs::write(&cue, bytes).expect("cue fixture");
        (cue, audio)
    }

    fn generated_cue_track_numbers_are_continuous(sheet: &crate::convert::cue_parser::CueSheet) {
        for (idx, track) in sheet.tracks.iter().enumerate() {
            assert_eq!(
                track.number,
                (idx + 1) as u32,
                "generated CUE must renumber tracks continuously in album order",
            );
        }
    }

    #[derive(Clone, Copy)]
    struct QueueCueFixturePart {
        stem: &'static str,
        title: &'static str,
        tracks: usize,
        header: &'static str,
        bom: bool,
        crlf: bool,
    }

    fn registered_queue_cue_roundtrip_fixtures() -> Vec<Vec<QueueCueFixturePart>> {
        vec![
            vec![
                QueueCueFixturePart {
                    stem: "fixture_a",
                    title: "Registered Album Side A",
                    tracks: 2,
                    header: "PERFORMER \"Artist\"\nREM DATE 1973\n",
                    bom: true,
                    crlf: true,
                },
                QueueCueFixturePart {
                    stem: "fixture_b",
                    title: "Registered Album Side B",
                    tracks: 3,
                    header: "rem genre \"Rock\"\nREM COMMENT \"mixed REM fields are tolerated\"\n",
                    bom: false,
                    crlf: false,
                },
                QueueCueFixturePart {
                    stem: "fixture_c",
                    title: "Registered Album Side C",
                    tracks: 2,
                    header: "CATALOG 1234567890123\n",
                    bom: false,
                    crlf: true,
                },
            ],
            vec![
                QueueCueFixturePart {
                    stem: "quoted_one",
                    title: "Quoted Album Side A",
                    tracks: 2,
                    header: "PERFORMER \"Artist With \\\"Quote\\\"\"\n",
                    bom: false,
                    crlf: false,
                },
                QueueCueFixturePart {
                    stem: "quoted_two",
                    title: "Quoted Album Side B",
                    tracks: 2,
                    header: "REM GENRE \"Progressive Rock\"\n",
                    bom: true,
                    crlf: true,
                },
            ],
        ]
    }

    fn build_queue_roundtrip_parts(
        dir: &Path,
        fixture: &[QueueCueFixturePart],
    ) -> Vec<SyntheticCueAlbumPart> {
        let mut parts = Vec::new();
        for part in fixture {
            let (cue_path, _audio_path) = write_queue_split_cue_part(
                dir,
                part.stem,
                part.title,
                part.tracks,
                part.header,
                part.bom,
                part.crlf,
            );
            let sheet = crate::convert::cue_parser::parse_cue_file(&cue_path)
                .expect("registered CUE fixture parses");
            let referenced_audio = match cue_queue_decision_for_path(&cue_path)
                .expect("registered CUE fixture analyzes")
            {
                CueQueueDecision::SplitSource { referenced_audio } => referenced_audio,
                CueQueueDecision::MetadataArtifact { .. } => {
                    panic!("registered fixture must be a split-source CUE")
                }
            };
            parts.push(SyntheticCueAlbumPart {
                cue_key: queue_path_key(&cue_path),
                cue_path,
                sheet,
                referenced_audio,
            });
        }
        parts
    }

    #[test]
    fn all_registered_queue_cue_fixtures_parse_generate_parse_round_trip() {
        let fixtures = registered_queue_cue_roundtrip_fixtures();
        assert!(
            fixtures.len() >= 2,
            "round-trip property suite must cover multiple registered fixture shapes"
        );
        for (fixture_idx, fixture) in fixtures.iter().enumerate() {
            let td = tempfile::tempdir().expect("tempdir");
            let parts = build_queue_roundtrip_parts(td.path(), fixture);
            let generated = generate_queue_synthetic_cue_album(&parts)
                .expect("registered fixture generates a synthetic CUE");
            let reparsed = crate::convert::cue_parser::parse_cue(&generated);
            generated_cue_track_numbers_are_continuous(&reparsed);
            assert_eq!(
                reparsed.tracks.len(),
                fixture.iter().map(|part| part.tracks).sum::<usize>(),
                "fixture {fixture_idx} must preserve total track cardinality"
            );

            let regenerated_parts = vec![SyntheticCueAlbumPart {
                cue_key: PathBuf::from(format!("/roundtrip/fixture-{fixture_idx}.cue")),
                cue_path: td.path().join(format!("roundtrip-{fixture_idx}.cue")),
                sheet: reparsed,
                referenced_audio: parts
                    .iter()
                    .flat_map(|part| part.referenced_audio.iter().cloned())
                    .collect(),
            }];
            let regenerated = generate_queue_synthetic_cue_album(&regenerated_parts)
                .expect("registered fixture regenerates from parsed synthetic CUE");
            assert_eq!(
                generated, regenerated,
                "parse(generate(model)) must be byte-stable for registered CUE fixture {fixture_idx}"
            );
        }
    }

    fn registered_project_cue_fixture_files() -> Vec<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest = root.join("tests/fixtures/cue_roundtrip/manifest.txt");
        let text = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|err| panic!("CUE fixture manifest {} must be readable: {err}", manifest.display()));
        let mut out = Vec::new();
        for (line_no, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if trimmed.contains("..") || Path::new(trimmed).is_absolute() {
                panic!(
                    "CUE fixture manifest {} line {} must contain a safe relative path, got {trimmed:?}",
                    manifest.display(),
                    line_no + 1,
                );
            }
            let path = root.join(trimmed);
            assert!(
                path.exists(),
                "CUE fixture manifest {} line {} references missing fixture {}",
                manifest.display(),
                line_no + 1,
                path.display(),
            );
            out.push(path);
        }
        out.sort();
        out
    }

    #[test]
    fn synthetic_cue_scavenger_never_deletes_live_owner_roots() {
        let root = synthetic_cue_album_root();
        std::fs::create_dir_all(&root).expect("synthetic root");
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();

        let live_root = root.join(format!(
            "{SYNTHETIC_CUE_ALBUM_PROCESS_PREFIX}live-{}-{nonce}",
            std::process::id(),
        ));
        let abandoned_root = root.join(format!(
            "{SYNTHETIC_CUE_ALBUM_PROCESS_PREFIX}abandoned-{}-{nonce}",
            std::process::id(),
        ));
        for (process_root, pid_text) in [
            (&live_root, std::process::id().to_string()),
            (&abandoned_root, "not-a-live-pid".to_string()),
        ] {
            std::fs::create_dir_all(process_root).expect("process root");
            std::fs::write(process_root.join(SYNTHETIC_CUE_ALBUM_PROCESS_LOCK), b"lock")
                .expect("owner lock");
            std::fs::write(process_root.join(SYNTHETIC_CUE_ALBUM_PROCESS_PID), pid_text)
                .expect("owner pid");
            let artifact_dir = process_root.join(format!("{SYNTHETIC_CUE_ALBUM_ARTIFACT_PREFIX}fixture"));
            std::fs::create_dir_all(&artifact_dir).expect("artifact dir");
            std::fs::write(artifact_dir.join(SYNTHETIC_CUE_ALBUM_FILE), b"TITLE \"Album\"\n")
                .expect("artifact cue");
        }

        scavenge_synthetic_cue_album_artifacts_older_than(Duration::ZERO);

        assert!(
            live_root.exists(),
            "scavenging must not delete an artifact tree whose owner process is live"
        );
        assert!(
            !abandoned_root.exists(),
            "scavenging should remove old roots only after proving there is no live owner"
        );
        let _ = std::fs::remove_dir_all(live_root);
        sync_directory_best_effort(&root);
    }

    #[test]
    fn complete_registered_project_cue_fixture_corpus_participates_in_roundtrip_property() {
        let cue_files = registered_project_cue_fixture_files();
        assert!(
            cue_files.len() >= 3,
            "project CUE fixture corpus must be explicit, hermetic, and non-vacuous"
        );
        let mut materializable = 0usize;
        for cue_file in &cue_files {
            let sheet = crate::convert::cue_parser::parse_cue_file(cue_file)
                .unwrap_or_else(|err| panic!("project CUE fixture {} must parse: {err}", cue_file.display()));
            assert!(
                !sheet.tracks.is_empty(),
                "project CUE fixture {} must contain at least one track",
                cue_file.display(),
            );
            let referenced_audio = match cue_queue_decision_for_path(cue_file)
                .unwrap_or_else(|err| panic!("project CUE fixture {} must analyze: {err}", cue_file.display()))
            {
                CueQueueDecision::SplitSource { referenced_audio } => referenced_audio,
                CueQueueDecision::MetadataArtifact { .. } => {
                    panic!(
                        "project CUE fixture {} must be materializable for the round-trip property",
                        cue_file.display(),
                    )
                }
            };
            materializable += 1;
            let part = SyntheticCueAlbumPart {
                cue_key: queue_path_key(cue_file),
                cue_path: cue_file.clone(),
                sheet,
                referenced_audio,
            };
            let generated = generate_queue_synthetic_cue_album(&[part])
                .unwrap_or_else(|err| panic!("project CUE fixture {} must generate: {err}", cue_file.display()));
            let reparsed = crate::convert::cue_parser::parse_cue(&generated);
            generated_cue_track_numbers_are_continuous(&reparsed);
            let regenerated_part = SyntheticCueAlbumPart {
                cue_key: queue_path_key(cue_file),
                cue_path: cue_file.clone(),
                sheet: reparsed,
                referenced_audio: Vec::new(),
            };
            let regenerated = generate_queue_synthetic_cue_album(&[regenerated_part])
                .unwrap_or_else(|err| panic!("project CUE fixture {} must regenerate: {err}", cue_file.display()));
            assert_eq!(
                generated,
                regenerated,
                "project CUE fixture {} must be byte-stable under parse-generate-parse",
                cue_file.display(),
            );
        }
        assert_eq!(
            materializable,
            cue_files.len(),
            "every registered project CUE fixture must exercise the full parse-generate-parse property"
        );
    }

    #[test]
    fn folder_expansion_merges_three_parts_with_different_track_counts_and_hostile_headers() {
        let td = tempfile::tempdir().expect("tempdir");
        let (cue_a, audio_a) = write_queue_split_cue_part(
            td.path(),
            "part_a",
            "Album Side A",
            2,
            "REM GENRE \"Rock\"\nREM DATE 1973\nPERFORMER \"Artist A\"\nREM COMMENT \"kept but not structural\"\n",
            true,
            true,
        );
        let (cue_b, audio_b) = write_queue_split_cue_part(
            td.path(),
            "part_b",
            "Album Side B",
            3,
            "rem genre \"Progressive Rock\"\nPERFORMER \"Artist B\"\n",
            false,
            false,
        );
        let (cue_c, audio_c) = write_queue_split_cue_part(
            td.path(),
            "part_c",
            "Album Side C",
            4,
            "CATALOG 1234567890123\nREM DISCID ABCD1234\n",
            false,
            true,
        );

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);

        assert_eq!(expanded.expansion_errors, Vec::<String>::new());
        assert_eq!(expanded.paths.len(), 1, "N-part split CUE albums must queue one synthetic CUE");
        assert_eq!(expanded.synthetic_cue_artifacts.len(), 1);
        for path in [&cue_a, &cue_b, &cue_c, &audio_a, &audio_b, &audio_c] {
            assert!(
                !path_list_contains(&expanded.paths, path),
                "merged folder conversion must not also queue member path {}",
                path.display(),
            );
        }
        let synthetic = std::fs::read_to_string(&expanded.paths[0]).expect("synthetic cue text");
        let parsed = crate::convert::cue_parser::parse_cue(&synthetic);
        assert_eq!(parsed.title.as_deref(), Some("Album"));
        assert_eq!(parsed.tracks.len(), 9, "2+3+4 member tracks must flatten to nine album tracks");
        generated_cue_track_numbers_are_continuous(&parsed);
        assert!(synthetic.contains("TRACK 09 AUDIO"));
        assert!(synthetic.contains(&audio_a.display().to_string()));
        assert!(synthetic.contains(&audio_b.display().to_string()));
        assert!(synthetic.contains(&audio_c.display().to_string()));
        assert!(
            synthetic.contains("FILE") && synthetic.contains("TRACK 03 AUDIO\n    TITLE \"part_b track 1\"\n    PERFORMER \"Artist B\"\n    INDEX 01 00:00:00"),
            "INDEX times must remain local to each member image, not accumulated: {synthetic}",
        );
        cleanup_synthetic_cue_artifacts(&expanded.synthetic_cue_artifacts);
    }

    #[test]
    fn generated_queue_synthetic_cue_parse_generate_parse_round_trips_fixture_matrix() {
        let td = tempfile::tempdir().expect("tempdir");
        let mut parts = Vec::new();
        for (stem, title, tracks, header, bom, crlf) in [
            (
                "one",
                "Fixture Side A",
                2usize,
                "PERFORMER \"Artist\"\nREM DATE 1973\n",
                true,
                true,
            ),
            (
                "two",
                "Fixture Side B",
                3usize,
                "REM GENRE \"Rock\"\nREM COMMENT \"mixed REM fields are tolerated\"\n",
                false,
                false,
            ),
            (
                "three",
                "Fixture Side C",
                2usize,
                "CATALOG 1234567890123\n",
                false,
                true,
            ),
        ] {
            let (cue_path, _audio_path) = write_queue_split_cue_part(td.path(), stem, title, tracks, header, bom, crlf);
            let sheet = crate::convert::cue_parser::parse_cue_file(&cue_path).expect("parse member cue");
            let referenced_audio = match cue_queue_decision_for_path(&cue_path).expect("analyze member cue") {
                CueQueueDecision::SplitSource { referenced_audio } => referenced_audio,
                CueQueueDecision::MetadataArtifact { .. } => panic!("fixture must be a split-source CUE"),
            };
            parts.push(SyntheticCueAlbumPart {
                cue_key: queue_path_key(&cue_path),
                cue_path,
                sheet,
                referenced_audio,
            });
        }

        let generated = generate_queue_synthetic_cue_album(&parts).expect("generate synthetic cue");
        let reparsed = crate::convert::cue_parser::parse_cue(&generated);
        let regenerated_parts = vec![SyntheticCueAlbumPart {
            cue_key: PathBuf::from("/roundtrip/generated.cue"),
            cue_path: td.path().join("roundtrip.cue"),
            sheet: reparsed.clone(),
            referenced_audio: parts
                .iter()
                .flat_map(|part| part.referenced_audio.iter().cloned())
                .collect(),
        }];
        let regenerated = generate_queue_synthetic_cue_album(&regenerated_parts)
            .expect("regenerate from parsed synthetic cue");

        assert_eq!(reparsed.tracks.len(), 7);
        generated_cue_track_numbers_are_continuous(&reparsed);
        assert_eq!(generated, regenerated, "parse(generate(model)) must be byte-stable for the generated model");
    }

    #[test]
    fn folder_expansion_merges_same_album_split_cue_parts_into_one_synthetic_cue() {
        let td = tempfile::tempdir().expect("tempdir");
        let a = td.path().join("side_a.flac");
        let b = td.path().join("side_b.flac");
        let cue_a = td.path().join("side_a.cue");
        let cue_b = td.path().join("side_b.cue");
        std::fs::write(&a, b"not real flac").unwrap();
        std::fs::write(&b, b"not real flac").unwrap();
        std::fs::write(
            &cue_a,
            r#"TITLE "Album Side A"
FILE "side_a.flac" WAVE
  TRACK 01 AUDIO
    TITLE "A1"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "A2"
    INDEX 01 03:00:00
"#,
        )
        .unwrap();
        std::fs::write(
            &cue_b,
            r#"TITLE "Album Side B"
FILE "side_b.flac" WAVE
  TRACK 01 AUDIO
    TITLE "B1"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "B2"
    INDEX 01 02:30:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert_eq!(expanded.paths.len(), 1, "folder conversion should queue one synthetic album CUE");
        assert_eq!(expanded.synthetic_cue_artifacts.len(), 1, "synthetic CUE path must be handed to lifecycle owners");
        assert!(is_cue_sheet_path(&expanded.paths[0]));
        assert!(expanded.synthetic_cue_artifacts.contains(&expanded.paths[0]));
        assert!(!path_list_contains(&expanded.paths, &cue_a));
        assert!(!path_list_contains(&expanded.paths, &cue_b));
        assert!(!path_list_contains(&expanded.paths, &a));
        assert!(!path_list_contains(&expanded.paths, &b));
        let synthetic = std::fs::read_to_string(&expanded.paths[0]).expect("synthetic cue text");
        assert!(synthetic.contains("TRACK 04 AUDIO"));
        assert!(synthetic.contains("TITLE \"Album\""), "synthetic CUE should use the reconciled album title, not a side title: {synthetic}");
        assert!(!synthetic.contains("TITLE \"Album Side A\""));
        assert!(!synthetic.contains("TITLE \"Album Side B\""));
        assert!(synthetic.contains(&a.display().to_string()));
        assert!(synthetic.contains(&b.display().to_string()));
        cleanup_synthetic_cue_artifacts(&expanded.synthetic_cue_artifacts);
        assert!(!expanded.paths[0].exists(), "synthetic artifact cleanup removes its owner directory");
    }

    #[test]
    fn folder_expansion_fails_closed_when_merged_cue_group_exceeds_track_limit() {
        let td = tempfile::tempdir().expect("tempdir");
        let a = td.path().join("side_a.flac");
        let b = td.path().join("side_b.flac");
        let cue_a = td.path().join("side_a.cue");
        let cue_b = td.path().join("side_b.cue");
        std::fs::write(&a, b"not real flac").unwrap();
        std::fs::write(&b, b"not real flac").unwrap();

        fn many_track_cue(title: &str, image: &str, first: usize, count: usize) -> String {
            let mut text = format!("TITLE \"{title}\"\nFILE \"{image}\" WAVE\n");
            for n in first..first + count {
                text.push_str(&format!("  TRACK {:02} AUDIO\n    INDEX 01 {:02}:00:00\n", ((n - first) % 99) + 1, n));
            }
            text
        }

        std::fs::write(&cue_a, many_track_cue("Album Side A", "side_a.flac", 0, 50)).unwrap();
        std::fs::write(&cue_b, many_track_cue("Album Side B", "side_b.flac", 50, 50)).unwrap();

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert!(expanded.paths.is_empty(), "track-limit failure must not fall back to side CUEs or raw images: {:?}", expanded.paths);
        assert!(expanded.cue_artifact_audio.is_empty());
        assert!(expanded.expansion_errors.iter().any(|err| err.contains("at most 99")), "expected a user-facing track-limit error, got {:?}", expanded.expansion_errors);

        let err = expand_paths_to_audio_with_metadata_limited(
            &[td.path().to_path_buf()],
            1_000,
            || false,
        )
        .expect_err("limited production expansion should surface the fatal planning error");
        assert!(!err.cancelled);
        assert!(err.message.contains("at most 99"));
    }


    #[test]
    fn folder_expansion_preserves_unrelated_work_when_one_synthetic_group_fails_closed() {
        let td = tempfile::tempdir().expect("tempdir");
        let bad = td.path().join("bad-disc");
        let good = td.path().join("good-disc");
        std::fs::create_dir_all(&bad).expect("bad dir");
        std::fs::create_dir_all(&good).expect("good dir");
        let standalone = td.path().join("standalone.flac");
        std::fs::write(&standalone, b"not real flac").unwrap();

        let bad_a = bad.join("side_a.flac");
        let bad_b = bad.join("side_b.flac");
        std::fs::write(&bad_a, b"not real flac").unwrap();
        std::fs::write(&bad_b, b"not real flac").unwrap();
        fn many_track_cue(title: &str, image: &str, first: usize, count: usize) -> String {
            let mut text = format!("TITLE \"{title}\"\nFILE \"{image}\" WAVE\n");
            for n in first..first + count {
                text.push_str(&format!("  TRACK {:02} AUDIO\n    INDEX 01 {:02}:00:00\n", ((n - first) % 99) + 1, n));
            }
            text
        }
        std::fs::write(bad.join("side_a.cue"), many_track_cue("Bad Side A", "side_a.flac", 0, 50)).unwrap();
        std::fs::write(bad.join("side_b.cue"), many_track_cue("Bad Side B", "side_b.flac", 50, 50)).unwrap();

        let (good_cue_a, good_audio_a) = write_queue_split_cue_part(
            &good,
            "side_a",
            "Good Side A",
            2,
            "",
            false,
            false,
        );
        let (good_cue_b, good_audio_b) = write_queue_split_cue_part(
            &good,
            "side_b",
            "Good Side B",
            2,
            "",
            false,
            false,
        );

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);

        assert!(
            expanded.expansion_errors.iter().any(|err| err.contains("at most 99")),
            "failed synthetic group must be reported as a planner warning/error, got {:?}",
            expanded.expansion_errors
        );
        assert_eq!(
            expanded.synthetic_cue_artifacts.len(),
            1,
            "the unrelated valid split-CUE group must still produce its synthetic album artifact"
        );
        let synthetic = expanded.synthetic_cue_artifacts.iter().next().unwrap().clone();
        assert!(path_list_contains(&expanded.paths, &synthetic));
        assert!(path_list_contains(&expanded.paths, &standalone));
        for blocked in [bad.join("side_a.cue"), bad.join("side_b.cue"), bad_a, bad_b] {
            assert!(
                !path_list_contains(&expanded.paths, &blocked),
                "fail-closed group member must not leak into the partial queue: {}",
                blocked.display()
            );
        }
        for merged_member in [good_cue_a, good_cue_b, good_audio_a, good_audio_b] {
            assert!(
                !path_list_contains(&expanded.paths, &merged_member),
                "successfully merged group must not also queue member path {}",
                merged_member.display()
            );
        }
        cleanup_synthetic_cue_artifacts(&expanded.synthetic_cue_artifacts);
    }


    #[test]
    fn folder_expansion_preserves_unrelated_per_cue_fallback_when_one_synthetic_group_fails_closed() {
        let td = tempfile::tempdir().expect("tempdir");
        let bad = td.path().join("bad-disc");
        let fallback_parent = td.path().join("12\" Mixes");
        std::fs::create_dir_all(&bad).expect("bad dir");
        if let Err(err) = std::fs::create_dir(&fallback_parent) {
            eprintln!("skipping quoted-path fixture: {err}");
            return;
        }

        let bad_a = bad.join("side_a.flac");
        let bad_b = bad.join("side_b.flac");
        std::fs::write(&bad_a, b"not real flac").unwrap();
        std::fs::write(&bad_b, b"not real flac").unwrap();
        fn many_track_cue(title: &str, image: &str, first: usize, count: usize) -> String {
            let mut text = format!("TITLE \"{title}\"\nFILE \"{image}\" WAVE\n");
            for n in first..first + count {
                text.push_str(&format!("  TRACK {:02} AUDIO\n    INDEX 01 {:02}:00:00\n", ((n - first) % 99) + 1, n));
            }
            text
        }
        std::fs::write(bad.join("side_a.cue"), many_track_cue("Bad Side A", "side_a.flac", 0, 50)).unwrap();
        std::fs::write(bad.join("side_b.cue"), many_track_cue("Bad Side B", "side_b.flac", 50, 50)).unwrap();

        let fallback_a = fallback_parent.join("side_a.flac");
        let fallback_b = fallback_parent.join("side_b.flac");
        let fallback_cue_a = fallback_parent.join("side_a.cue");
        let fallback_cue_b = fallback_parent.join("side_b.cue");
        std::fs::write(&fallback_a, b"not real flac").unwrap();
        std::fs::write(&fallback_b, b"not real flac").unwrap();
        for (cue, image, title) in [
            (&fallback_cue_a, "side_a.flac", "Fallback Side A"),
            (&fallback_cue_b, "side_b.flac", "Fallback Side B"),
        ] {
            std::fs::write(
                cue,
                format!(
                    r#"TITLE "{title}"
FILE "{image}" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 01:00:00
"#,
                ),
            )
            .unwrap();
        }

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);

        assert!(expanded.synthetic_cue_artifacts.is_empty());
        assert!(expanded.expansion_errors.iter().any(|err| err.contains("at most 99")));
        assert!(expanded.expansion_errors.iter().any(|err| err.contains("double quote")));
        assert!(path_list_contains(&expanded.paths, &fallback_cue_a));
        assert!(path_list_contains(&expanded.paths, &fallback_cue_b));
        assert!(!path_list_contains(&expanded.paths, &fallback_a));
        assert!(!path_list_contains(&expanded.paths, &fallback_b));
        for blocked in [bad.join("side_a.cue"), bad.join("side_b.cue"), bad_a, bad_b] {
            assert!(
                !path_list_contains(&expanded.paths, &blocked),
                "failed group member must not leak into the partial queue: {}",
                blocked.display()
            );
        }
    }

    #[test]
    fn folder_expansion_fails_closed_when_merged_cue_group_cannot_be_parsed() {
        let td = tempfile::tempdir().expect("tempdir");
        let a = td.path().join("side_a.flac");
        let b = td.path().join("side_b.flac");
        let cue_a = td.path().join("side_a.cue");
        let cue_b = td.path().join("side_b.cue");
        std::fs::write(&a, b"not real flac").unwrap();
        std::fs::write(&b, b"not real flac").unwrap();
        std::fs::write(
            &cue_a,
            r#"TITLE "Album Side A"
FILE "side_a.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 01:00:00
"#,
        )
        .unwrap();
        std::fs::write(&cue_b, [0xff, 0xfe, 0x00]).unwrap();

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert!(expanded.paths.is_empty(), "parse failure must not fall back to side CUEs or raw images: {:?}", expanded.paths);
        assert!(expanded.expansion_errors.iter().any(|err| err.contains("failed to parse") || err.contains("failed to analyze")), "expected a parse/analyze error, got {:?}", expanded.expansion_errors);
    }


    #[test]
    fn folder_expansion_declines_synthetic_merge_when_member_image_absolute_path_contains_quote() {
        let td = tempfile::tempdir().expect("tempdir");
        let quoted = td.path().join("12\" Mixes");
        if let Err(err) = std::fs::create_dir(&quoted) {
            eprintln!("skipping quoted-path fixture: {err}");
            return;
        }
        let a = quoted.join("side_a.flac");
        let b = quoted.join("side_b.flac");
        let cue_a = quoted.join("side_a.cue");
        let cue_b = quoted.join("side_b.cue");
        std::fs::write(&a, b"not real flac").unwrap();
        std::fs::write(&b, b"not real flac").unwrap();
        for (cue, image, title) in [(&cue_a, "side_a.flac", "Album Side A"), (&cue_b, "side_b.flac", "Album Side B")] {
            std::fs::write(
                cue,
                format!(
                    r#"TITLE "{title}"
FILE "{image}" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 01:00:00
"#,
                ),
            )
            .unwrap();
        }

        let expanded = expand_paths_to_audio_with_metadata(&[quoted.clone()]);
        assert!(
            expanded.synthetic_cue_artifacts.is_empty(),
            "quoted absolute member paths cannot be represented safely in a generated FILE line"
        );
        assert!(
            expanded.expansion_errors.iter().any(|err| err.contains("double quote") && err.contains("Falling back to per-CUE")),
            "expected quoted-path fallback warning, got {:?}",
            expanded.expansion_errors
        );
        assert!(path_list_contains(&expanded.paths, &cue_a), "side A CUE should survive fallback");
        assert!(path_list_contains(&expanded.paths, &cue_b), "side B CUE should survive fallback");
        assert!(!path_list_contains(&expanded.paths, &a), "member image should stay suppressed by its side CUE");
        assert!(!path_list_contains(&expanded.paths, &b), "member image should stay suppressed by its side CUE");
    }

    #[test]
    fn explicit_single_cue_selection_bypasses_synthetic_album_grouping() {
        let td = tempfile::tempdir().expect("tempdir");
        let a = td.path().join("side_a.flac");
        let b = td.path().join("side_b.flac");
        let cue_a = td.path().join("side_a.cue");
        let cue_b = td.path().join("side_b.cue");
        std::fs::write(&a, b"not real flac").unwrap();
        std::fs::write(&b, b"not real flac").unwrap();
        for (cue, image, title) in [(&cue_a, "side_a.flac", "Album Side A"), (&cue_b, "side_b.flac", "Album Side B")] {
            std::fs::write(
                cue,
                format!(
                    r#"TITLE "{title}"
FILE "{image}" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 01:00:00
"#,
                ),
            )
            .unwrap();
        }

        let expanded = expand_paths_to_audio(&[cue_a.clone()]);
        assert_eq!(expanded.len(), 1);
        assert!(path_list_contains(&expanded, &cue_a));
        assert!(!path_list_contains(&expanded, &cue_b));
        let queued = crate::convert::cue_parser::parse_cue_file(&expanded[0])
            .expect("explicit cue remains parseable after bypass");
        assert_eq!(
            queued.title.as_deref(),
            Some("Album Side A"),
            "explicit single-CUE conversion must preserve the selected side's own album title"
        );
    }

    #[test]
    fn expand_paths_to_audio_suppresses_child_directory_split_source_cue_by_design() {
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
  TRACK 02 AUDIO
    INDEX 01 03:12:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        // Same-directory references are required by design. This suppresses
        // some materializable layouts, such as `album.cue` + `disc/image.flac`,
        // in favor of queueing discovered audio files without crossing the
        // CUE directory boundary.
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &loose));
        assert!(path_list_contains(&expanded, &image));
    }

    #[test]
    fn expand_paths_to_audio_suppresses_cue_that_references_external_audio() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue_dir = td.path().join("cue_dir");
        std::fs::create_dir(&cue_dir).unwrap();
        let external = td.path().join("external.flac");
        let cue = cue_dir.join("album.cue");
        std::fs::write(&external, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "../external.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:12:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[cue_dir, external.clone()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &external));
    }

    #[test]
    fn expand_paths_to_audio_suppresses_ambiguous_cue_and_keeps_audio() {
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

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &flac));
        assert!(path_list_contains(&expanded, &wav));
    }

    #[test]
    fn expand_paths_to_audio_suppresses_cue_with_subdirectory_reference() {
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
  TRACK 02 AUDIO
    INDEX 01 03:12:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &image));
    }

    #[test]
    fn expand_paths_to_audio_suppresses_cue_missing_index01_and_keeps_audio() {
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

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &image));
    }

    #[test]
    fn expand_paths_to_audio_suppresses_non_increasing_cue_and_keeps_audio() {
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

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &image));
    }


    #[test]
    fn materializable_cue_suppresses_per_track_cue_and_keeps_stem_matched_audio() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let track1 = td.path().join("01.flac");
        let track2 = td.path().join("02.opus");
        std::fs::write(&track1, b"not real flac").unwrap();
        std::fs::write(&track2, b"not real opus").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "01.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
FILE "02.wav" WAVE
  TRACK 02 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("per-track CUE should be materializer-compatible");
        assert!(referenced.is_empty());

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &track1));
        assert!(path_list_contains(&expanded, &track2));
    }

    #[test]
    fn materializable_cue_suppresses_frostbite_style_per_track_cue() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("Frostbite.cue");
        let track1 = td.path().join("01 - If You Love Me Like You Say.flac");
        let track2 = td.path().join("02 - Blue Monday Hangover.flac");
        std::fs::write(&track1, b"not real flac").unwrap();
        std::fs::write(&track2, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "01 - If You Love Me Like You Say.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 00 04:06:70
FILE "02 - Blue Monday Hangover.wav" WAVE
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("Frostbite-style per-track CUE should be materializer-compatible");
        assert!(referenced.is_empty());

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &track1));
        assert!(path_list_contains(&expanded, &track2));
    }

    #[test]
    fn materializable_cue_returns_single_image_stem_matched_audio_for_suppression() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:12:00
"#,
        )
        .unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("single-image CUE should be materializer-compatible");
        assert_eq!(referenced.len(), 1);
        assert!(path_list_contains(&referenced, &image));

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded, &cue));
        assert!(!path_list_contains(&expanded, &image));
    }

    #[test]
    fn materializable_cue_returns_each_shared_multi_image_audio_for_suppression() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let side_a = td.path().join("side-a.flac");
        let side_b = td.path().join("side-b.wv");
        std::fs::write(&side_a, b"not real flac").unwrap();
        std::fs::write(&side_b, b"not real wavpack").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "side-a.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:12:00
  TRACK 03 AUDIO
    INDEX 01 07:24:00
FILE "side-b.wav" WAVE
  TRACK 04 AUDIO
    INDEX 01 00:00:00
  TRACK 05 AUDIO
    INDEX 01 04:00:00
  TRACK 06 AUDIO
    INDEX 01 08:00:00
"#,
        )
        .unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("multi-image CUE should be materializer-compatible");
        assert_eq!(referenced.len(), 2);
        assert!(path_list_contains(&referenced, &side_a));
        assert!(path_list_contains(&referenced, &side_b));

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded, &cue));
        assert!(!path_list_contains(&expanded, &side_a));
        assert!(!path_list_contains(&expanded, &side_b));
    }

    #[test]
    fn materializable_cue_with_any_shared_file_is_split_source_and_suppresses_all_references() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let main = td.path().join("album-main.flac");
        let bonus = td.path().join("09 - Bonus Track.flac");
        let live = td.path().join("10 - Live Version.aac");
        std::fs::write(&main, b"not real flac").unwrap();
        std::fs::write(&bonus, b"not real flac").unwrap();
        std::fs::write(&live, b"not real aac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album-main.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:00:00
  TRACK 03 AUDIO
    INDEX 01 06:00:00
  TRACK 04 AUDIO
    INDEX 01 09:00:00
  TRACK 05 AUDIO
    INDEX 01 12:00:00
  TRACK 06 AUDIO
    INDEX 01 15:00:00
  TRACK 07 AUDIO
    INDEX 01 18:00:00
  TRACK 08 AUDIO
    INDEX 01 21:00:00
FILE "09 - Bonus Track.wav" WAVE
  TRACK 09 AUDIO
    INDEX 01 00:00:00
FILE "10 - Live Version.wav" WAVE
  TRACK 10 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("mixed-layout CUE should be materializer-compatible");
        assert_eq!(referenced.len(), 3);
        assert!(path_list_contains(&referenced, &main));
        assert!(path_list_contains(&referenced, &bonus));
        assert!(path_list_contains(&referenced, &live));

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded, &cue));
        assert!(!path_list_contains(&expanded, &main));
        assert!(!path_list_contains(&expanded, &bonus));
        assert!(!path_list_contains(&expanded, &live));
        assert!(expanded.cue_artifact_audio.is_empty());
    }


    #[test]
    fn expand_paths_to_audio_queues_tracks_and_suppresses_twelve_track_cue_artifact() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let mut cue_text = String::new();
        let mut tracks = Vec::new();

        for number in 1..=12 {
            let stem = format!("{number:02}");
            let audio = td.path().join(format!("{stem}.flac"));
            std::fs::write(&audio, b"not real flac").unwrap();
            tracks.push(audio);
            cue_text.push_str(&format!(
                "FILE \"{stem}.wav\" WAVE\n  TRACK {number:02} AUDIO\n    INDEX 01 00:00:00\n"
            ));
        }

        std::fs::write(&cue, cue_text).unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("per-track CUE should be materializer-compatible");
        assert!(referenced.is_empty());

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert_eq!(expanded.len(), tracks.len());
        assert!(!path_list_contains(&expanded, &cue));
        for track in tracks {
            assert!(path_list_contains(&expanded, &track));
            assert!(
                expanded.cue_artifact_audio.contains(&track),
                "per-track audio must carry EmbeddedOnly override metadata"
            );
        }
    }

    #[test]
    fn expand_paths_to_audio_marks_sibling_audio_when_nonexplicit_cue_errors() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("broken.cue");
        let track1 = td.path().join("01.flac");
        let track2 = td.path().join("02.flac");
        let nested = td.path().join("nested");
        let nested_track = nested.join("03.flac");

        std::fs::create_dir(&nested).unwrap();
        std::fs::write(&track1, b"not real flac").unwrap();
        std::fs::write(&track2, b"not real flac").unwrap();
        std::fs::write(&nested_track, b"not real flac").unwrap();
        std::fs::write(&cue, "this is not a cue sheet").unwrap();

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &track1));
        assert!(path_list_contains(&expanded, &track2));
        assert!(path_list_contains(&expanded, &nested_track));
        assert!(
            expanded.cue_artifact_audio.contains(&track1),
            "audio next to an error-classified non-explicit CUE must carry EmbeddedOnly override metadata"
        );
        assert!(
            expanded.cue_artifact_audio.contains(&track2),
            "audio next to an error-classified non-explicit CUE must carry EmbeddedOnly override metadata"
        );
        assert!(
            !expanded.cue_artifact_audio.contains(&nested_track),
            "CUE error fallback only applies to sibling audio that downstream sidecar discovery could associate with the CUE"
        );
    }

    #[test]
    fn explicit_split_source_cues_queue_side_cues_and_suppress_side_images() {
        let td = tempfile::tempdir().expect("tempdir");
        let side_a_cue = td.path().join("side_a.cue");
        let side_b_cue = td.path().join("side_b.cue");
        let side_a = td.path().join("side_a.wav");
        let side_b = td.path().join("side_b.wav");
        std::fs::write(&side_a, b"not real wav").unwrap();
        std::fs::write(&side_b, b"not real wav").unwrap();
        std::fs::write(
            &side_a_cue,
            r#"FILE "side_a.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:00:00
"#,
        )
        .unwrap();
        std::fs::write(
            &side_b_cue,
            r#"FILE "side_b.wav" WAVE
  TRACK 03 AUDIO
    INDEX 01 00:00:00
  TRACK 04 AUDIO
    INDEX 01 04:00:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[side_a_cue.clone(), side_b_cue.clone()]);
        assert!(path_list_contains(&expanded, &side_a_cue));
        assert!(path_list_contains(&expanded, &side_b_cue));
        assert!(!path_list_contains(&expanded, &side_a));
        assert!(!path_list_contains(&expanded, &side_b));
    }


    #[test]
    fn split_source_cue_supported_decode_only_images_queue_via_cue() {
        for (ext, cue_type) in [
            ("ape", "WAVE"),
            ("dsf", "WAVE"),
            ("dff", "WAVE"),
            ("shn", "WAVE"),
            ("ogg", "WAVE"),
            ("tta", "WAVE"),
        ] {
            let td = tempfile::tempdir().expect("tempdir");
            let cue = td.path().join(format!("album-{ext}.cue"));
            let image = td.path().join(format!("album-{ext}.{ext}"));
            std::fs::write(&image, b"not real audio").unwrap();
            std::fs::write(
                &cue,
                format!(
                    "FILE \"{}\" {cue_type}\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n",
                    image.file_name().unwrap().to_string_lossy()
                ),
            )
            .unwrap();

            let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
            assert!(path_list_contains(&expanded, &cue), "{ext}");
            assert!(!path_list_contains(&expanded, &image), "{ext}");

            let metadata_view = expand_paths_to_all_audio(&[td.path().to_path_buf()]);
            assert!(path_list_contains(&metadata_view, &image), "{ext}");
        }
    }

    #[test]
    fn expand_paths_to_audio_suppresses_unparseable_cue_and_keeps_audio() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("broken.cue");
        let audio = td.path().join("track.flac");
        std::fs::write(&cue, b"this is not a cue sheet").unwrap();
        std::fs::write(&audio, b"not real flac").unwrap();

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &audio));
    }

    #[test]
    fn expand_paths_to_audio_always_queues_explicit_cue_selection() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("broken.cue");
        std::fs::write(&cue, b"this is not a cue sheet").unwrap();

        let expanded = expand_paths_to_audio(&[cue.clone()]);
        assert_eq!(expanded.len(), 1);
        assert!(path_list_contains(&expanded, &cue));
    }


    #[test]
    fn split_source_cue_resolves_case_insensitive_stem_matched_audio() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let image = td.path().join("album.FLAC");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "ALBUM.WAV" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:12:00
"#,
        )
        .unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("case-insensitive stem match should resolve the image");
        assert_eq!(referenced.len(), 1);
        assert!(path_list_contains(&referenced, &image));

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded, &cue));
        assert!(!path_list_contains(&expanded, &image));
    }

    #[test]
    fn two_split_source_cues_can_reference_same_image_and_suppress_it_once() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue_a = td.path().join("album-main.cue");
        let cue_b = td.path().join("album-alt.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue_a,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:00:00
"#,
        )
        .unwrap();
        std::fs::write(
            &cue_b,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 04:00:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded, &cue_a));
        assert!(path_list_contains(&expanded, &cue_b));
        assert!(!path_list_contains(&expanded, &image));
    }

    #[test]
    fn split_source_cue_suppresses_audio_shared_with_artifact_cue() {
        let td = tempfile::tempdir().expect("tempdir");
        let split_cue = td.path().join("album.cue");
        let artifact_cue = td.path().join("album-index.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &split_cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:00:00
"#,
        )
        .unwrap();
        std::fs::write(
            &artifact_cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded, &split_cue));
        assert!(!path_list_contains(&expanded, &artifact_cue));
        assert!(!path_list_contains(&expanded, &image));
    }

    #[test]
    fn one_track_image_cue_is_metadata_artifact_by_design() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("one-track image CUE should parse but provide no split points");
        assert!(referenced.is_empty());

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert_eq!(expanded.len(), 1);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &image));
    }

    #[test]
    fn explicit_split_source_cue_suppresses_explicit_audio_by_design() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
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

        let expanded = expand_paths_to_audio(&[cue.clone(), image.clone()]);
        assert_eq!(expanded.len(), 1);
        assert!(path_list_contains(&expanded, &cue));
        assert!(!path_list_contains(&expanded, &image));
    }
}

#[cfg(test)]
mod bulk_audio_count_tests {
    use super::*;

    #[test]
    fn count_audio_files_bounded_returns_limit_plus_one_for_large_flat_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        for idx in 0..75 {
            std::fs::write(
                temp.path().join(format!("track-{idx:03}.flac")),
                b"extension-only audio fixture",
            )
            .expect("audio fixture");
        }
        for idx in 0..20 {
            std::fs::write(temp.path().join(format!("note-{idx:03}.txt")), b"not audio")
                .expect("non-audio fixture");
        }

        assert_eq!(
            count_audio_files_bounded(&[temp.path().to_path_buf()], 50),
            51,
            "guard counting should cap at limit + 1 instead of computing a full exact count"
        );
    }

    #[test]
    fn count_audio_files_bounded_counts_nested_audio_without_following_directory_symlinks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nested = temp.path().join("disc-one");
        std::fs::create_dir_all(&nested).expect("nested dir fixture");
        std::fs::write(nested.join("track.flac"), b"extension-only audio fixture")
            .expect("nested audio fixture");

        #[cfg(unix)]
        {
            let linked = temp.path().join("linked-disc");
            std::os::unix::fs::symlink(&nested, &linked).expect("directory symlink fixture");
        }

        assert_eq!(count_audio_files_bounded(&[temp.path().to_path_buf()], 50), 1);
    }
}


#[cfg(test)]
mod limited_queue_expansion_tests {
    use super::*;

    #[test]
    fn limited_expansion_uses_canonical_cue_artifact_metadata() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let track = td.path().join("album.flac");
        std::fs::write(&track, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let (expanded, visited) = expand_paths_to_audio_with_metadata_limited(
            &[td.path().to_path_buf()],
            64,
            || false,
        )
        .expect("bounded expansion should succeed");

        assert!(visited > 0);
        assert_eq!(expanded.paths.len(), 1);
        assert!(path_list_contains(&expanded.paths, &track));
        assert!(expanded.cue_artifact_audio.contains(&track));
    }

    #[test]
    fn limited_expansion_reports_cancellation_without_partial_queue() {
        let td = tempfile::tempdir().expect("tempdir");
        std::fs::write(td.path().join("track.flac"), b"not real flac").unwrap();

        let err = expand_paths_to_audio_with_metadata_limited(
            &[td.path().to_path_buf()],
            64,
            || true,
        )
        .expect_err("pre-cancelled expansion should stop before publishing a plan");

        assert!(err.cancelled);
        assert_eq!(err.visited, 0);
    }

    #[test]
    fn limited_expansion_collects_entire_selection_before_split_cue_suppression() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
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

        let (expanded, _visited) = expand_paths_to_audio_with_metadata_limited(
            &[td.path().to_path_buf(), image.clone()],
            64,
            || false,
        )
        .expect("bounded expansion should succeed");

        assert_eq!(expanded.paths.len(), 1);
        assert!(path_list_contains(&expanded.paths, &cue));
        assert!(!path_list_contains(&expanded.paths, &image));
    }

    #[test]
    fn limited_expansion_explicit_cue_suppresses_discovered_split_source_audio() {
        let td = tempfile::tempdir().expect("tempdir");
        let album = td.path().join("album");
        std::fs::create_dir_all(&album).unwrap();
        let cue = album.join("album.cue");
        let image = album.join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
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

        let (expanded, _visited) = expand_paths_to_audio_with_metadata_limited(
            &[album, cue.clone()],
            64,
            || false,
        )
        .expect("bounded expansion should succeed");

        assert_eq!(expanded.paths.len(), 1);
        assert!(path_list_contains(&expanded.paths, &cue));
        assert!(!path_list_contains(&expanded.paths, &image));
    }

    #[cfg(unix)]
    #[test]
    fn limited_expansion_deduplicates_canonical_equivalent_explicit_paths() {
        let td = tempfile::tempdir().expect("tempdir");
        let real = td.path().join("track.flac");
        let link = td.path().join("linked-track.flac");
        std::fs::write(&real, b"not real flac").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let (expanded, _visited) = expand_paths_to_audio_with_metadata_limited(
            &[link.clone(), real.clone()],
            64,
            || false,
        )
        .expect("bounded expansion should succeed");

        assert_eq!(expanded.paths.len(), 1);
        assert!(path_list_contains(&expanded.paths, &real));
        assert!(path_list_contains(&expanded.paths, &link));
    }
}


#[cfg(test)]
mod planner_embedded_authority_tests {
    use super::*;
    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::{ItemKey, ItemValue, Tag, TagItem};
    use std::process::Command;

    fn tool_available(tool: &str) -> bool {
        Command::new(tool)
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn require_flac_fixture_tool(test_name: &str) -> bool {
        if tool_available("ffmpeg") {
            return true;
        }
        eprintln!("skipping {test_name}: ffmpeg is required to create FLAC fixtures");
        false
    }

    fn create_flac(path: &Path) {
        let output = Command::new("ffmpeg")
            .arg("-y")
            .arg("-hide_banner")
            .arg("-nostdin")
            .arg("-loglevel")
            .arg("error")
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg("sine=frequency=440:sample_rate=44100:duration=1")
            .arg("-c:a")
            .arg("flac")
            .arg(path)
            .output()
            .expect("run ffmpeg fixture encode");
        assert!(
            output.status.success(),
            "ffmpeg fixture encode failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn set_embedded_cuesheet(image: &Path, cue_text: &str) {
        let mut tagged = lofty::read_from_path(image)
            .unwrap_or_else(|err| panic!("read FLAC fixture for CUESHEET write {}: {err}", image.display()));
        if tagged.primary_tag().is_none() {
            let tag_type = tagged.primary_tag_type();
            tagged.insert_tag(Tag::new(tag_type));
        }
        let tag = tagged.primary_tag_mut().expect("primary tag after insertion");
        let key = ItemKey::Unknown("CUESHEET".to_string());
        tag.remove_key(&key);
        tag.insert_unchecked(TagItem::new(
            key,
            ItemValue::Text(cue_text.trim().to_string()),
        ));
        tagged
            .save_to_path(image, WriteOptions::default())
            .unwrap_or_else(|err| panic!("save CUESHEET tag via lofty {}: {err}", image.display()));
    }

    fn write_split_album(dir: &Path) -> (PathBuf, PathBuf) {
        let side_a = dir.join("side_a.flac");
        let side_b = dir.join("side_b.flac");
        create_flac(&side_a);
        create_flac(&side_b);
        std::fs::write(
            dir.join("side_a.cue"),
            "PERFORMER \"Pink Floyd\"\nTITLE \"The Dark Side Of The Moon Side A\"\nFILE \"side_a.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"A1\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"A2\"\n    INDEX 01 00:00:37\n",
        )
        .expect("side A cue");
        std::fs::write(
            dir.join("side_b.cue"),
            "PERFORMER \"Pink Floyd\"\nTITLE \"The Dark Side Of The Moon Side B\"\nFILE \"side_b.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"B1\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"B2\"\n    INDEX 01 00:00:37\n",
        )
        .expect("side B cue");
        (side_a, side_b)
    }

    fn authoritative_album_cue(title: &str) -> String {
        format!(
            "CATALOG EOP-80778\nPERFORMER \"Pink Floyd\"\nTITLE \"{title}\"\nREM DATE 1973\nREM GENRE \"Rock\"\nFILE \"side_a.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"A1\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"A2\"\n    INDEX 01 00:00:37\nFILE \"side_b.flac\" WAVE\n  TRACK 03 AUDIO\n    TITLE \"B1\"\n    INDEX 01 00:00:00\n  TRACK 04 AUDIO\n    TITLE \"B2\"\n    INDEX 01 00:00:37\n"
        )
    }

    fn single_synthetic_text(expansion: &QueueExpansionResult) -> String {
        assert_eq!(expansion.expansion_errors, Vec::<String>::new());
        assert_eq!(expansion.paths.len(), 1, "expected one synthetic queue item");
        assert_eq!(expansion.synthetic_cue_artifacts.len(), 1);
        std::fs::read_to_string(&expansion.paths[0]).expect("synthetic cue text")
    }

    #[test]
    fn identical_member_embedded_cuesheet_is_authoritative_for_synthetic_artifact() {
        if !require_flac_fixture_tool("identical_member_embedded_cuesheet_is_authoritative_for_synthetic_artifact") {
            return;
        }
        let td = tempfile::tempdir().expect("tempdir");
        let (side_a, side_b) = write_split_album(td.path());
        let full_title = "The Dark Side of the Moon (Japan Toshiba Harvest-Odeon EOP-80778 LP / 24-192)";
        let embedded = authoritative_album_cue(full_title);
        set_embedded_cuesheet(&side_a, &embedded);
        set_embedded_cuesheet(&side_b, &embedded);

        let expansion = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        let synthetic = single_synthetic_text(&expansion);

        assert!(synthetic.contains(&format!("TITLE \"{full_title}\"")), "embedded album title must win: {synthetic}");
        assert!(synthetic.contains("CATALOG EOP-80778"));
        assert!(synthetic.contains(&format!("FILE \"{}\" FLAC", side_a.display())) || synthetic.contains(&format!("FILE \"{}\" WAVE", side_a.display())));
        assert!(synthetic.contains(&format!("FILE \"{}\" FLAC", side_b.display())) || synthetic.contains(&format!("FILE \"{}\" WAVE", side_b.display())));
        assert!(!synthetic.contains("TITLE \"The Dark Side Of The Moon\""), "sidecar common-prefix title must not overwrite embedded authority");
        cleanup_synthetic_cue_artifacts(&expansion.synthetic_cue_artifacts);
    }

    #[test]
    fn embedded_authority_resolves_nested_member_files_from_common_album_root() {
        if !require_flac_fixture_tool("embedded_authority_resolves_nested_member_files_from_common_album_root") {
            return;
        }
        let td = tempfile::tempdir().expect("tempdir");
        let disc1 = td.path().join("disc1");
        let disc2 = td.path().join("disc2");
        std::fs::create_dir_all(&disc1).expect("disc1");
        std::fs::create_dir_all(&disc2).expect("disc2");
        let side_a = disc1.join("side_a.flac");
        let side_b = disc2.join("side_b.flac");
        create_flac(&side_a);
        create_flac(&side_b);

        let embedded = r#"CATALOG EOP-80778
PERFORMER "Pink Floyd"
TITLE "Nested Member Authority"
FILE "disc1/side_a.flac" WAVE
  TRACK 01 AUDIO
    TITLE "A1"
    INDEX 01 00:00:00
FILE "disc2/side_b.flac" WAVE
  TRACK 02 AUDIO
    TITLE "B1"
    INDEX 01 00:00:00
"#;
        set_embedded_cuesheet(&side_a, embedded);
        set_embedded_cuesheet(&side_b, embedded);

        let accepted = authoritative_embedded_cuesheet_for_member_audio_with_base_dirs(
            &[side_a.clone(), side_b.clone()],
            &[disc1.clone(), td.path().to_path_buf(), disc2.clone()],
        )
        .expect("common-root base must accept nested member FILE references");

        assert!(accepted.contains("TITLE \"Nested Member Authority\""));
        assert!(
            accepted.contains(&format!("FILE \"{}\" FLAC", side_a.display()))
                || accepted.contains(&format!("FILE \"{}\" WAVE", side_a.display())),
            "side A FILE must be rewritten to its absolute nested path: {accepted}"
        );
        assert!(
            accepted.contains(&format!("FILE \"{}\" FLAC", side_b.display()))
                || accepted.contains(&format!("FILE \"{}\" WAVE", side_b.display())),
            "side B FILE must be rewritten to its absolute nested path: {accepted}"
        );
    }

    #[test]
    fn missing_or_differing_member_embedded_cuesheets_fall_back_to_sidecar_regeneration() {
        if !require_flac_fixture_tool("missing_or_differing_member_embedded_cuesheets_fall_back_to_sidecar_regeneration") {
            return;
        }
        let td = tempfile::tempdir().expect("tempdir");
        let (side_a, side_b) = write_split_album(td.path());
        set_embedded_cuesheet(&side_a, &authoritative_album_cue("Authoritative A"));
        set_embedded_cuesheet(&side_b, &authoritative_album_cue("Authoritative B"));

        let differing = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        let differing_text = single_synthetic_text(&differing);
        assert!(differing_text.contains("TITLE \"The Dark Side Of The Moon\""), "differing embedded sheets must fall back to regenerated sidecar model: {differing_text}");
        assert!(!differing_text.contains("Authoritative A"));
        assert!(!differing_text.contains("Authoritative B"));
        cleanup_synthetic_cue_artifacts(&differing.synthetic_cue_artifacts);

        let td_missing = tempfile::tempdir().expect("tempdir");
        let (missing_a, _missing_b) = write_split_album(td_missing.path());
        set_embedded_cuesheet(&missing_a, &authoritative_album_cue("Only One Member Has This"));
        let missing = expand_paths_to_audio_with_metadata(&[td_missing.path().to_path_buf()]);
        let missing_text = single_synthetic_text(&missing);
        assert!(missing_text.contains("TITLE \"The Dark Side Of The Moon\""));
        assert!(!missing_text.contains("Only One Member Has This"));
        cleanup_synthetic_cue_artifacts(&missing.synthetic_cue_artifacts);
    }

    #[test]
    fn stale_subset_embedded_cuesheet_falls_back_to_sidecar_regeneration() {
        if !require_flac_fixture_tool("stale_subset_embedded_cuesheet_falls_back_to_sidecar_regeneration") {
            return;
        }
        let td = tempfile::tempdir().expect("tempdir");
        let (side_a, side_b) = write_split_album(td.path());
        let stale_subset = "PERFORMER \"Pink Floyd\"\nTITLE \"Stale Side A Only\"\nFILE \"side_a.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"A1\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"A2\"\n    INDEX 01 00:00:37\n";
        set_embedded_cuesheet(&side_a, stale_subset);
        set_embedded_cuesheet(&side_b, stale_subset);

        let expansion = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        let synthetic = single_synthetic_text(&expansion);
        assert!(synthetic.contains("TITLE \"The Dark Side Of The Moon\""), "FILE-set mismatch must reject embedded subset authority: {synthetic}");
        assert!(!synthetic.contains("Stale Side A Only"));
        cleanup_synthetic_cue_artifacts(&expansion.synthetic_cue_artifacts);
    }

    #[test]
    fn planner_ignores_hidden_dot_cues_when_building_synthetic_groups() {
        let td = tempfile::tempdir().expect("tempdir");
        std::fs::write(td.path().join("side_a.flac"), b"placeholder audio").expect("side A audio");
        std::fs::write(td.path().join("side_b.flac"), b"placeholder audio").expect("side B audio");
        std::fs::write(
            td.path().join("side_a.cue"),
            "TITLE \"Album Side A\"\nFILE \"side_a.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 00:30:00\n",
        )
        .expect("visible cue A");
        std::fs::write(
            td.path().join("side_b.cue"),
            "TITLE \"Album Side B\"\nFILE \"side_b.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 00:30:00\n",
        )
        .expect("visible cue B");
        std::fs::write(td.path().join("._album.cue"), b"not a cue").expect("appledouble cue");

        let expansion = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert_eq!(expansion.expansion_errors, Vec::<String>::new(), "hidden cue must not poison planning");
        assert_eq!(expansion.paths.len(), 1);
        assert_eq!(expansion.synthetic_cue_artifacts.len(), 1);
        cleanup_synthetic_cue_artifacts(&expansion.synthetic_cue_artifacts);
    }
}

#[cfg(test)]
mod all_audio_expansion_tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::write(path, b"fixture").expect("fixture");
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    /// The regression: a single-image CUE album must yield its image file for
    /// metadata/analysis surfaces even though queue expansion suppresses it
    /// in favor of the CUE (Edit Metadata and :tags-mb reported "no audio
    /// files" on such albums).
    #[test]
    fn single_image_album_yields_image_for_metadata_and_cue_for_queue() {
        let td = tempfile::tempdir().expect("tempdir");
        touch(&td.path().join("album.flac"));
        std::fs::write(
            td.path().join("album.cue"),
            "FILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n",
        )
        .expect("cue");

        let metadata_view = expand_paths_to_all_audio(&[td.path().to_path_buf()]);
        assert_eq!(names(&metadata_view), vec!["album.flac"]);

        let queue_view = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert_eq!(names(&queue_view), vec!["album.cue"]);
    }

    /// One-to-one per-track CUEs: both views agree on the audio files.
    #[test]
    fn per_track_cue_pairs_yield_audio_in_both_views() {
        let td = tempfile::tempdir().expect("tempdir");
        for n in ["01 - One", "02 - Two"] {
            touch(&td.path().join(format!("{n}.flac")));
            std::fs::write(
                td.path().join(format!("{n}.cue")),
                format!("FILE \"{n}.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n"),
            )
            .expect("cue");
        }

        let metadata_view = expand_paths_to_all_audio(&[td.path().to_path_buf()]);
        assert_eq!(names(&metadata_view), vec!["01 - One.flac", "02 - Two.flac"]);

        let queue_view = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert_eq!(names(&queue_view), vec!["01 - One.flac", "02 - Two.flac"]);
    }

    /// The all-audio walk stays deterministic, deduplicated, and never
    /// returns non-audio files.
    #[test]
    fn all_audio_walk_is_sorted_deduplicated_and_audio_only() {
        let td = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(td.path().join("sub")).expect("sub");
        touch(&td.path().join("sub").join("b.flac"));
        touch(&td.path().join("a.flac"));
        touch(&td.path().join("notes.txt"));
        touch(&td.path().join("cover.jpg"));

        let file_arg = td.path().join("a.flac");
        let out = expand_paths_to_all_audio(&[
            td.path().to_path_buf(),
            file_arg.clone(),
            file_arg,
        ]);
        assert_eq!(names(&out), vec!["a.flac", "b.flac"]);
    }
}


#[cfg(test)]
mod ogg_tta_cue_queue_tests {
    use super::*;

    #[test]
    fn explicit_and_folder_cue_conversion_paths_accept_ogg_and_tta_images() {
        for ext in ["ogg", "tta"] {
            let td = tempfile::tempdir().expect("tempdir");
            let cue = td.path().join(format!("album.{ext}.cue"));
            let image = td.path().join(format!("album.{ext}"));
            std::fs::write(&image, b"placeholder audio").expect("audio fixture");
            std::fs::write(
                &cue,
                format!(
                    "FILE \"{}\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n",
                    image.file_name().unwrap().to_string_lossy()
                ),
            )
            .expect("cue fixture");

            let explicit = expand_paths_to_audio_with_metadata(&[cue.clone()]);
            assert_eq!(explicit.paths, vec![cue.clone()], "explicit .cue conversion must queue the CUE for {ext}");
            assert!(explicit.cue_artifact_audio.is_empty(), "explicit split-source CUE must not be tagged as metadata artifact for {ext}");

            let folder = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
            assert_eq!(folder.paths, vec![cue.clone()], "folder conversion must stage the CUE, not the image, for {ext}");
            assert!(folder.cue_artifact_audio.is_empty(), "split-source CUE must not request EmbeddedOnly sidecar policy for {ext}");

            let metadata = expand_paths_to_all_audio(&[td.path().to_path_buf()]);
            assert_eq!(metadata, vec![image.clone()], "metadata surfaces must still see the backing image for {ext}");
        }
    }
}

#[cfg(test)]
mod direct_queue_source_policy_expansion_tests {
    use super::*;

    #[test]
    fn folder_queue_expansion_uses_shared_direct_source_policy_not_local_audio_list() {
        let td = tempfile::tempdir().expect("tempdir");
        let admitted = [
            "track.ape",
            "track.dsf",
            "track.dff",
            "track.shn",
            "track.ogg",
            "track.tta",
            "track.7z",
        ];
        for name in admitted {
            std::fs::write(td.path().join(name), b"fixture").expect("fixture");
        }
        for name in ["notes.txt", "track.zip", "track.tar.gz", "track.dmg", "track.cab"] {
            std::fs::write(td.path().join(name), b"fixture").expect("fixture");
        }

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        for name in admitted {
            assert!(path_list_contains(&expanded, &td.path().join(name)), "{name}");
        }
        for name in ["notes.txt", "track.zip", "track.tar.gz", "track.dmg", "track.cab"] {
            assert!(!path_list_contains(&expanded, &td.path().join(name)), "{name}");
        }
    }
}

#[cfg(test)]
mod authoritative_split_cue_grouping_tests {
    use super::*;

    fn write_side(dir: &Path, stem: &str, title: &str, tracks: &[u32]) -> PathBuf {
        let image = dir.join(format!("{stem}.flac"));
        std::fs::write(&image, b"placeholder audio").expect("audio fixture");
        let cue = dir.join(format!("{stem}.cue"));
        let mut text = format!(
            "TITLE \"{title}\"\nFILE \"{stem}.flac\" WAVE\n"
        );
        for (idx, minute) in tracks.iter().enumerate() {
            text.push_str(&format!(
                "  TRACK {:02} AUDIO\n    TITLE \"Track {}\"\n    INDEX 01 {:02}:00:00\n",
                idx + 1,
                idx + 1,
                minute
            ));
        }
        std::fs::write(&cue, text).expect("cue fixture");
        cue
    }

    #[test]
    fn conversion_honors_authoritative_split_decision_instead_of_conservative_merge() {
        let td = tempfile::tempdir().expect("tempdir");
        let left = write_side(td.path(), "left", "Left", &[0, 3]);
        let right = write_side(td.path(), "right", "Right", &[0, 4]);
        let cue_paths = vec![left.clone(), right.clone()];
        let mut decisions = QueueSplitCueAlbumGroupingDecisions::new();
        decisions.insert(
            split_cue_album_grouping_key_for_queue(&cue_paths),
            crate::convert::split_cue_album::split_each_decision(
                &cue_paths,
                SplitCueAlbumGroupingReason::PerCueDistinctTocHits,
            ),
        );

        let authoritative = expand_paths_to_audio_with_metadata_using_grouping_decisions(
            &[td.path().to_path_buf()],
            &decisions,
        );
        assert!(authoritative.expansion_errors.is_empty());
        assert_eq!(authoritative.paths, cue_paths);

        let fallback = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert_eq!(fallback.paths.len(), 1, "without an authoritative split decision, the conversion planner would conservatively merge this ambiguous folder");
        assert!(is_synthetic_cue_album_artifact(&fallback.paths[0]));
        cleanup_synthetic_cue_artifacts(&fallback.synthetic_cue_artifacts);
    }

    #[test]
    fn authoritative_split_lookup_is_order_independent_for_native_vs_casefolded_sort() {
        let td = tempfile::tempdir().expect("tempdir");
        let upper = write_side(td.path(), "B", "Unrelated Upper", &[0, 3]);
        let lower = write_side(td.path(), "a", "Unrelated Lower", &[0, 4]);

        // PathBuf's native ordering sorts `B.cue` before `a.cue` on Unix, while
        // queue expansion's deterministic browse order is case-folded and sorts
        // `a.cue` before `B.cue`. The grouping decision key must therefore be
        // a canonical member set, not the caller's traversal order.
        let metadata_order = vec![upper.clone(), lower.clone()];
        let mut decisions = QueueSplitCueAlbumGroupingDecisions::new();
        decisions.insert(
            split_cue_album_grouping_key_for_queue(&metadata_order),
            crate::convert::split_cue_album::split_each_decision(
                &metadata_order,
                SplitCueAlbumGroupingReason::PerCueDistinctTocHits,
            ),
        );

        let expanded = expand_paths_to_audio_with_metadata_using_grouping_decisions(
            &[td.path().to_path_buf()],
            &decisions,
        );

        assert!(expanded.expansion_errors.is_empty());
        assert_eq!(expanded.paths.len(), 2);
        assert!(expanded.paths.iter().any(|path| queue_path_key(path) == queue_path_key(&lower)));
        assert!(expanded.paths.iter().any(|path| queue_path_key(path) == queue_path_key(&upper)));
        assert!(
            expanded.synthetic_cue_artifacts.is_empty(),
            "a cached per-cue split must not miss lookup and fall back to a synthetic merged album"
        );

        let fallback = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert_eq!(fallback.paths.len(), 1);
        assert!(is_synthetic_cue_album_artifact(&fallback.paths[0]));
        cleanup_synthetic_cue_artifacts(&fallback.synthetic_cue_artifacts);
    }
}
