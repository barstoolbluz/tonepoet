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

impl SplitCueAlbumGroupingReason {
    #[must_use]
    pub fn merges_cues(self) -> bool {
        !matches!(self, Self::PerCueDistinctTocHits)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitCueAlbumGroupingDecision {
    pub groups: Vec<Vec<PathBuf>>,
    pub reason: SplitCueAlbumGroupingReason,
    /// Validated, operation/session-scoped proof for merged groups. The map is
    /// private so callers cannot construct membership by combining unrelated
    /// paths. Provenance is installed only through `with_current_member_provenance`,
    /// which captures file-object identity and parsed CUE membership while all
    /// members are readable.
    merged_group_provenance: BTreeMap<Vec<PathBuf>, SplitCueMergedGroupProvenance>,
}

const SPLIT_CUE_GROUP_PROVENANCE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SplitCueFileIdentity {
    canonical_path: PathBuf,
    device_inode: Option<(u64, u64)>,
    created: Option<(u64, u32)>,
    size: u64,
    modified: Option<(u64, u32)>,
}

impl SplitCueFileIdentity {
    fn capture(path: &Path) -> Option<Self> {
        let metadata = std::fs::symlink_metadata(path).ok()?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return None;
        }
        #[cfg(unix)]
        let device_inode = {
            use std::os::unix::fs::MetadataExt;
            Some((metadata.dev(), metadata.ino()))
        };
        #[cfg(not(unix))]
        let device_inode = None;
        let created = metadata.created().ok().and_then(system_time_key);
        if device_inode.is_none() && created.is_none() {
            return None;
        }

        Some(Self {
            canonical_path: cue_path_key(path),
            device_inode,
            created,
            size: metadata.len(),
            modified: metadata.modified().ok().and_then(system_time_key),
        })
    }

    fn same_file_object_now(&self) -> bool {
        let Some(current) = Self::capture(&self.canonical_path) else {
            return false;
        };
        if current.canonical_path != self.canonical_path {
            return false;
        }
        match (self.device_inode, current.device_inode) {
            (Some(expected), Some(actual)) => {
                expected == actual
                    && match (self.created, current.created) {
                        (Some(expected_created), Some(actual_created)) => {
                            expected_created == actual_created
                        }
                        _ => true,
                    }
            }
            _ => match (self.created, current.created) {
                (Some(expected), Some(actual)) => expected == actual,
                _ => false,
            },
        }
    }

    fn same_snapshot_now(&self) -> bool {
        Self::capture(&self.canonical_path).is_some_and(|current| &current == self)
    }

    fn same_snapshot_or_missing_now(&self) -> bool {
        match std::fs::symlink_metadata(&self.canonical_path) {
            Ok(_) => self.same_snapshot_now(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => false,
        }
    }
}

fn system_time_key(value: std::time::SystemTime) -> Option<(u64, u32)> {
    let duration = value.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some((duration.as_secs(), duration.subsec_nanos()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SplitCueMembershipFingerprint {
    album_title: Option<String>,
    tracks: Vec<SplitCueTrackMembershipFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SplitCueTrackMembershipFingerprint {
    number: u32,
    file: Option<String>,
    index01_frames: Option<u32>,
}

impl SplitCueMembershipFingerprint {
    fn from_sheet(sheet: &crate::convert::cue_parser::CueSheet) -> Self {
        Self {
            album_title: sheet.title.as_deref().map(str::trim).map(str::to_owned),
            tracks: sheet
                .tracks
                .iter()
                .map(|track| SplitCueTrackMembershipFingerprint {
                    number: track.number,
                    file: track.file.as_deref().map(normalize_cue_file_reference),
                    index01_frames: track.index01_frames,
                })
                .collect(),
        }
    }
}

fn normalize_cue_file_reference(value: &str) -> String {
    value.replace('\\', "/").to_ascii_lowercase()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SplitCueMemberProvenance {
    cue_identity: SplitCueFileIdentity,
    membership: SplitCueMembershipFingerprint,
    audio_identities: Vec<SplitCueFileIdentity>,
}

impl SplitCueMemberProvenance {
    fn capture(
        cue_path: &Path,
        sheet: &crate::convert::cue_parser::CueSheet,
        audio_paths: &[PathBuf],
    ) -> Option<Self> {
        if audio_paths.is_empty() {
            return None;
        }
        let cue_identity = SplitCueFileIdentity::capture(cue_path)?;
        let mut audio_identities = audio_paths
            .iter()
            .map(|path| SplitCueFileIdentity::capture(path))
            .collect::<Option<Vec<_>>>()?;
        audio_identities.sort_by(|left, right| {
            split_cue_path_cmp(&left.canonical_path, &right.canonical_path)
        });
        audio_identities.dedup_by(|left, right| left.canonical_path == right.canonical_path);
        if audio_identities.is_empty() {
            return None;
        }
        Some(Self {
            cue_identity,
            membership: SplitCueMembershipFingerprint::from_sheet(sheet),
            audio_identities,
        })
    }

    fn audio_paths(&self) -> Vec<PathBuf> {
        self.audio_identities
            .iter()
            .map(|identity| identity.canonical_path.clone())
            .collect()
    }

    fn matches_admitted_member(&self, member: &SplitCueAdmissionMember) -> bool {
        if !self.cue_identity.same_file_object_now()
            || self.membership != SplitCueMembershipFingerprint::from_sheet(&member.sheet)
        {
            return false;
        }
        let current_audio_keys: BTreeSet<PathBuf> = member
            .referenced_audio
            .iter()
            .map(|path| cue_path_key(path))
            .collect();
        let recorded_audio_keys: BTreeSet<PathBuf> = self
            .audio_identities
            .iter()
            .map(|identity| identity.canonical_path.clone())
            .collect();
        current_audio_keys == recorded_audio_keys
            && self
                .audio_identities
                .iter()
                .all(SplitCueFileIdentity::same_snapshot_now)
    }

    fn matches_rejected_member(&self, _rejection: &SplitCueMemberRejection) -> bool {
        // Rejection is the expected failure transition: the proven CUE may now
        // be unreadable or parseable-but-invalid, so its current membership
        // text cannot be required to equal the readable snapshot. File-object
        // identity proves that this is the same established member; atomic
        // replacement at the pathname fails that check. Original member audio
        // may be absent, but any current occupant must still match its captured
        // object/snapshot before the proven group can suppress it.
        self.cue_identity.same_file_object_now()
            && self
                .audio_identities
                .iter()
                .all(SplitCueFileIdentity::same_snapshot_or_missing_now)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SplitCueMergedGroupProvenance {
    version: u32,
    members: BTreeMap<PathBuf, SplitCueMemberProvenance>,
}

#[derive(Debug, Clone)]
pub struct SplitCueValidatedMergedGroupFailure {
    pub cue_paths: Vec<PathBuf>,
    pub audio_paths: Vec<PathBuf>,
    pub rejections: Vec<SplitCueMemberRejection>,
}

impl SplitCueAlbumGroupingDecision {
    /// Capture authoritative membership evidence from the current filesystem
    /// while every member is readable and admissible. The decision's own group
    /// paths are the only inputs: callers cannot attach an unrelated parsed
    /// sheet or audio list. Any incomplete, cross-folder, non-merge, or
    /// identity-less group is omitted atomically rather than partially recorded.
    #[must_use]
    pub fn with_current_member_provenance(mut self) -> Self {
        let mut proven = BTreeMap::new();
        if self.reason.merges_cues() {
            for group in &self.groups {
                let group_key = grouping_key_from_paths(group);
                if group_key.len() < 2 || !same_folder_cue_paths(&group_key) {
                    continue;
                }
                let mut captured = BTreeMap::new();
                let mut complete = true;
                for cue_path in &group_key {
                    let Ok(admitted) = admit_split_cue_member(cue_path) else {
                        complete = false;
                        break;
                    };
                    let Some(member) = SplitCueMemberProvenance::capture(
                        &admitted.cue_path,
                        &admitted.sheet,
                        &admitted.referenced_audio,
                    ) else {
                        complete = false;
                        break;
                    };
                    let cue_parent = cue_path.parent().map(cue_path_key);
                    if cue_parent.is_none()
                        || member.audio_identities.iter().any(|identity| {
                            identity.canonical_path.parent().map(cue_path_key) != cue_parent
                        })
                    {
                        complete = false;
                        break;
                    }
                    captured.insert(cue_path.clone(), member);
                }
                if complete && captured.len() == group_key.len() {
                    proven.insert(
                        group_key,
                        SplitCueMergedGroupProvenance {
                            version: SPLIT_CUE_GROUP_PROVENANCE_VERSION,
                            members: captured,
                        },
                    );
                }
            }
        }
        self.merged_group_provenance = proven;
        self
    }

    #[must_use]
    pub fn member_audio_matches(&self, cue_path: &Path, expected: &[PathBuf]) -> bool {
        let cue_key = cue_path_key(cue_path);
        let expected_keys: BTreeSet<PathBuf> =
            expected.iter().map(|path| cue_path_key(path)).collect();
        self.merged_group_provenance.values().any(|group| {
            group.members.get(&cue_key).is_some_and(|member| {
                member.audio_paths().into_iter().collect::<BTreeSet<_>>() == expected_keys
            })
        })
    }

    #[must_use]
    pub fn complete_member_audio_for_group(&self, group: &[PathBuf]) -> Option<Vec<PathBuf>> {
        let group_key = grouping_key_from_paths(group);
        let provenance = self.merged_group_provenance.get(&group_key)?;
        if provenance.version != SPLIT_CUE_GROUP_PROVENANCE_VERSION
            || provenance.members.len() != group_key.len()
        {
            return None;
        }
        let mut audio_paths = Vec::new();
        for cue_path in &group_key {
            audio_paths.extend(provenance.members.get(cue_path)?.audio_paths());
        }
        Some(dedup_split_cue_paths(audio_paths))
    }

    /// Validate an incomplete merged group against current file identity and
    /// parsed membership. A changed-in-place CUE may become unreadable while
    /// retaining its file-object identity; an atomic replacement at the same
    /// pathname is rejected as stale. Captured audio may be absent, but any
    /// file currently occupying a captured audio path must match its snapshot.
    #[must_use]
    pub fn validated_failed_merged_group(
        &self,
        group: &[PathBuf],
        admitted: &[SplitCueAdmissionMember],
        rejected: &[SplitCueMemberRejection],
    ) -> Option<SplitCueValidatedMergedGroupFailure> {
        if !self.reason.merges_cues() {
            return None;
        }
        let group_key = grouping_key_from_paths(group);
        let provenance = self.merged_group_provenance.get(&group_key)?;
        if provenance.version != SPLIT_CUE_GROUP_PROVENANCE_VERSION
            || group_key.len() < 2
            || provenance.members.len() != group_key.len()
        {
            return None;
        }

        let admitted_by_key: BTreeMap<PathBuf, &SplitCueAdmissionMember> = admitted
            .iter()
            .map(|member| (cue_path_key(&member.cue_path), member))
            .collect();
        let rejected_by_key: BTreeMap<PathBuf, &SplitCueMemberRejection> = rejected
            .iter()
            .map(|rejection| (cue_path_key(&rejection.cue_path), rejection))
            .collect();
        let mut group_rejections = Vec::new();
        let mut group_audio = Vec::new();
        let mut admitted_count = 0usize;

        for cue_path in &group_key {
            let member_provenance = provenance.members.get(cue_path)?;
            if let Some(member) = admitted_by_key.get(cue_path) {
                if !member_provenance.matches_admitted_member(member) {
                    return None;
                }
                admitted_count = admitted_count.saturating_add(1);
            } else if let Some(rejection) = rejected_by_key.get(cue_path) {
                if !member_provenance.matches_rejected_member(rejection) {
                    return None;
                }
                group_rejections.push((*rejection).clone());
            } else {
                return None;
            }
            group_audio.extend(member_provenance.audio_paths());
        }

        if admitted_count == 0 || group_rejections.is_empty() {
            return None;
        }
        Some(SplitCueValidatedMergedGroupFailure {
            cue_paths: group_key,
            audio_paths: dedup_split_cue_paths(group_audio),
            rejections: group_rejections,
        })
    }
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
        merged_group_provenance: BTreeMap::new(),
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
    SplitCueAlbumGroupingDecision {
        groups,
        reason,
        merged_group_provenance: BTreeMap::new(),
    }
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
    /// True only when every FILE token resolved to the literal path named by
    /// the CUE (including its extension). Case-insensitive filename recovery
    /// and same-stem/other-extension recovery are deliberately lower-ranked.
    pub all_file_references_exact: bool,
}

impl SplitCueAdmissionMember {
    #[must_use]
    pub fn contributes_synthetic_album_part(&self) -> bool {
        self.role == SplitCueMemberRole::SyntheticAlbumPart
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitCueMemberRejectionReason {
    ParseFailure { detail: String },
    NoTracks,
    NoParent,
    MissingFileReference { track_number: u32 },
    MissingIndex01 { track_number: u32 },
    MissingImage { file_reference: String },
    AmbiguousImage {
        file_reference: String,
        candidates: Vec<PathBuf>,
    },
    UnsupportedImage { path: PathBuf },
    NonRegularImage { path: PathBuf },
    ImageOutsideCueFolder { path: PathBuf },
    NonIncreasingIndex {
        track_number: u32,
        path: PathBuf,
        previous_track_number: u32,
        previous_index: u32,
    },
}

impl SplitCueMemberRejectionReason {
    #[must_use]
    pub fn is_parse_failure(&self) -> bool {
        matches!(self, Self::ParseFailure { .. })
    }
}

impl std::fmt::Display for SplitCueMemberRejectionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ParseFailure { detail } => write!(f, "member CUE invalid: {detail}"),
            Self::NoTracks => f.write_str("member CUE has no tracks"),
            Self::NoParent => f.write_str("member CUE has no parent"),
            Self::MissingFileReference { track_number } => {
                write!(f, "member CUE track {track_number} has no FILE reference")
            }
            Self::MissingIndex01 { track_number } => {
                write!(f, "member CUE track {track_number} has no INDEX 01")
            }
            Self::MissingImage { file_reference } => {
                write!(f, "member image missing: {file_reference}")
            }
            Self::AmbiguousImage {
                file_reference,
                candidates,
            } => write!(
                f,
                "member image ambiguous: {file_reference}: {}",
                candidates
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::UnsupportedImage { path } => {
                write!(f, "member image is not supported audio: {}", path.display())
            }
            Self::NonRegularImage { path } => {
                write!(f, "member image is not a regular file: {}", path.display())
            }
            Self::ImageOutsideCueFolder { path } => {
                write!(f, "member image is outside the CUE folder: {}", path.display())
            }
            Self::NonIncreasingIndex {
                track_number,
                path,
                previous_track_number,
                previous_index,
            } => write!(
                f,
                "member CUE has non-increasing INDEX 01 for track {track_number} in {}; previous track {previous_track_number} was at frame {previous_index}",
                path.display()
            ),
        }
    }
}

/// Typed admission rejection. `parsed_sheet` is present only when byte-level
/// parsing succeeded, allowing callers to use title/FILE evidence without
/// string parsing. It is deliberately absent for parse failures: membership of
/// an unreadable CUE must come from authoritative grouping provenance, never
/// from its filename or an error-message prefix.
#[derive(Debug, Clone)]
pub struct SplitCueMemberRejection {
    pub cue_path: PathBuf,
    pub reason: SplitCueMemberRejectionReason,
    pub parsed_sheet: Option<Box<crate::convert::cue_parser::CueSheet>>,
    pub resolved_in_folder_audio: Vec<PathBuf>,
}

impl SplitCueMemberRejection {
    fn parse_failure(cue_path: &Path, detail: String) -> Self {
        Self {
            cue_path: cue_path.to_path_buf(),
            reason: SplitCueMemberRejectionReason::ParseFailure { detail },
            parsed_sheet: None,
            resolved_in_folder_audio: Vec::new(),
        }
    }

    fn from_parsed_sheet(
        cue_path: &Path,
        sheet: &crate::convert::cue_parser::CueSheet,
        reason: SplitCueMemberRejectionReason,
    ) -> Self {
        Self {
            cue_path: cue_path.to_path_buf(),
            reason,
            parsed_sheet: Some(Box::new(sheet.clone())),
            resolved_in_folder_audio: resolved_in_folder_audio_references(cue_path, sheet),
        }
    }

    #[must_use]
    pub fn is_parse_failure(&self) -> bool {
        self.reason.is_parse_failure()
    }
}

impl std::fmt::Display for SplitCueMemberRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.cue_path.display(), self.reason)
    }
}

impl std::error::Error for SplitCueMemberRejection {}

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
    /// Structured rejection facts (one per rejected folder). Callers that
    /// need to distinguish "an alien cue referencing nothing in this folder"
    /// from "a local cue-backed album that is malformed" read these instead
    /// of string-matching warnings.
    pub rejections: Vec<SplitCueFolderRejection>,
}

#[derive(Debug, Clone)]
pub struct SplitCueFolderRejection {
    pub parent: PathBuf,
    pub offending_cue: PathBuf,
    /// True when the offending cue plausibly describes local content — it
    /// resolved at least one EXISTING audio file inside its own folder tree,
    /// or it could not be parsed at all (fail closed: an unparseable local
    /// cue is the MOST malformed case, not an alien one). Atomic refusal
    /// must hold. False only for alien sheets (absolute/outside refs,
    /// nothing local): safe to degrade to a plain-files editor.
    pub references_in_folder_audio: bool,
    /// True when at least one OTHER cue in the same folder admitted cleanly.
    /// The folder then genuinely holds a local cue album, so a stray alien
    /// cue must not downgrade it to plain image-level editing.
    pub folder_admitted_local_members: bool,
}

/// Non-destructive provenance that the shared folder-selection policy
/// classified multiple parseable multi-FILE CUEs as one album before one or
/// more members could be rejected. Queue expansion may use this only to
/// preserve the selected album's synthetic representation; it is deliberately
/// insufficient for fail-closed suppression, which requires complete
/// cue-to-audio provenance in `SplitCueAlbumGroupingDecision`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitCueSelectionAlbumGroup {
    cue_paths: Vec<PathBuf>,
}

impl SplitCueSelectionAlbumGroup {
    #[must_use]
    pub fn contains(&self, cue_path: &Path) -> bool {
        let cue_key = cue_path_key(cue_path);
        self.cue_paths.iter().any(|path| *path == cue_key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitCueReferenceResolution {
    Resolved(PathBuf),
    Missing,
    Ambiguous(Vec<PathBuf>),
    /// The referenced path exists but is not a supported audio target
    /// (non-audio extension, symlink, or not a regular file). Distinct from
    /// Missing so callers can report the real problem instead of
    /// "was not found", and so an existing-but-unsupported direct target is
    /// never silently rebound to a same-stem audio sibling.
    UnsupportedTarget(PathBuf),
}

/// Folder-local cue selection outcome shared by metadata editing and queue
/// expansion. Selection happens before the full title/TOC grouping ladder so
/// alternative descriptions of the same image can never be merged into a
/// synthetic multi-part album. It records only a non-destructive title-rung
/// result needed to preserve representation after a parseable member rejects.
#[derive(Debug, Clone)]
pub enum SplitCueFolderSelection {
    /// One unambiguous cue, or an all-exact pairwise-disjoint cue set that may
    /// continue through the existing album grouping ladder.
    Selected {
        members: Vec<SplitCueAdmissionMember>,
        /// Resolved audio claimed only by excluded alternatives (including
        /// partially-resolvable rejected CUEs). Queue expansion suppresses
        /// these paths so a selection cannot leak alternatives back as raw jobs.
        excluded_audio: Vec<PathBuf>,
        rejected: Vec<SplitCueMemberRejection>,
        /// Album-group membership established by the shared title rung from
        /// parseable multi-FILE candidates before selection removed invalid
        /// members. This is representational provenance only, not authority
        /// to suppress an unreadable member's unknown audio.
        selection_album_group: Option<SplitCueSelectionAlbumGroup>,
    },
    /// More than one viable alternative remains after exact-match ranking.
    /// The caller must ask the user to choose exactly one.
    NeedsChoice {
        candidates: Vec<SplitCueAdmissionMember>,
        rejected: Vec<SplitCueMemberRejection>,
    },
    /// No candidate resolves safely to local audio. Callers must fall back to
    /// their ordinary file/TOC path and surface the rejection visibly.
    NoViable {
        rejected: Vec<SplitCueMemberRejection>,
    },
}

/// Select viable CUE descriptions for one folder.
///
/// Precedence is intentionally narrow and deterministic:
/// 1. the sole viable CUE, regardless of its downstream role;
/// 2. the highest-ranked exact candidates, retaining all of them only when
///    their resolved audio sets are pairwise disjoint (the established
///    split-album case);
/// 3. otherwise a validated operation-scoped choice among the currently tied,
///    equally best-ranked candidates.
///
/// Role classification is consulted only for one narrow duplicate-description
/// tie: an equally ranked single-image split source dominates a one-track
/// metadata sidecar for that exact same image. Exact-reference ranking still comes first,
/// so an exact metadata sidecar continues to beat a fallback split source.
/// Otherwise selection remains role-neutral. Rejected alternatives never
/// poison a viable winner.
#[must_use]
pub fn select_split_cue_folder_members(
    cue_paths: &[PathBuf],
    selected_cue: Option<&Path>,
) -> SplitCueFolderSelection {
    let mut ordered_paths = cue_paths.to_vec();
    ordered_paths.sort_by(|left, right| split_cue_path_cmp(left, right));
    ordered_paths.dedup_by(|left, right| cue_path_key(left) == cue_path_key(right));

    let mut members = Vec::new();
    let mut rejected = Vec::new();
    let mut rejected_audio = Vec::new();
    for cue_path in ordered_paths {
        match admit_split_cue_member(&cue_path) {
            Ok(member) => members.push(member),
            Err(rejection) => {
                rejected_audio.extend(rejection.resolved_in_folder_audio.iter().cloned());
                rejected.push(rejection);
            }
        }
    }

    let selection_album_group =
        selection_album_group_from_parseable_multi_file_candidates(&members, &rejected);

    if members.is_empty() {
        return SplitCueFolderSelection::NoViable { rejected };
    }

    if members.len() == 1 {
        return selected_split_cue_folder_members(
            &members,
            members.clone(),
            rejected_audio,
            rejected,
            selection_album_group,
        );
    }

    let exact_members: Vec<SplitCueAdmissionMember> = members
        .iter()
        .filter(|member| member.all_file_references_exact)
        .cloned()
        .collect();
    let mut candidates = if exact_members.is_empty() {
        members.clone()
    } else {
        exact_members.clone()
    };
    if candidates.len() == 1 {
        return selected_split_cue_folder_members(
            &members,
            candidates,
            rejected_audio,
            rejected,
            selection_album_group,
        );
    }

    candidates = remove_metadata_sidecars_covered_by_split_sources(candidates);
    if candidates.len() == 1 {
        return selected_split_cue_folder_members(
            &members,
            candidates,
            rejected_audio,
            rejected,
            selection_album_group,
        );
    }

    if !exact_members.is_empty()
        && split_cue_member_audio_sets_are_pairwise_disjoint(&candidates)
    {
        return selected_split_cue_folder_members(
            &members,
            candidates,
            rejected_audio,
            rejected,
            selection_album_group,
        );
    }

    if let Some(selected_cue) = selected_cue {
        let selected_key = cue_path_key(selected_cue);
        if let Some(selected) = candidates
            .iter()
            .find(|member| cue_path_key(&member.cue_path) == selected_key)
            .cloned()
        {
            return selected_split_cue_folder_members(
                &members,
                vec![selected],
                rejected_audio,
                rejected,
                selection_album_group,
            );
        }
    }

    SplitCueFolderSelection::NeedsChoice {
        candidates,
        rejected,
    }
}

fn selection_album_group_from_parseable_multi_file_candidates(
    members: &[SplitCueAdmissionMember],
    rejected: &[SplitCueMemberRejection],
) -> Option<SplitCueSelectionAlbumGroup> {
    let mut cue_paths = Vec::new();
    let mut titles = Vec::new();

    for member in members.iter().filter(|member| {
        member.contributes_synthetic_album_part() && member.referenced_audio.len() > 1
    }) {
        cue_paths.push(member.cue_path.clone());
        titles.push(member.sheet.title.clone().unwrap_or_default());
    }

    for rejection in rejected {
        let Some(sheet) = rejection.parsed_sheet.as_deref() else {
            continue;
        };
        if rejection.resolved_in_folder_audio.is_empty()
            || !cue_sheet_has_multi_file_split_shape(sheet)
        {
            continue;
        }
        cue_paths.push(rejection.cue_path.clone());
        titles.push(sheet.title.clone().unwrap_or_default());
    }

    let decision = title_rung_decision(&cue_paths, &titles)?;
    let cue_paths = decision.groups.into_iter().next()?;
    (cue_paths.len() >= 2).then_some(SplitCueSelectionAlbumGroup { cue_paths })
}

fn cue_sheet_has_multi_file_split_shape(sheet: &crate::convert::cue_parser::CueSheet) -> bool {
    let mut tracks_by_file: BTreeMap<String, usize> = BTreeMap::new();
    for file_ref in sheet
        .tracks
        .iter()
        .filter_map(|track| track.file.as_deref())
    {
        let normalized = file_ref.replace('\\', "/").to_ascii_lowercase();
        *tracks_by_file.entry(normalized).or_default() += 1;
    }
    tracks_by_file.len() > 1 && tracks_by_file.values().any(|count| *count > 1)
}

fn remove_metadata_sidecars_covered_by_split_sources(
    candidates: Vec<SplitCueAdmissionMember>,
) -> Vec<SplitCueAdmissionMember> {
    let single_image_split_sources: Vec<PathBuf> = candidates
        .iter()
        .filter(|member| {
            member.role == SplitCueMemberRole::SyntheticAlbumPart
                && member.referenced_audio.len() == 1
        })
        .filter_map(|member| member.referenced_audio.first().map(|path| cue_path_key(path)))
        .collect();
    if single_image_split_sources.is_empty() {
        return candidates;
    }

    candidates
        .into_iter()
        .filter(|member| {
            let is_one_track_single_image_artifact = member.role
                == SplitCueMemberRole::MetadataSidecar
                && member.sheet.tracks.len() == 1
                && member.referenced_audio.len() == 1;
            !is_one_track_single_image_artifact
                || match member.referenced_audio.first() {
                    Some(path) => !single_image_split_sources.contains(&cue_path_key(path)),
                    None => true,
                }
        })
        .collect()
}

fn selected_split_cue_folder_members(
    all_members: &[SplitCueAdmissionMember],
    mut selected_members: Vec<SplitCueAdmissionMember>,
    mut excluded_audio: Vec<PathBuf>,
    rejected: Vec<SplitCueMemberRejection>,
    selection_album_group: Option<SplitCueSelectionAlbumGroup>,
) -> SplitCueFolderSelection {
    let selected_cue_keys: HashSet<PathBuf> = selected_members
        .iter()
        .map(|member| cue_path_key(&member.cue_path))
        .collect();
    let selected_audio_keys: HashSet<PathBuf> = selected_members
        .iter()
        .flat_map(|member| member.referenced_audio.iter())
        .map(|path| cue_path_key(path))
        .collect();

    excluded_audio.extend(
        all_members
            .iter()
            .filter(|member| !selected_cue_keys.contains(&cue_path_key(&member.cue_path)))
            .flat_map(|member| member.referenced_audio.iter().cloned()),
    );
    excluded_audio.retain(|path| !selected_audio_keys.contains(&cue_path_key(path)));
    selected_members.sort_by(|left, right| split_cue_path_cmp(&left.cue_path, &right.cue_path));

    SplitCueFolderSelection::Selected {
        members: selected_members,
        excluded_audio: dedup_split_cue_paths(excluded_audio),
        rejected,
        selection_album_group,
    }
}

fn resolved_in_folder_audio_references(
    cue_path: &Path,
    sheet: &crate::convert::cue_parser::CueSheet,
) -> Vec<PathBuf> {
    let Some(parent) = cue_path.parent() else {
        return Vec::new();
    };
    let parent_key = cue_path_key(parent);
    let mut resolved = Vec::new();
    for file_ref in sheet
        .tracks
        .iter()
        .filter_map(|track| track.file.as_deref())
    {
        let SplitCueReferenceResolution::Resolved(path) =
            resolve_split_cue_file_reference(parent, file_ref)
        else {
            continue;
        };
        if path
            .parent()
            .map(cue_path_key)
            .as_ref()
            == Some(&parent_key)
        {
            resolved.push(path);
        }
    }
    dedup_split_cue_paths(resolved)
}

fn dedup_split_cue_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort_by(|left, right| split_cue_path_cmp(left, right));
    paths.dedup_by(|left, right| cue_path_key(left) == cue_path_key(right));
    paths
}

fn split_cue_member_audio_sets_are_pairwise_disjoint(
    members: &[SplitCueAdmissionMember],
) -> bool {
    let mut seen = HashSet::new();
    for member in members {
        for audio_path in &member.referenced_audio {
            if !seen.insert(cue_path_key(audio_path)) {
                return false;
            }
        }
    }
    true
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
        // Examine EVERY cue in the folder — stopping at the first offender
        // would let an alien cue that sorts first mask a local malformed one,
        // and would discard the fact that other members admitted cleanly.
        // Both facts feed the caller's degrade-vs-refuse decision.
        let mut members = Vec::with_capacity(cues.len());
        let mut rejections: Vec<SplitCueMemberRejection> = Vec::new();
        for cue_path in &cues {
            match admit_split_cue_member(cue_path) {
                Ok(member) => members.push(member),
                Err(rejection) => rejections.push(rejection),
            }
        }
        if !rejections.is_empty() {
            report.rejected_folders.push(parent_key.clone());
            let folder_admitted_local_members = !members.is_empty();
            for rejection in rejections {
                let offending_cue = rejection.cue_path.clone();
                report.rejections.push(SplitCueFolderRejection {
                    parent: parent_key.clone(),
                    offending_cue: offending_cue.clone(),
                    references_in_folder_audio: rejection.is_parse_failure()
                        || !rejection.resolved_in_folder_audio.is_empty(),
                    folder_admitted_local_members,
                });
                report.warnings.push(format!(
                    "offending CUE {}: {} — conversion will not include this folder ({})",
                    offending_cue.display(),
                    rejection.reason,
                    parent_key.display()
                ));
            }
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
pub fn admit_split_cue_member(
    cue_path: &Path,
) -> Result<SplitCueAdmissionMember, SplitCueMemberRejection> {
    let sheet = crate::convert::cue_parser::parse_cue_file(cue_path)
        .map_err(|err| SplitCueMemberRejection::parse_failure(cue_path, err.to_string()))?;
    if sheet.tracks.is_empty() {
        return Err(SplitCueMemberRejection::from_parsed_sheet(
            cue_path,
            &sheet,
            SplitCueMemberRejectionReason::NoTracks,
        ));
    }
    let Some(parent) = cue_path.parent() else {
        return Err(SplitCueMemberRejection::from_parsed_sheet(
            cue_path,
            &sheet,
            SplitCueMemberRejectionReason::NoParent,
        ));
    };
    let parent_key = cue_path_key(parent);
    let mut referenced_audio = Vec::new();
    let mut referenced_keys = HashSet::new();
    let mut track_audio_paths = Vec::with_capacity(sheet.tracks.len());
    let mut previous_by_file: BTreeMap<PathBuf, (u32, u32)> = BTreeMap::new();
    let mut all_file_references_exact = true;

    for track in &sheet.tracks {
        let Some(file_ref) = track.file.as_deref() else {
            return Err(SplitCueMemberRejection::from_parsed_sheet(
                cue_path,
                &sheet,
                SplitCueMemberRejectionReason::MissingFileReference {
                    track_number: track.number,
                },
            ));
        };
        let Some(index01) = track.index01_frames else {
            return Err(SplitCueMemberRejection::from_parsed_sheet(
                cue_path,
                &sheet,
                SplitCueMemberRejectionReason::MissingIndex01 {
                    track_number: track.number,
                },
            ));
        };
        let normalized_ref = file_ref.replace('\\', &std::path::MAIN_SEPARATOR.to_string());
        let raw_path = PathBuf::from(&normalized_ref);
        let direct = if raw_path.is_absolute() {
            raw_path
        } else {
            parent.join(raw_path)
        };
        let direct_is_exact_audio = split_cue_regular_audio_file(&direct);
        let resolved = match resolve_split_cue_file_reference(parent, file_ref) {
            SplitCueReferenceResolution::Resolved(path) => path,
            SplitCueReferenceResolution::Missing => {
                return Err(SplitCueMemberRejection::from_parsed_sheet(
                    cue_path,
                    &sheet,
                    SplitCueMemberRejectionReason::MissingImage {
                        file_reference: file_ref.to_string(),
                    },
                ));
            }
            SplitCueReferenceResolution::Ambiguous(paths) => {
                return Err(SplitCueMemberRejection::from_parsed_sheet(
                    cue_path,
                    &sheet,
                    SplitCueMemberRejectionReason::AmbiguousImage {
                        file_reference: file_ref.to_string(),
                        candidates: paths,
                    },
                ));
            }
            SplitCueReferenceResolution::UnsupportedTarget(path) => {
                return Err(SplitCueMemberRejection::from_parsed_sheet(
                    cue_path,
                    &sheet,
                    SplitCueMemberRejectionReason::UnsupportedImage { path },
                ));
            }
        };
        all_file_references_exact &= direct_is_exact_audio
            && cue_path_key(&direct) == cue_path_key(&resolved);
        let meta = match std::fs::symlink_metadata(&resolved) {
            Ok(meta) => meta,
            Err(_) => {
                return Err(SplitCueMemberRejection::from_parsed_sheet(
                    cue_path,
                    &sheet,
                    SplitCueMemberRejectionReason::MissingImage {
                        file_reference: file_ref.to_string(),
                    },
                ));
            }
        };
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(SplitCueMemberRejection::from_parsed_sheet(
                cue_path,
                &sheet,
                SplitCueMemberRejectionReason::NonRegularImage { path: resolved },
            ));
        }
        if !matches!(
            crate::convert::classify::classify_file(&resolved),
            crate::convert::classify::EntryKind::AudioFile(_)
        ) {
            return Err(SplitCueMemberRejection::from_parsed_sheet(
                cue_path,
                &sheet,
                SplitCueMemberRejectionReason::UnsupportedImage { path: resolved },
            ));
        }
        if resolved.parent().map(cue_path_key).as_ref() != Some(&parent_key) {
            return Err(SplitCueMemberRejection::from_parsed_sheet(
                cue_path,
                &sheet,
                SplitCueMemberRejectionReason::ImageOutsideCueFolder { path: resolved },
            ));
        }
        let resolved_key = cue_path_key(&resolved);
        if let Some((previous_track, previous_index)) = previous_by_file.get(&resolved_key) {
            if index01 <= *previous_index {
                return Err(SplitCueMemberRejection::from_parsed_sheet(
                    cue_path,
                    &sheet,
                    SplitCueMemberRejectionReason::NonIncreasingIndex {
                        track_number: track.number,
                        path: resolved,
                        previous_track_number: *previous_track,
                        previous_index: *previous_index,
                    },
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
        all_file_references_exact,
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
    if split_cue_regular_audio_file(&direct) {
        return SplitCueReferenceResolution::Resolved(direct);
    }
    if std::fs::symlink_metadata(&direct).is_ok() {
        // The literal target exists but failed the audio test — report that,
        // and never rebind past it to a same-stem sibling.
        return SplitCueReferenceResolution::UnsupportedTarget(direct);
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

fn split_cue_regular_audio_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    !meta.file_type().is_symlink()
        && meta.is_file()
        && matches!(
            crate::convert::classify::classify_file(path),
            crate::convert::classify::EntryKind::AudioFile(_)
        )
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

    // A complete FLAC container with one final STREAMINFO block describing an
    // empty 44.1 kHz, 16-bit stereo stream. Admission does not probe content,
    // but using a structurally valid container keeps the real-fixture test
    // faithful without requiring an encoder or external test dependency.
    const TINY_VALID_FLAC: &[u8] = &[
        0x66, 0x4c, 0x61, 0x43, 0x80, 0x00, 0x00, 0x22, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x0a, 0xc4, 0x42, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn merged_group_provenance_requires_every_member_and_ignores_outsiders() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue_a = temp.path().join("side-a.cue");
        let cue_b = temp.path().join("side-b.cue");
        let outsider = temp.path().join("bonus.cue");
        let audio_a = temp.path().join("side-a.flac");
        let audio_b = temp.path().join("side-b.flac");
        let outsider_audio = temp.path().join("bonus.flac");
        for audio in [&audio_a, &audio_b, &outsider_audio] {
            std::fs::write(audio, b"audio").expect("audio fixture");
        }
        for (cue, title, audio) in [
            (&cue_a, "Album Side A", "side-a.flac"),
            (&cue_b, "Album Side B", "side-b.flac"),
            (&outsider, "Bonus", "bonus.flac"),
        ] {
            std::fs::write(
                cue,
                format!(
                    "TITLE \"{title}\"\nFILE \"{audio}\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n"
                ),
            )
            .expect("cue fixture");
        }
        let group = vec![cue_a.clone(), cue_b.clone()];

        let original_b = std::fs::read(&cue_b).expect("read side B cue");
        std::fs::write(&cue_b, [0xff, 0xfe, 0x00]).expect("temporarily invalidate side B cue");
        let incomplete = merge_decision(&group, SplitCueAlbumGroupingReason::AmbiguousMerge)
            .with_current_member_provenance();
        assert!(incomplete.complete_member_audio_for_group(&group).is_none());

        std::fs::write(&cue_b, original_b).expect("restore side B cue");
        let complete = merge_decision(&group, SplitCueAlbumGroupingReason::AmbiguousMerge)
            .with_current_member_provenance();
        let recorded = complete
            .complete_member_audio_for_group(&group)
            .expect("complete group provenance");
        assert_eq!(
            recorded.into_iter().collect::<BTreeSet<_>>(),
            [cue_path_key(&audio_a), cue_path_key(&audio_b)]
                .into_iter()
                .collect::<BTreeSet<_>>(),
        );
        assert!(!complete.member_audio_matches(&outsider, &[outsider_audio]));
    }

    #[test]
    fn alien_cue_rejection_reports_no_in_folder_audio() {
        let temp = tempfile::tempdir().expect("temp dir");
        // Folder of plain audio + a published planner sheet whose FILE refs
        // are absolute paths OUTSIDE the folder — the poisoned-output shape.
        std::fs::write(temp.path().join("01 - One.flac"), b"audio").expect("fixture");
        let outside = tempfile::tempdir().expect("outside dir");
        let outside_image = outside.path().join("source.wv");
        std::fs::write(&outside_image, b"image").expect("fixture");
        let cue = temp.path().join("album.cue");
        std::fs::write(
            &cue,
            format!(
                "FILE \"{}\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
                outside_image.display()
            ),
        )
        .expect("fixture cue");

        let report = admit_split_cue_folders(&[cue]);
        assert!(report.folders.is_empty());
        assert_eq!(report.rejections.len(), 1);
        assert!(
            !report.rejections[0].references_in_folder_audio,
            "an alien sheet resolves no in-folder audio: {:?}",
            report.rejections[0]
        );
    }

    #[test]
    fn malformed_local_cue_rejection_reports_in_folder_audio() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join("side.flac"), b"audio").expect("fixture");
        // Local image resolves fine, but TRACK 01 has no INDEX 01 — a
        // genuinely local cue-backed album that must keep atomic refusal.
        let cue = temp.path().join("side.cue");
        std::fs::write(
            &cue,
            "FILE \"side.flac\" WAVE\n  TRACK 01 AUDIO\n  TRACK 02 AUDIO\n    INDEX 01 01:00:00\n",
        )
        .expect("fixture cue");

        let report = admit_split_cue_folders(&[cue]);
        assert!(report.folders.is_empty());
        assert_eq!(report.rejections.len(), 1);
        assert!(
            report.rejections[0].references_in_folder_audio,
            "a local cue-backed album must keep the atomic refusal: {:?}",
            report.rejections[0]
        );
    }

    #[test]
    fn alien_cue_does_not_downgrade_folder_with_admitted_local_album() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join("side.flac"), b"audio").expect("fixture");
        // A valid local cue album...
        let local_cue = temp.path().join("side.cue");
        std::fs::write(
            &local_cue,
            "FILE \"side.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 01:00:00\n",
        )
        .expect("fixture cue");
        // ...plus an alien sheet that SORTS FIRST (album < side): the
        // rejection sweep must still examine the local cue and record that
        // the folder admitted a genuine member, so the caller refuses
        // atomically instead of degrading to image-level plain editing.
        let outside = tempfile::tempdir().expect("outside dir");
        let outside_image = outside.path().join("source.wv");
        std::fs::write(&outside_image, b"image").expect("fixture");
        let alien_cue = temp.path().join("album.cue");
        std::fs::write(
            &alien_cue,
            format!(
                "FILE \"{}\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
                outside_image.display()
            ),
        )
        .expect("fixture cue");

        let report = admit_split_cue_folders(&[temp.path().to_path_buf()]);
        assert!(report.folders.is_empty(), "folder must stay rejected");
        assert!(
            !report.rejections.is_empty(),
            "alien cue must be recorded: {:?}",
            report.warnings
        );
        assert!(
            report
                .rejections
                .iter()
                .all(|rejection| rejection.folder_admitted_local_members),
            "the admitted local album must veto the alien-only degrade: {:?}",
            report.rejections
        );
    }

    #[test]
    fn unreadable_cue_rejection_is_typed() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue = temp.path().join("ghost.cue");
        let rejection = admit_split_cue_member(&cue).expect_err("missing cue rejects");
        assert!(
            rejection.is_parse_failure(),
            "an unreadable cue must retain a typed parse-failure reason"
        );
    }

    #[test]
    fn existing_non_audio_direct_target_reports_unsupported_not_missing() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join("notes.txt"), b"not audio").expect("fixture");
        // A same-stem audio sibling exists — the resolver must NOT silently
        // rebind past the existing literal target.
        std::fs::write(temp.path().join("notes.flac"), b"audio").expect("fixture");

        match resolve_split_cue_file_reference(temp.path(), "notes.txt") {
            SplitCueReferenceResolution::UnsupportedTarget(path) => {
                assert!(path.ends_with("notes.txt"));
            }
            other => panic!("expected UnsupportedTarget, got {other:?}"),
        }

        // A genuinely absent reference still reports Missing... unless a
        // same-name audio candidate exists (the deliberate fallback search).
        match resolve_split_cue_file_reference(temp.path(), "absent.txt") {
            SplitCueReferenceResolution::Missing => {}
            other => panic!("expected Missing, got {other:?}"),
        }
    }


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
    fn real_world_frame_overflow_admits_the_complete_multi_file_album() {
        const SIDE_1: &str = "Kool & The Gang, Emergency, 1984 (Side 1).flac";
        const SIDE_2: &str = "Kool & The Gang, Emergency, 1984 (Side 2).flac";

        let td = tempfile::tempdir().expect("tempdir");
        let side_1 = td.path().join(SIDE_1);
        let side_2 = td.path().join(SIDE_2);
        std::fs::write(&side_1, TINY_VALID_FLAC).expect("side 1 FLAC stub");
        std::fs::write(&side_2, TINY_VALID_FLAC).expect("side 2 FLAC stub");

        let cue = td.path().join("Kool & The Gang, Emergency, 1984.cue");
        std::fs::write(
            &cue,
            include_bytes!("../../fixtures/Kool_and_The_Gang_Emergency_1984.cue"),
        )
        .expect("real malformed CUE fixture");

        let member = admit_split_cue_member(&cue)
            .expect("recoverable frame overflow must not suppress the album");
        assert_eq!(member.role, SplitCueMemberRole::SyntheticAlbumPart);
        assert_eq!(member.sheet.tracks.len(), 7);
        assert_eq!(member.track_audio_paths.len(), 7);
        assert_eq!(member.referenced_audio.len(), 2);
        assert_eq!(member.referenced_audio, vec![side_1.clone(), side_2.clone()]);
        assert_eq!(member.sheet.tracks[2].index01_frames, Some(43955));
        assert_eq!(
            member.track_audio_paths,
            vec![
                side_1.clone(),
                side_1.clone(),
                side_1.clone(),
                side_1,
                side_2.clone(),
                side_2.clone(),
                side_2,
            ]
        );

        let SplitCueFolderSelection::Selected { members, .. } =
            select_split_cue_folder_members(&[cue], None)
        else {
            panic!("the shared metadata/conversion selector must retain the CUE surface");
        };
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].role, SplitCueMemberRole::SyntheticAlbumPart);
        assert_eq!(members[0].track_audio_paths.len(), 7);
    }

    #[test]
    fn normalized_overflow_cannot_bypass_per_file_index_monotonicity() {
        let td = tempfile::tempdir().expect("tempdir");
        std::fs::write(td.path().join("side.flac"), TINY_VALID_FLAC).expect("FLAC stub");
        let cue = td.path().join("album.cue");
        std::fs::write(
            &cue,
            concat!(
                "FILE \"side.flac\" WAVE\n",
                "  TRACK 01 AUDIO\n    INDEX 01 00:59:74\n",
                // 58 seconds + 149 frames also normalizes to frame 4499.
                "  TRACK 02 AUDIO\n    INDEX 01 00:58:149\n",
            ),
        )
        .expect("cue");

        let rejection = admit_split_cue_member(&cue)
            .expect_err("equal normalized offsets must remain unusable for splitting");
        assert!(matches!(
            rejection.reason,
            SplitCueMemberRejectionReason::NonIncreasingIndex {
                track_number: 2,
                previous_track_number: 1,
                previous_index: 4499,
                ..
            }
        ));
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

    #[test]
    fn same_image_alternatives_prefer_the_unique_exact_filename_match() {
        let td = tempfile::tempdir().expect("tempdir");
        let image = td.path().join("Get Off (LP).wv");
        std::fs::write(&image, b"audio").expect("audio");
        let fallback = td.path().join("Get Off (LP).cue");
        let exact = td.path().join("Get Off (LP) WV.cue");
        for (cue, file_ref) in [
            (&fallback, "Get Off (LP).wav"),
            (&exact, "Get Off (LP).wv"),
        ] {
            std::fs::write(
                cue,
                format!(
                    "TITLE \"Get Off (LP)\"\nFILE \"{file_ref}\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n"
                ),
            )
            .expect("cue");
        }

        let SplitCueFolderSelection::Selected { members, .. } =
            select_split_cue_folder_members(&[fallback, exact.clone()], None)
        else {
            panic!("unique exact match must be selected without a prompt");
        };
        assert_eq!(members.len(), 1);
        assert_eq!(cue_path_key(&members[0].cue_path), cue_path_key(&exact));
        assert!(members[0].all_file_references_exact);
        assert_eq!(members[0].referenced_audio, vec![image]);
    }

    #[test]
    fn exact_metadata_sidecar_beats_fallback_split_source_for_the_same_image() {
        let td = tempfile::tempdir().expect("tempdir");
        let image = td.path().join("album.wv");
        std::fs::write(&image, b"audio").expect("audio");
        let fallback_split = td.path().join("fallback.cue");
        let exact_metadata = td.path().join("exact.cue");
        std::fs::write(
            &fallback_split,
            concat!(
                "FILE \"album.wav\" WAVE\n",
                "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
                "  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n",
            ),
        )
        .expect("fallback split cue");
        std::fs::write(
            &exact_metadata,
            "FILE \"album.wv\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .expect("exact metadata cue");

        let SplitCueFolderSelection::Selected {
            members,
            excluded_audio,
            ..
        } = select_split_cue_folder_members(
            &[fallback_split, exact_metadata.clone()],
            None,
        )
        else {
            panic!("the role-neutral exact match must win");
        };
        assert_eq!(members.len(), 1);
        assert_eq!(cue_path_key(&members[0].cue_path), cue_path_key(&exact_metadata));
        assert_eq!(members[0].role, SplitCueMemberRole::MetadataSidecar);
        assert!(members[0].all_file_references_exact);
        assert!(
            excluded_audio.is_empty(),
            "audio owned by the winner must never be suppressed as alternative-only"
        );
        assert_eq!(members[0].referenced_audio, vec![image]);
    }

    #[test]
    fn split_source_role_tiebreak_does_not_remove_unrelated_one_track_sidecar() {
        let td = tempfile::tempdir().expect("tempdir");
        let split_image = td.path().join("album.flac");
        let unrelated_image = td.path().join("interview.flac");
        let split_cue = td.path().join("album.cue");
        let unrelated_sidecar = td.path().join("interview.cue");
        std::fs::write(&split_image, b"audio").expect("split image");
        std::fs::write(&unrelated_image, b"audio").expect("unrelated image");
        std::fs::write(
            &split_cue,
            concat!(
                "FILE \"album.flac\" WAVE\n",
                "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
                "  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n",
            ),
        )
        .expect("split cue");
        std::fs::write(
            &unrelated_sidecar,
            "FILE \"interview.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .expect("unrelated metadata cue");

        let SplitCueFolderSelection::Selected { members, .. } =
            select_split_cue_folder_members(
                &[split_cue.clone(), unrelated_sidecar.clone()],
                None,
            )
        else {
            panic!("disjoint exact CUEs must remain independently queueable");
        };
        let selected: BTreeSet<PathBuf> = members
            .iter()
            .map(|member| cue_path_key(&member.cue_path))
            .collect();
        assert_eq!(selected.len(), 2);
        assert!(selected.contains(&cue_path_key(&split_cue)));
        assert!(selected.contains(&cue_path_key(&unrelated_sidecar)));
    }

    #[test]
    fn split_source_role_tiebreak_does_not_remove_partial_overlap_sidecar() {
        let td = tempfile::tempdir().expect("tempdir");
        let first = td.path().join("first.flac");
        let second = td.path().join("second.flac");
        let split_cue = td.path().join("album-split.cue");
        let partial_sidecar = td.path().join("first-index.cue");
        std::fs::write(&first, b"audio").expect("first image");
        std::fs::write(&second, b"audio").expect("second image");
        std::fs::write(
            &split_cue,
            concat!(
                "FILE \"first.flac\" WAVE\n",
                "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
                "  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n",
                "FILE \"second.flac\" WAVE\n",
                "  TRACK 03 AUDIO\n    INDEX 01 00:00:00\n",
            ),
        )
        .expect("multi-image split cue");
        std::fs::write(
            &partial_sidecar,
            "FILE \"first.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .expect("partial sidecar");

        let SplitCueFolderSelection::NeedsChoice { candidates, .. } =
            select_split_cue_folder_members(
                &[split_cue.clone(), partial_sidecar.clone()],
                None,
            )
        else {
            panic!("partial overlap must remain visible instead of being role-suppressed");
        };
        let candidate_paths: BTreeSet<PathBuf> = candidates
            .iter()
            .map(|member| cue_path_key(&member.cue_path))
            .collect();
        assert_eq!(candidate_paths.len(), 2);
        assert!(candidate_paths.contains(&cue_path_key(&split_cue)));
        assert!(candidate_paths.contains(&cue_path_key(&partial_sidecar)));
    }

    #[test]
    fn exact_metadata_sidecar_beats_fallback_metadata_alternative() {
        let td = tempfile::tempdir().expect("tempdir");
        let image = td.path().join("album.wv");
        std::fs::write(&image, b"audio").expect("audio");
        let fallback = td.path().join("fallback.cue");
        let exact = td.path().join("exact.cue");
        std::fs::write(
            &fallback,
            "FILE \"album.wav\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .expect("fallback metadata cue");
        std::fs::write(
            &exact,
            "FILE \"album.wv\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .expect("exact metadata cue");

        let SplitCueFolderSelection::Selected { members, .. } =
            select_split_cue_folder_members(&[fallback, exact.clone()], None)
        else {
            panic!("the unique exact metadata sidecar must win without a prompt");
        };
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].role, SplitCueMemberRole::MetadataSidecar);
        assert_eq!(cue_path_key(&members[0].cue_path), cue_path_key(&exact));
        assert_eq!(members[0].referenced_audio, vec![image]);
    }

    #[test]
    fn equally_ranked_metadata_sidecars_for_one_image_prompt_and_honor_the_choice() {
        let td = tempfile::tempdir().expect("tempdir");
        let image = td.path().join("album.wv");
        std::fs::write(&image, b"audio").expect("audio");
        let first = td.path().join("first.cue");
        let second = td.path().join("second.cue");
        for cue in [&first, &second] {
            std::fs::write(
                cue,
                "FILE \"album.wv\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
            )
            .expect("metadata cue");
        }

        let SplitCueFolderSelection::NeedsChoice { candidates, .. } =
            select_split_cue_folder_members(&[first.clone(), second.clone()], None)
        else {
            panic!("equally ranked metadata alternatives must prompt");
        };
        assert_eq!(candidates.len(), 2);
        assert!(candidates
            .iter()
            .all(|member| member.role == SplitCueMemberRole::MetadataSidecar));

        let SplitCueFolderSelection::Selected {
            members,
            excluded_audio,
            ..
        } = select_split_cue_folder_members(
            &[first, second.clone()],
            Some(&second),
        )
        else {
            panic!("the operation-scoped metadata-sidecar choice must be honored");
        };
        assert_eq!(members.len(), 1);
        assert_eq!(cue_path_key(&members[0].cue_path), cue_path_key(&second));
        assert_eq!(members[0].referenced_audio, vec![image]);
        assert!(excluded_audio.is_empty());
    }

    #[test]
    fn all_exact_pairwise_disjoint_cues_continue_to_album_grouping() {
        let td = tempfile::tempdir().expect("tempdir");
        let mut cues = Vec::new();
        for side in ["a", "b"] {
            std::fs::write(td.path().join(format!("side_{side}.flac")), b"audio")
                .expect("audio");
            let cue = td.path().join(format!("side_{side}.cue"));
            std::fs::write(
                &cue,
                format!(
                    "FILE \"side_{side}.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n"
                ),
            )
            .expect("cue");
            cues.push(cue);
        }

        let SplitCueFolderSelection::Selected { members, .. } =
            select_split_cue_folder_members(&cues, None)
        else {
            panic!("disjoint exact split-album cues must remain groupable");
        };
        assert_eq!(members.len(), 2);
        assert!(members.iter().all(|member| member.all_file_references_exact));

        let SplitCueFolderSelection::Selected { members, .. } =
            select_split_cue_folder_members(&cues, Some(&cues[0]))
        else {
            panic!("a stale choice must not collapse a currently disjoint exact set");
        };
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn operation_scoped_choice_is_honored_after_revalidation() {
        let td = tempfile::tempdir().expect("tempdir");
        let mut cues = Vec::new();
        for side in ["a", "b"] {
            std::fs::write(td.path().join(format!("side_{side}.flac")), b"audio")
                .expect("audio");
            let cue = td.path().join(format!("side_{side}.cue"));
            std::fs::write(
                &cue,
                format!(
                    "FILE \"side_{side}.wav\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n"
                ),
            )
            .expect("cue");
            cues.push(cue);
        }

        assert!(matches!(
            select_split_cue_folder_members(&cues, None),
            SplitCueFolderSelection::NeedsChoice { .. }
        ));
        let selected = cues[1].clone();
        let SplitCueFolderSelection::Selected { members, .. } =
            select_split_cue_folder_members(&cues, Some(&selected))
        else {
            panic!("a validated operation-scoped choice must remain authoritative");
        };
        assert_eq!(members.len(), 1);
        assert_eq!(cue_path_key(&members[0].cue_path), cue_path_key(&selected));
    }

    #[test]
    fn equally_ranked_fallback_cues_require_an_operation_scoped_choice() {
        let td = tempfile::tempdir().expect("tempdir");
        let mut cues = Vec::new();
        for side in ["a", "b"] {
            std::fs::write(td.path().join(format!("side_{side}.wv")), b"audio")
                .expect("audio");
            let cue = td.path().join(format!("side_{side}.cue"));
            std::fs::write(
                &cue,
                format!(
                    "FILE \"side_{side}.wav\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n"
                ),
            )
            .expect("cue");
            cues.push(cue);
        }

        let SplitCueFolderSelection::NeedsChoice { candidates, .. } =
            select_split_cue_folder_members(&cues, None)
        else {
            panic!("equally-ranked fallback cues must prompt");
        };
        assert_eq!(candidates.len(), 2);
        assert!(candidates
            .iter()
            .all(|member| !member.all_file_references_exact));
    }

    #[test]
    fn sole_viable_cue_wins_even_when_an_alternative_is_dangling() {
        let td = tempfile::tempdir().expect("tempdir");
        std::fs::write(td.path().join("album.flac"), b"audio").expect("audio");
        let good = td.path().join("good.cue");
        let bad = td.path().join("bad.cue");
        std::fs::write(
            &good,
            "FILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n",
        )
        .expect("good cue");
        std::fs::write(
            &bad,
            "FILE \"missing.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .expect("bad cue");

        let SplitCueFolderSelection::Selected { members, rejected, .. } =
            select_split_cue_folder_members(&[bad.clone(), good.clone()], None)
        else {
            panic!("the sole viable cue must be selected");
        };
        assert_eq!(members.len(), 1);
        assert_eq!(cue_path_key(&members[0].cue_path), cue_path_key(&good));
        assert_eq!(rejected.len(), 1);
        assert_eq!(cue_path_key(&rejected[0].cue_path), cue_path_key(&bad));
    }

    #[test]
    fn no_viable_cue_reports_fallback_instead_of_inventing_a_merge() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("missing.cue");
        std::fs::write(
            &cue,
            "FILE \"missing.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .expect("cue");

        let SplitCueFolderSelection::NoViable { rejected } =
            select_split_cue_folder_members(&[cue], None)
        else {
            panic!("no viable cue must fall back");
        };
        assert_eq!(rejected.len(), 1);
        assert!(matches!(
            rejected[0].reason,
            SplitCueMemberRejectionReason::MissingImage { .. }
        ));
    }
}
