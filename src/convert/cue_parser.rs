//! CUE sheet parser for metadata import, queue expansion, and bulk rename.
//!
//! Extracts album/track metadata (title, performer, date, genre) and
//! file references from a CUE sheet, with bounded non-UTF8 encoding
//! detection. Handles both single-image (one FILE + many TRACKs) and
//! track-by-track (one FILE per TRACK) layouts. Conversion-domain: no
//! TUI dependencies. Single-image *detection* (which probes audio and
//! resolves file references) lives in `crate::tui::cue_parser`.

use std::path::{Path, PathBuf};

use encoding_rs::{BIG5, EUC_JP, GBK, SHIFT_JIS, WINDOWS_1252};

/// A parsed CUE sheet.
#[derive(Debug, Clone, Default)]
pub struct CueSheet {
    /// Album-level title (from a TITLE before the first TRACK).
    pub title: Option<String>,
    /// Album-level performer.
    pub performer: Option<String>,
    /// Release date (from `REM DATE` or `REM YEAR`).
    pub date: Option<String>,
    /// Genre (from `REM GENRE`).
    pub genre: Option<String>,
    /// 13-digit UPC/EAN from a `CATALOG` line.
    pub catalog: Option<String>,
    /// Tracks in order.
    pub tracks: Vec<CueTrack>,
}

/// A single track from a CUE sheet.
#[derive(Debug, Clone, Default)]
pub struct CueTrack {
    /// Track number (1-based).
    pub number: u32,
    /// Track title from TITLE inside the TRACK block.
    pub title: Option<String>,
    /// Track performer (falls back to album performer if absent).
    pub performer: Option<String>,
    /// FILE reference associated with this track. For single-image CUEs,
    /// all tracks share the same file; for track-by-track, each has its own.
    pub file: Option<String>,
    /// INDEX 01 offset in CUE frames (75 frames per second).
    /// None if the CUE didn't have an INDEX 01 for this track.
    pub index01_frames: Option<u32>,
    /// INDEX 00 (pregap) offset, in CUE frames. For multi-file noncompliant
    /// CUEs this is the position inside the **previous** FILE block; for
    /// single-image it's an absolute cumulative position.
    pub index00_frames: Option<u32>,
    /// CD ISRC code from an `ISRC` line inside the TRACK block.
    pub isrc: Option<String>,
}

/// Parse a CUE sheet from a file path.
pub fn parse_cue_file(path: &Path) -> Result<CueSheet, String> {
    let raw = std::fs::read(path).map_err(|e| format!("failed to read CUE file: {}", e))?;
    let content = decode_cue_bytes_for_path(&raw, path)
        .map_err(|e| format!("failed to decode CUE file {}: {}", path.display(), e))?;
    Ok(parse_cue(&content))
}

/// Decode raw CUE bytes without using replacement characters.
///
/// CUE files from older ripping tools are often not UTF-8. We first accept
/// UTF-8 and Unicode files that declare a BOM. Legacy CUEs are decoded by
/// trying common, non-lossy encodings used by real-world rips: CP932/Shift-JIS,
/// EUC-JP, GBK, Big5, and Windows-1252. Candidates are scored by CUE parse
/// quality and, when a CUE path is available, by whether decoded FILE references
/// resolve on disk using the same separator-normalization and extension-fallback
/// semantics used by the materializer. Unlike `String::from_utf8_lossy`, this never substitutes
/// U+FFFD and therefore never silently changes filenames.
pub fn decode_cue_bytes(raw: &[u8]) -> Result<String, String> {
    decode_cue_bytes_with_context(raw, None)
}

/// Decode raw CUE bytes, using the CUE path as an optional signal when choosing
/// among legacy encodings. If two byte-for-byte valid encodings both parse as
/// CUE text, prefer the one whose decoded FILE references resolve beside the
/// CUE file. Path scoring mirrors the materializer resolver, including backslash
/// normalization and subdirectory extension-mismatch fallback. This avoids
/// Western-only fallback corrupting CP932/Shift-JIS names.
pub fn decode_cue_bytes_for_path(raw: &[u8], cue_path: &Path) -> Result<String, String> {
    decode_cue_bytes_with_context(raw, cue_path.parent())
}

fn decode_cue_bytes_with_context(
    raw: &[u8],
    cue_parent: Option<&Path>,
) -> Result<String, String> {
    decode_cue_bytes_with_context_for_write(raw, cue_parent).map(|decoded| decoded.text)
}

#[derive(Debug, Clone, Copy)]
enum CueWriteEncoding {
    Utf8,
    Utf8Bom,
    Utf16LeBom,
    Utf16BeBom,
    Legacy {
        name: &'static str,
        encoding: &'static encoding_rs::Encoding,
    },
}

impl CueWriteEncoding {
    fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf8Bom => "UTF-8 BOM",
            Self::Utf16LeBom => "UTF-16LE BOM",
            Self::Utf16BeBom => "UTF-16BE BOM",
            Self::Legacy { name, .. } => name,
        }
    }
}

#[derive(Debug, Clone)]
struct DecodedCueForWrite {
    text: String,
    encoding: CueWriteEncoding,
}

fn decode_cue_bytes_with_context_for_write(
    raw: &[u8],
    cue_parent: Option<&Path>,
) -> Result<DecodedCueForWrite, String> {
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let text = std::str::from_utf8(&raw[3..])
            .map(|text| text.to_string())
            .map_err(|e| format!("invalid UTF-8 after BOM: {}", e))?;
        return Ok(DecodedCueForWrite {
            text,
            encoding: CueWriteEncoding::Utf8Bom,
        });
    }

    if let Some((encoding, bom_len)) = encoding_rs::Encoding::for_bom(raw) {
        let Some(text) = encoding.decode_without_bom_handling_and_without_replacement(&raw[bom_len..]) else {
            return Err(format!(
                "{} CUE contains invalid byte sequences",
                encoding.name()
            ));
        };
        let write_encoding = match encoding.name() {
            "UTF-16LE" => CueWriteEncoding::Utf16LeBom,
            "UTF-16BE" => CueWriteEncoding::Utf16BeBom,
            _ => CueWriteEncoding::Utf8,
        };
        return Ok(DecodedCueForWrite {
            text: text.into_owned(),
            encoding: write_encoding,
        });
    }

    if let Ok(text) = std::str::from_utf8(raw) {
        return Ok(DecodedCueForWrite {
            text: text.to_string(),
            encoding: CueWriteEncoding::Utf8,
        });
    }

    let legacy_candidates = [
        LegacyCueEncoding { name: "CP932/Shift-JIS", encoding: SHIFT_JIS, priority: 1 },
        LegacyCueEncoding { name: "EUC-JP", encoding: EUC_JP, priority: 2 },
        LegacyCueEncoding { name: "GBK", encoding: GBK, priority: 3 },
        LegacyCueEncoding { name: "Big5", encoding: BIG5, priority: 4 },
        // Prefer Windows-1252 on pure syntax ties. Path-aware scoring below
        // still lets CP932/Shift-JIS, EUC-JP, GBK, or Big5 win when their
        // decoded FILE references actually exist.
        LegacyCueEncoding { name: "Windows-1252", encoding: WINDOWS_1252, priority: 0 },
    ];

    let mut decoded_candidates = Vec::new();
    for candidate in legacy_candidates {
        let Some(decoded) = candidate
            .encoding
            .decode_without_bom_handling_and_without_replacement(raw)
        else {
            continue;
        };
        let text = decoded.into_owned();
        let score = cue_decode_score(&text, cue_parent);
        decoded_candidates.push(DecodedCueCandidate {
            name: candidate.name,
            text,
            score,
            priority: candidate.priority,
            encoding: candidate.encoding,
        });
    }

    decoded_candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.priority.cmp(&b.priority))
            .then_with(|| a.name.cmp(b.name))
    });

    decoded_candidates
        .into_iter()
        .next()
        .map(|candidate| DecodedCueForWrite {
            text: candidate.text,
            encoding: CueWriteEncoding::Legacy {
                name: candidate.name,
                encoding: candidate.encoding,
            },
        })
        .ok_or_else(|| {
            "CUE is not valid UTF-8, UTF-16, CP932/Shift-JIS, EUC-JP, GBK, Big5, or Windows-1252"
                .to_string()
        })
}

#[derive(Debug, Clone, Copy)]
struct LegacyCueEncoding {
    name: &'static str,
    encoding: &'static encoding_rs::Encoding,
    priority: usize,
}

#[derive(Debug)]
struct DecodedCueCandidate {
    name: &'static str,
    text: String,
    score: i64,
    priority: usize,
    encoding: &'static encoding_rs::Encoding,
}

fn cue_decode_score(text: &str, cue_parent: Option<&Path>) -> i64 {
    let sheet = parse_cue(text);
    let mut score = 0i64;

    score += (sheet.tracks.len() as i64) * 1_000;
    score += sheet
        .tracks
        .iter()
        .filter(|track| track.file.is_some())
        .count() as i64
        * 300;
    score += sheet
        .tracks
        .iter()
        .filter(|track| track.index01_frames.is_some())
        .count() as i64
        * 300;

    if let Some(parent) = cue_parent {
        for file in sheet.tracks.iter().filter_map(|track| track.file.as_deref()) {
            // Keep decoder scoring aligned with the materializer resolver:
            // normalize CUE backslashes, search inside referenced subdirectories,
            // allow deterministic extension mismatch fallback, and penalize
            // ambiguous same-stem results. This prevents a legacy decoder from
            // losing only because its decoded path needs the same fallback the
            // materializer will later use.
            score += match cue_decode_path_resolution(parent, file) {
                CueDecodePathResolution::Exact => 5_000,
                CueDecodePathResolution::UniqueNameFallback => 4_500,
                CueDecodePathResolution::UniqueStemFallback => 4_000,
                CueDecodePathResolution::Ambiguous => -2_500,
                CueDecodePathResolution::SearchDirectoryExists => 50,
                CueDecodePathResolution::Missing => 0,
            };
        }
    }

    for line in text.lines() {
        let upper = line.trim_start().to_ascii_uppercase();
        if upper.starts_with("FILE ") {
            score += 200;
        } else if upper.starts_with("TRACK ") {
            score += 200;
        } else if upper.starts_with("INDEX 01 ") {
            score += 200;
        }
    }

    for ch in text.chars() {
        if ch == '\u{FFFD}' {
            score -= 100_000;
        } else if ch.is_control() && !matches!(ch, '\n' | '\r' | '\t') {
            score -= 500;
        } else if cue_parent.is_some() && is_cjk_or_kana(ch) {
            // CJK/kana is a weak signal by itself because some unrelated byte
            // sequences are valid in multiple East Asian encodings. Use it only
            // when path context exists; actual FILE resolution above remains
            // the strong signal.
            score += 10;
        }
    }

    score
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CueDecodePathResolution {
    Exact,
    UniqueNameFallback,
    UniqueStemFallback,
    Ambiguous,
    SearchDirectoryExists,
    Missing,
}

fn cue_decode_path_resolution(cue_parent: &Path, file_ref: &str) -> CueDecodePathResolution {
    let normalized_ref = file_ref.replace('\\', &std::path::MAIN_SEPARATOR.to_string());
    let raw_path = PathBuf::from(&normalized_ref);

    let direct = if raw_path.is_absolute() {
        raw_path.clone()
    } else {
        cue_parent.join(&raw_path)
    };
    if direct.is_file() {
        return CueDecodePathResolution::Exact;
    }

    let fallback_dir = cue_decode_fallback_search_dir(cue_parent, &raw_path);
    let wanted_name = raw_path.file_name().and_then(|value| value.to_str());
    let wanted_stem = raw_path.file_stem().and_then(|value| value.to_str());

    if let Some(wanted) = wanted_name {
        match cue_decode_collect_audio_candidates(&fallback_dir, |path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case(wanted))
                .unwrap_or(false)
        }) {
            CandidateSet::Unique => return CueDecodePathResolution::UniqueNameFallback,
            CandidateSet::Ambiguous => return CueDecodePathResolution::Ambiguous,
            CandidateSet::None => {}
        }
    }

    if let Some(wanted) = wanted_stem {
        match cue_decode_collect_audio_candidates(&fallback_dir, |path| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case(wanted))
                .unwrap_or(false)
        }) {
            CandidateSet::Unique => return CueDecodePathResolution::UniqueStemFallback,
            CandidateSet::Ambiguous => return CueDecodePathResolution::Ambiguous,
            CandidateSet::None => {}
        }
    }

    if fallback_dir.is_dir() {
        CueDecodePathResolution::SearchDirectoryExists
    } else {
        CueDecodePathResolution::Missing
    }
}

