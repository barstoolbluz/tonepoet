//! Metadata-based pre-emphasis evidence detection.
//!
//! Checks audio file tags, associated CUE files, and associated EAC/XLD log
//! files for positive pre-emphasis indicators. This tier is authoritative:
//! if metadata says pre-emphasis is present, we trust it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use encoding_rs::{WINDOWS_1251, WINDOWS_1252};

/// Evidence source for pre-emphasis detection.
#[derive(Debug, Clone, PartialEq)]
pub enum PreemphasisEvidence {
    /// PRE_EMPHASIS or PRE-EMPHASIS tag found in the audio file.
    Tag,
    /// COMMENT tag gives positive pre-emphasis evidence.
    CommentTag,
    /// FLAGS PRE found in an associated CUE file.
    CueFile,
    /// Pre-emphasis positively reported in an associated EAC/XLD log file.
    LogFile,
    /// Catalog number exactly matches a known PE pressing.
    CatalogExact,
    /// Catalog number matches a known PE series, but not an exact title.
    CatalogSeries,
}

impl PreemphasisEvidence {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Tag => "tag",
            Self::CommentTag => "comment tag",
            Self::CueFile => "CUE file",
            Self::LogFile => "log file",
            Self::CatalogExact => "catalog match",
            Self::CatalogSeries => "catalog series",
        }
    }
}

/// Check audio file tags for pre-emphasis indicators via lofty.
/// Returns the evidence type if found.
pub fn check_tag_evidence(audio_path: &Path) -> Option<PreemphasisEvidence> {
    if let Some(evidence) = check_pre_flag_tag_evidence(audio_path) {
        return Some(evidence);
    }

    use lofty::file::TaggedFileExt;
    use lofty::tag::ItemKey;

    let tagged = lofty::read_from_path(audio_path).ok()?;
    for tag in tagged.tags() {
        // Historical broader detector: check COMMENT tag for a positive
        // pre-emphasis statement. The Phase 2 metadata editor does not call
        // this function; it calls check_pre_flag_tag_evidence instead.
        if let Some(comment) = tag.get_string(&ItemKey::Comment) {
            if comment_has_positive_preemphasis(comment) {
                return Some(PreemphasisEvidence::CommentTag);
            }
        }
    }
    None
}

/// Check only explicit metadata PRE flag fields, excluding COMMENT heuristics.
/// This is the tag-tier function used by the Phase 2 metadata editor path.
pub fn check_pre_flag_tag_evidence(audio_path: &Path) -> Option<PreemphasisEvidence> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::ItemKey;

    let tagged = lofty::read_from_path(audio_path).ok()?;
    for tag in tagged.tags() {
        let pe_keys = [
            ItemKey::Unknown("PRE_EMPHASIS".to_string()),
            ItemKey::Unknown("PRE-EMPHASIS".to_string()),
            ItemKey::Unknown("PRE EMPHASIS".to_string()),
            ItemKey::Unknown("PREEMPHASIS".to_string()),
            ItemKey::Unknown("Pre-emphasis".to_string()),
            ItemKey::Unknown("Pre Emphasis".to_string()),
            ItemKey::Unknown("Preemphasis".to_string()),
            ItemKey::Unknown("pre_emphasis".to_string()),
            ItemKey::Unknown("pre-emphasis".to_string()),
            ItemKey::Unknown("pre emphasis".to_string()),
            ItemKey::Unknown("preemphasis".to_string()),
        ];
        for key in &pe_keys {
            if let Some(val) = tag.get_string(key) {
                if is_affirmative_preemphasis_value(val) {
                    return Some(PreemphasisEvidence::Tag);
                }
            }
        }
    }
    None
}

/// Check associated CUE files for explicit FLAGS PRE evidence.
///
/// This deliberately excludes EAC/XLD log heuristics so Phase 2 metadata-editor
/// behavior is driven only by PRE flags and catalog matching.
pub fn check_cue_evidence(audio_path: &Path) -> Option<PreemphasisEvidence> {
    let dir = audio_path.parent()?;

    if directory_has_cue_preemphasis_for_audio(dir, audio_path) {
        return Some(PreemphasisEvidence::CueFile);
    }

    if let Some(parent) = dir.parent() {
        if directory_has_cue_preemphasis_for_audio(parent, audio_path) {
            return Some(PreemphasisEvidence::CueFile);
        }
    }

    None
}

/// Check associated CUE and log files for pre-emphasis evidence.
///
/// Same-directory sidecars are checked first. Parent-directory sidecars are
/// checked for common disc 01/disc 02 layouts, but only when the CUE/log can be
/// associated with the target audio file. The scan order is stable, so repeated
/// runs over the same tree report the same evidence kind.
pub fn check_file_evidence(audio_path: &Path) -> Option<PreemphasisEvidence> {
    let dir = audio_path.parent()?;

    if directory_has_cue_preemphasis_for_audio(dir, audio_path) {
        return Some(PreemphasisEvidence::CueFile);
    }

    if directory_has_log_preemphasis_for_audio(dir, audio_path) {
        return Some(PreemphasisEvidence::LogFile);
    }

    if let Some(parent) = dir.parent() {
        if directory_has_cue_preemphasis_for_audio(parent, audio_path) {
            return Some(PreemphasisEvidence::CueFile);
        }

        if directory_has_log_preemphasis_for_audio(parent, audio_path) {
            return Some(PreemphasisEvidence::LogFile);
        }
    }

    None
}

fn directory_has_cue_preemphasis_for_audio(dir: &Path, audio_path: &Path) -> bool {
    sorted_paths_with_extension(dir, "cue")
        .iter()
        .filter(|path| crate::tui::cue_parser::is_user_visible_cue_path(path))
        .any(|path| check_cue_file_for_audio_preemphasis(path, audio_path))
}

fn directory_has_log_preemphasis_for_audio(dir: &Path, audio_path: &Path) -> bool {
    sorted_paths_with_extension(dir, "log")
        .iter()
        .any(|path| check_log_file_for_audio_preemphasis(path, audio_path))
}

fn sorted_paths_with_extension(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path_has_extension(path, extension))
            .collect(),
        Err(_) => Vec::new(),
    };

    paths.sort_by(|a, b| normalized_path_text(a).cmp(&normalized_path_text(b)));
    paths
}

fn path_has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map_or(false, |ext| ext.eq_ignore_ascii_case(extension))
}

/// Parse a CUE file looking for FLAGS PRE on a track whose canonical FILE
/// ownership resolves to the target audio file.
///
/// Syntax and FILE/TRACK ownership deliberately come from the canonical CUE
/// parser. In particular, this handles older EAC sheets that put a new FILE
/// between a track's INDEX 00 and INDEX 01; the parser rebinds that track to
/// the later FILE before this layer interprets FLAGS PRE.
fn check_cue_file_for_audio_preemphasis(cue_path: &Path, audio_path: &Path) -> bool {
    if !crate::tui::cue_parser::is_user_visible_cue_path(cue_path) {
        return false;
    }
    let Ok(sheet) = crate::tui::cue_parser::parse_cue_file(cue_path) else {
        return false;
    };
    let Some(cue_dir) = cue_path.parent() else {
        return false;
    };
    let audio_key = crate::convert::queue_expansion::queue_path_key(audio_path);

    sheet.tracks.iter().any(|track| {
        if !cue_track_has_pre_flag(track) {
            return false;
        }
        let Some(file_ref) = track.file.as_deref() else {
            return false;
        };
        match resolve_cue_file_reference_for_preemphasis(cue_dir, file_ref) {
            crate::tui::browse::CueReferenceResolution::Resolved(path) => {
                crate::convert::queue_expansion::queue_path_key(&path) == audio_key
            }
            crate::tui::browse::CueReferenceResolution::Missing
            | crate::tui::browse::CueReferenceResolution::Ambiguous(_)
            | crate::tui::browse::CueReferenceResolution::UnsupportedTarget(_) => false,
        }
    })
}

