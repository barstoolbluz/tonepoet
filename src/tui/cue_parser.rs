//! CUE sheet parser for metadata import and bulk rename.
//!
//! Extracts album/track metadata (title, performer, date, genre) and
//! file references from a CUE sheet. Handles both single-image (one
//! FILE + many TRACKs) and track-by-track (one FILE per TRACK) layouts.

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
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return std::str::from_utf8(&raw[3..])
            .map(|text| text.to_string())
            .map_err(|e| format!("invalid UTF-8 after BOM: {}", e));
    }

    if let Some((encoding, bom_len)) = encoding_rs::Encoding::for_bom(raw) {
        let Some(text) = encoding.decode_without_bom_handling_and_without_replacement(&raw[bom_len..]) else {
            return Err(format!(
                "{} CUE contains invalid byte sequences",
                encoding.name()
            ));
        };
        return Ok(text.into_owned());
    }

    if let Ok(text) = std::str::from_utf8(raw) {
        return Ok(text.to_string());
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
        .map(|candidate| candidate.text)
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

// ── Single-image detection ──────────────────────────────────────────

/// Information about a single-image CUE album (one audio file + CUE sheet).
#[derive(Debug, Clone)]
pub struct SingleImageInfo {
    /// Path to the audio image file.
    pub audio_path: PathBuf,
    /// Path to the CUE sheet.
    pub cue_path: PathBuf,
    /// Parsed CUE sheet.
    pub sheet: CueSheet,
    /// Audio sample rate (e.g., 44100).
    pub sample_rate: u32,
    /// Total samples in the image file.
    pub total_samples: u64,
    /// Per-track boundaries: (start_sample, sample_count).
    pub track_boundaries: Vec<(u64, u64)>,
}

/// Detect if `dir` contains a single-image CUE layout.
///
/// Returns `Some` if the directory has a CUE sheet with one FILE
/// reference, multiple TRACKs with INDEX 01 timestamps, and the
/// referenced audio file exists. Returns `None` for track-per-file
/// layouts or directories without CUE sheets.
pub fn detect_single_image(dir: &Path) -> Option<SingleImageInfo> {
    let cue_path = crate::tui::gnudb::find_cue_in_dir(dir)?;
    let sheet = parse_cue_file(&cue_path).ok()?;

    // Must have multiple tracks with INDEX 01 timestamps.
    if sheet.tracks.len() < 2 {
        return None;
    }
    if !sheet.tracks.iter().all(|t| t.index01_frames.is_some()) {
        return None;
    }

    // Must be a single-image layout (all tracks share the same FILE).
    let first_file = sheet.tracks[0].file.as_ref()?;
    let all_same_file = sheet
        .tracks
        .iter()
        .all(|t| t.file.as_ref() == Some(first_file));
    if !all_same_file {
        return None;
    }

    // Resolve the audio file (handles extension mismatches).
    let audio_path = crate::tui::accuraterip::resolve_cue_file_reference(dir, first_file)?;

    // Probe for sample rate and total samples.
    let (total_samples, sample_rate) =
        crate::tui::accuraterip::probe_sample_count(&audio_path).ok()?;
    let samples_per_frame = (sample_rate / 75) as u64;

    // Compute per-track boundaries from INDEX 01 frames.
    let n = sheet.tracks.len();
    let mut boundaries = Vec::with_capacity(n);
    for i in 0..n {
        let start_frames = sheet.tracks[i].index01_frames.unwrap() as u64;
        let start_sample = start_frames * samples_per_frame;
        let end_sample = if i + 1 < n {
            let next_frames = sheet.tracks[i + 1].index01_frames.unwrap() as u64;
            next_frames * samples_per_frame
        } else {
            total_samples
        };
        if end_sample <= start_sample {
            return None; // invalid CUE: overlapping or zero-length tracks
        }
        boundaries.push((start_sample, end_sample - start_sample));
    }

    Some(SingleImageInfo {
        audio_path,
        cue_path,
        sheet,
        sample_rate,
        total_samples,
        track_boundaries: boundaries,
    })
}

/// Extract each track from a single-image file to separate temp FLACs.
///
/// Uses `ffmpeg -ss <start> -t <duration>` to extract each segment.
/// For WavPack v4 files (ffmpeg can't read), decodes the full image
/// to a temp WAV via wvunpack first, then extracts segments from that.
///
/// Returns the list of temp track paths in order.
pub fn extract_single_image_tracks(
    info: &SingleImageInfo,
    tmp_dir: &Path,
) -> Result<Vec<PathBuf>, String> {
    use std::process::Command;

    // Try extracting directly from the source file. If ffmpeg can't
    // read it (e.g., WavPack v4), decode to a temp WAV first.
    let source_path = if can_ffmpeg_read(&info.audio_path) {
        info.audio_path.clone()
    } else {
        // Decode via wvunpack to temp WAV.
        let tmp_wav = tmp_dir.join("_image.wav");
        let status = Command::new("wvunpack")
            .args(["-q", "-o"])
            .arg(&tmp_wav)
            .arg(&info.audio_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("wvunpack failed: {}", e))?;
        if !status.success() {
            return Err("wvunpack decode failed".into());
        }
        tmp_wav
    };

    let mut track_paths = Vec::with_capacity(info.track_boundaries.len());

    for (i, &(start_sample, sample_count)) in info.track_boundaries.iter().enumerate() {
        let start_secs = start_sample as f64 / info.sample_rate as f64;
        let duration_secs = sample_count as f64 / info.sample_rate as f64;

        let out_path = tmp_dir.join(format!("track_{:02}.flac", i + 1));

        let status = Command::new("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-ss",
                &format!("{:.6}", start_secs),
                "-t",
                &format!("{:.6}", duration_secs),
                "-i",
            ])
            .arg(&source_path)
            .args(["-c:a", "flac", "-compression_level", "0"]) // fast compression
            .arg(&out_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
            .map_err(|e| format!("ffmpeg segment extract failed: {}", e))?;

        if !status.success() {
            return Err(format!("ffmpeg failed to extract track {}", i + 1));
        }

        track_paths.push(out_path);
    }

    Ok(track_paths)
}

/// Quick check if ffmpeg can open a file (without full decode).
pub fn can_ffmpeg_read(path: &Path) -> bool {
    use std::process::Command;
    Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type"])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

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