fn cue_decode_fallback_search_dir(base: &Path, raw_path: &Path) -> PathBuf {
    raw_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| base.join(parent))
        .unwrap_or_else(|| base.to_path_buf())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateSet {
    None,
    Unique,
    Ambiguous,
}

fn cue_decode_collect_audio_candidates(
    dir: &Path,
    matches_reference: impl Fn(&Path) -> bool,
) -> CandidateSet {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return CandidateSet::None;
    };

    let mut count = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && cue_decode_has_audio_extension(&path) && matches_reference(&path) {
            count += 1;
            if count > 1 {
                return CandidateSet::Ambiguous;
            }
        }
    }

    if count == 1 {
        CandidateSet::Unique
    } else {
        CandidateSet::None
    }
}

fn cue_decode_has_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "flac"
                    | "wav"
                    | "wave"
                    | "aiff"
                    | "aif"
                    | "ape"
                    | "wv"
                    | "tta"
                    | "mp3"
                    | "m4a"
                    | "aac"
                    | "ogg"
                    | "opus"
                    | "dsf"
                    | "dff"
            )
        })
        .unwrap_or(false)
}

fn is_cjk_or_kana(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3040..=0x30FF  // Hiragana + Katakana
            | 0x3400..=0x4DBF  // CJK Extension A
            | 0x4E00..=0x9FFF  // CJK Unified Ideographs
            | 0xAC00..=0xD7AF  // Hangul syllables
    )
}


/// Result of a targeted sidecar CUE metadata write-back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CueSidecarWritebackOutcome {
    /// The decoded sidecar already contained the requested metadata.
    Unchanged,
    /// The sidecar was rewritten in its original encoding.
    Rewritten { encoding: String },
    /// The source encoding could not represent the corrected text, so the
    /// sidecar was atomically rewritten as UTF-8 without a BOM.
    RewrittenUtf8Fallback { source_encoding: String },
}

/// Rewrite only editable metadata fields in a sidecar CUE file using the
/// metadata carried by the replacement CUESHEET text.
///
/// Encoding policy is deliberate and test-covered: preserve the source CUE's
/// encoding (UTF-8, UTF-8 BOM, UTF-16 BOM, CP932/Shift-JIS, EUC-JP, GBK, Big5,
/// or Windows-1252) when every corrected character is representable. For UTF-8
/// without BOM and ASCII-compatible legacy encodings, preservation is byte-span
/// targeted: the decoded text identifies scopes and commands, but the writer
/// replaces only the value bytes for editable quoted/REM metadata and copies
/// untouched raw lines unchanged. UTF-8 BOM and UTF-16 BOM files are re-encoded
/// from line-preserved text so the BOM remains byte 0 even when album metadata
/// must be inserted before the first logical CUE line.
/// CATALOG and ISRC are intentionally structure/identifier fields for this writer
/// and are never rewritten. If a legacy source encoding cannot represent the
/// corrected metadata, write UTF-8 without a BOM rather than lossy replacement.
/// Values that cannot be represented losslessly in CUE syntax, such as embedded
/// double quotes in quoted fields, are rejected so the caller can leave the
/// sidecar stale instead of silently changing metadata. The function never truncates in
/// place: it writes a same-directory temporary file, syncs it, then renames it
/// over the original. A read-only sidecar is reported as an error before any
/// write attempt, so the caller can leave the audio-file save successful while
/// surfacing that the sidecar stayed stale.
pub fn rewrite_cue_sidecar_metadata_from_cuesheet(
    cue_path: &Path,
    replacement_cuesheet: &str,
) -> Result<CueSidecarWritebackOutcome, String> {
    let raw = std::fs::read(cue_path)
        .map_err(|e| format!("failed to read sidecar CUE '{}': {}", cue_path.display(), e))?;
    let decoded = decode_cue_bytes_with_context_for_write(&raw, cue_path.parent())?;
    validate_replacement_cuesheet_quoted_metadata(replacement_cuesheet)?;
    let desired = explicit_cue_metadata(replacement_cuesheet);
    if desired.tracks.is_empty() {
        return Err("replacement CUESHEET has no audio tracks".to_string());
    }

    let rewrite = rewrite_cue_metadata_preserving_bytes(&raw, &decoded, &desired)?;
    let (bytes, encoding_outcome) = match rewrite {
        CueByteRewrite::Unchanged => return Ok(CueSidecarWritebackOutcome::Unchanged),
        CueByteRewrite::Rewritten { bytes, outcome } => (bytes, outcome),
        CueByteRewrite::NeedsUtf8Fallback { rewritten_text, source_encoding } => (
            rewritten_text.into_bytes(),
            CueSidecarWritebackOutcome::RewrittenUtf8Fallback { source_encoding },
        ),
    };

    let metadata = std::fs::metadata(cue_path)
        .map_err(|e| format!("failed to stat sidecar CUE '{}': {}", cue_path.display(), e))?;
    if metadata.permissions().readonly() {
        return Err(format!(
            "sidecar CUE '{}' is read-only; image tags were saved but the CUE was left stale",
            cue_path.display()
        ));
    }

    atomic_replace(cue_path, &bytes)?;
    Ok(encoding_outcome)
}

#[derive(Debug, Clone, Default)]
struct ExplicitCueMetadata {
    album: ExplicitCueScopeMetadata,
    tracks: Vec<ExplicitCueScopeMetadata>,
}

#[derive(Debug, Clone, Default)]
struct ExplicitCueScopeMetadata {
    title: Option<String>,
    performer: Option<String>,
    songwriter: Option<String>,
    date: Option<String>,
    genre: Option<String>,
}

fn validate_replacement_cuesheet_quoted_metadata(text: &str) -> Result<(), String> {
    for (line_idx, line) in text.lines().enumerate() {
        let line = if line_idx == 0 {
            line.trim_start_matches('\u{FEFF}')
        } else {
            line
        };
        let trimmed = line.trim_start();
        for keyword in ["TITLE", "PERFORMER", "SONGWRITER"] {
            if strip_keyword_ci(trimmed, keyword).is_some() {
                validate_replacement_quoted_line(trimmed, keyword, line_idx + 1)?;
            }
        }

        let Some(rem_rest) = strip_keyword_ci(trimmed, "REM") else { continue; };
        let rem_field = rem_rest.trim_start();
        for field in ["DATE", "YEAR", "GENRE"] {
            let Some(field_rest) = strip_keyword_ci(rem_field, field) else { continue; };
            if field_rest.trim_start().starts_with('"') {
                validate_replacement_quoted_tail(field_rest.trim_start(), &format!("REM {}", field), line_idx + 1)?;
            }
        }
    }
    Ok(())
}

fn validate_replacement_quoted_line(line: &str, keyword: &str, line_number: usize) -> Result<(), String> {
    let rest = strip_keyword_ci(line, keyword)
        .ok_or_else(|| format!("internal error: expected {} replacement CUESHEET line", keyword))?
        .trim_start();
    if rest.starts_with('"') {
        validate_replacement_quoted_tail(rest, keyword, line_number)?;
    }
    Ok(())
}

fn validate_replacement_quoted_tail(rest: &str, field: &str, line_number: usize) -> Result<(), String> {
    let after_open = rest.strip_prefix('"').ok_or_else(|| {
        format!(
            "internal error: expected replacement CUESHEET line {} {} value to be quoted",
            line_number,
            field
        )
    })?;
    let Some(closing_rel) = after_open.find('"') else {
        return Err(format!(
            "sidecar CUE was left stale because replacement CUESHEET line {} has {} with malformed double quotes, which CUE quoted strings cannot represent losslessly",
            line_number,
            field
        ));
    };
    let trailing = &after_open[closing_rel + 1..];
    if !trailing.trim().is_empty() {
        return Err(format!(
            "sidecar CUE was left stale because replacement CUESHEET line {} has {} with embedded or malformed double quotes, which CUE quoted strings cannot represent losslessly",
            line_number,
            field
        ));
    }
    Ok(())
}

fn explicit_cue_metadata(text: &str) -> ExplicitCueMetadata {
    let mut metadata = ExplicitCueMetadata::default();
    let mut current_track: Option<usize> = None;
    let mut ignored_track_block = false;

    for (i, line) in text.lines().enumerate() {
        let line = if i == 0 {
            line.trim_start_matches('\u{FEFF}')
        } else {
            line
        };
        let trimmed = line.trim();

        if let Some((_num, is_audio)) = parse_track_header(trimmed) {
            if is_audio {
                metadata.tracks.push(ExplicitCueScopeMetadata::default());
                current_track = Some(metadata.tracks.len() - 1);
                ignored_track_block = false;
            } else {
                current_track = None;
                ignored_track_block = true;
            }
            continue;
        }

        if ignored_track_block {
            continue;
        }

        if let Some(track_idx) = current_track {
            if let Some(value) = parse_quoted_field(trimmed, "TITLE") {
                metadata.tracks[track_idx].title = Some(value);
            } else if let Some(value) = parse_quoted_field(trimmed, "PERFORMER") {
                metadata.tracks[track_idx].performer = Some(value);
            } else if let Some(value) = parse_quoted_field(trimmed, "SONGWRITER") {
                metadata.tracks[track_idx].songwriter = Some(value);
            }
            continue;
        }

        if let Some(value) = parse_quoted_field(trimmed, "TITLE") {
            metadata.album.title = Some(value);
        } else if let Some(value) = parse_quoted_field(trimmed, "PERFORMER") {
            metadata.album.performer = Some(value);
        } else if let Some(value) = parse_quoted_field(trimmed, "SONGWRITER") {
            metadata.album.songwriter = Some(value);
        } else if let Some(value) =
            parse_rem_field(trimmed, "DATE").or_else(|| parse_rem_field(trimmed, "YEAR"))
        {
            metadata.album.date = Some(value);
        } else if let Some(value) = parse_rem_field(trimmed, "GENRE") {
            metadata.album.genre = Some(value);
        }
    }

    metadata
}

#[derive(Debug, Clone)]
struct CueTextLine {
    body: String,
    eol: String,
}

#[derive(Debug, Clone, Default)]
struct CueMetadataLayout {
    album: CueScopeLayout,
    tracks: Vec<CueScopeLayout>,
    album_insert_at: usize,
}

#[derive(Debug, Clone, Default)]
struct CueScopeLayout {
    title: Option<usize>,
    performer: Option<usize>,
    songwriter: Option<usize>,
    date: Option<usize>,
    genre: Option<usize>,
    insert_after: Option<usize>,
    indent: Option<String>,
}

fn rewrite_cue_metadata_text(
    original_text: &str,
    desired: &ExplicitCueMetadata,
) -> Result<String, String> {
    let lines = split_cue_lines_preserving_eol(original_text);
    let layout = cue_metadata_layout(&lines);
    if layout.tracks.len() != desired.tracks.len() {
        return Err(format!(
            "sidecar CUE has {} audio tracks but replacement CUESHEET has {}; sidecar left unchanged",
            layout.tracks.len(),
            desired.tracks.len()
        ));
    }

    let mut replacements = std::collections::BTreeMap::<usize, String>::new();
    let mut album_insertions = Vec::<String>::new();
    let mut track_insertions = std::collections::BTreeMap::<usize, Vec<String>>::new();

    queue_scope_metadata_edits(
        &lines,
        &layout.album,
        &desired.album,
        &mut replacements,
        &mut album_insertions,
        true,
    )?;

    for (track_layout, desired_track) in layout.tracks.iter().zip(desired.tracks.iter()) {
        let mut insertions = Vec::new();
        queue_scope_metadata_edits(
            &lines,
            track_layout,
            desired_track,
            &mut replacements,
            &mut insertions,
            false,
        )?;
        if !insertions.is_empty() {
            let insert_after = track_layout.insert_after.ok_or_else(|| {
                "internal error: CUE track metadata layout missing insertion point".to_string()
            })?;
            track_insertions.insert(insert_after, insertions);
        }
    }

    let default_eol = dominant_eol(&lines);
    let mut output = String::with_capacity(original_text.len());
    for (idx, line) in lines.iter().enumerate() {
        if idx == layout.album_insert_at {
            for body in &album_insertions {
                output.push_str(body);
                output.push_str(default_eol);
            }
        }
        output.push_str(
            replacements
                .get(&idx)
                .map(String::as_str)
                .unwrap_or(line.body.as_str()),
        );
        output.push_str(&line.eol);
        if let Some(insertions) = track_insertions.get(&idx) {
            let eol = if line.eol.is_empty() {
                default_eol
            } else {
                line.eol.as_str()
            };
            for body in insertions {
                output.push_str(body);
                output.push_str(eol);
            }
        }
    }

    if lines.is_empty() {
        for body in &album_insertions {
            output.push_str(body);
            output.push_str(default_eol);
        }
    } else if layout.album_insert_at == lines.len() {
        for body in &album_insertions {
            output.push_str(body);
            output.push_str(default_eol);
        }
    }

    Ok(output)
}