/// Resolve a CUE FILE reference for pre-emphasis evidence without weakening
/// the canonical split-CUE resolver. Obsolete absolute paths are a historical
/// EAC artifact: if and only if the canonical result is Missing, retry the
/// basename in the CUE's current directory through the same fail-closed
/// resolver. Ambiguous and unsupported canonical results remain authoritative.
fn resolve_cue_file_reference_for_preemphasis(
    cue_dir: &Path,
    file_ref: &str,
) -> crate::tui::browse::CueReferenceResolution {
    let primary = crate::tui::browse::resolve_cue_file_reference_for_queue(cue_dir, file_ref);
    if !matches!(
        &primary,
        crate::tui::browse::CueReferenceResolution::Missing
    ) {
        return primary;
    }
    if !is_unmistakably_absolute_legacy_cue_reference(file_ref) {
        return primary;
    }
    let Some(basename) = sidecar_reference_basename(file_ref) else {
        return primary;
    };
    crate::tui::browse::resolve_cue_file_reference_for_queue(cue_dir, &basename)
}

fn is_unmistakably_absolute_legacy_cue_reference(file_ref: &str) -> bool {
    let normalized = file_ref.trim().replace('\\', "/");
    if normalized.starts_with('/') {
        return true;
    }
    let bytes = normalized.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes[2] == b'/'
}

/// Return true only for real CUE FLAGS lines that include PRE as a flag token.
pub(crate) fn cue_line_has_pre_flag(line: &str) -> bool {
    let line = line.trim();

    if line.is_empty() {
        return false;
    }

    let mut parts = line.split_whitespace();

    match parts.next() {
        Some(first) if first.eq_ignore_ascii_case("FLAGS") => {
            parts.any(|part| part.eq_ignore_ascii_case("PRE"))
        }
        _ => false,
    }
}

fn cue_track_has_pre_flag(track: &crate::tui::cue_parser::CueTrack) -> bool {
    track
        .directives
        .iter()
        .any(|directive| cue_line_has_pre_flag(directive))
}

pub(crate) fn cue_sheet_has_pre_flag(sheet: &crate::tui::cue_parser::CueSheet) -> bool {
    sheet.tracks.iter().any(cue_track_has_pre_flag)
}

/// Return the subset of `audio_paths` with reliably associated CUE `FLAGS PRE`
/// evidence. The caller supplies only technically eligible Convert sources.
///
/// Candidate CUEs/logs are read once per shallow search directory, so a batch
/// of album tracks does not reparse the same sidecars once per member.
pub(crate) fn convert_cue_flag_evidence_for_paths(audio_paths: &[PathBuf]) -> BTreeSet<PathBuf> {
    if audio_paths.is_empty() {
        return BTreeSet::new();
    }

    let target_by_key: BTreeMap<PathBuf, PathBuf> = audio_paths
        .iter()
        .map(|path| {
            (
                crate::convert::queue_expansion::queue_path_key(path),
                path.clone(),
            )
        })
        .collect();
    let target_dirs = unique_parent_dirs(audio_paths);
    let search_dirs = sidecar_search_dirs(audio_paths);

    let mut cues = Vec::new();
    let mut log_paths = Vec::new();
    for dir in &search_dirs {
        for cue_path in sorted_paths_with_extension(dir, "cue") {
            if !crate::tui::cue_parser::is_user_visible_cue_path(&cue_path) {
                continue;
            }
            let Ok(sheet) = crate::tui::cue_parser::parse_cue_file(&cue_path) else {
                continue;
            };
            cues.push(ConvertCueCandidate {
                path: cue_path,
                sheet,
            });
        }
        for log_path in sorted_paths_with_extension(dir, "log") {
            if path_is_hidden_sidecar(&log_path) {
                continue;
            }
            log_paths.push(log_path);
        }
    }

    dedup_cue_candidates(&mut cues);
    dedup_sidecar_paths(&mut log_paths);

    let mut evidence_keys = BTreeSet::new();
    let mut unresolved_pre_tracks = BTreeMap::<usize, BTreeSet<u32>>::new();

    for (cue_index, cue) in cues.iter().enumerate() {
        let Some(cue_dir) = cue.path.parent() else {
            continue;
        };
        for track in &cue.sheet.tracks {
            if !cue_track_has_pre_flag(track) {
                continue;
            }
            let resolved_key = track.file.as_deref().and_then(|file_ref| {
                match resolve_cue_file_reference_for_preemphasis(cue_dir, file_ref) {
                    crate::tui::browse::CueReferenceResolution::Resolved(path) => {
                        Some(crate::convert::queue_expansion::queue_path_key(&path))
                    }
                    crate::tui::browse::CueReferenceResolution::Missing
                    | crate::tui::browse::CueReferenceResolution::Ambiguous(_)
                    | crate::tui::browse::CueReferenceResolution::UnsupportedTarget(_) => None,
                }
            });
            if let Some(key) = resolved_key {
                if target_by_key.contains_key(&key) {
                    evidence_keys.insert(key);
                    continue;
                }
            }
            unresolved_pre_tracks
                .entry(cue_index)
                .or_default()
                .insert(track.number);
        }
    }

    if unresolved_pre_tracks.is_empty() {
        return evidence_keys
            .into_iter()
            .filter_map(|key| target_by_key.get(&key).cloned())
            .collect();
    }

    // A log is a fallback bridge only: avoid opening logs at all when direct
    // CUE association was sufficient. It can associate an unresolved CUE
    // track with a current carrier, but never contributes PRE evidence itself.
    let bridge_dirs: BTreeSet<PathBuf> = unresolved_pre_tracks
        .keys()
        .filter_map(|index| cues.get(*index)?.path.parent().map(Path::to_path_buf))
        .collect();
    let mut logs: Vec<EacLogBridge> = log_paths
        .into_iter()
        .filter(|path| {
            path.parent()
                .map(Path::to_path_buf)
                .is_some_and(|parent| bridge_dirs.contains(&parent))
        })
        .filter_map(|path| parse_eac_log_bridge(&path))
        .collect();
    dedup_log_candidates(&mut logs);

    for (cue_index, log_index) in
        mutually_unambiguous_cue_log_pairs(&cues, &logs, unresolved_pre_tracks.keys().copied())
    {
        let Some(track_numbers) = unresolved_pre_tracks.get(&cue_index) else {
            continue;
        };
        let log = &logs[log_index];
        for track_number in track_numbers {
            let Some(filename) = log.track_filenames.get(track_number) else {
                continue;
            };
            if let Some(key) = resolve_log_filename_to_target_key(
                filename,
                &target_dirs,
                &target_by_key,
            ) {
                evidence_keys.insert(key);
            }
        }
    }

    evidence_keys
        .into_iter()
        .filter_map(|key| target_by_key.get(&key).cloned())
        .collect()
}

pub(crate) fn convert_cue_flag_evidence_for_path(audio_path: &Path) -> bool {
    convert_cue_flag_evidence_for_paths(&[audio_path.to_path_buf()]).contains(audio_path)
}

#[derive(Debug, Clone)]
struct ConvertCueCandidate {
    path: PathBuf,
    sheet: crate::tui::cue_parser::CueSheet,
}

#[derive(Debug, Clone)]
struct EacLogBridge {
    path: PathBuf,
    album_performer: Option<String>,
    album_title: Option<String>,
    track_filenames: BTreeMap<u32, String>,
    track_count: usize,
}

fn unique_parent_dirs(paths: &[PathBuf]) -> Vec<PathBuf> {
    let dirs: BTreeMap<PathBuf, PathBuf> = paths
        .iter()
        .filter_map(|path| {
            let dir = path.parent()?.to_path_buf();
            Some((
                crate::convert::queue_expansion::queue_path_key(&dir),
                dir,
            ))
        })
        .collect();
    dirs.into_values().collect()
}

