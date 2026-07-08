//! Conversion-domain queue expansion and CUE artifact policy.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::convert::classify::{classify_file, is_audio_file_path, is_cue_sheet_path, EntryKind};
use crate::convert::pipeline::CueSidecarPolicy;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueExpansionResult {
    /// Paths to queue for conversion, in deterministic browse order.
    pub paths: Vec<PathBuf>,
    /// Audio paths whose sibling sidecar CUE was already classified as a
    /// metadata artifact during queue expansion. Downstream conversion must
    /// skip sidecar CUE discovery for these paths while still honoring
    /// embedded CUESHEET tags.
    pub cue_artifact_audio: HashSet<PathBuf>,
}

impl QueueExpansionResult {
    #[must_use]
    pub fn into_paths(self) -> Vec<PathBuf> {
        self.paths
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
/// `Vec<PathBuf>` API, applying conversion-queue CUE semantics: a
/// split-source CUE is the queueable path and its referenced audio (for a
/// single-image album, the image file itself) is suppressed. Only queue
/// construction should use this.
pub fn expand_paths_to_audio(paths: &[PathBuf]) -> Vec<PathBuf> {
    expand_paths_to_audio_with_metadata(paths).into_paths()
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
    let mut plan = QueueExpansionPlan::default();
    for path in paths {
        collect_queue_candidates(path, &mut plan);
    }
    plan.into_queue_paths()
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
    expand_paths_to_audio_with_preserved_disc_roots_limited(
        paths,
        &[],
        max_visited,
        is_cancelled,
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
    mut is_cancelled: F,
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

    Ok((plan.into_queue_paths(), state.visited))
}

pub(crate) fn expand_paths_to_audio_with_preserved_disc_roots(
    paths: &[PathBuf],
    preserved_disc_roots: &[PathBuf],
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
    plan.into_queue_paths()
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
        } else if is_queueable_file(&path) {
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

    fn into_queue_paths(self) -> QueueExpansionResult {
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

        for cue in cue_sheets {
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

        QueueExpansionResult {
            paths: result,
            cue_artifact_audio,
        }
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




/// A file is queueable for conversion if it's an audio file, a CUE sheet,
/// a supported archive (7z), or a supported disc ISO. Generic ISOs, zips,
/// rars, etc. that the pipeline can't handle are excluded to avoid noisy queue errors.
fn is_queueable_file(path: &Path) -> bool {
    if is_cue_sheet_path(path) {
        return true;
    }

    let kind = classify_file(path);
    match kind {
        EntryKind::AudioFile(_) => true,
        EntryKind::Archive => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase());
            match ext.as_deref() {
                // 7z archives are always queueable (pipeline supports them).
                Some("7z") => true,
                // ISOs are only queueable if they're supported disc images.
                Some("iso") => crate::convert::sacd::is_sacd_iso(path)
                    || crate::disc::dvda_utils::is_dvda_iso(path)
                    || crate::disc::dvdv_utils::is_dvdv_iso(path)
                    || crate::disc::bluray_utils::is_bluray_iso(path),
                // Other archive formats (zip, rar, tar, etc.) are not
                // supported by the conversion pipeline.
                _ => false,
            }
        }
        _ => false,
    }
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
    fn expand_paths_to_audio_queues_side_cues_and_suppresses_side_images() {
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

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded, &side_a_cue));
        assert!(path_list_contains(&expanded, &side_b_cue));
        assert!(!path_list_contains(&expanded, &side_a));
        assert!(!path_list_contains(&expanded, &side_b));
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