enum CueByteRewrite {
    Unchanged,
    Rewritten {
        bytes: Vec<u8>,
        outcome: CueSidecarWritebackOutcome,
    },
    NeedsUtf8Fallback {
        rewritten_text: String,
        source_encoding: String,
    },
}

#[derive(Debug, Clone)]
struct CueRawLine {
    body: Vec<u8>,
    eol: Vec<u8>,
    text_body: String,
}

fn rewrite_cue_metadata_preserving_bytes(
    raw: &[u8],
    decoded: &DecodedCueForWrite,
    desired: &ExplicitCueMetadata,
) -> Result<CueByteRewrite, String> {
    if !encoding_supports_ascii_byte_span_rewrite(decoded.encoding) {
        return rewrite_cue_metadata_by_text_reencode(&decoded.text, desired, decoded.encoding);
    }

    let raw_lines = split_cue_raw_lines_preserving_eol(raw, decoded.encoding)?;
    let text_lines: Vec<CueTextLine> = raw_lines
        .iter()
        .map(|line| CueTextLine {
            body: line.text_body.clone(),
            eol: raw_eol_to_text(&line.eol),
        })
        .collect();
    let layout = cue_metadata_layout(&text_lines);
    if layout.tracks.len() != desired.tracks.len() {
        return Err(format!(
            "sidecar CUE has {} audio tracks but replacement CUESHEET has {}; sidecar left unchanged",
            layout.tracks.len(),
            desired.tracks.len()
        ));
    }

    let mut replacements = std::collections::BTreeMap::<usize, Vec<u8>>::new();
    let mut album_insertions = Vec::<Vec<u8>>::new();
    let mut track_insertions = std::collections::BTreeMap::<usize, Vec<Vec<u8>>>::new();
    let mut need_utf8_fallback = false;

    queue_scope_metadata_byte_edits(
        &raw_lines,
        &layout.album,
        &desired.album,
        decoded.encoding,
        &mut replacements,
        &mut album_insertions,
        true,
        &mut need_utf8_fallback,
    )?;

    for (track_layout, desired_track) in layout.tracks.iter().zip(desired.tracks.iter()) {
        let mut insertions = Vec::new();
        queue_scope_metadata_byte_edits(
            &raw_lines,
            track_layout,
            desired_track,
            decoded.encoding,
            &mut replacements,
            &mut insertions,
            false,
            &mut need_utf8_fallback,
        )?;
        if !insertions.is_empty() {
            let insert_after = track_layout.insert_after.ok_or_else(|| {
                "internal error: CUE track metadata layout missing insertion point".to_string()
            })?;
            track_insertions.insert(insert_after, insertions);
        }
    }

    if need_utf8_fallback {
        let rewritten_text = rewrite_cue_metadata_text(&decoded.text, desired)?;
        if rewritten_text == decoded.text {
            return Ok(CueByteRewrite::Unchanged);
        }
        return Ok(CueByteRewrite::NeedsUtf8Fallback {
            rewritten_text,
            source_encoding: decoded.encoding.name().to_string(),
        });
    }

    let default_eol = dominant_raw_eol(&raw_lines);
    let mut output = Vec::with_capacity(raw.len());
    for (idx, line) in raw_lines.iter().enumerate() {
        if idx == layout.album_insert_at {
            append_raw_insertions(&mut output, &album_insertions, default_eol);
        }

        let body = replacements
            .get(&idx)
            .map(Vec::as_slice)
            .unwrap_or(line.body.as_slice());
        output.extend_from_slice(body);
        output.extend_from_slice(&line.eol);

        if let Some(insertions) = track_insertions.get(&idx) {
            let insertion_eol = if line.eol.is_empty() {
                if !output.ends_with(default_eol) {
                    output.extend_from_slice(default_eol);
                }
                default_eol
            } else {
                line.eol.as_slice()
            };
            append_raw_insertions(&mut output, insertions, insertion_eol);
        }
    }

    if raw_lines.is_empty() {
        append_raw_insertions(&mut output, &album_insertions, default_eol);
    } else if layout.album_insert_at == raw_lines.len() {
        if !output.ends_with(default_eol) {
            output.extend_from_slice(default_eol);
        }
        append_raw_insertions(&mut output, &album_insertions, default_eol);
    }

    if output == raw {
        Ok(CueByteRewrite::Unchanged)
    } else {
        Ok(CueByteRewrite::Rewritten {
            bytes: output,
            outcome: CueSidecarWritebackOutcome::Rewritten {
                encoding: decoded.encoding.name().to_string(),
            },
        })
    }
}

fn rewrite_cue_metadata_by_text_reencode(
    original_text: &str,
    desired: &ExplicitCueMetadata,
    source_encoding: CueWriteEncoding,
) -> Result<CueByteRewrite, String> {
    let rewritten = rewrite_cue_metadata_text(original_text, desired)?;
    if rewritten == original_text {
        return Ok(CueByteRewrite::Unchanged);
    }
    let (bytes, outcome) = encode_cue_text_for_write(&rewritten, source_encoding);
    Ok(CueByteRewrite::Rewritten { bytes, outcome })
}

fn encoding_supports_ascii_byte_span_rewrite(encoding: CueWriteEncoding) -> bool {
    matches!(encoding, CueWriteEncoding::Utf8 | CueWriteEncoding::Legacy { .. })
}

fn split_cue_raw_lines_preserving_eol(
    raw: &[u8],
    encoding: CueWriteEncoding,
) -> Result<Vec<CueRawLine>, String> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut idx = 0usize;
    while idx < raw.len() {
        if raw[idx] == b'\n' {
            let body_end = if idx > start && raw[idx - 1] == b'\r' {
                idx - 1
            } else {
                idx
            };
            let body = raw[start..body_end].to_vec();
            let eol = raw[body_end..=idx].to_vec();
            let text_body = decode_cue_raw_line_body(&body, encoding, lines.is_empty())?;
            lines.push(CueRawLine { body, eol, text_body });
            start = idx + 1;
        }
        idx += 1;
    }
    if start < raw.len() {
        let body = raw[start..].to_vec();
        let text_body = decode_cue_raw_line_body(&body, encoding, lines.is_empty())?;
        lines.push(CueRawLine {
            body,
            eol: Vec::new(),
            text_body,
        });
    }
    Ok(lines)
}

fn decode_cue_raw_line_body(
    body: &[u8],
    encoding: CueWriteEncoding,
    first_line: bool,
) -> Result<String, String> {
    match encoding {
        CueWriteEncoding::Utf8 | CueWriteEncoding::Utf8Bom => {
            let body = if first_line && body.starts_with(&[0xEF, 0xBB, 0xBF]) {
                &body[3..]
            } else {
                body
            };
            std::str::from_utf8(body)
                .map(|text| text.to_string())
                .map_err(|e| format!("invalid UTF-8 in CUE line: {}", e))
        }
        CueWriteEncoding::Legacy { name, encoding } => encoding
            .decode_without_bom_handling_and_without_replacement(body)
            .map(|text| text.into_owned())
            .ok_or_else(|| format!("{} CUE contains an invalid line byte sequence", name)),
        CueWriteEncoding::Utf16LeBom | CueWriteEncoding::Utf16BeBom => Err(
            "internal error: UTF-16 CUE cannot use ASCII byte-span rewrite".to_string(),
        ),
    }
}

fn raw_eol_to_text(eol: &[u8]) -> String {
    match eol {
        b"\r\n" => "\r\n".to_string(),
        b"\n" => "\n".to_string(),
        _ => String::new(),
    }
}

fn dominant_raw_eol(lines: &[CueRawLine]) -> &'static [u8] {
    let crlf = lines.iter().filter(|line| line.eol.as_slice() == b"\r\n").count();
    let lf = lines.iter().filter(|line| line.eol.as_slice() == b"\n").count();
    if crlf > lf { b"\r\n" } else { b"\n" }
}

fn append_raw_insertions(output: &mut Vec<u8>, insertions: &[Vec<u8>], eol: &[u8]) {
    for body in insertions {
        output.extend_from_slice(body);
        output.extend_from_slice(eol);
    }
}

fn queue_scope_metadata_byte_edits(
    lines: &[CueRawLine],
    layout: &CueScopeLayout,
    desired: &ExplicitCueScopeMetadata,
    source_encoding: CueWriteEncoding,
    replacements: &mut std::collections::BTreeMap<usize, Vec<u8>>,
    insertions: &mut Vec<Vec<u8>>,
    album_scope: bool,
    need_utf8_fallback: &mut bool,
) -> Result<(), String> {
    queue_quoted_metadata_byte_edit(
        lines,
        layout.title,
        desired.title.as_deref(),
        "TITLE",
        layout,
        source_encoding,
        replacements,
        insertions,
        need_utf8_fallback,
    )?;
    queue_quoted_metadata_byte_edit(
        lines,
        layout.performer,
        desired.performer.as_deref(),
        "PERFORMER",
        layout,
        source_encoding,
        replacements,
        insertions,
        need_utf8_fallback,
    )?;
    queue_quoted_metadata_byte_edit(
        lines,
        layout.songwriter,
        desired.songwriter.as_deref(),
        "SONGWRITER",
        layout,
        source_encoding,
        replacements,
        insertions,
        need_utf8_fallback,
    )?;

    if album_scope {
        queue_rem_metadata_byte_edit(
            lines,
            layout.date,
            desired.date.as_deref(),
            "DATE",
            layout,
            source_encoding,
            replacements,
            insertions,
            need_utf8_fallback,
        )?;
        queue_rem_metadata_byte_edit(
            lines,
            layout.genre,
            desired.genre.as_deref(),
            "GENRE",
            layout,
            source_encoding,
            replacements,
            insertions,
            need_utf8_fallback,
        )?;
    }

    Ok(())
}

fn queue_quoted_metadata_byte_edit(
    lines: &[CueRawLine],
    existing_idx: Option<usize>,
    desired: Option<&str>,
    keyword: &str,
    layout: &CueScopeLayout,
    source_encoding: CueWriteEncoding,
    replacements: &mut std::collections::BTreeMap<usize, Vec<u8>>,
    insertions: &mut Vec<Vec<u8>>,
    need_utf8_fallback: &mut bool,
) -> Result<(), String> {
    let Some(value) = desired else { return Ok(()); };
    validate_cue_quoted_metadata_value(keyword, value)?;
    if let Some(idx) = existing_idx {
        match replace_quoted_value_bytes(&lines[idx].body, keyword, value, source_encoding)? {
            ByteLineEdit::Unchanged => {}
            ByteLineEdit::Rewritten(bytes) => {
                replacements.insert(idx, bytes);
            }
            ByteLineEdit::NeedUtf8Fallback => *need_utf8_fallback = true,
        }
    } else {
        let line = format_quoted_line(&scope_indent(layout), keyword, value);
        match encode_text_for_source_line(&line, source_encoding) {
            Some(bytes) => insertions.push(bytes),
            None => *need_utf8_fallback = true,
        }
    }
    Ok(())
}