fn sidecar_search_dirs(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = unique_parent_dirs(paths);
    let parents: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|dir| dir.parent().map(Path::to_path_buf))
        .collect();
    dirs.extend(parents);
    let by_key: BTreeMap<PathBuf, PathBuf> = dirs
        .into_iter()
        .map(|dir| {
            (
                crate::convert::queue_expansion::queue_path_key(&dir),
                dir,
            )
        })
        .collect();
    by_key.into_values().collect()
}

fn path_is_hidden_sidecar(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

fn dedup_cue_candidates(cues: &mut Vec<ConvertCueCandidate>) {
    cues.sort_by(|a, b| a.path.cmp(&b.path));
    cues.dedup_by(|a, b| a.path == b.path);
}

fn dedup_sidecar_paths(paths: &mut Vec<PathBuf>) {
    paths.sort();
    paths.dedup();
}

fn dedup_log_candidates(logs: &mut Vec<EacLogBridge>) {
    logs.sort_by(|a, b| a.path.cmp(&b.path));
    logs.dedup_by(|a, b| a.path == b.path);
}

fn read_eac_log_text(path: &Path) -> Option<String> {
    let raw = fs::read(path).ok()?;
    if raw.starts_with(&[0xFF, 0xFE]) {
        let u16s: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        return Some(String::from_utf16_lossy(&u16s));
    }
    if raw.starts_with(&[0xFE, 0xFF]) {
        let u16s: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
            .collect();
        return Some(String::from_utf16_lossy(&u16s));
    }
    if let Ok(text) = std::str::from_utf8(&raw) {
        return Some(text.to_owned());
    }

    // EAC logs in the CD-ripping ecosystem commonly use either the Western
    // Windows code page or Windows-1251. Both are broad single-byte encodings,
    // so "first decoder that accepts the bytes" is not meaningful. Score the
    // two deterministic candidates instead: Windows-1252 remains the tie-break
    // for ordinary Western logs, while coherent Cyrillic runs let Windows-1251
    // win without turning isolated Western accents into Cyrillic mojibake.
    let mut candidates = Vec::new();
    if let Some(text) = WINDOWS_1252.decode_without_bom_handling_and_without_replacement(&raw) {
        candidates.push((legacy_eac_log_decode_score(&text), 0u8, text.into_owned()));
    }
    if let Some(text) = WINDOWS_1251.decode_without_bom_handling_and_without_replacement(&raw) {
        candidates.push((legacy_eac_log_decode_score(&text), 1u8, text.into_owned()));
    }
    candidates
        .into_iter()
        .max_by(|left, right| {
            left.0
                .cmp(&right.0)
                // Lower priority wins ties: Windows-1252 is the established
                // Western default for pathless legacy text.
                .then_with(|| right.1.cmp(&left.1))
        })
        .map(|(_, _, text)| text)
}

fn legacy_eac_log_decode_score(text: &str) -> i64 {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Script {
        Latin,
        Cyrillic,
    }

    fn script(ch: char) -> Option<Script> {
        let code = ch as u32;
        if ch.is_ascii_alphabetic()
            || (0x00C0..=0x024F).contains(&code)
            || (0x1E00..=0x1EFF).contains(&code)
        {
            Some(Script::Latin)
        } else if (0x0400..=0x052F).contains(&code)
            || (0x2DE0..=0x2DFF).contains(&code)
            || (0xA640..=0xA69F).contains(&code)
        {
            Some(Script::Cyrillic)
        } else {
            None
        }
    }

    let mut score = 0i64;
    let mut previous_script = None;
    let mut cyrillic_run = 0usize;
    let mut cyrillic_run_bonus = 0i64;

    for ch in text.chars() {
        if ch.is_control() && !matches!(ch, '\n' | '\r' | '\t') {
            score -= 100;
        }
        match script(ch) {
            Some(current) => {
                score += 1;
                if previous_script.is_some_and(|previous| previous != current) {
                    score -= 20;
                }
                previous_script = Some(current);
                if current == Script::Cyrillic {
                    cyrillic_run += 1;
                    if cyrillic_run >= 3 {
                        // Genuine Windows-1251 names normally contain runs of
                        // Cyrillic letters; Western text misdecoded as 1251
                        // tends to create isolated Cyrillic characters instead.
                        cyrillic_run_bonus += 8;
                    }
                } else {
                    cyrillic_run = 0;
                }
            }
            None => {
                previous_script = None;
                cyrillic_run = 0;
            }
        }
    }

    score + cyrillic_run_bonus
}

fn parse_eac_log_bridge(path: &Path) -> Option<EacLogBridge> {
    let content = read_eac_log_text(path)?;
    let mut album_identity_candidates = BTreeSet::<(String, String)>::new();
    let mut current_track = None;
    let mut track_filenames = BTreeMap::<u32, String>::new();
    let mut ambiguous_track_filenames = BTreeSet::<u32>::new();
    let mut track_numbers = BTreeSet::<u32>::new();
    let mut saw_track = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(track_number) = eac_track_header_number(trimmed) {
            current_track = Some(track_number);
            track_numbers.insert(track_number);
            saw_track = true;
            continue;
        }

        if !saw_track {
            if let Some((performer, title)) = eac_album_identity_line(trimmed) {
                album_identity_candidates.insert((
                    normalized_identity_text(&performer),
                    normalized_identity_text(&title),
                ));
            }
        }

        if let (Some(track_number), Some(filename)) =
            (current_track, eac_filename_reference(trimmed))
        {
            if ambiguous_track_filenames.contains(&track_number) {
                continue;
            }
            match track_filenames.get(&track_number) {
                None => {
                    track_filenames.insert(track_number, filename);
                }
                Some(existing)
                    if normalized_sidecar_filename(existing)
                        == normalized_sidecar_filename(&filename) => {}
                Some(_) => {
                    // Conflicting Filename records for one track make that
                    // track unusable as a deterministic bridge.
                    track_filenames.remove(&track_number);
                    ambiguous_track_filenames.insert(track_number);
                }
            }
        }
    }

    if track_numbers.is_empty() {
        return None;
    }

    let (album_performer, album_title) = if album_identity_candidates.len() == 1 {
        let (performer, title) = album_identity_candidates.into_iter().next()?;
        (Some(performer), Some(title))
    } else {
        (None, None)
    };

    Some(EacLogBridge {
        path: path.to_path_buf(),
        album_performer,
        album_title,
        track_filenames,
        track_count: track_numbers.len(),
    })
}

fn eac_track_header_number(line: &str) -> Option<u32> {
    let mut parts = line.split_whitespace();
    let first = parts.next()?;
    if !first.eq_ignore_ascii_case("Track") {
        return None;
    }
    let token = parts.next()?;
    if !token.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    parse_track_number(token)
}

fn eac_filename_reference(line: &str) -> Option<String> {
    let keyword_len = "Filename".len();
    let prefix = line.get(..keyword_len)?;
    if !prefix.eq_ignore_ascii_case("Filename") {
        return None;
    }
    if line
        .as_bytes()
        .get(keyword_len)
        .is_some_and(|byte| !byte.is_ascii_whitespace())
    {
        return None;
    }
    let value = line.get(keyword_len..)?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn eac_album_identity_line(line: &str) -> Option<(String, String)> {
    if line.is_empty() || line.contains('\\') || line.contains("://") {
        return None;
    }
    let separator = line.match_indices('/').find_map(|(index, _)| {
        let left_is_space = line[..index]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
        let right_is_space = line[index + 1..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace);
        (left_is_space && right_is_space).then_some(index)
    })?;
    let performer = line[..separator].trim();
    let title = line[separator + 1..].trim();
    if performer.is_empty() || title.is_empty() {
        return None;
    }
    Some((performer.to_string(), title.to_string()))
}

fn normalized_identity_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn normalized_file_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(normalized_identity_text)
}

