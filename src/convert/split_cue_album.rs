//! Shared split-CUE album grouping policy.
//!
//! This module is deliberately below both the TUI and queue-expansion layers so
//! metadata dispatch and conversion queue construction use the same album
//! identity ladder instead of maintaining separate title-normalization rules.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitCueAlbumGroupingReason {
    TitleSharedPrefix,
    ConcatTocHit,
    PerCueDistinctTocHits,
    AmbiguousMerge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitCueAlbumGroupingDecision {
    pub groups: Vec<Vec<PathBuf>>,
    pub reason: SplitCueAlbumGroupingReason,
}

/// Album title for text lookup over a multi-part CUE album. Side-split rips
/// often title each cue by side; use the longest meaningful shared prefix and
/// fall back to the first title only for presentation/search text, not as merge
/// evidence.
pub fn common_cue_album_title(titles: &[String]) -> Option<String> {
    let first = titles.first()?.clone();
    meaningful_common_cue_album_prefix(titles).or(Some(first))
}

/// Return the decisive TITLE-rung shared prefix used by the split-CUE album
/// grouping ladder. `Some(_)` is merge evidence; `None` is not split evidence.
pub fn meaningful_common_cue_album_prefix(titles: &[String]) -> Option<String> {
    if titles.len() < 2 || titles.iter().any(|t| t.trim().is_empty()) {
        return None;
    }
    // Case-insensitive comparison, preserving the FIRST title's casing:
    // real rips mix "Of The Moon (Side B)" with "of the Moon (Japan ...)",
    // and a case-sensitive compare would cut the shared title mid-phrase.
    let mut prefix: Vec<char> = titles[0].chars().collect();
    for title in &titles[1..] {
        let chars: Vec<char> = title.chars().collect();
        let mut common = 0;
        while common < prefix.len()
            && common < chars.len()
            && (prefix[common] == chars[common]
                || prefix[common].to_lowercase().eq(chars[common].to_lowercase()))
        {
            common += 1;
        }
        prefix.truncate(common);
    }
    let mut candidate: String = prefix.into_iter().collect();
    loop {
        let trimmed = candidate.trim_end();
        if trimmed.len() != candidate.len() {
            candidate.truncate(trimmed.len());
            continue;
        }
        if let Some(last) = candidate.chars().last() {
            if matches!(last, '-' | '\u{2013}' | ':' | ',' | '&' | '/') {
                candidate.pop();
                continue;
            }
        }
        let opens = candidate.matches(['(', '[']).count();
        let closes = candidate.matches([')', ']']).count();
        if opens > closes {
            if let Some(cut) = candidate.rfind(['(', '[']) {
                candidate.truncate(cut);
                continue;
            }
        }
        if strip_trailing_split_designator(&mut candidate) {
            continue;
        }
        break;
    }
    (candidate.chars().count() >= 4).then_some(candidate)
}

fn strip_trailing_split_designator(candidate: &mut String) -> bool {
    let lowered = candidate.trim_end().to_ascii_lowercase();
    let designators = [" side", " disc", " disk", " part", " volume", " vol"];
    for designator in designators {
        if lowered.ends_with(designator) {
            let trimmed_len = candidate.trim_end().len();
            candidate.truncate(trimmed_len - designator.len());
            return true;
        }
    }
    false
}

pub fn same_folder_cue_paths(paths: &[PathBuf]) -> bool {
    if paths.len() < 2 {
        return false;
    }
    let Some(first_dir) = paths.first().and_then(|path| path.parent()) else {
        return false;
    };
    paths.iter().all(|path| path.parent() == Some(first_dir))
}

/// Canonical order-independent key for a set of split-CUE member paths.
///
/// Metadata, GNUDB/MB preflight, and conversion expansion may discover the
/// same folder's CUE files in different orders. The resolved album decision is
/// a property of the member set, not the caller traversal order, so the key
/// canonicalizes each path, sorts with one case-folded slash-normalized
/// comparator, and deduplicates before lookup/storage.
pub fn grouping_key_from_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut keys: Vec<PathBuf> = paths.iter().map(|path| cue_path_key(path)).collect();
    keys.sort_by(|left, right| split_cue_path_cmp(left, right));
    keys.dedup();
    keys
}