fn queue_rem_metadata_byte_edit(
    lines: &[CueRawLine],
    existing_idx: Option<usize>,
    desired: Option<&str>,
    field: &str,
    layout: &CueScopeLayout,
    source_encoding: CueWriteEncoding,
    replacements: &mut std::collections::BTreeMap<usize, Vec<u8>>,
    insertions: &mut Vec<Vec<u8>>,
    need_utf8_fallback: &mut bool,
) -> Result<(), String> {
    let Some(value) = desired else { return Ok(()); };
    validate_cue_line_metadata_value(field, value)?;
    if value.contains('"') {
        return Err(format!(
            "sidecar CUE was left stale because REM {} contains a double quote, which this writer will not serialize lossily",
            field
        ));
    }
    if let Some(idx) = existing_idx {
        match replace_rem_value_bytes(&lines[idx].body, field, value, source_encoding)? {
            ByteLineEdit::Unchanged => {}
            ByteLineEdit::Rewritten(bytes) => {
                replacements.insert(idx, bytes);
            }
            ByteLineEdit::NeedUtf8Fallback => *need_utf8_fallback = true,
        }
    } else {
        let line = format_rem_line(&scope_indent(layout), field, value);
        match encode_text_for_source_line(&line, source_encoding) {
            Some(bytes) => insertions.push(bytes),
            None => *need_utf8_fallback = true,
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ByteLineEdit {
    Unchanged,
    Rewritten(Vec<u8>),
    NeedUtf8Fallback,
}

fn replace_quoted_value_bytes(
    body: &[u8],
    keyword: &str,
    value: &str,
    source_encoding: CueWriteEncoding,
) -> Result<ByteLineEdit, String> {
    let keyword_end = match_ascii_keyword_at_line_start(body, keyword, source_encoding).ok_or_else(|| {
        format!("failed to byte-locate {} metadata line in sidecar CUE", keyword)
    })?;
    let quote_start = find_byte(body, keyword_end, b'"').ok_or_else(|| {
        format!("failed to byte-locate opening quote for {} in sidecar CUE", keyword)
    })?;
    let quote_end = find_byte(body, quote_start + 1, b'"').ok_or_else(|| {
        format!("failed to byte-locate closing quote for {} in sidecar CUE", keyword)
    })?;
    let escaped = escape_cue_quoted(value);
    let Some(encoded_value) = encode_text_for_source_line(escaped, source_encoding) else {
        return Ok(ByteLineEdit::NeedUtf8Fallback);
    };
    let mut out = Vec::with_capacity(body.len() + encoded_value.len());
    out.extend_from_slice(&body[..quote_start + 1]);
    out.extend_from_slice(&encoded_value);
    out.extend_from_slice(&body[quote_end..]);
    if out == body {
        Ok(ByteLineEdit::Unchanged)
    } else {
        Ok(ByteLineEdit::Rewritten(out))
    }
}

fn replace_rem_value_bytes(
    body: &[u8],
    field: &str,
    value: &str,
    source_encoding: CueWriteEncoding,
) -> Result<ByteLineEdit, String> {
    let rem_end = match_ascii_keyword_at_line_start(body, "REM", source_encoding)
        .ok_or_else(|| "failed to byte-locate REM metadata line in sidecar CUE".to_string())?;
    let field_start = skip_ascii_whitespace(body, rem_end);
    let field_end = match_ascii_keyword_at(body, field_start, field).or_else(|| {
        if field.eq_ignore_ascii_case("DATE") {
            match_ascii_keyword_at(body, field_start, "YEAR")
        } else {
            None
        }
    }).ok_or_else(|| {
        format!("failed to byte-locate REM {} metadata line in sidecar CUE", field)
    })?;
    let value_start = skip_ascii_whitespace(body, field_end);
    if value_start < body.len() && body[value_start] == b'"' {
        let quote_end = find_byte(body, value_start + 1, b'"').ok_or_else(|| {
            format!("failed to byte-locate closing quote for REM {} in sidecar CUE", field)
        })?;
        let escaped = escape_cue_quoted(value);
        let Some(encoded_value) = encode_text_for_source_line(escaped, source_encoding) else {
            return Ok(ByteLineEdit::NeedUtf8Fallback);
        };
        let mut out = Vec::with_capacity(body.len() + encoded_value.len());
        out.extend_from_slice(&body[..value_start + 1]);
        out.extend_from_slice(&encoded_value);
        out.extend_from_slice(&body[quote_end..]);
        return if out == body {
            Ok(ByteLineEdit::Unchanged)
        } else {
            Ok(ByteLineEdit::Rewritten(out))
        };
    }

    let encoded_text = format_rem_value(value);
    let Some(encoded_value) = encode_text_for_source_line(&encoded_text, source_encoding) else {
        return Ok(ByteLineEdit::NeedUtf8Fallback);
    };
    let value_end = trim_ascii_end(body);
    let mut out = Vec::with_capacity(body.len() + encoded_value.len() + 1);
    out.extend_from_slice(&body[..field_end]);
    if value_start == field_end {
        out.push(b' ');
    } else {
        out.extend_from_slice(&body[field_end..value_start]);
    }
    out.extend_from_slice(&encoded_value);
    out.extend_from_slice(&body[value_end..]);
    if out == body {
        Ok(ByteLineEdit::Unchanged)
    } else {
        Ok(ByteLineEdit::Rewritten(out))
    }
}

fn encode_text_for_source_line(text: &str, source_encoding: CueWriteEncoding) -> Option<Vec<u8>> {
    match source_encoding {
        CueWriteEncoding::Utf8 | CueWriteEncoding::Utf8Bom => Some(text.as_bytes().to_vec()),
        CueWriteEncoding::Legacy { encoding, .. } => {
            let (encoded, _actual_encoding, had_errors) = encoding.encode(text);
            if had_errors {
                None
            } else {
                Some(encoded.into_owned())
            }
        }
        CueWriteEncoding::Utf16LeBom | CueWriteEncoding::Utf16BeBom => None,
    }
}

fn match_ascii_keyword_at_line_start(
    body: &[u8],
    keyword: &str,
    source_encoding: CueWriteEncoding,
) -> Option<usize> {
    let start = skip_line_start_syntax_bytes(body, source_encoding);
    match_ascii_keyword_at(body, start, keyword)
}

fn skip_line_start_syntax_bytes(body: &[u8], source_encoding: CueWriteEncoding) -> usize {
    let mut idx = 0usize;
    if matches!(source_encoding, CueWriteEncoding::Utf8Bom) && body.starts_with(&[0xEF, 0xBB, 0xBF]) {
        idx = 3;
    }
    skip_ascii_whitespace(body, idx)
}

fn match_ascii_keyword_at(body: &[u8], start: usize, keyword: &str) -> Option<usize> {
    let keyword_bytes = keyword.as_bytes();
    if body.len() < start + keyword_bytes.len() {
        return None;
    }
    for (offset, wanted) in keyword_bytes.iter().enumerate() {
        if body[start + offset].to_ascii_uppercase() != (*wanted).to_ascii_uppercase() {
            return None;
        }
    }
    let end = start + keyword_bytes.len();
    if end == body.len() || is_ascii_cue_whitespace(body[end]) {
        Some(end)
    } else {
        None
    }
}

fn skip_ascii_whitespace(body: &[u8], mut idx: usize) -> usize {
    while idx < body.len() && is_ascii_cue_whitespace(body[idx]) {
        idx += 1;
    }
    idx
}

fn trim_ascii_end(body: &[u8]) -> usize {
    let mut end = body.len();
    while end > 0 && is_ascii_cue_whitespace(body[end - 1]) {
        end -= 1;
    }
    end
}

fn find_byte(body: &[u8], start: usize, needle: u8) -> Option<usize> {
    body.get(start..)?
        .iter()
        .position(|byte| *byte == needle)
        .map(|offset| start + offset)
}

fn is_ascii_cue_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 0x0B | 0x0C)
}

fn queue_scope_metadata_edits(
    lines: &[CueTextLine],
    layout: &CueScopeLayout,
    desired: &ExplicitCueScopeMetadata,
    replacements: &mut std::collections::BTreeMap<usize, String>,
    insertions: &mut Vec<String>,
    album_scope: bool,
) -> Result<(), String> {
    queue_quoted_metadata_edit(
        lines,
        layout.title,
        desired.title.as_deref(),
        "TITLE",
        layout,
        replacements,
        insertions,
    )?;
    queue_quoted_metadata_edit(
        lines,
        layout.performer,
        desired.performer.as_deref(),
        "PERFORMER",
        layout,
        replacements,
        insertions,
    )?;
    queue_quoted_metadata_edit(
        lines,
        layout.songwriter,
        desired.songwriter.as_deref(),
        "SONGWRITER",
        layout,
        replacements,
        insertions,
    )?;

    if album_scope {
        queue_rem_metadata_edit(
            lines,
            layout.date,
            desired.date.as_deref(),
            "DATE",
            layout,
            replacements,
            insertions,
        )?;
        queue_rem_metadata_edit(
            lines,
            layout.genre,
            desired.genre.as_deref(),
            "GENRE",
            layout,
            replacements,
            insertions,
        )?;
    }

    Ok(())
}

fn queue_quoted_metadata_edit(
    lines: &[CueTextLine],
    existing_idx: Option<usize>,
    desired: Option<&str>,
    keyword: &str,
    layout: &CueScopeLayout,
    replacements: &mut std::collections::BTreeMap<usize, String>,
    insertions: &mut Vec<String>,
) -> Result<(), String> {
    let Some(value) = desired else { return Ok(()); };
    validate_cue_quoted_metadata_value(keyword, value)?;
    if let Some(idx) = existing_idx {
        let rewritten = replace_quoted_value(&lines[idx].body, keyword, value)
            .unwrap_or_else(|| format_quoted_line(&leading_indent(&lines[idx].body), keyword, value));
        if rewritten != lines[idx].body {
            replacements.insert(idx, rewritten);
        }
    } else {
        insertions.push(format_quoted_line(&scope_indent(layout), keyword, value));
    }
    Ok(())
}

fn queue_rem_metadata_edit(
    lines: &[CueTextLine],
    existing_idx: Option<usize>,
    desired: Option<&str>,
    field: &str,
    layout: &CueScopeLayout,
    replacements: &mut std::collections::BTreeMap<usize, String>,
    insertions: &mut Vec<String>,
) -> Result<(), String> {
    let Some(value) = desired else { return Ok(()); };
    validate_cue_line_metadata_value(field, value)?;
    if value.contains('"') {
        return Err(format!(
            "sidecar CUE was left stale because REM {} contains a double quote, which this writer will not serialize lossily",
            field
        ));
    }
    if let Some(idx) = existing_idx {
        let rewritten = replace_rem_value(&lines[idx].body, field, value)
            .unwrap_or_else(|| format_rem_line(&leading_indent(&lines[idx].body), field, value));
        if rewritten != lines[idx].body {
            replacements.insert(idx, rewritten);
        }
    } else {
        insertions.push(format_rem_line(&scope_indent(layout), field, value));
    }
    Ok(())
}

fn cue_metadata_layout(lines: &[CueTextLine]) -> CueMetadataLayout {
    let mut layout = CueMetadataLayout {
        album_insert_at: lines.len(),
        ..Default::default()
    };
    let mut current_track: Option<usize> = None;
    let mut ignored_track_block = false;

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.body.trim();
        if layout.album_insert_at == lines.len()
            && (strip_keyword_ci(trimmed, "FILE").is_some()
                || strip_keyword_ci(trimmed, "TRACK").is_some())
        {
            layout.album_insert_at = idx;
        }

        if let Some((_num, is_audio)) = parse_track_header(trimmed) {
            if is_audio {
                layout.tracks.push(CueScopeLayout {
                    insert_after: Some(idx),
                    indent: Some(child_indent_for_track_line(&line.body)),
                    ..Default::default()
                });
                current_track = Some(layout.tracks.len() - 1);
                ignored_track_block = false;
            } else {
                current_track = None;
                ignored_track_block = true;
            }
            continue;
        }

        if ignored_track_block {
            continue;
        }

        if let Some(track_idx) = current_track {
            if let Some(target) = layout.tracks.get_mut(track_idx) {
                record_scope_line(target, idx, &line.body, false);
            }
        } else {
            record_scope_line(&mut layout.album, idx, &line.body, true);
        }
    }

    layout
}