fn cue_log_identity_score(cue: &ConvertCueCandidate, log: &EacLogBridge) -> Option<u16> {
    if cue.path.parent() != log.path.parent() {
        return None;
    }

    let mut score = 0u16;
    if normalized_file_stem(&cue.path) == normalized_file_stem(&log.path) {
        score += 8;
    }

    if let (Some(cue_title), Some(log_title)) =
        (cue.sheet.title.as_deref(), log.album_title.as_deref())
    {
        if normalized_identity_text(cue_title) != normalized_identity_text(log_title) {
            return None;
        }
        score += 3;
    }
    if let (Some(cue_performer), Some(log_performer)) =
        (cue.sheet.performer.as_deref(), log.album_performer.as_deref())
    {
        if normalized_identity_text(cue_performer) != normalized_identity_text(log_performer) {
            return None;
        }
        score += 3;
    }
    if !cue.sheet.tracks.is_empty() && cue.sheet.tracks.len() == log.track_count {
        score += 1;
    }

    // Directory colocation or track count alone is never enough to join a CUE
    // and log. Require an exact album field or a natural same-stem pair.
    (score >= 3).then_some(score)
}

fn unique_best_index(scores: &[(usize, u16)]) -> Option<usize> {
    let best_score = scores.iter().map(|(_, score)| *score).max()?;
    let mut best = scores.iter().filter(|(_, score)| *score == best_score);
    let first = best.next()?.0;
    if best.next().is_some() {
        None
    } else {
        Some(first)
    }
}

fn mutually_unambiguous_cue_log_pairs(
    cues: &[ConvertCueCandidate],
    logs: &[EacLogBridge],
    cue_indices: impl IntoIterator<Item = usize>,
) -> Vec<(usize, usize)> {
    let cue_indices: BTreeSet<usize> = cue_indices.into_iter().collect();
    let cue_best: BTreeMap<usize, Option<usize>> = cue_indices
        .iter()
        .filter_map(|cue_index| {
            let cue = cues.get(*cue_index)?;
            let scores: Vec<(usize, u16)> = logs
                .iter()
                .enumerate()
                .filter_map(|(index, log)| {
                    cue_log_identity_score(cue, log).map(|score| (index, score))
                })
                .collect();
            Some((*cue_index, unique_best_index(&scores)))
        })
        .collect();
    let log_best: Vec<Option<usize>> = logs
        .iter()
        .map(|log| {
            let scores: Vec<(usize, u16)> = cues
                .iter()
                .enumerate()
                .filter(|(index, _)| cue_indices.contains(index))
                .filter_map(|(index, cue)| {
                    cue_log_identity_score(cue, log).map(|score| (index, score))
                })
                .collect();
            unique_best_index(&scores)
        })
        .collect();

    cue_best
        .into_iter()
        .filter_map(|(cue_index, log_index)| {
            let log_index = log_index?;
            (log_best.get(log_index).copied().flatten() == Some(cue_index))
                .then_some((cue_index, log_index))
        })
        .collect()
}

fn normalized_sidecar_filename(value: &str) -> String {
    value.trim().replace('\\', "/").to_lowercase()
}