fn split_cue_path_sort_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn split_cue_path_cmp(left: &Path, right: &Path) -> Ordering {
    split_cue_path_sort_key(left)
        .cmp(&split_cue_path_sort_key(right))
        .then_with(|| left.to_string_lossy().cmp(&right.to_string_lossy()))
}

pub fn merge_decision(
    cue_paths: &[PathBuf],
    reason: SplitCueAlbumGroupingReason,
) -> SplitCueAlbumGroupingDecision {
    SplitCueAlbumGroupingDecision {
        groups: vec![grouping_key_from_paths(cue_paths)],
        reason,
    }
}

pub fn split_each_decision(
    cue_paths: &[PathBuf],
    reason: SplitCueAlbumGroupingReason,
) -> SplitCueAlbumGroupingDecision {
    let mut groups: Vec<Vec<PathBuf>> = cue_paths
        .iter()
        .map(|path| vec![cue_path_key(path)])
        .collect();
    groups.sort_by(|left, right| match (left.first(), right.first()) {
        (Some(left), Some(right)) => split_cue_path_cmp(left, right),
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    });
    groups.dedup();
    SplitCueAlbumGroupingDecision { groups, reason }
}

pub fn title_rung_decision(
    cue_paths: &[PathBuf],
    titles: &[String],
) -> Option<SplitCueAlbumGroupingDecision> {
    if !same_folder_cue_paths(cue_paths) {
        return None;
    }
    meaningful_common_cue_album_prefix(titles)
        .map(|_| merge_decision(cue_paths, SplitCueAlbumGroupingReason::TitleSharedPrefix))
}

/// Apply the non-network split-CUE ladder given any TOC evidence already known
/// to the caller. `concat_toc_has_release == Some(true)` is merge evidence.
/// `per_cue_release_ids == Some(..)` is split evidence only when every cue has
/// a distinct, non-empty release id. Anything incomplete or ambiguous falls
/// through to the required conservative merge.
pub fn decide_with_toc_evidence(
    cue_paths: &[PathBuf],
    titles: &[String],
    concat_toc_has_release: Option<bool>,
    per_cue_release_ids: Option<Vec<Option<String>>>,
) -> Option<SplitCueAlbumGroupingDecision> {
    if !same_folder_cue_paths(cue_paths) {
        return None;
    }
    if let Some(decision) = title_rung_decision(cue_paths, titles) {
        return Some(decision);
    }
    if concat_toc_has_release == Some(true) {
        return Some(merge_decision(
            cue_paths,
            SplitCueAlbumGroupingReason::ConcatTocHit,
        ));
    }
    if let Some(ids) = per_cue_release_ids {
        if ids.len() == cue_paths.len() {
            let release_ids: Option<Vec<String>> = ids
                .into_iter()
                .map(|id| id.map(|value| value.trim().to_string()))
                .map(|id| id.filter(|value| !value.is_empty()))
                .collect();
            if let Some(release_ids) = release_ids {
                let unique: BTreeSet<String> = release_ids.iter().cloned().collect();
                if unique.len() == cue_paths.len() {
                    return Some(split_each_decision(
                        cue_paths,
                        SplitCueAlbumGroupingReason::PerCueDistinctTocHits,
                    ));
                }
            }
        }
    }
    Some(merge_decision(
        cue_paths,
        SplitCueAlbumGroupingReason::AmbiguousMerge,
    ))
}

fn cue_path_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}


/// One CUE member admitted by the shared editor/planner membership policy.
/// `track_audio_paths` is position-aligned with `sheet.tracks`; multi-FILE CUEs
/// therefore retain the exact image owning each track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitCueMemberRole {
    /// The CUE contributes at least one real split point because one referenced
    /// image owns multiple tracks. It may participate in a synthetic album.
    SyntheticAlbumPart,
    /// Every referenced FILE maps one-to-one to one track. The CUE is metadata
    /// for already-split files and must not be admitted as a synthetic part.
    MetadataSidecar,
}

#[derive(Debug, Clone)]
pub struct SplitCueAdmissionMember {
    pub cue_path: PathBuf,
    pub sheet: crate::convert::cue_parser::CueSheet,
    pub referenced_audio: Vec<PathBuf>,
    pub track_audio_paths: Vec<PathBuf>,
    pub role: SplitCueMemberRole,
}

impl SplitCueAdmissionMember {
    #[must_use]
    pub fn contributes_synthetic_album_part(&self) -> bool {
        self.role == SplitCueMemberRole::SyntheticAlbumPart
    }
}