fn record_scope_line(layout: &mut CueScopeLayout, idx: usize, body: &str, album_scope: bool) {
    let trimmed = body.trim();
    let mut recorded = false;
    if parse_quoted_field(trimmed, "TITLE").is_some() {
        layout.title = Some(idx);
        recorded = true;
    } else if parse_quoted_field(trimmed, "PERFORMER").is_some() {
        layout.performer = Some(idx);
        recorded = true;
    } else if parse_quoted_field(trimmed, "SONGWRITER").is_some() {
        layout.songwriter = Some(idx);
        recorded = true;
    } else if album_scope
        && (parse_rem_field(trimmed, "DATE").is_some() || parse_rem_field(trimmed, "YEAR").is_some())
    {
        layout.date = Some(idx);
        recorded = true;
    } else if album_scope && parse_rem_field(trimmed, "GENRE").is_some() {
        layout.genre = Some(idx);
        recorded = true;
    }

    if recorded && layout.indent.is_none() {
        layout.indent = Some(leading_indent(body));
    }
}

fn split_cue_lines_preserving_eol(text: &str) -> Vec<CueTextLine> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut idx = 0usize;
    while idx < bytes.len() {
        if bytes[idx] == b'\n' {
            let (body_end, eol) = if idx > start && bytes[idx - 1] == b'\r' {
                (idx - 1, "\r\n")
            } else {
                (idx, "\n")
            };
            lines.push(CueTextLine {
                body: text[start..body_end].to_string(),
                eol: eol.to_string(),
            });
            start = idx + 1;
        }
        idx += 1;
    }
    if start < text.len() {
        lines.push(CueTextLine {
            body: text[start..].to_string(),
            eol: String::new(),
        });
    }
    lines
}

fn dominant_eol(lines: &[CueTextLine]) -> &'static str {
    let crlf = lines.iter().filter(|line| line.eol == "\r\n").count();
    let lf = lines.iter().filter(|line| line.eol == "\n").count();
    if crlf > lf { "\r\n" } else { "\n" }
}

fn leading_indent(body: &str) -> String {
    body.chars().take_while(|ch| ch.is_whitespace()).collect()
}

fn child_indent_for_track_line(body: &str) -> String {
    let mut indent = leading_indent(body);
    indent.push_str("  ");
    indent
}

fn scope_indent(layout: &CueScopeLayout) -> String {
    layout.indent.clone().unwrap_or_default()
}

fn replace_quoted_value(body: &str, keyword: &str, value: &str) -> Option<String> {
    let trimmed = body.trim_start();
    strip_keyword_ci(trimmed, keyword)?;
    let body_offset = body.len() - trimmed.len();
    let quote_rel = body[body_offset..].find('"')?;
    let quote_start = body_offset + quote_rel;
    let rest = &body[quote_start + 1..];
    let quote_end = quote_start + 1 + rest.find('"')?;
    let mut out = String::new();
    out.push_str(&body[..quote_start + 1]);
    out.push_str(escape_cue_quoted(value));
    out.push_str(&body[quote_end..]);
    Some(out)
}

fn replace_rem_value(body: &str, field: &str, value: &str) -> Option<String> {
    let trimmed = body.trim_start();
    let rem_rest = strip_keyword_ci(trimmed, "REM")?;
    let field_text = rem_rest.trim_start();
    strip_keyword_ci(field_text, field).or_else(|| {
        if field.eq_ignore_ascii_case("DATE") {
            strip_keyword_ci(field_text, "YEAR")
        } else {
            None
        }
    })?;

    let field_start = body.len() - field_text.len();
    let value_start = field_start + field_token_len(field_text);
    let existing_spacing = &body[value_start..];
    let spacing_len = existing_spacing
        .char_indices()
        .find(|(_, ch)| !ch.is_whitespace())
        .map(|(idx, _)| idx)
        .unwrap_or(existing_spacing.len());
    let spacing = &existing_spacing[..spacing_len];
    let formatted = format_rem_value(value);

    let mut out = String::new();
    out.push_str(&body[..value_start]);
    if spacing.is_empty() {
        out.push(' ');
    } else {
        out.push_str(spacing);
    }
    out.push_str(&formatted);
    Some(out)
}

fn field_token_len(rem_rest: &str) -> usize {
    rem_rest
        .char_indices()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(idx, _)| idx)
        .unwrap_or(rem_rest.len())
}

fn format_quoted_line(indent: &str, keyword: &str, value: &str) -> String {
    format!("{}{} \"{}\"", indent, keyword, escape_cue_quoted(value))
}

fn format_rem_line(indent: &str, field: &str, value: &str) -> String {
    format!("{}REM {} {}", indent, field, format_rem_value(value))
}

fn format_rem_value(value: &str) -> String {
    if value.chars().any(char::is_whitespace) {
        format!("\"{}\"", escape_cue_quoted(value))
    } else {
        value.trim().to_string()
    }
}

fn escape_cue_quoted(value: &str) -> &str {
    value
}

fn validate_cue_quoted_metadata_value(field: &str, value: &str) -> Result<(), String> {
    validate_cue_line_metadata_value(field, value)?;
    if value.contains('"') {
        return Err(format!(
            "sidecar CUE was left stale because {} contains a double quote, which CUE quoted strings cannot represent losslessly",
            field
        ));
    }
    Ok(())
}

fn validate_cue_line_metadata_value(field: &str, value: &str) -> Result<(), String> {
    if value.contains('\r') || value.contains('\n') {
        return Err(format!(
            "sidecar CUE was left stale because {} contains a line break, which cannot be written as one CUE metadata line",
            field
        ));
    }
    Ok(())
}

fn encode_cue_text_for_write(
    text: &str,
    source_encoding: CueWriteEncoding,
) -> (Vec<u8>, CueSidecarWritebackOutcome) {
    match source_encoding {
        CueWriteEncoding::Utf8 => (
            text.as_bytes().to_vec(),
            CueSidecarWritebackOutcome::Rewritten { encoding: source_encoding.name().to_string() },
        ),
        CueWriteEncoding::Utf8Bom => {
            let mut bytes = vec![0xEF, 0xBB, 0xBF];
            bytes.extend_from_slice(text.as_bytes());
            (bytes, CueSidecarWritebackOutcome::Rewritten { encoding: source_encoding.name().to_string() })
        }
        CueWriteEncoding::Utf16LeBom => {
            let mut bytes = vec![0xFF, 0xFE];
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            (bytes, CueSidecarWritebackOutcome::Rewritten { encoding: source_encoding.name().to_string() })
        }
        CueWriteEncoding::Utf16BeBom => {
            let mut bytes = vec![0xFE, 0xFF];
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&unit.to_be_bytes());
            }
            (bytes, CueSidecarWritebackOutcome::Rewritten { encoding: source_encoding.name().to_string() })
        }
        CueWriteEncoding::Legacy { name, encoding } => {
            let (encoded, _actual_encoding, had_errors) = encoding.encode(text);
            if had_errors {
                (
                    text.as_bytes().to_vec(),
                    CueSidecarWritebackOutcome::RewrittenUtf8Fallback {
                        source_encoding: name.to_string(),
                    },
                )
            } else {
                (
                    encoded.into_owned(),
                    CueSidecarWritebackOutcome::Rewritten { encoding: name.to_string() },
                )
            }
        }
    }
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;

    let parent = path.parent().ok_or_else(|| {
        format!("sidecar CUE '{}' has no parent directory", path.display())
    })?;
    let mut temp_path = parent.join(format!(
        ".cue-sidecar-writeback.{}.{}.tmp",
        std::process::id(),
        monotonic_temp_nonce(),
    ));

    for attempt in 0..128u32 {
        if attempt > 0 {
            temp_path = parent.join(format!(
                ".cue-sidecar-writeback.{}.{}.{}.tmp",
                std::process::id(),
                monotonic_temp_nonce(),
                attempt,
            ));
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path);
        let mut file = match file {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(format!(
                    "failed to create temporary sidecar CUE '{}': {}",
                    temp_path.display(),
                    err
                ));
            }
        };
        if let Ok(metadata) = std::fs::metadata(path) {
            if let Err(err) = file.set_permissions(metadata.permissions()) {
                let _ = std::fs::remove_file(&temp_path);
                return Err(format!(
                    "failed to preserve permissions on temporary sidecar CUE '{}': {}",
                    temp_path.display(),
                    err
                ));
            }
        }

        let write_result = (|| -> Result<(), String> {
            file.write_all(bytes).map_err(|e| {
                format!("failed to write temporary sidecar CUE '{}': {}", temp_path.display(), e)
            })?;
            file.sync_all().map_err(|e| {
                format!("failed to sync temporary sidecar CUE '{}': {}", temp_path.display(), e)
            })?;
            drop(file);
            std::fs::rename(&temp_path, path).map_err(|e| {
                format!(
                    "failed to replace sidecar CUE '{}' atomically from '{}': {}",
                    path.display(),
                    temp_path.display(),
                    e
                )
            })?;
            sync_parent_dir(parent);
            Ok(())
        })();

        if write_result.is_err() {
            let _ = std::fs::remove_file(&temp_path);
        }
        return write_result;
    }

    Err(format!(
        "failed to allocate a unique temporary sidecar CUE path beside '{}'",
        path.display()
    ))
}

fn sync_parent_dir(parent: &Path) {
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
}

fn monotonic_temp_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

/// Locate a sidecar `.cue` file in the same directory as `audio_path`.
/// Returns the lexicographically first match (case-insensitive
/// extension check), or None when:
/// - `audio_path` has no parent directory
/// - the parent can't be read (permissions, etc.)
/// - no `.cue` file exists in the parent
///
/// Used by the metadata editor to surface a sidecar's per-track
/// structure as a synthetic embedded-CUESHEET entry — letting `:tags-mb`
/// and per-track save flows operate uniformly whether the truth lives
/// in the file's tag or alongside on disk.
pub fn find_sidecar_cue(audio_path: &Path) -> Option<PathBuf> {
    let parent = audio_path.parent()?;
    let entries = std::fs::read_dir(parent).ok()?;
    let mut cues: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .map(|ext| ext.to_ascii_lowercase() == "cue")
                .unwrap_or(false)
        })
        .collect();
    cues.sort();
    cues.into_iter().next()
}

