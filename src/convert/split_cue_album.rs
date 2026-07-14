//! Shared split-CUE album grouping policy.
//!
//! This module is deliberately below both the TUI and queue-expansion layers so
//! metadata dispatch and conversion queue construction use the same album
//! identity ladder instead of maintaining separate title-normalization rules.

use std::cmp::Ordering;
use std::collections::BTreeSet;
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