fn sidecar_reference_basename(value: &str) -> Option<String> {
    let normalized = value.trim().replace('\\', "/");
    Path::new(&normalized)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn resolve_log_filename_to_target_key(
    filename: &str,
    target_dirs: &[PathBuf],
    target_by_key: &BTreeMap<PathBuf, PathBuf>,
) -> Option<PathBuf> {
    let basename = sidecar_reference_basename(filename)?;
    let mut resolved_target_keys = BTreeSet::new();

    for dir in target_dirs {
        if let crate::tui::browse::CueReferenceResolution::Resolved(path) =
            crate::tui::browse::resolve_cue_file_reference_for_queue(dir, &basename)
        {
            let key = crate::convert::queue_expansion::queue_path_key(&path);
            if target_by_key.contains_key(&key) {
                resolved_target_keys.insert(key);
            }
        }
    }

    if resolved_target_keys.len() == 1 {
        resolved_target_keys.into_iter().next()
    } else {
        None
    }
}

/// Check an EAC/XLD log file for positive pre-emphasis evidence associated
/// with the target audio file.
fn check_log_file_for_audio_preemphasis(log_path: &Path, audio_path: &Path) -> bool {
    let content = match fs::read_to_string(log_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let association = if sidecar_path_stem_matches_audio(log_path, audio_path) {
        LogAssociation::DedicatedSidecar
    } else {
        LogAssociation::SharedLog
    };

    log_content_has_positive_preemphasis_for_audio(&content, audio_path, association)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogAssociation {
    /// The log filename stem matches the target audio filename stem in the same directory.
    DedicatedSidecar,
    /// The log is shared by more than one possible target, such as an album-level rip log.
    SharedLog,
}

#[derive(Debug, Default)]
struct LogBlockState {
    track_number: Option<u32>,
    has_target_audio_reference: bool,
    has_other_audio_reference: bool,
}

fn sidecar_path_stem_matches_audio(sidecar_path: &Path, audio_path: &Path) -> bool {
    paths_have_same_parent(sidecar_path, audio_path)
        && paths_have_same_audio_stem(sidecar_path, audio_path)
}

fn log_content_has_positive_preemphasis_for_audio(
    content: &str,
    audio_path: &Path,
    association: LogAssociation,
) -> bool {
    let audio_track_number = leading_track_number(audio_path);
    let mut block = LogBlockState::default();

    for line in content.lines() {
        let track_number = log_line_track_number(line);
        let has_filename_field = line_has_filename_field(line);

        if track_number.is_some() || has_filename_field {
            block = LogBlockState {
                track_number,
                ..LogBlockState::default()
            };
        }

        let line_mentions_audio = line_mentions_audio_file(line, audio_path);
        let line_mentions_some_audio_file = line_mentions_known_audio_file(line);

        if line_mentions_audio {
            block.has_target_audio_reference = true;
        } else if has_filename_field || line_mentions_some_audio_file {
            block.has_other_audio_reference = true;
        }

        if line_has_positive_preemphasis_statement(line)
            && log_block_applies_to_audio(
                &block,
                line_mentions_audio,
                audio_track_number,
                association,
            )
        {
            return true;
        }
    }

    false
}

fn log_block_applies_to_audio(
    block: &LogBlockState,
    line_mentions_audio: bool,
    audio_track_number: Option<u32>,
    association: LogAssociation,
) -> bool {
    if line_mentions_audio || block.has_target_audio_reference {
        return true;
    }

    if association != LogAssociation::DedicatedSidecar {
        return false;
    }

    if block.has_other_audio_reference {
        return false;
    }

    match block.track_number {
        Some(track_number) => audio_track_number == Some(track_number),
        None => true,
    }
}

fn is_affirmative_preemphasis_value(value: &str) -> bool {
    crate::convert::pipeline::is_affirmative_preemphasis_value(value)
}

fn comment_has_positive_preemphasis(comment: &str) -> bool {
    text_has_positive_preemphasis_statement(comment) || is_bare_preemphasis_text(comment)
}

fn text_has_positive_preemphasis_statement(text: &str) -> bool {
    text.lines().any(line_has_positive_preemphasis_statement)
}

fn line_has_positive_preemphasis_statement(line: &str) -> bool {
    let lowercase = line.to_ascii_lowercase();
    let tokens = alphanumeric_tokens(&lowercase);

    let mut index = 0;
    while index < tokens.len() {
        if let Some(end) = preemphasis_span_end(&tokens, index) {
            if !has_negative_preemphasis_context(&tokens, index, end)
                && has_positive_preemphasis_context(&tokens, index, end)
            {
                return true;
            }
            index = end;
        } else {
            index += 1;
        }
    }

    false
}

fn preemphasis_span_end(tokens: &[&str], index: usize) -> Option<usize> {
    if tokens.get(index) == Some(&"preemphasis") {
        return Some(index + 1);
    }

    if tokens.get(index) == Some(&"pre") && tokens.get(index + 1) == Some(&"emphasis") {
        return Some(index + 2);
    }

    None
}

fn has_negative_preemphasis_context(tokens: &[&str], start: usize, end: usize) -> bool {
    let before_start = start.saturating_sub(3);
    let before = &tokens[before_start..start];
    if before
        .iter()
        .any(|token| matches!(*token, "no" | "not" | "non" | "without"))
    {
        return true;
    }

    let after_end = usize::min(end + 4, tokens.len());
    let after = &tokens[end..after_end];
    if after
        .iter()
        .any(|token| matches!(*token, "no" | "none" | "absent" | "without"))
    {
        return true;
    }

    after.windows(2).any(|pair| {
        pair[0] == "not" && matches!(pair[1], "detected" | "found" | "present" | "enabled")
    })
}

fn has_positive_preemphasis_context(tokens: &[&str], start: usize, end: usize) -> bool {
    let before_start = start.saturating_sub(2);
    let before = &tokens[before_start..start];
    if before
        .iter()
        .any(|token| matches!(*token, "with" | "has" | "contains"))
    {
        return true;
    }

    let after_end = usize::min(end + 4, tokens.len());
    let after = &tokens[end..after_end];
    after.iter().any(|token| {
        matches!(
            *token,
            "1" | "yes" | "true" | "on" | "present" | "detected" | "found" | "enabled" | "set"
        )
    })
}

fn is_bare_preemphasis_text(text: &str) -> bool {
    let lowercase = text.trim().to_ascii_lowercase();
    let tokens = alphanumeric_tokens(&lowercase);

    matches!(tokens.as_slice(), ["preemphasis"] | ["pre", "emphasis"])
}

fn line_mentions_audio_file(line: &str, audio_path: &Path) -> bool {
    let lowercase_line = line.to_ascii_lowercase().replace('\\', "/");

    if let Some(file_name) = audio_path.file_name().and_then(|name| name.to_str()) {
        if lowercase_line.contains(&file_name.to_ascii_lowercase()) {
            return true;
        }
    }

    if let Some(stem) = audio_path.file_stem().and_then(|stem| stem.to_str()) {
        let stem = stem.to_ascii_lowercase();
        if is_discriminating_stem(&stem) && lowercase_line.contains(&stem) {
            return true;
        }
    }

    false
}

fn line_has_filename_field(line: &str) -> bool {
    let lowercase = line.to_ascii_lowercase();
    let tokens = alphanumeric_tokens(&lowercase);

    tokens.iter().any(|token| *token == "filename")
        || tokens
            .windows(2)
            .any(|pair| pair[0] == "file" && pair[1] == "name")
}

fn line_mentions_known_audio_file(line: &str) -> bool {
    let lowercase = line.to_ascii_lowercase();
    const AUDIO_EXTENSIONS: &[&str] = &[
        ".aif", ".aiff", ".alac", ".ape", ".flac", ".m4a", ".mp3", ".ogg", ".opus", ".wav",
        ".wave", ".wv",
    ];

    AUDIO_EXTENSIONS
        .iter()
        .any(|extension| lowercase.contains(extension))
}

fn log_line_track_number(line: &str) -> Option<u32> {
    let lowercase = line.to_ascii_lowercase();
    let tokens = alphanumeric_tokens(&lowercase);

    for (index, token) in tokens.iter().enumerate() {
        if let Some(rest) = token.strip_prefix("track") {
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                return parse_track_number(rest);
            }
        }

        if *token == "track" {
            if let Some(next) = tokens.get(index + 1) {
                if next.chars().all(|c| c.is_ascii_digit()) {
                    return parse_track_number(next);
                }
            }
        }
    }

    None
}

pub(crate) fn leading_track_number(audio_path: &Path) -> Option<u32> {
    let stem = audio_path.file_stem()?.to_str()?.trim_start();
    let digit_count = stem.chars().take_while(|c| c.is_ascii_digit()).count();

    if digit_count == 0 || digit_count > 3 {
        return None;
    }

    let rest = &stem[digit_count..];
    if rest
        .chars()
        .next()
        .map_or(false, |c| c.is_ascii_alphanumeric())
    {
        return None;
    }

    parse_track_number(&stem[..digit_count])
}

fn parse_track_number(text: &str) -> Option<u32> {
    let value = text.parse::<u32>().ok()?;
    if value == 0 {
        None
    } else {
        Some(value)
    }
}

fn alphanumeric_tokens(text: &str) -> Vec<&str> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect()
}

fn paths_have_same_parent(left: &Path, right: &Path) -> bool {
    match (left.parent(), right.parent()) {
        (Some(left_parent), Some(right_parent)) => {
            normalized_path_text(left_parent) == normalized_path_text(right_parent)
        }
        _ => false,
    }
}

fn paths_have_same_audio_stem(left: &Path, right: &Path) -> bool {
    match (
        left.file_stem().and_then(|stem| stem.to_str()),
        right.file_stem().and_then(|stem| stem.to_str()),
    ) {
        (Some(left_stem), Some(right_stem)) => left_stem.eq_ignore_ascii_case(right_stem),
        _ => false,
    }
}

fn is_discriminating_stem(stem: &str) -> bool {
    stem.len() >= 4 && !stem.chars().all(|c| c.is_ascii_digit())
}

fn normalized_path_text(path: &Path) -> String {
    let path_text = path.to_string_lossy().replace('\\', "/");
    let mut parts = Vec::new();

    for part in path_text.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }

        if part == ".." {
            if !parts.is_empty() {
                parts.pop();
            } else {
                parts.push(part.to_string());
            }
            continue;
        }

        parts.push(part.to_ascii_lowercase());
    }

    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let mut path = env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("metadata_rs_{}_{}", name, nanos));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn cue_line_matches_flags_pre() {
        assert!(cue_line_has_pre_flag("    FLAGS PRE"));
        assert!(cue_line_has_pre_flag("FLAGS DCP PRE"));
        assert!(cue_line_has_pre_flag("flags pre scms"));
    }

    #[test]
    fn cue_line_rejects_non_flag_lines() {
        assert!(!cue_line_has_pre_flag("TITLE \"Pre-release Flags\""));
        assert!(!cue_line_has_pre_flag("REM FLAGS PRE was not found"));
        assert!(!cue_line_has_pre_flag("PERFORMER \"Pre Flags Quartet\""));
        assert!(!cue_line_has_pre_flag("FLAGS PREGAP"));
    }

    #[test]
    fn cue_preemphasis_must_belong_to_target_audio_file() {
        let dir = temp_dir("cue_association");
        let audio = dir.join("01 - Zimbabwe.flac");
        let cue = dir.join("album.cue");
        write_file(&audio, "");
        write_file(
            &cue,
            "FILE \"02 - Gondwana.wav\" WAVE\n  TRACK 02 AUDIO\n    FLAGS PRE\n",
        );

        assert!(!check_cue_file_for_audio_preemphasis(&cue, &audio));
    }

    #[test]
    fn cue_preemphasis_accepts_same_stem_converted_audio() {
        let dir = temp_dir("cue_same_stem");
        let audio = dir.join("01 - Zimbabwe.flac");
        let cue = dir.join("album.cue");
        write_file(&audio, "");
        write_file(
            &cue,
            "FILE \"01 - Zimbabwe.wav\" WAVE\n  TRACK 01 AUDIO\n    FLAGS PRE\n",
        );

        assert!(check_cue_file_for_audio_preemphasis(&cue, &audio));
    }

    #[test]
    fn obsolete_absolute_windows_cue_reference_retries_local_basename_only_after_missing() {
        let dir = temp_dir("cue_absolute_legacy_basename");
        let audio = dir.join("01 - Track.flac");
        let cue = dir.join("album.cue");
        write_file(&audio, "");
        write_file(
            &cue,
            "FILE \"C:\\old\\rips\\01 - Track.wav\" WAVE\n  TRACK 01 AUDIO\n    FLAGS PRE\n    INDEX 01 00:00:00\n",
        );

        assert!(check_cue_file_for_audio_preemphasis(&cue, &audio));
        assert!(convert_cue_flag_evidence_for_path(&audio));
    }

    #[test]
    fn obsolete_absolute_windows_cue_reference_remains_ambiguous_with_two_local_same_stem_carriers() {
        let dir = temp_dir("cue_absolute_legacy_ambiguous");
        let flac = dir.join("01 - Track.flac");
        let wavpack = dir.join("01 - Track.wv");
        let cue = dir.join("album.cue");
        write_file(&flac, "");
        write_file(&wavpack, "");
        write_file(
            &cue,
            "FILE \"C:\\old\\rips\\01 - Track.wav\" WAVE\n  TRACK 01 AUDIO\n    FLAGS PRE\n    INDEX 01 00:00:00\n",
        );

        assert!(!check_cue_file_for_audio_preemphasis(&cue, &flac));
        assert!(!check_cue_file_for_audio_preemphasis(&cue, &wavpack));
        assert!(!convert_cue_flag_evidence_for_path(&flac));
        assert!(!convert_cue_flag_evidence_for_path(&wavpack));
    }

    #[test]
    fn canonical_cue_parser_rebinds_flags_pre_track_after_index00_file_transition() {
        let dir = temp_dir("cue_eac_rebind");
        let track4 = dir.join("04 - A Man I'll Never Be.flac");
        let track5 = dir.join("05 - Feelin' Satisfied.flac");
        let cue = dir.join("album.cue");
        write_file(&track4, "");
        write_file(&track5, "");
        write_file(
            &cue,
            concat!(
                "FILE \"04 - A Man I'll Never Be.wav\" WAVE\n",
                "  TRACK 04 AUDIO\n",
                "    FLAGS PRE\n",
                "    INDEX 01 00:00:00\n",
                "  TRACK 05 AUDIO\n",
                "    FLAGS PRE\n",
                "    INDEX 00 06:44:00\n",
                "FILE \"05 - Feelin' Satisfied.wav\" WAVE\n",
                "    INDEX 01 00:00:00\n",
            ),
        );

        let parsed = crate::tui::cue_parser::parse_cue_file(&cue).unwrap();
        assert_eq!(parsed.tracks[1].file.as_deref(), Some("05 - Feelin' Satisfied.wav"));
        assert!(check_cue_file_for_audio_preemphasis(&cue, &track5));
        assert!(convert_cue_flag_evidence_for_path(&track5));
    }

    #[test]
    fn cue_preemphasis_same_stem_fallback_fails_closed_when_ambiguous() {
        let dir = temp_dir("cue_same_stem_ambiguous");
        let flac = dir.join("01.flac");
        let wavpack = dir.join("01.wv");
        let cue = dir.join("album.cue");
        write_file(&flac, "");
        write_file(&wavpack, "");
        write_file(
            &cue,
            "FILE \"01.wav\" WAVE\n  TRACK 01 AUDIO\n    FLAGS PRE\n    INDEX 01 00:00:00\n",
        );

        assert!(!check_cue_file_for_audio_preemphasis(&cue, &flac));
    }

    #[test]
    fn hidden_cue_is_not_convert_preemphasis_evidence() {
        let dir = temp_dir("cue_hidden");
        let audio = dir.join("01.flac");
        let cue = dir.join(".scratch.cue");
        write_file(&audio, "");
        write_file(
            &cue,
            "FILE \"01.flac\" WAVE\n  TRACK 01 AUDIO\n    FLAGS PRE\n    INDEX 01 00:00:00\n",
        );

        assert!(!convert_cue_flag_evidence_for_path(&audio));
    }

    #[test]
    fn convert_cue_log_bridge_maps_flags_pre_by_track_number_and_filename() {
        let dir = temp_dir("cue_log_bridge");
        let track1 = dir.join("01 - Speak To Me.flac");
        let track2 = dir.join("02 - On The Run.flac");
        let cue = dir.join("dark-side.cue");
        let log = dir.join("rip.log");
        write_file(&track1, "");
        write_file(&track2, "");
        write_file(
            &cue,
            concat!(
                "PERFORMER \"Pink Floyd\"\n",
                "TITLE \"The Dark Side Of The Moon\"\n",
                "FILE \"Range.wav\" WAVE\n",
                "  TRACK 01 AUDIO\n",
                "    INDEX 01 00:00:00\n",
                "  TRACK 02 AUDIO\n",
                "    FLAGS PRE\n",
                "    INDEX 01 03:57:35\n",
            ),
        );
        write_file(
            &log,
            concat!(
                "Exact Audio Copy V1.6\n\n",
                "  pink   floyd   /   THE DARK SIDE OF THE MOON  \n\n",
                "Track  1\n",
                "     Filename C:\\old\\01 - Speak To Me.wav\n\n",
                "Track  2\n",
                "     Filename C:\\old\\02 - On The Run.wav\n",
            ),
        );

        let evidence = convert_cue_flag_evidence_for_paths(&[track1.clone(), track2.clone()]);
        assert!(!evidence.contains(&track1));
        assert!(evidence.contains(&track2));
    }

    #[test]
    fn convert_cue_log_bridge_decodes_utf16_bom_identity_and_filename() {
        let dir = temp_dir("cue_log_utf16_bom");
        let audio = dir.join("01 - Track.flac");
        let cue = dir.join("album.cue");
        let log = dir.join("rip.log");
        write_file(&audio, "");
        write_file(
            &cue,
            concat!(
                "PERFORMER \"Example Artist\"\n",
                "TITLE \"Example Album\"\n",
                "FILE \"Range.wav\" WAVE\n",
                "  TRACK 01 AUDIO\n",
                "    FLAGS PRE\n",
                "    INDEX 01 00:00:00\n",
            ),
        );
        let text = concat!(
            "Example Artist / Example Album\n\n",
            "Track  1\n",
            "     Filename C:\\old\\01 - Track.wav\n",
        );
        let mut encoded = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            encoded.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(&log, encoded).unwrap();

        assert!(convert_cue_flag_evidence_for_path(&audio));
    }

    #[test]
    fn convert_cue_log_bridge_decodes_windows_1252_identity_and_filename() {
        let dir = temp_dir("cue_log_windows_1252");
        let audio = dir.join("01 - Jóga.flac");
        let cue = dir.join("debut.cue");
        let log = dir.join("rip.log");
        write_file(&audio, "");
        write_file(
            &cue,
            concat!(
                "PERFORMER \"Björk\"\n",
                "TITLE \"Début\"\n",
                "FILE \"Range.wav\" WAVE\n",
                "  TRACK 01 AUDIO\n",
                "    FLAGS PRE\n",
                "    INDEX 01 00:00:00\n",
            ),
        );
        let text = concat!(
            "Björk / Début\n\n",
            "Track  1\n",
            "     Filename C:\\old\\01 - Jóga.wav\n",
        );
        let (encoded, _, had_errors) = WINDOWS_1252.encode(text);
        assert!(!had_errors);
        fs::write(&log, encoded.as_ref()).unwrap();

        assert!(convert_cue_flag_evidence_for_path(&audio));
    }

    #[test]
    fn convert_cue_log_bridge_decodes_windows_1251_identity_and_filename() {
        let dir = temp_dir("cue_log_windows_1251");
        let audio = dir.join("01 - Группа крови.flac");
        let cue = dir.join("album.cue");
        let log = dir.join("rip.log");
        write_file(&audio, "");
        write_file(
            &cue,
            concat!(
                "PERFORMER \"Кино\"\n",
                "TITLE \"Группа крови\"\n",
                "FILE \"Range.wav\" WAVE\n",
                "  TRACK 01 AUDIO\n",
                "    FLAGS PRE\n",
                "    INDEX 01 00:00:00\n",
            ),
        );
        let text = concat!(
            "Кино / Группа крови\n\n",
            "Track  1\n",
            "     Filename C:\\old\\01 - Группа крови.wav\n",
        );
        let (encoded, _, had_errors) = WINDOWS_1251.encode(text);
        assert!(!had_errors);
        fs::write(&log, encoded.as_ref()).unwrap();

        assert!(convert_cue_flag_evidence_for_path(&audio));
    }

    #[test]
    fn eac_album_identity_preserves_unspaced_slash_inside_artist_name() {
        assert_eq!(
            eac_album_identity_line("AC/DC / Back in Black"),
            Some(("AC/DC".to_string(), "Back in Black".to_string()))
        );

        let dir = temp_dir("cue_log_artist_slash");
        let audio = dir.join("01 - Hells Bells.flac");
        let cue = dir.join("disc.cue");
        let log = dir.join("rip.log");
        write_file(&audio, "");
        write_file(
            &cue,
            concat!(
                "PERFORMER \"AC/DC\"\n",
                "TITLE \"Back in Black\"\n",
                "FILE \"Range.wav\" WAVE\n",
                "  TRACK 01 AUDIO\n",
                "    FLAGS PRE\n",
                "    INDEX 01 00:00:00\n",
            ),
        );
        write_file(
            &log,
            concat!(
                "AC/DC / Back in Black\n\n",
                "Track  1\n",
                "     Filename C:\\old\\01 - Hells Bells.wav\n",
            ),
        );

        assert!(convert_cue_flag_evidence_for_path(&audio));
    }

    #[test]
    fn convert_cue_log_bridge_rejects_matching_album_when_track_filename_does_not_match_audio() {
        let dir = temp_dir("cue_log_wrong_audio");
        let audio = dir.join("01 - Intended.flac");
        let cue = dir.join("album.cue");
        let log = dir.join("rip.log");
        write_file(&audio, "");
        write_file(
            &cue,
            concat!(
                "PERFORMER \"Artist\"\n",
                "TITLE \"Album\"\n",
                "FILE \"Range.wav\" WAVE\n",
                "  TRACK 01 AUDIO\n",
                "    FLAGS PRE\n",
                "    INDEX 01 00:00:00\n",
            ),
        );
        write_file(
            &log,
            concat!(
                "Artist / Album\n\n",
                "Track  1\n",
                "     Filename C:\\old\\02 - Different.wav\n",
            ),
        );

        assert!(!convert_cue_flag_evidence_for_path(&audio));
    }

    #[test]
    fn convert_cue_log_bridge_rejects_mismatched_album_identity() {
        let dir = temp_dir("cue_log_identity_mismatch");
        let audio = dir.join("01 - Speak To Me.flac");
        let cue = dir.join("dark-side.cue");
        let log = dir.join("rip.log");
        write_file(&audio, "");
        write_file(
            &cue,
            concat!(
                "PERFORMER \"Pink Floyd\"\n",
                "TITLE \"The Dark Side Of The Moon\"\n",
                "FILE \"Range.wav\" WAVE\n",
                "  TRACK 01 AUDIO\n",
                "    FLAGS PRE\n",
                "    INDEX 01 00:00:00\n",
            ),
        );
        write_file(
            &log,
            concat!(
                "Pink Floyd / Wish You Were Here\n\n",
                "Track  1\n",
                "     Filename C:\\old\\01 - Speak To Me.wav\n",
            ),
        );

        assert!(!convert_cue_flag_evidence_for_path(&audio));
    }

    #[test]
    fn convert_cue_log_bridge_rejects_ambiguous_equal_pairings() {
        let dir = temp_dir("cue_log_pair_ambiguous");
        let audio = dir.join("01 - Speak To Me.flac");
        let cue = dir.join("album.cue");
        write_file(&audio, "");
        write_file(
            &cue,
            concat!(
                "PERFORMER \"Pink Floyd\"\n",
                "TITLE \"The Dark Side Of The Moon\"\n",
                "FILE \"Range.wav\" WAVE\n",
                "  TRACK 01 AUDIO\n",
                "    FLAGS PRE\n",
                "    INDEX 01 00:00:00\n",
            ),
        );
        let log_body = concat!(
            "Pink Floyd / The Dark Side Of The Moon\n\n",
            "Track  1\n",
            "     Filename C:\\old\\01 - Speak To Me.wav\n",
        );
        write_file(&dir.join("rip-a.log"), log_body);
        write_file(&dir.join("rip-b.log"), log_body);

        assert!(!convert_cue_flag_evidence_for_path(&audio));
    }

    #[test]
    fn log_without_cue_never_becomes_convert_cue_evidence() {
        let dir = temp_dir("log_without_cue");
        let audio = dir.join("01 - Speak To Me.flac");
        write_file(&audio, "");
        write_file(
            &dir.join("rip.log"),
            concat!(
                "Pink Floyd / The Dark Side Of The Moon\n\n",
                "Track  1\n",
                "     Filename C:\\old\\01 - Speak To Me.wav\n",
                "     Pre-emphasis: Yes\n",
            ),
        );

        assert!(!convert_cue_flag_evidence_for_path(&audio));
    }

    #[test]
    fn parent_cue_matches_relative_subdirectory_reference() {
        let root = temp_dir("parent_cue_match");
        let disc = root.join("Disc 1");
        let audio = disc.join("01 - Zimbabwe.flac");
        let cue = root.join("album.cue");
        write_file(&audio, "");
        write_file(
            &cue,
            "FILE \"Disc 1/01 - Zimbabwe.wav\" WAVE\n  TRACK 01 AUDIO\n    FLAGS PRE\n",
        );

        assert!(check_cue_file_for_audio_preemphasis(&cue, &audio));
        assert!(convert_cue_flag_evidence_for_path(&audio));
    }

    #[test]
    fn convert_cue_log_bridge_pairs_multiple_flat_disc_sidecars_by_natural_stem() {
        let dir = temp_dir("cue_log_multi_disc");
        let disc1_audio = dir.join("01 - Disc One.flac");
        let disc2_audio = dir.join("01 - Disc Two.flac");
        write_file(&disc1_audio, "");
        write_file(&disc2_audio, "");

        for (stem, range, filename) in [
            ("CD1", "Range-CD1.wav", "01 - Disc One.wav"),
            ("CD2", "Range-CD2.wav", "01 - Disc Two.wav"),
        ] {
            write_file(
                &dir.join(format!("{stem}.cue")),
                &format!(
                    "PERFORMER \"Artist\"\nTITLE \"Album\"\nFILE \"{range}\" WAVE\n  TRACK 01 AUDIO\n    FLAGS PRE\n    INDEX 01 00:00:00\n"
                ),
            );
            write_file(
                &dir.join(format!("{stem}.log")),
                &format!(
                    "Artist / Album\n\nTrack  1\n     Filename C:\\old\\{filename}\n"
                ),
            );
        }

        let evidence = convert_cue_flag_evidence_for_paths(&[
            disc1_audio.clone(),
            disc2_audio.clone(),
        ]);
        assert!(evidence.contains(&disc1_audio));
        assert!(evidence.contains(&disc2_audio));
    }

    #[test]
    fn parent_cue_rejects_sibling_disc_with_same_track_filename() {
        let root = temp_dir("parent_cue_reject_sibling");
        let disc_one = root.join("Disc 1");
        let audio = disc_one.join("01.flac");
        let cue = root.join("album.cue");
        write_file(&audio, "");
        write_file(
            &cue,
            "FILE \"Disc 2/01.wav\" WAVE\n  TRACK 01 AUDIO\n    FLAGS PRE\n",
        );

        assert!(!check_cue_file_for_audio_preemphasis(&cue, &audio));
    }

    #[test]
    fn file_evidence_ignores_unrelated_parent_cue() {
        let root = temp_dir("file_evidence_parent_cue");
        let disc_one = root.join("Disc 1");
        let audio = disc_one.join("01.flac");
        let cue = root.join("unrelated.cue");
        write_file(&audio, "");
        write_file(
            &cue,
            "FILE \"Disc 2/01.wav\" WAVE\n  TRACK 01 AUDIO\n    FLAGS PRE\n",
        );

        assert_eq!(check_file_evidence(&audio), None);
    }

    #[test]
    fn file_evidence_prefers_associated_cue_over_associated_log() {
        let dir = temp_dir("file_evidence_priority");
        let audio = dir.join("01 - Zimbabwe.flac");
        let cue = dir.join("album.cue");
        let log = dir.join("01 - Zimbabwe.log");
        write_file(&audio, "");
        write_file(
            &cue,
            "FILE \"01 - Zimbabwe.wav\" WAVE\n  TRACK 01 AUDIO\n    FLAGS PRE\n",
        );
        write_file(&log, "Pre-emphasis: Yes\n");

        assert_eq!(
            check_file_evidence(&audio),
            Some(PreemphasisEvidence::CueFile)
        );
    }

    #[test]
    fn logs_reject_negative_preemphasis_mentions() {
        assert!(!text_has_positive_preemphasis_statement(
            "No pre-emphasis detected"
        ));
        assert!(!text_has_positive_preemphasis_statement("Pre-emphasis: No"));
        assert!(!text_has_positive_preemphasis_statement(
            "without pre emphasis"
        ));
        assert!(!text_has_positive_preemphasis_statement(
            "preemphasis not present"
        ));
    }

    #[test]
    fn logs_accept_positive_preemphasis_mentions() {
        assert!(text_has_positive_preemphasis_statement("Pre-emphasis: Yes"));
        assert!(text_has_positive_preemphasis_statement("with pre emphasis"));
        assert!(text_has_positive_preemphasis_statement("has pre-emphasis"));
        assert!(text_has_positive_preemphasis_statement(
            "contains pre-emphasis"
        ));
        assert!(text_has_positive_preemphasis_statement(
            "Preemphasis detected"
        ));
        assert!(text_has_positive_preemphasis_statement(
            "pre-emphasis enabled"
        ));
    }

    #[test]
    fn logs_accept_mixed_track_yes_and_no() {
        assert!(text_has_positive_preemphasis_statement(
            "Track 01\nPre-emphasis: Yes\nTrack 02\nPre-emphasis: No"
        ));
    }

    #[test]
    fn logs_reject_bare_or_ambiguous_mentions() {
        assert!(!text_has_positive_preemphasis_statement(
            "Check for pre-emphasis"
        ));
        assert!(!text_has_positive_preemphasis_statement(
            "Pre-emphasis unknown"
        ));
        assert!(!text_has_positive_preemphasis_statement("Pre-emphasis"));
    }

    #[test]
    fn shared_log_rejects_track_number_only_association() {
        let audio = Path::new("/music/Disc 1/01 - Zimbabwe.flac");
        let log = "Track 01\nPre-emphasis: Yes\nTrack 02\nPre-emphasis: No\n";

        assert!(!log_content_has_positive_preemphasis_for_audio(
            log,
            audio,
            LogAssociation::SharedLog
        ));
    }

    #[test]
    fn dedicated_sidecar_log_accepts_matching_track_number_association() {
        let audio = Path::new("/music/Disc 1/01 - Zimbabwe.flac");
        let log = "Track 01\nPre-emphasis: Yes\nTrack 02\nPre-emphasis: No\n";

        assert!(log_content_has_positive_preemphasis_for_audio(
            log,
            audio,
            LogAssociation::DedicatedSidecar
        ));
    }

    #[test]
    fn log_rejects_positive_statement_for_different_track() {
        let audio = Path::new("/music/Disc 1/01 - Zimbabwe.flac");
        let log = "Track 02\nPre-emphasis: Yes\n";

        assert!(!log_content_has_positive_preemphasis_for_audio(
            log,
            audio,
            LogAssociation::SharedLog
        ));
    }

    #[test]
    fn log_associates_by_filename_or_stem_in_track_block() {
        let audio = Path::new("/music/Disc 1/01 - Zimbabwe.flac");
        let log = "Track 99\nFilename 01 - Zimbabwe.wav\nPre-emphasis: Yes\n";

        assert!(log_content_has_positive_preemphasis_for_audio(
            log,
            audio,
            LogAssociation::SharedLog
        ));
    }

    #[test]
    fn log_rejects_filename_for_other_audio_in_same_log() {
        let audio = Path::new("/music/Disc 1/01 - Zimbabwe.flac");
        let log = "Track 01\nFilename 02 - Gondwana.wav\nPre-emphasis: Yes\n";

        assert!(!log_content_has_positive_preemphasis_for_audio(
            log,
            audio,
            LogAssociation::SharedLog
        ));
    }

    #[test]
    fn shared_log_accepts_filename_match_in_track_block() {
        let audio = Path::new("/music/Disc 1/01 - Zimbabwe.flac");
        let log = "Track 01\nFilename 01 - Zimbabwe.wav\nPre-emphasis: Yes\n";

        assert!(log_content_has_positive_preemphasis_for_audio(
            log,
            audio,
            LogAssociation::SharedLog
        ));
    }

    #[test]
    fn shared_log_rejects_parent_disc_track_number_only_false_positive() {
        let root = temp_dir("parent_log_track_only_reject");
        let disc_one = root.join("Disc 1");
        let audio = disc_one.join("01 - Zimbabwe.flac");
        let log = root.join("rip.log");
        write_file(&audio, "");
        write_file(&log, "Disc 2\nTrack 01\nPre-emphasis: Yes\n");

        assert_eq!(check_file_evidence(&audio), None);
    }

    #[test]
    fn parent_log_accepts_target_filename_in_positive_block() {
        let root = temp_dir("parent_log_filename_accept");
        let disc_one = root.join("Disc 1");
        let audio = disc_one.join("01 - Zimbabwe.flac");
        let log = root.join("rip.log");
        write_file(&audio, "");
        write_file(
            &log,
            "Disc 1\nTrack 01\nFilename Disc 1/01 - Zimbabwe.wav\nPre-emphasis: Yes\n",
        );

        assert_eq!(
            check_file_evidence(&audio),
            Some(PreemphasisEvidence::LogFile)
        );
    }

    #[test]
    fn dedicated_sidecar_log_rejects_positive_block_for_other_named_file() {
        let audio = Path::new("/music/Disc 1/01 - Zimbabwe.flac");
        let log = "Track 02\nFilename 02 - Gondwana.wav\nPre-emphasis: Yes\n";

        assert!(!log_content_has_positive_preemphasis_for_audio(
            log,
            audio,
            LogAssociation::DedicatedSidecar
        ));
    }

    #[test]
    fn same_directory_sidecar_log_can_match_by_stem() {
        let dir = temp_dir("log_sidecar_stem");
        let audio = dir.join("01 - Zimbabwe.flac");
        let log = dir.join("01 - Zimbabwe.log");
        write_file(&audio, "");
        write_file(&log, "Pre-emphasis: Yes\n");

        assert!(check_log_file_for_audio_preemphasis(&log, &audio));
    }

    #[test]
    fn comments_accept_bare_preemphasis_tag_values() {
        assert!(comment_has_positive_preemphasis("pre-emphasis"));
        assert!(comment_has_positive_preemphasis("Pre Emphasis"));
        assert!(comment_has_positive_preemphasis("preemphasis"));
    }

    #[test]
    fn affirmative_tag_values_are_case_and_whitespace_insensitive() {
        assert!(is_affirmative_preemphasis_value(" yes "));
        assert!(is_affirmative_preemphasis_value("TRUE"));
        assert!(is_affirmative_preemphasis_value("1"));
        assert!(!is_affirmative_preemphasis_value("no"));
        assert!(!is_affirmative_preemphasis_value("false"));
    }
}