/// Parse CUE sheet content from a string.
pub fn parse_cue(content: &str) -> CueSheet {
    let mut sheet = CueSheet::default();

    // State: are we inside a TRACK block?
    let mut current_track: Option<CueTrack> = None;
    // State: are we inside a non-AUDIO TRACK block that must be ignored?
    let mut ignored_track_block = false;
    // The most recent FILE line (used for both single-image and per-track).
    let mut current_file: Option<String> = None;

    for (i, line) in content.lines().enumerate() {
        // Strip a UTF-8 BOM (U+FEFF) if it appears on the first line —
        // some Windows editors prefix it. Rust's `trim()` doesn't
        // classify BOM as whitespace (post-Unicode-2018), so without
        // this `strip_prefix("TITLE")` etc. would silently fail on
        // the album-level header line.
        let line = if i == 0 {
            line.trim_start_matches('\u{FEFF}')
        } else {
            line
        };
        let trimmed = line.trim();

        // FILE "filename.ext" WAVE
        if let Some(file) = parse_file_line(trimmed) {
            current_file = Some(file);
            ignored_track_block = false;
            // In compliant CUE sheets, FILE appears before TRACK and becomes
            // the current file for the next track. Some older rippers emit a
            // noncompliant multi-file pregap shape instead:
            //
            //   FILE "previous.wav" WAVE
            //     TRACK 02 AUDIO
            //       INDEX 00 03:43:37
            //   FILE "current.wav" WAVE
            //       INDEX 01 00:00:00
            //
            // In that form INDEX 00 describes pregap audio in the previous
            // file, but INDEX 01 and the track body belong to the later FILE.
            // Rebind only while INDEX 01 has not been seen yet; once a track
            // has its start index, a following FILE is for the next TRACK and
            // must not mutate the already-complete track.
            if let Some(ref mut track) = current_track {
                if track.index01_frames.is_none() {
                    track.file = current_file.clone();
                }
            }
            continue;
        }

        // TRACK NN AUDIO
        if let Some((num, is_audio)) = parse_track_header(trimmed) {
            // Commit the previous track (if any).
            if let Some(track) = current_track.take() {
                sheet.tracks.push(track);
            }
            if !is_audio {
                ignored_track_block = true;
                continue;
            }
            ignored_track_block = false;
            current_track = Some(CueTrack {
                number: num,
                file: current_file.clone(),
                ..Default::default()
            });
            continue;
        }

        if ignored_track_block {
            continue;
        }

        // TITLE "..."
        if let Some(title) = parse_quoted_field(trimmed, "TITLE") {
            if let Some(ref mut track) = current_track {
                track.title = Some(title);
            } else {
                sheet.title = Some(title);
            }
            continue;
        }

        // PERFORMER "..."
        if let Some(performer) = parse_quoted_field(trimmed, "PERFORMER") {
            if let Some(ref mut track) = current_track {
                track.performer = Some(performer);
            } else {
                sheet.performer = Some(performer);
            }
            continue;
        }

        // REM DATE / REM YEAR / REM GENRE (album-level, before any TRACK).
        if current_track.is_none() {
            if let Some(val) =
                parse_rem_field(trimmed, "DATE").or_else(|| parse_rem_field(trimmed, "YEAR"))
            {
                sheet.date = Some(val);
                continue;
            }
            if let Some(val) = parse_rem_field(trimmed, "GENRE") {
                sheet.genre = Some(val);
                continue;
            }
            // CATALOG <13 digits>
            if let Some(val) = strip_keyword_ci(trimmed, "CATALOG").map(|s| s.trim()) {
                if !val.is_empty() {
                    sheet.catalog = Some(val.to_string());
                    continue;
                }
            }
        }

        // INDEX 01 MM:SS:FF (track start position).
        if let Some(ref mut track) = current_track {
            if let Some(frames) = parse_index_line(trimmed, "01") {
                track.index01_frames = Some(frames);
                continue;
            }
            // INDEX 00 MM:SS:FF (pregap position).
            if let Some(frames) = parse_index_line(trimmed, "00") {
                track.index00_frames = Some(frames);
                continue;
            }
            // ISRC <code>
            if let Some(rest) = strip_keyword_ci(trimmed, "ISRC") {
                let val = rest.trim();
                if !val.is_empty() {
                    track.isrc = Some(val.to_string());
                    continue;
                }
            }
        }

        // Other lines (FLAGS, etc.) are ignored.
    }

    // Commit the last track.
    if let Some(track) = current_track {
        sheet.tracks.push(track);
    }

    // For tracks without an explicit performer, inherit album performer.
    if let Some(ref album_performer) = sheet.performer {
        for track in &mut sheet.tracks {
            if track.performer.is_none() {
                track.performer = Some(album_performer.clone());
            }
        }
    }

    sheet
}

/// Parse a `FILE "filename" WAVE` line, returning the filename.
fn parse_file_line(line: &str) -> Option<String> {
    let rest = strip_keyword_ci(line, "FILE")?.trim_start();
    if rest.starts_with('"') {
        return extract_quoted(rest);
    }

    // CUE syntax requires a trailing file type token. Accept all text before
    // the final whitespace run as the filename so unquoted legacy sheets with
    // spaces still parse deterministically: `FILE album image.wav WAVE`.
    let split = rest.rfind(char::is_whitespace)?;
    let filename = rest[..split].trim_end();
    let file_type = rest[split..].trim();
    if filename.is_empty() || file_type.is_empty() {
        None
    } else {
        Some(filename.to_string())
    }
}

/// Parse a `TRACK NN AUDIO` line, returning the track number.
#[cfg(test)]
fn parse_track_line(line: &str) -> Option<u32> {
    let (number, is_audio) = parse_track_header(line)?;
    if is_audio {
        Some(number)
    } else {
        None
    }
}

fn parse_track_header(line: &str) -> Option<(u32, bool)> {
    let rest = strip_keyword_ci(line, "TRACK")?.trim_start();
    let mut parts = rest.split_whitespace();
    let number = parts.next()?;
    let mode = parts.next()?;
    let number = number.parse().ok()?;
    Some((number, mode.eq_ignore_ascii_case("AUDIO")))
}

/// Parse a line like `TITLE "Some Title"` or `PERFORMER "Name"`.
fn parse_quoted_field(line: &str, keyword: &str) -> Option<String> {
    let rest = strip_keyword_ci(line, keyword)?.trim_start();
    extract_quoted(rest)
}

/// Extract a double-quoted string from the start of `s`.
fn extract_quoted(s: &str) -> Option<String> {
    let s = s.strip_prefix('"')?;
    let end = s.find('"')?;
    Some(s[..end].to_string())
}

/// Parse a `REM FIELD value` or `REM FIELD "value"` line.
fn parse_rem_field(line: &str, field: &str) -> Option<String> {
    let rest = strip_keyword_ci(line, "REM")?.trim_start();
    let rest = strip_keyword_ci(rest, field)?.trim_start();
    // Handle both quoted and unquoted values.
    if rest.starts_with('"') {
        extract_quoted(rest)
    } else {
        let val = rest.trim();
        if val.is_empty() {
            None
        } else {
            Some(val.to_string())
        }
    }
}

fn strip_keyword_ci<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    if line.len() < keyword.len() {
        return None;
    }
    let head = line.get(..keyword.len())?;
    let rest = line.get(keyword.len()..)?;
    if !head.eq_ignore_ascii_case(keyword) {
        return None;
    }
    if rest.is_empty() || rest.chars().next().map_or(false, char::is_whitespace) {
        Some(rest)
    } else {
        None
    }
}

fn parse_index_line(line: &str, wanted_index: &str) -> Option<u32> {
    let rest = strip_keyword_ci(line, "INDEX")?.trim_start();
    let mut parts = rest.split_whitespace();
    let index = parts.next()?;
    let timestamp = parts.next()?;
    if index != wanted_index {
        return None;
    }
    parse_cue_timestamp(timestamp)
}

