//! Metadata-based pre-emphasis evidence detection.
//!
//! Checks audio file tags, associated CUE files, and associated EAC/XLD log
//! files for positive pre-emphasis indicators. This tier is authoritative:
//! if metadata says pre-emphasis is present, we trust it.

use std::fs;
use std::path::{Path, PathBuf};

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
    use lofty::file::TaggedFileExt;
    use lofty::tag::ItemKey;

    let tagged = lofty::read_from_path(audio_path).ok()?;
    for tag in tagged.tags() {
        // Check for explicit PRE_EMPHASIS or PRE-EMPHASIS tags.
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

        // Check COMMENT tag for a positive pre-emphasis statement.
        if let Some(comment) = tag.get_string(&ItemKey::Comment) {
            if comment_has_positive_preemphasis(comment) {
                return Some(PreemphasisEvidence::CommentTag);
            }
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

    paths.sort_by(|a, b| {
        normalized_path_text(a)
            .cmp(&normalized_path_text(b))
    });
    paths
}

fn path_has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map_or(false, |ext| ext.eq_ignore_ascii_case(extension))
}

/// Parse a CUE file looking for FLAGS PRE on a FILE entry associated with the
/// target audio file.
fn check_cue_file_for_audio_preemphasis(cue_path: &Path, audio_path: &Path) -> bool {
    let content = match fs::read_to_string(cue_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let mut current_file_matches_audio = false;

    for line in content.lines() {
        if let Some(file_ref) = parse_cue_file_reference(line) {
            current_file_matches_audio = cue_file_reference_matches_audio(cue_path, &file_ref, audio_path);
            continue;
        }

        if current_file_matches_audio && cue_line_has_pre_flag(line) {
            return true;
        }
    }

    false
}

/// Return true only for real CUE FLAGS lines that include PRE as a flag token.
fn cue_line_has_pre_flag(line: &str) -> bool {
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

fn parse_cue_file_reference(line: &str) -> Option<String> {
    let line = line.trim();
    let mut parts = line.splitn(2, |c: char| c.is_whitespace());
    let directive = parts.next()?;

    if !directive.eq_ignore_ascii_case("FILE") {
        return None;
    }

    let rest = parts.next()?.trim_start();
    if rest.is_empty() {
        return None;
    }

    if let Some(after_quote) = rest.strip_prefix('"') {
        let end_quote = after_quote.find('"')?;
        return Some(after_quote[..end_quote].to_string());
    }

    rest.split_whitespace().next().map(str::to_string)
}

fn cue_file_reference_matches_audio(cue_path: &Path, file_ref: &str, audio_path: &Path) -> bool {
    let cue_dir = cue_path.parent().unwrap_or_else(|| Path::new("."));
    let ref_path = path_from_sidecar_reference(file_ref);
    let ref_is_absolute_like = sidecar_reference_is_absolute_like(file_ref);

    let candidate_path = if ref_is_absolute_like {
        ref_path.clone()
    } else {
        cue_dir.join(&ref_path)
    };

    if paths_match_same_audio_file(&candidate_path, audio_path) {
        return true;
    }

    // Some CUE sheets contain old absolute Windows/macOS paths. If the CUE file
    // sits beside the target audio, a stem match is a reasonable association.
    if ref_is_absolute_like && paths_have_same_parent(cue_path, audio_path) {
        return paths_have_same_audio_stem(&ref_path, audio_path);
    }

    false
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
    paths_have_same_parent(sidecar_path, audio_path) && paths_have_same_audio_stem(sidecar_path, audio_path)
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
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "yes" | "true" | "on" | "y"
    )
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
        || tokens.windows(2).any(|pair| pair[0] == "file" && pair[1] == "name")
}

fn line_mentions_known_audio_file(line: &str) -> bool {
    let lowercase = line.to_ascii_lowercase();
    const AUDIO_EXTENSIONS: &[&str] = &[
        ".aif", ".aiff", ".alac", ".ape", ".flac", ".m4a", ".mp3", ".ogg", ".opus",
        ".wav", ".wave", ".wv",
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

fn leading_track_number(audio_path: &Path) -> Option<u32> {
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

fn path_from_sidecar_reference(file_ref: &str) -> PathBuf {
    PathBuf::from(file_ref.replace('\\', "/"))
}

fn sidecar_reference_is_absolute_like(file_ref: &str) -> bool {
    let file_ref = file_ref.trim();
    file_ref.starts_with('/')
        || file_ref.starts_with('\\')
        || file_ref.as_bytes().get(1) == Some(&b':')
}

fn paths_match_same_audio_file(left: &Path, right: &Path) -> bool {
    if normalized_path_text(left) == normalized_path_text(right) {
        return true;
    }

    paths_have_same_parent(left, right)
        && paths_have_same_audio_stem(left, right)
        && path_has_known_audio_extension(left)
        && path_has_known_audio_extension(right)
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

fn path_has_known_audio_extension(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => matches!(
            ext.to_ascii_lowercase().as_str(),
            "aif" | "aiff" | "alac" | "ape" | "flac" | "m4a" | "mp3" | "ogg" | "opus" | "wav" | "wave" | "wv"
        ),
        None => false,
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
    fn cue_parser_extracts_quoted_and_unquoted_file_references() {
        assert_eq!(
            parse_cue_file_reference("FILE \"01 - Zimbabwe.wav\" WAVE"),
            Some("01 - Zimbabwe.wav".to_string())
        );
        assert_eq!(
            parse_cue_file_reference("FILE 01.wav WAVE"),
            Some("01.wav".to_string())
        );
        assert_eq!(parse_cue_file_reference("TITLE \"Zimbabwe\""), None);
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

        assert_eq!(check_file_evidence(&audio), Some(PreemphasisEvidence::CueFile));
    }

    #[test]
    fn logs_reject_negative_preemphasis_mentions() {
        assert!(!text_has_positive_preemphasis_statement(
            "No pre-emphasis detected"
        ));
        assert!(!text_has_positive_preemphasis_statement(
            "Pre-emphasis: No"
        ));
        assert!(!text_has_positive_preemphasis_statement(
            "without pre emphasis"
        ));
        assert!(!text_has_positive_preemphasis_statement(
            "preemphasis not present"
        ));
    }

    #[test]
    fn logs_accept_positive_preemphasis_mentions() {
        assert!(text_has_positive_preemphasis_statement(
            "Pre-emphasis: Yes"
        ));
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

        assert!(!log_content_has_positive_preemphasis_for_audio(log, audio, LogAssociation::SharedLog));
    }

    #[test]
    fn log_associates_by_filename_or_stem_in_track_block() {
        let audio = Path::new("/music/Disc 1/01 - Zimbabwe.flac");
        let log = "Track 99\nFilename 01 - Zimbabwe.wav\nPre-emphasis: Yes\n";

        assert!(log_content_has_positive_preemphasis_for_audio(log, audio, LogAssociation::SharedLog));
    }

    #[test]
    fn log_rejects_filename_for_other_audio_in_same_log() {
        let audio = Path::new("/music/Disc 1/01 - Zimbabwe.flac");
        let log = "Track 01\nFilename 02 - Gondwana.wav\nPre-emphasis: Yes\n";

        assert!(!log_content_has_positive_preemphasis_for_audio(log, audio, LogAssociation::SharedLog));
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

        assert_eq!(check_file_evidence(&audio), Some(PreemphasisEvidence::LogFile));
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