#[derive(Debug, Clone)]
pub struct SplitCueFolderAdmission {
    pub parent: PathBuf,
    pub members: Vec<SplitCueAdmissionMember>,
}

#[derive(Debug, Clone, Default)]
pub struct SplitCueAdmissionReport {
    pub folders: Vec<SplitCueFolderAdmission>,
    pub warnings: Vec<String>,
    pub rejected_folders: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitCueReferenceResolution {
    Resolved(PathBuf),
    Missing,
    Ambiguous(Vec<PathBuf>),
}

/// Collect candidate CUEs without recursion. A selected directory contributes
/// only direct-child CUE files; a selected CUE contributes itself. Audio-file
/// selection is intentionally left to callers, which can re-run this policy on
/// its parent when they want album-scope behavior.
pub fn split_cue_candidate_paths(selected: &[PathBuf]) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for path in selected {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            let Ok(entries) = std::fs::read_dir(path) else {
                continue;
            };
            let mut direct: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|candidate| {
                    let Ok(meta) = std::fs::symlink_metadata(candidate) else {
                        return false;
                    };
                    !meta.file_type().is_symlink()
                        && meta.is_file()
                        && crate::convert::classify::is_cue_sheet_path(candidate)
                        && candidate
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| !name.starts_with('.'))
                })
                .collect();
            direct.sort_by(|left, right| split_cue_path_cmp(left, right));
            for candidate in direct {
                let key = cue_path_key(&candidate);
                if seen.insert(key) {
                    candidates.push(candidate);
                }
            }
        } else if meta.is_file() && crate::convert::classify::is_cue_sheet_path(path) {
            let key = cue_path_key(path);
            if seen.insert(key) {
                candidates.push(path.clone());
            }
        }
    }
    candidates.sort_by(|left, right| split_cue_path_cmp(left, right));
    candidates
}

/// Admit complete same-folder CUE memberships atomically. Any malformed,
/// incomplete, ambiguous, cross-directory, or unsupported member rejects the
/// entire folder, so the editor can never present a subset that the planner
/// would later refuse or expand differently.
pub fn admit_split_cue_folders(selected: &[PathBuf]) -> SplitCueAdmissionReport {
    admit_split_cue_candidate_paths(&split_cue_candidate_paths(selected))
}

pub fn admit_split_cue_candidate_paths(cue_paths: &[PathBuf]) -> SplitCueAdmissionReport {
    let mut by_parent: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for cue_path in cue_paths {
        let Ok(meta) = std::fs::symlink_metadata(cue_path) else {
            continue;
        };
        if meta.file_type().is_symlink()
            || !meta.is_file()
            || !crate::convert::classify::is_cue_sheet_path(cue_path)
        {
            continue;
        }
        let Some(parent) = cue_path.parent() else {
            continue;
        };
        by_parent
            .entry(cue_path_key(parent))
            .or_default()
            .push(cue_path.clone());
    }

    let mut report = SplitCueAdmissionReport::default();
    for (parent_key, mut cues) in by_parent {
        cues.sort_by(|left, right| split_cue_path_cmp(left, right));
        cues.dedup_by(|left, right| cue_path_key(left) == cue_path_key(right));
        let mut members = Vec::with_capacity(cues.len());
        let mut rejection = None;
        for cue_path in &cues {
            match admit_split_cue_member(cue_path) {
                Ok(member) => members.push(member),
                Err(message) => {
                    rejection = Some(message);
                    break;
                }
            }
        }
        if let Some(message) = rejection {
            report.rejected_folders.push(parent_key.clone());
            report.warnings.push(format!(
                "{} — conversion will not include this folder ({})",
                message,
                parent_key.display()
            ));
            continue;
        }
        if !members.is_empty() {
            report.folders.push(SplitCueFolderAdmission {
                parent: parent_key,
                members,
            });
        }
    }
    report
}