/// Parse a CUE "MM:SS:FF" timestamp to a frame count (75 frames/second).
pub fn parse_cue_timestamp(ts: &str) -> Option<u32> {
    let mut parts = ts.split(':');
    let mm: u32 = parts.next()?.parse().ok()?;
    let ss: u32 = parts.next()?.parse().ok()?;
    let ff: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || ss >= 60 || ff >= 75 {
        return None;
    }
    mm.checked_mul(60)?
        .checked_add(ss)?
        .checked_mul(75)?
        .checked_add(ff)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_writeback_rewrites_utf8_metadata_only_as_golden_bytes_and_is_idempotent() {
        let dir = unique_cue_parser_test_dir("sidecar_writeback_utf8_golden");
        let cue_path = dir.join("album.cue");
        let original = concat!(
            "; untouched comment\r\n",
            "REM COMMENT untouched\n",
            "PERFORMER \"Old Artist\"\r\n",
            "TITLE \"Old Album\"\n",
            "CATALOG 1111111111111\r\n",
            "REM GENRE Rock\n",
            "FILE \"image.wav\" WAVE\r\n",
            "  TRACK 01 AUDIO\n",
            "    TITLE \"Old One\"\r\n",
            "    FLAGS PRE\n",
            "    PREGAP 00:02:00\r\n",
            "    INDEX 00 00:00:00\n",
            "    INDEX 01 00:00:32\r\n",
            "    REM UNKNOWN \"untouched one\"\n",
            "  TRACK 02 AUDIO\r\n",
            "    TITLE \"Old Two\"\n",
            "    ISRC USRC17607840\r\n",
            "    FLAGS DCP\n",
            "    INDEX 00 02:59:00\r\n",
            "    INDEX 01 03:00:00\n",
        );
        std::fs::write(&cue_path, original.as_bytes()).expect("write original cue");

        let replacement = "PERFORMER \"New Artist\"\n\
TITLE \"New Album\"\n\
CATALOG 2222222222222\n\
REM GENRE Jazz\n\
FILE \"different-generated-name.flac\" FLAC\n\
  TRACK 01 AUDIO\n\
    TITLE \"New One\"\n\
    INDEX 01 00:00:32\n\
  TRACK 02 AUDIO\n\
    TITLE \"New Two\"\n\
    ISRC USRC17607841\n\
    INDEX 01 03:00:00\n";

        let outcome = rewrite_cue_sidecar_metadata_from_cuesheet(&cue_path, replacement)
            .expect("sidecar rewrite succeeds");
        assert_eq!(
            outcome,
            CueSidecarWritebackOutcome::Rewritten { encoding: "UTF-8".to_string() }
        );
        let expected = concat!(
            "; untouched comment\r\n",
            "REM COMMENT untouched\n",
            "PERFORMER \"New Artist\"\r\n",
            "TITLE \"New Album\"\n",
            "CATALOG 1111111111111\r\n",
            "REM GENRE Jazz\n",
            "FILE \"image.wav\" WAVE\r\n",
            "  TRACK 01 AUDIO\n",
            "    TITLE \"New One\"\r\n",
            "    FLAGS PRE\n",
            "    PREGAP 00:02:00\r\n",
            "    INDEX 00 00:00:00\n",
            "    INDEX 01 00:00:32\r\n",
            "    REM UNKNOWN \"untouched one\"\n",
            "  TRACK 02 AUDIO\r\n",
            "    TITLE \"New Two\"\n",
            "    ISRC USRC17607840\r\n",
            "    FLAGS DCP\n",
            "    INDEX 00 02:59:00\r\n",
            "    INDEX 01 03:00:00\n",
        );
        let raw = std::fs::read(&cue_path).expect("read rewritten cue");
        assert_eq!(
            raw.as_slice(),
            expected.as_bytes(),
            "UTF-8 sidecar output must match the golden byte fixture exactly"
        );
        let rewritten = std::str::from_utf8(&raw).expect("rewritten UTF-8 cue");
        assert!(!rewritten.contains("different-generated-name.flac"));
        assert!(!rewritten.contains("2222222222222"));
        assert!(!rewritten.contains("USRC17607841"));

        let before_second_save = std::fs::read(&cue_path).expect("read before second save");
        let second = rewrite_cue_sidecar_metadata_from_cuesheet(&cue_path, replacement)
            .expect("second rewrite succeeds");
        assert_eq!(second, CueSidecarWritebackOutcome::Unchanged);
        assert_eq!(
            std::fs::read(&cue_path).expect("read after second save"),
            before_second_save,
            "saving the same metadata twice must be byte-stable"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sidecar_writeback_utf8_bom_album_insertion_keeps_bom_at_byte_zero_and_is_idempotent() {
        let dir = unique_cue_parser_test_dir("sidecar_writeback_utf8_bom_insert");
        let cue_path = dir.join("album.cue");
        let original_body = "FILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Old\"\n    INDEX 01 00:00:00\n";
        let mut original = vec![0xEF, 0xBB, 0xBF];
        original.extend_from_slice(original_body.as_bytes());
        std::fs::write(&cue_path, &original).expect("write UTF-8 BOM cue");

        let replacement = "TITLE \"New Album\"\nFILE \"generated.flac\" FLAC\n  TRACK 01 AUDIO\n    TITLE \"New Track\"\n    INDEX 01 00:00:00\n";
        let outcome = rewrite_cue_sidecar_metadata_from_cuesheet(&cue_path, replacement)
            .expect("rewrite UTF-8 BOM cue with inserted album metadata");
        assert_eq!(
            outcome,
            CueSidecarWritebackOutcome::Rewritten { encoding: "UTF-8 BOM".to_string() }
        );

        let expected_body = "TITLE \"New Album\"\nFILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"New Track\"\n    INDEX 01 00:00:00\n";
        let mut expected = vec![0xEF, 0xBB, 0xBF];
        expected.extend_from_slice(expected_body.as_bytes());
        let raw = std::fs::read(&cue_path).expect("read rewritten UTF-8 BOM cue");
        assert_eq!(
            raw, expected,
            "UTF-8 BOM sidecar must keep the BOM at byte zero and insert album metadata after it"
        );
        assert!(raw.starts_with(&[0xEF, 0xBB, 0xBF]));

        let decoded_with_bom = std::str::from_utf8(&raw).expect("valid UTF-8 BOM output");
        assert!(decoded_with_bom.starts_with('\u{FEFF}'));
        assert!(decoded_with_bom.contains("\nFILE \"album.flac\" WAVE"));
        assert!(
            !decoded_with_bom.contains("\u{FEFF}FILE"),
            "BOM must not migrate onto the FILE line"
        );
        assert!(!decoded_with_bom.contains("generated.flac"));

        let sheet = parse_cue(decoded_with_bom);
        assert_eq!(sheet.title.as_deref(), Some("New Album"));
        assert_eq!(sheet.tracks.len(), 1);
        assert_eq!(sheet.tracks[0].title.as_deref(), Some("New Track"));
        assert_eq!(sheet.tracks[0].file.as_deref(), Some("album.flac"));

        let before_second_save = std::fs::read(&cue_path).expect("read before second save");
        let second = rewrite_cue_sidecar_metadata_from_cuesheet(&cue_path, replacement)
            .expect("second UTF-8 BOM rewrite succeeds");
        assert_eq!(second, CueSidecarWritebackOutcome::Unchanged);
        assert_eq!(
            std::fs::read(&cue_path).expect("read after second save"),
            before_second_save,
            "UTF-8 BOM sidecar rewrite must be deterministic"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sidecar_writeback_refuses_double_quotes_in_quoted_metadata_without_changing_sidecar() {
        let dir = unique_cue_parser_test_dir("sidecar_writeback_quote_refusal");
        let cue_path = dir.join("album.cue");
        let original = "TITLE \"Old\"\nFILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Old One\"\n    INDEX 01 00:00:00\n";
        std::fs::write(&cue_path, original).expect("write original cue");

        let replacement = "TITLE \"New \"Quoted\" Album\"\nFILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"New One\"\n    INDEX 01 00:00:00\n";
        let err = rewrite_cue_sidecar_metadata_from_cuesheet(&cue_path, replacement)
            .expect_err("embedded quotes in quoted metadata should not be serialized lossily");
        assert!(err.contains("double quote"));
        assert_eq!(std::fs::read_to_string(&cue_path).expect("read cue"), original);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sidecar_writeback_rewrites_shift_jis_metadata_only_as_golden_bytes() {
        let dir = unique_cue_parser_test_dir("sidecar_writeback_sjis_golden");
        let cue_path = dir.join("album.cue");
        let original = concat!(
            "; コメント未変更\r\n",
            "REM COMMENT \"未変更\"\n",
            "TITLE \"日本\"\r\n",
            "PERFORMER \"古いアーティスト\"\n",
            "FILE \"音源.wav\" WAVE\r\n",
            "  TRACK 01 AUDIO\n",
            "    TITLE \"日本一\"\r\n",
            "    FLAGS PRE\n",
            "    PREGAP 00:02:00\r\n",
            "    INDEX 00 00:00:00\n",
            "    INDEX 01 00:00:32\r\n",
            "    REM UNKNOWN \"トラック未知\"\n",
            "  TRACK 02 AUDIO\r\n",
            "    TITLE \"日本二\"\n",
            "    ISRC JPN123456789\r\n",
            "    FLAGS DCP\n",
            "    INDEX 00 00:59:00\r\n",
            "    INDEX 01 01:00:00\n",
        );
        let (encoded, _encoding, had_errors) = SHIFT_JIS.encode(original);
        assert!(!had_errors);
        let original_bytes = encoded.into_owned();
        std::fs::write(&cue_path, &original_bytes).expect("write Shift-JIS cue");
        std::fs::write(dir.join("音源.wav"), b"").expect("write referenced image fixture");

        let replacement = "TITLE \"東京\"\n\
PERFORMER \"新しいアーティスト\"\n\
FILE \"generated.flac\" FLAC\n\
  TRACK 01 AUDIO\n\
    TITLE \"東京一\"\n\
    INDEX 01 00:00:32\n\
  TRACK 02 AUDIO\n\
    TITLE \"東京二\"\n\
    ISRC JPN987654321\n\
    INDEX 01 01:00:00\n";
        let outcome = rewrite_cue_sidecar_metadata_from_cuesheet(&cue_path, replacement)
            .expect("rewrite Shift-JIS cue");
        assert_eq!(
            outcome,
            CueSidecarWritebackOutcome::Rewritten { encoding: "CP932/Shift-JIS".to_string() }
        );

        let expected = concat!(
            "; コメント未変更\r\n",
            "REM COMMENT \"未変更\"\n",
            "TITLE \"東京\"\r\n",
            "PERFORMER \"新しいアーティスト\"\n",
            "FILE \"音源.wav\" WAVE\r\n",
            "  TRACK 01 AUDIO\n",
            "    TITLE \"東京一\"\r\n",
            "    FLAGS PRE\n",
            "    PREGAP 00:02:00\r\n",
            "    INDEX 00 00:00:00\n",
            "    INDEX 01 00:00:32\r\n",
            "    REM UNKNOWN \"トラック未知\"\n",
            "  TRACK 02 AUDIO\r\n",
            "    TITLE \"東京二\"\n",
            "    ISRC JPN123456789\r\n",
            "    FLAGS DCP\n",
            "    INDEX 00 00:59:00\r\n",
            "    INDEX 01 01:00:00\n",
        );
        let (expected_bytes, _encoding, had_errors) = SHIFT_JIS.encode(expected);
        assert!(!had_errors);
        let raw = std::fs::read(&cue_path).expect("read Shift-JIS output");
        assert_eq!(
            raw.as_slice(),
            expected_bytes.as_ref(),
            "Shift-JIS sidecar output must match the golden byte fixture exactly"
        );
        assert!(std::str::from_utf8(&raw).is_err(), "output should remain non-UTF-8");
        let decoded = decode_cue_bytes_for_path(&raw, &cue_path).expect("decode rewritten cue");
        assert!(decoded.contains("TITLE \"東京\""));
        assert!(decoded.contains("PERFORMER \"新しいアーティスト\""));
        assert!(decoded.contains("TITLE \"東京一\""));
        assert!(decoded.contains("TITLE \"東京二\""));
        assert!(decoded.contains("FILE \"音源.wav\" WAVE"));
        assert!(!decoded.contains("generated.flac"));
        assert!(!decoded.contains("JPN987654321"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sidecar_writeback_falls_back_to_utf8_when_legacy_encoding_cannot_represent_text() {
        let dir = unique_cue_parser_test_dir("sidecar_writeback_utf8_fallback");
        let cue_path = dir.join("album.cue");
        let original = "TITLE \"日本\"\nFILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"日本一\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"日本二\"\n    INDEX 01 01:00:00\n";
        let (encoded, _encoding, had_errors) = SHIFT_JIS.encode(original);
        assert!(!had_errors);
        std::fs::write(&cue_path, encoded.as_ref()).expect("write Shift-JIS cue");
        std::fs::write(dir.join("image.wav"), b"").expect("write referenced image fixture");

        let replacement = "TITLE \"Smile 😀\"\nFILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Smile 😀 One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Smile 😀 Two\"\n    INDEX 01 01:00:00\n";
        let outcome = rewrite_cue_sidecar_metadata_from_cuesheet(&cue_path, replacement)
            .expect("rewrite with UTF-8 fallback");
        assert_eq!(
            outcome,
            CueSidecarWritebackOutcome::RewrittenUtf8Fallback {
                source_encoding: "CP932/Shift-JIS".to_string(),
            }
        );
        let raw = std::fs::read(&cue_path).expect("read fallback output");
        let decoded = std::str::from_utf8(&raw).expect("fallback output is UTF-8");
        assert!(decoded.contains("TITLE \"Smile 😀\""));
        assert!(decoded.contains("TITLE \"Smile 😀 One\""));
        assert!(decoded.contains("TITLE \"Smile 😀 Two\""));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sidecar_writeback_read_only_sidecar_is_left_unchanged() {
        let dir = unique_cue_parser_test_dir("sidecar_writeback_readonly");
        let cue_path = dir.join("album.cue");
        let original = "TITLE \"Old\"\nFILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Old One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Old Two\"\n    INDEX 01 01:00:00\n";
        std::fs::write(&cue_path, original).expect("write original cue");
        let mut permissions = std::fs::metadata(&cue_path).expect("metadata").permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&cue_path, permissions).expect("mark read-only");

        let replacement = "TITLE \"New\"\nFILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"New One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"New Two\"\n    INDEX 01 01:00:00\n";
        let err = rewrite_cue_sidecar_metadata_from_cuesheet(&cue_path, replacement)
            .expect_err("read-only cue should not be rewritten");
        assert!(err.contains("read-only"));
        assert_eq!(std::fs::read_to_string(&cue_path).expect("read cue"), original);

        let mut permissions = std::fs::metadata(&cue_path).expect("metadata").permissions();
        permissions.set_readonly(false);
        let _ = std::fs::set_permissions(&cue_path, permissions);
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn single_image_cue() {
        let cue = r#"
PERFORMER "Miles Davis"
TITLE "Kind of Blue"
FILE "album.wav" WAVE
  TRACK 01 AUDIO
    TITLE "So What"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Freddie Freeloader"
    INDEX 01 09:22:00
  TRACK 03 AUDIO
    TITLE "Blue in Green"
    PERFORMER "Bill Evans"
    INDEX 01 19:14:00
"#;
        let sheet = parse_cue(cue);
        assert_eq!(sheet.title.as_deref(), Some("Kind of Blue"));
        assert_eq!(sheet.performer.as_deref(), Some("Miles Davis"));
        assert_eq!(sheet.tracks.len(), 3);

        assert_eq!(sheet.tracks[0].number, 1);
        assert_eq!(sheet.tracks[0].title.as_deref(), Some("So What"));
        assert_eq!(sheet.tracks[0].performer.as_deref(), Some("Miles Davis")); // inherited
        assert_eq!(sheet.tracks[0].file.as_deref(), Some("album.wav"));

        assert_eq!(sheet.tracks[2].number, 3);
        assert_eq!(sheet.tracks[2].title.as_deref(), Some("Blue in Green"));
        assert_eq!(sheet.tracks[2].performer.as_deref(), Some("Bill Evans")); // overridden
    }

    #[test]
    fn track_by_track_cue() {
        let cue = r#"
PERFORMER "Artist"
TITLE "Album"
FILE "01 - First.flac" WAVE
  TRACK 01 AUDIO
    TITLE "First Song"
    INDEX 01 00:00:00
FILE "02 - Second.flac" WAVE
  TRACK 02 AUDIO
    TITLE "Second Song"
    INDEX 01 00:00:00
"#;
        let sheet = parse_cue(cue);
        assert_eq!(sheet.tracks.len(), 2);
        assert_eq!(sheet.tracks[0].file.as_deref(), Some("01 - First.flac"));
        assert_eq!(sheet.tracks[1].file.as_deref(), Some("02 - Second.flac"));
        assert_eq!(sheet.tracks[0].title.as_deref(), Some("First Song"));
        assert_eq!(sheet.tracks[1].title.as_deref(), Some("Second Song"));
    }

    #[test]
    fn minimal_cue() {
        let cue = "TRACK 01 AUDIO\n  TITLE \"Only Track\"\n";
        let sheet = parse_cue(cue);
        assert_eq!(sheet.tracks.len(), 1);
        assert_eq!(sheet.tracks[0].title.as_deref(), Some("Only Track"));
        assert!(sheet.performer.is_none());
    }

    #[test]
    fn empty_cue() {
        let sheet = parse_cue("");
        assert!(sheet.tracks.is_empty());
    }

    #[test]
    fn track_number_parsing() {
        assert_eq!(parse_track_line("TRACK 01 AUDIO"), Some(1));
        assert_eq!(parse_track_line("track 02 audio"), Some(2));
        assert_eq!(parse_track_line("TrAcK 03 AuDiO"), Some(3));
        assert_eq!(parse_track_line("TRACK 12 AUDIO"), Some(12));
        assert_eq!(parse_track_line("TRACK 1 AUDIO"), Some(1));
        assert_eq!(parse_track_line("TRACK 04 DATA"), None);
        assert_eq!(parse_track_line("  TRACK 05 AUDIO"), None); // leading whitespace stripped by caller
    }

    #[test]
    fn parses_case_insensitive_keywords_and_unquoted_file_refs() {
        let cue = r#"
performer "Artist"
title "Album"
file album image.wav wave
  track 01 audio
    title "First"
    index 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Second"
    INDEX 01 00:02:00
"#;
        let sheet = parse_cue(cue);
        assert_eq!(sheet.title.as_deref(), Some("Album"));
        assert_eq!(sheet.performer.as_deref(), Some("Artist"));
        assert_eq!(sheet.tracks.len(), 2);
        assert_eq!(sheet.tracks[0].file.as_deref(), Some("album image.wav"));
        assert_eq!(sheet.tracks[0].index01_frames, Some(0));
        assert_eq!(sheet.tracks[1].index01_frames, Some(150));
    }

    #[test]
    fn ignores_non_audio_track_blocks() {
        let cue = r#"
FILE "data.bin" BINARY
  TRACK 01 MODE1/2352
    TITLE "Data Track"
FILE "album.wav" WAVE
  TRACK 02 AUDIO
    TITLE "Audio Track"
    INDEX 01 00:00:00
"#;
        let sheet = parse_cue(cue);
        assert_eq!(sheet.tracks.len(), 1);
        assert_eq!(sheet.tracks[0].number, 2);
        assert_eq!(sheet.tracks[0].title.as_deref(), Some("Audio Track"));
        assert_eq!(sheet.tracks[0].file.as_deref(), Some("album.wav"));
        assert!(sheet.title.is_none());
    }

    #[test]
    fn timestamp_parser_validates_ranges_and_overflow() {
        assert_eq!(parse_cue_timestamp("00:00:00"), Some(0));
        assert_eq!(parse_cue_timestamp("03:43:37"), Some(16762));
        assert_eq!(parse_cue_timestamp("00:60:00"), None);
        assert_eq!(parse_cue_timestamp("00:00:75"), None);
        assert_eq!(parse_cue_timestamp("00:00"), None);
        assert_eq!(parse_cue_timestamp("999999999999:00:00"), None);
    }

    #[test]
    fn cue_byte_decoder_avoids_replacement_loss() {
        let windows_1252 = b"TITLE \"B\xF6rk\"\nFILE album.wav WAVE\n  TRACK 01 AUDIO\n";
        let decoded = decode_cue_bytes(windows_1252).expect("windows-1252 fallback decodes");
        assert!(decoded.contains("Börk"));
        assert!(!decoded.contains('\u{FFFD}'));

        let utf16le = [
            0xFF, 0xFE, b'T', 0, b'I', 0, b'T', 0, b'L', 0, b'E', 0, b' ', 0, b'"', 0,
            b'A', 0, b'"', 0,
        ];
        let decoded_utf16 = decode_cue_bytes(&utf16le).expect("utf-16le BOM decodes");
        assert_eq!(decoded_utf16, "TITLE \"A\"");
    }

    #[test]
    fn cue_byte_decoder_uses_path_context_for_cp932_shift_jis() {
        let dir = unique_cue_parser_test_dir("cp932");
        let audio_path = dir.join("日本.flac");
        std::fs::write(&audio_path, b"").expect("create referenced audio");
        let cue_path = dir.join("album.cue");

        let cp932 = b"TITLE \"\x93\xFA\x96\x7B\"\nFILE \"\x93\xFA\x96\x7B.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n";
        let decoded = decode_cue_bytes_for_path(cp932, &cue_path)
            .expect("CP932/Shift-JIS fallback decodes with path context");
        assert!(decoded.contains("日本.flac"));
        assert!(!decoded.contains('\u{FFFD}'));

        let sheet = parse_cue(&decoded);
        assert_eq!(sheet.tracks[0].file.as_deref(), Some("日本.flac"));

        let _ = std::fs::remove_dir_all(dir);
    }


    #[test]
    fn cue_byte_decoder_path_score_uses_resolver_semantics() {
        let dir = unique_cue_parser_test_dir("resolver_semantics");
        let disc = dir.join("disc");
        std::fs::create_dir_all(&disc).expect("create disc dir");
        std::fs::write(disc.join("image.flac"), b"").expect("create referenced audio");

        assert_eq!(
            cue_decode_path_resolution(&dir, "disc\\image.wav"),
            CueDecodePathResolution::UniqueStemFallback
        );

        let cue_text = "FILE \"disc\\image.wav\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n";
        assert!(cue_decode_score(cue_text, Some(&dir)) >= 5_000);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cue_byte_decoder_scores_cp932_subdirectory_extension_fallback() {
        let dir = unique_cue_parser_test_dir("cp932_subdir_ext");
        let disc = dir.join("disc");
        std::fs::create_dir_all(&disc).expect("create disc dir");
        std::fs::write(disc.join("日本.flac"), b"").expect("create referenced audio");
        let cue_path = dir.join("album.cue");

        let cp932 = b"TITLE \"\x93\xFA\x96\x7B\"\nFILE \"disc\\\x93\xFA\x96\x7B.wav\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n";
        let decoded = decode_cue_bytes_for_path(cp932, &cue_path)
            .expect("CP932/Shift-JIS fallback decodes with resolver-aware path context");
        assert!(decoded.contains("disc\\日本.wav"));
        assert!(!decoded.contains('\u{FFFD}'));

        let sheet = parse_cue(&decoded);
        assert_eq!(sheet.tracks[0].file.as_deref(), Some("disc\\日本.wav"));

        let _ = std::fs::remove_dir_all(dir);
    }

    fn unique_cue_parser_test_dir(label: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        dir.push(format!(
            "cue_parser_{}_{}_{}",
            label,
            std::process::id(),
            unique
        ));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    #[test]
    fn quoted_extraction() {
        assert_eq!(
            extract_quoted("\"hello world\" extra"),
            Some("hello world".to_string())
        );
        assert_eq!(extract_quoted("no quotes"), None);
        assert_eq!(extract_quoted("\"\""), Some("".to_string()));
    }

    #[test]
    fn parse_extracts_index00_isrc_and_catalog() {
        // Noncompliant CUE shape: TRACK 03 declared inside FILE 02 with
        // INDEX 00, then FILE 03 + INDEX 01.
        let content = "\
CATALOG 0044007735428
PERFORMER \"The Allman Brothers Band\"
TITLE \"At Fillmore East\"
REM DATE 1971
FILE \"02 - Trouble.wav\" WAVE
  TRACK 02 AUDIO
    TITLE \"Trouble No More\"
    ISRC USRC17607840
    INDEX 01 00:00:00
  TRACK 03 AUDIO
    TITLE \"Don't Keep Me Wonderin'\"
    ISRC H2HF37290000
    INDEX 00 03:43:37
FILE \"03 - Wonderin.wav\" WAVE
    INDEX 01 00:00:00
";
        let sheet = parse_cue(content);
        assert_eq!(sheet.catalog.as_deref(), Some("0044007735428"));
        assert_eq!(sheet.tracks.len(), 2);
        assert_eq!(sheet.tracks[0].isrc.as_deref(), Some("USRC17607840"));
        assert!(sheet.tracks[0].index00_frames.is_none());
        assert_eq!(sheet.tracks[1].isrc.as_deref(), Some("H2HF37290000"));
        assert_eq!(
            sheet.tracks[1].file.as_deref(),
            Some("03 - Wonderin.wav"),
            "a FILE line between INDEX 00 and INDEX 01 must become the track image"
        );
        // 03:43:37 = 3*60*75 + 43*75 + 37 = 13500 + 3225 + 37 = 16762
        assert_eq!(sheet.tracks[1].index00_frames, Some(16762));
        assert_eq!(sheet.tracks[1].index01_frames, Some(0));
    }

    #[test]
    fn file_after_index01_does_not_reassign_completed_track() {
        let cue = r#"
FILE "01.flac" WAVE
  TRACK 01 AUDIO
    TITLE "One"
    INDEX 01 00:00:00
FILE "02.flac" WAVE
  TRACK 02 AUDIO
    TITLE "Two"
    INDEX 01 00:00:00
"#;
        let sheet = parse_cue(cue);
        assert_eq!(sheet.tracks.len(), 2);
        assert_eq!(sheet.tracks[0].file.as_deref(), Some("01.flac"));
        assert_eq!(sheet.tracks[1].file.as_deref(), Some("02.flac"));
    }

    #[test]
    fn file_between_index00_and_index01_reassigns_current_track() {
        let cue = r#"
FILE "previous.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 00 03:43:37
FILE "current.wav" WAVE
    INDEX 01 00:00:00
"#;
        let sheet = parse_cue(cue);
        assert_eq!(sheet.tracks.len(), 2);
        assert_eq!(sheet.tracks[0].file.as_deref(), Some("previous.wav"));
        assert_eq!(sheet.tracks[1].file.as_deref(), Some("current.wav"));
        assert_eq!(sheet.tracks[1].index00_frames, Some(16762));
        assert_eq!(sheet.tracks[1].index01_frames, Some(0));
    }

    #[test]
    fn parse_cue_strips_utf8_bom_on_first_line() {
        // Some Windows editors prefix CUE files with a UTF-8 BOM
        // (EF BB BF → U+FEFF). Without stripping, the first line's
        // strip_prefix("TITLE") fails silently and album-level title
        // gets dropped.
        let mut content = String::new();
        content.push('\u{FEFF}');
        content.push_str("TITLE \"My Album\"\nFILE \"album.flac\" FLAC\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n");
        let sheet = parse_cue(&content);
        assert_eq!(
            sheet.title.as_deref(),
            Some("My Album"),
            "BOM-prefixed first line still yields album-level TITLE"
        );
        assert_eq!(sheet.tracks.len(), 1);
    }

}


#[cfg(test)]
mod writeback_end_to_end_tests {
    use super::*;

    fn tool_ok(tool: &str) -> bool {
        std::process::Command::new(tool).arg("-version")
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .status().map(|s| s.success()).unwrap_or(false)
    }

    /// Full loop: an album whose image carries an editor-regenerated embedded
    /// CUESHEET gets its sidecar rewritten with the corrected metadata,
    /// structure byte-preserved, and a second save is a no-op.
    #[test]
    fn editor_corrected_album_writes_back_and_is_idempotent() {
        if !tool_ok("ffmpeg") || !tool_ok("metaflac") {
            eprintln!("skipping: ffmpeg or metaflac unavailable");
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let image = temp.path().join("album.flac");
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                   "sine=frequency=440:sample_rate=44100:duration=1", "-c:a", "flac"])
            .arg(&image).status().expect("ffmpeg").success());
        let sidecar = temp.path().join("album.cue");
        std::fs::write(&sidecar,
            "REM DATE 1969\r\nPERFORMER \"Creedence Clearwater Revival\"\r\nTITLE \"Green River (DCC GZS-1064)\"\r\nFILE \"album.flac\" WAVE\r\n  TRACK 01 AUDIO\r\n    TITLE \"Stale One\"\r\n    INDEX 01 00:00:00\r\n  TRACK 02 AUDIO\r\n    TITLE \"Stale Two\"\r\n    INDEX 01 00:30:00\r\n",
        ).expect("sidecar");
        let corrected = "PERFORMER \"Creedence Clearwater Revival\"\nTITLE \"Green River (DCC)\"\nFILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Green River\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Commotion\"\n    INDEX 01 00:30:00\n";

        let before = std::fs::read(&sidecar).expect("before");
        let outcome = rewrite_cue_sidecar_metadata_from_cuesheet(&sidecar, corrected)
            .expect("writeback succeeds");
        eprintln!("outcome: {outcome:?}");
        let after = std::fs::read(&sidecar).expect("after");
        let text = String::from_utf8(after.clone()).expect("utf8");
        assert!(text.contains("TITLE \"Green River (DCC)\""), "album title corrected");
        assert!(text.contains("TITLE \"Green River\""), "track 1 corrected");
        assert!(text.contains("TITLE \"Commotion\""), "track 2 corrected");
        assert!(text.contains("INDEX 01 00:30:00"), "structure preserved");
        assert!(text.contains("REM DATE 1969"), "untouched REM preserved");
        assert!(text.contains("\r\n"), "CRLF line endings preserved");
        assert_ne!(before, after);

        let second = rewrite_cue_sidecar_metadata_from_cuesheet(&sidecar, corrected)
            .expect("second save succeeds");
        eprintln!("second outcome: {second:?}");
        assert_eq!(std::fs::read(&sidecar).expect("final"), after, "re-save is a byte no-op");
    }
}