/// Admit and classify one CUE through the canonical editor/planner policy.
/// Callers must not re-derive membership or metadata-sidecar semantics.
pub fn admit_split_cue_member(cue_path: &Path) -> Result<SplitCueAdmissionMember, String> {
    let sheet = crate::convert::cue_parser::parse_cue_file(cue_path)
        .map_err(|err| format!("member CUE invalid: {}: {err}", cue_path.display()))?;
    if sheet.tracks.is_empty() {
        return Err(format!("member CUE has no tracks: {}", cue_path.display()));
    }
    let parent = cue_path
        .parent()
        .ok_or_else(|| format!("member CUE has no parent: {}", cue_path.display()))?;
    let parent_key = cue_path_key(parent);
    let mut referenced_audio = Vec::new();
    let mut referenced_keys = HashSet::new();
    let mut track_audio_paths = Vec::with_capacity(sheet.tracks.len());
    let mut previous_by_file: BTreeMap<PathBuf, (u32, u32)> = BTreeMap::new();

    for track in &sheet.tracks {
        let file_ref = track.file.as_deref().ok_or_else(|| {
            format!(
                "member CUE track {} has no FILE reference: {}",
                track.number,
                cue_path.display()
            )
        })?;
        let index01 = track.index01_frames.ok_or_else(|| {
            format!(
                "member CUE track {} has no INDEX 01: {}",
                track.number,
                cue_path.display()
            )
        })?;
        let resolved = match resolve_split_cue_file_reference(parent, file_ref) {
            SplitCueReferenceResolution::Resolved(path) => path,
            SplitCueReferenceResolution::Missing => {
                return Err(format!("member image missing: {file_ref}"));
            }
            SplitCueReferenceResolution::Ambiguous(paths) => {
                return Err(format!(
                    "member image ambiguous: {file_ref}: {}",
                    paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        };
        let meta = std::fs::symlink_metadata(&resolved)
            .map_err(|_| format!("member image missing: {file_ref}"))?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(format!("member image is not a regular file: {}", resolved.display()));
        }
        if !matches!(
            crate::convert::classify::classify_file(&resolved),
            crate::convert::classify::EntryKind::AudioFile(_)
        ) {
            return Err(format!(
                "member image is not supported audio: {}",
                resolved.display()
            ));
        }
        if resolved
            .parent()
            .map(cue_path_key)
            .as_ref()
            != Some(&parent_key)
        {
            return Err(format!(
                "member image is outside the CUE folder: {}",
                resolved.display()
            ));
        }
        let resolved_key = cue_path_key(&resolved);
        if let Some((previous_track, previous_index)) = previous_by_file.get(&resolved_key) {
            if index01 <= *previous_index {
                return Err(format!(
                    "member CUE has non-increasing INDEX 01 for track {} in {}; previous track {} was at frame {}",
                    track.number,
                    resolved.display(),
                    previous_track,
                    previous_index
                ));
            }
        }
        previous_by_file.insert(resolved_key.clone(), (track.number, index01));
        if referenced_keys.insert(resolved_key) {
            referenced_audio.push(resolved.clone());
        }
        track_audio_paths.push(resolved);
    }

    let mut tracks_per_image: BTreeMap<PathBuf, usize> = BTreeMap::new();
    for audio_path in &track_audio_paths {
        *tracks_per_image.entry(cue_path_key(audio_path)).or_insert(0) += 1;
    }
    let role = if tracks_per_image.values().any(|count| *count > 1) {
        SplitCueMemberRole::SyntheticAlbumPart
    } else {
        SplitCueMemberRole::MetadataSidecar
    };

    Ok(SplitCueAdmissionMember {
        cue_path: cue_path.to_path_buf(),
        sheet,
        referenced_audio,
        track_audio_paths,
        role,
    })
}

pub fn resolve_split_cue_file_reference(
    parent: &Path,
    file_ref: &str,
) -> SplitCueReferenceResolution {
    let normalized_ref = file_ref.replace('\\', &std::path::MAIN_SEPARATOR.to_string());
    let raw_path = PathBuf::from(&normalized_ref);
    let direct = if raw_path.is_absolute() {
        raw_path.clone()
    } else {
        parent.join(&raw_path)
    };
    if direct.is_file() {
        return SplitCueReferenceResolution::Resolved(direct);
    }

    let search_dir = raw_path
        .parent()
        .filter(|component| !component.as_os_str().is_empty())
        .map(|component| parent.join(component))
        .unwrap_or_else(|| parent.to_path_buf());
    let wanted_name = raw_path.file_name().and_then(|value| value.to_str());
    if let Some(wanted_name) = wanted_name {
        let matches = collect_split_cue_audio_candidates(&search_dir, |candidate| {
            candidate
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(wanted_name))
        });
        match unique_split_cue_candidate(matches) {
            SplitCueReferenceResolution::Missing => {}
            other => return other,
        }
    }
    if let Some(wanted_stem) = raw_path.file_stem().and_then(|value| value.to_str()) {
        return unique_split_cue_candidate(collect_split_cue_audio_candidates(
            &search_dir,
            |candidate| {
                candidate
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value.eq_ignore_ascii_case(wanted_stem))
            },
        ));
    }
    SplitCueReferenceResolution::Missing
}

fn collect_split_cue_audio_candidates(
    directory: &Path,
    predicate: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut candidates: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let Ok(meta) = std::fs::symlink_metadata(path) else {
                return false;
            };
            !meta.file_type().is_symlink()
                && meta.is_file()
                && matches!(
                    crate::convert::classify::classify_file(path),
                    crate::convert::classify::EntryKind::AudioFile(_)
                )
                && predicate(path)
        })
        .collect();
    candidates.sort_by(|left, right| split_cue_path_cmp(left, right));
    let mut seen = HashSet::new();
    candidates.retain(|path| seen.insert(cue_path_key(path)));
    candidates
}

fn unique_split_cue_candidate(candidates: Vec<PathBuf>) -> SplitCueReferenceResolution {
    match candidates.len() {
        0 => SplitCueReferenceResolution::Missing,
        1 => match candidates.into_iter().next() {
            Some(path) => SplitCueReferenceResolution::Resolved(path),
            None => SplitCueReferenceResolution::Missing,
        },
        _ => SplitCueReferenceResolution::Ambiguous(candidates),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue_paths() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/tmp/album/side-a.cue"),
            PathBuf::from("/tmp/album/side-b.cue"),
        ]
    }

    #[test]
    fn title_shared_prefix_is_merge_evidence_without_suffix_buckets() {
        let titles = vec!["Album - Alpha".to_string(), "Album - Omega".to_string()];
        let decision = decide_with_toc_evidence(&cue_paths(), &titles, None, None)
            .expect("same-folder decision");
        assert_eq!(decision.reason, SplitCueAlbumGroupingReason::TitleSharedPrefix);
        assert_eq!(decision.groups.len(), 1);
    }

    #[test]
    fn common_title_drops_dangling_side_word_from_shared_prefix() {
        let titles = vec!["Album Side A".to_string(), "Album Side B".to_string()];
        assert_eq!(common_cue_album_title(&titles).as_deref(), Some("Album"));
    }

    #[test]
    fn common_title_prefix_is_case_insensitive_and_keeps_first_casing() {
        // Real-tree shape: sides cased differently and carrying different
        // parenthesized suffixes. A case-sensitive compare cuts at "Of"/"of"
        // and the designator strip then eats "Side", leaving "The Dark".
        let titles = vec![
            "The Dark Side of the Moon (Japan Toshiba Harvest-Odeon EOP-80778 LP / 24-192)"
                .to_string(),
            "The Dark Side Of The Moon (Side B)".to_string(),
        ];
        assert_eq!(
            common_cue_album_title(&titles).as_deref(),
            Some("The Dark Side of the Moon")
        );
    }

    #[test]
    fn distinct_per_cue_release_ids_split_when_supplied() {
        let titles = vec!["Left".to_string(), "Right".to_string()];
        let decision = decide_with_toc_evidence(
            &cue_paths(),
            &titles,
            Some(false),
            Some(vec![Some("release-a".to_string()), Some("release-b".to_string())]),
        )
        .expect("same-folder decision");
        assert_eq!(decision.reason, SplitCueAlbumGroupingReason::PerCueDistinctTocHits);
        assert_eq!(decision.groups.len(), 2);
    }

    #[test]
    fn missing_toc_evidence_conservatively_merges() {
        let titles = vec!["Left".to_string(), "Right".to_string()];
        let decision = decide_with_toc_evidence(&cue_paths(), &titles, None, None)
            .expect("same-folder decision");
        assert_eq!(decision.reason, SplitCueAlbumGroupingReason::AmbiguousMerge);
        assert_eq!(decision.groups.len(), 1);
    }

    #[test]
    fn candidate_collection_is_direct_child_only() {
        let td = tempfile::tempdir().expect("tempdir");
        let nested = td.path().join("disc-1");
        std::fs::create_dir_all(&nested).expect("nested");
        let root_cue = td.path().join("root.cue");
        let nested_cue = nested.join("nested.cue");
        std::fs::write(&root_cue, b"").expect("root cue");
        std::fs::write(&nested_cue, b"").expect("nested cue");

        assert_eq!(
            split_cue_candidate_paths(&[td.path().to_path_buf()]),
            vec![root_cue]
        );
        assert_eq!(
            split_cue_candidate_paths(&[nested.clone()]),
            vec![nested_cue]
        );
    }

    #[test]
    fn admission_retains_multi_file_track_ownership() {
        let td = tempfile::tempdir().expect("tempdir");
        let a = td.path().join("side-a.flac");
        let b = td.path().join("side-b.flac");
        std::fs::write(&a, b"audio").expect("a");
        std::fs::write(&b, b"audio").expect("b");
        let cue = td.path().join("album.cue");
        std::fs::write(
            &cue,
            concat!(
                "FILE \"side-a.flac\" WAVE\n",
                "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
                "  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n",
                "FILE \"side-b.flac\" WAVE\n",
                "  TRACK 03 AUDIO\n    INDEX 01 00:00:00\n",
                "  TRACK 04 AUDIO\n    INDEX 01 04:00:00\n",
            ),
        )
        .expect("cue");

        let report = admit_split_cue_candidate_paths(&[cue.clone()]);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let member = &report.folders[0].members[0];
        assert_eq!(member.cue_path, cue);
        assert_eq!(member.referenced_audio, vec![a.clone(), b.clone()]);
        assert_eq!(member.track_audio_paths, vec![a.clone(), a, b.clone(), b]);
        assert_eq!(member.role, SplitCueMemberRole::SyntheticAlbumPart);
    }

    #[test]
    fn one_track_per_file_is_classified_as_metadata_sidecar() {
        let td = tempfile::tempdir().expect("tempdir");
        let a = td.path().join("track-01.flac");
        let b = td.path().join("track-02.flac");
        std::fs::write(&a, b"audio").expect("a");
        std::fs::write(&b, b"audio").expect("b");
        let cue = td.path().join("album.cue");
        std::fs::write(
            &cue,
            concat!(
                "FILE \"track-01.flac\" WAVE\n",
                "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
                "FILE \"track-02.flac\" WAVE\n",
                "  TRACK 02 AUDIO\n    INDEX 01 00:00:00\n",
            ),
        )
        .expect("cue");

        let member = admit_split_cue_member(&cue).expect("admit one-track-per-file CUE");
        assert_eq!(member.role, SplitCueMemberRole::MetadataSidecar);
        assert!(!member.contributes_synthetic_album_part());
        assert_eq!(member.referenced_audio, vec![a, b]);
    }

    #[test]
    fn one_dangling_member_rejects_the_entire_folder() {
        let td = tempfile::tempdir().expect("tempdir");
        std::fs::write(td.path().join("good.flac"), b"audio").expect("audio");
        let good = td.path().join("good.cue");
        let bad = td.path().join("bad.cue");
        std::fs::write(
            &good,
            "FILE \"good.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .expect("good cue");
        std::fs::write(
            &bad,
            "FILE \"missing.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .expect("bad cue");

        let report = admit_split_cue_candidate_paths(&[good, bad]);
        assert!(report.folders.is_empty());
        assert_eq!(report.rejected_folders.len(), 1);
        assert!(report.warnings.iter().any(|warning| {
            warning.contains("member image missing: missing.flac")
                && warning.contains("conversion will not include this folder")
        }));
    }

    #[test]
    fn grouping_key_is_order_independent_case_folded_and_deduplicated() {
        let td = tempfile::tempdir().expect("tempdir");
        let upper = td.path().join("B.cue");
        let lower = td.path().join("a.cue");
        std::fs::write(&upper, b"").expect("upper cue");
        std::fs::write(&lower, b"").expect("lower cue");

        let native_order = vec![upper.clone(), lower.clone(), upper.clone()];
        let queue_order = vec![lower.clone(), upper.clone()];

        let native_key = grouping_key_from_paths(&native_order);
        let queue_key = grouping_key_from_paths(&queue_order);

        assert_eq!(native_key, queue_key);
        assert_eq!(native_key.len(), 2);
        assert_eq!(
            native_key[0].file_name().and_then(|name| name.to_str()),
            Some("a.cue")
        );
        assert_eq!(
            native_key[1].file_name().and_then(|name| name.to_str()),
            Some("B.cue")
        );
    }
}
