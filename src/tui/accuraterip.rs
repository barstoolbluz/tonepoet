//! AccurateRip verification: disc ID computation, CRCv1/v2, binary response
//! parsing, offset scanning, and per-track PCM decode + CRC computation.
//!
//! Reference: <http://www.accuraterip.com/driveoffsets.htm>
//! Algorithm details: <https://hydrogenaud.io/index.php?topic=36162.0>

use std::path::{Path, PathBuf};

// ── Data structures ─────────────────────────────────────────────────

/// AccurateRip disc ID (three components used in the database URL).
#[derive(Debug, Clone)]
pub struct ArDiscId {
    /// Number of tracks.
    pub track_count: u8,
    /// Sum of frame offsets (tracks + leadout).
    pub id1: u32,
    /// Sum of (offset × position), wrapping.
    pub id2: u32,
    /// CDDB/FreeDB disc ID.
    pub freedb_id: u32,
}

/// A single pressing entry from the AccurateRip database.
#[derive(Debug, Clone)]
pub struct ArPressing {
    pub track_count: u8,
    pub id1: u32,
    pub id2: u32,
    pub freedb_id: u32,
    pub tracks: Vec<ArTrackEntry>,
}

/// Per-track entry within a pressing.
#[derive(Debug, Clone)]
pub struct ArTrackEntry {
    pub confidence: u8,
    pub crc: u32,
    /// Offset-finding CRC (can be used for offset detection but we
    /// brute-force instead).
    pub offset_crc: u32,
}

/// Full response from the AccurateRip database (one or more pressings).
#[derive(Debug, Clone)]
pub struct ArDiscResponse {
    pub pressings: Vec<ArPressing>,
}

/// Result of verifying a single track.
#[derive(Debug, Clone)]
pub struct ArTrackResult {
    pub path: PathBuf,
    pub track_number: u32,
    pub status: ArTrackStatus,
    /// Confidence from the matching pressing (if verified).
    pub confidence: Option<u8>,
    /// Sample offset where the match was found (0 = perfect rip).
    pub offset: Option<i32>,
    /// Computed CRCv1 at offset 0.
    pub crc_v1: u32,
    /// Computed CRCv2 at offset 0.
    pub crc_v2: u32,
}

/// Status of a single track's verification.
#[derive(Debug, Clone, PartialEq)]
pub enum ArTrackStatus {
    /// CRC matches a database pressing.
    Verified,
    /// CRC does not match any pressing.
    Mismatch,
    /// Disc not found in the AccurateRip database.
    NoDiscInDatabase,
    /// Error during verification (decode failure, etc.).
    Error(String),
}

/// Overall result of an album verification.
#[derive(Debug, Clone)]
pub struct ArVerifyResult {
    pub tracks: Vec<ArTrackResult>,
    /// True if the common-offsets scan was used (false = full scan).
    pub was_common_scan: bool,
    /// Disc ID used for the lookup (for diagnostics).
    pub disc_id_str: String,
    /// URL that was queried (for diagnostics).
    pub url: String,
}

/// Result of a batch AR verification across a library.
#[derive(Debug, Clone)]
pub struct ArBatchResult {
    pub albums: Vec<ArBatchAlbumResult>,
    pub scan_dir: PathBuf,
    pub report_path: Option<PathBuf>,
}

/// Summary for one album in a batch verification.
#[derive(Debug, Clone)]
pub struct ArBatchAlbumResult {
    pub dir: PathBuf,
    pub album_name: String,
    pub total_tracks: usize,
    pub verified: usize,
    pub mismatched: usize,
    pub not_in_db: bool,
    pub confidence: Option<u8>,
    pub offset: Option<i32>,
    pub error: Option<String>,
}

// ── Disc ID computation ─────────────────────────────────────────────

/// Compute the AccurateRip disc ID from exact per-track sample counts.
///
/// AccurateRip uses raw LBA offsets **without** the 150-frame lead-in.
/// Frame offsets are computed from exact sample counts (`samples / 588`
/// for 44100 Hz) to avoid floating-point rounding errors.
///
/// The freedb component uses the CDDB algorithm with the 150-frame
/// lead-in added back.
pub fn compute_ar_disc_id(sample_counts: &[u64], sample_rate: u32) -> ArDiscId {
    let n = sample_counts.len() as u8;
    let samples_per_frame = (sample_rate / 75) as u64; // 588 for 44100 Hz

    // Build frame offsets WITHOUT lead-in (LBA: track 1 starts at 0).
    let mut offsets: Vec<u32> = Vec::with_capacity(n as usize + 1);
    let mut frame = 0u32;
    for &samples in sample_counts {
        offsets.push(frame);
        frame += (samples / samples_per_frame) as u32;
    }
    let leadout = frame;
    offsets.push(leadout); // offsets now has n+1 entries (tracks + leadout)

    // id1 = sum of all offsets (tracks + leadout)
    let id1: u32 = offsets.iter().fold(0u32, |acc, &o| acc.wrapping_add(o));

    // id2 = sum of (offset * position), where position is 1-based
    // and offset 0 is treated as 1 for the multiplication.
    let id2: u32 = offsets.iter().enumerate().fold(0u32, |acc, (i, &o)| {
        let pos = (i + 1) as u32;
        let val = if o == 0 { 1 } else { o };
        acc.wrapping_add(val.wrapping_mul(pos))
    });

    // CDDB disc ID — computed with 150-frame lead-in added.
    let freedb_id = compute_freedb_id(&offsets, n as usize);

    ArDiscId {
        track_count: n,
        id1,
        id2,
        freedb_id,
    }
}

/// Compute the AccurateRip disc ID from exact CD TOC sector offsets.
///
/// `toc_sectors` includes track start sectors + leadout, all WITH the
/// 150-frame lead-in (standard TOC format from EAC logs, MusicBrainz, etc.).
pub fn compute_ar_disc_id_from_toc(toc_sectors: &[u32]) -> ArDiscId {
    let n = (toc_sectors.len() - 1) as u8; // tracks (last entry is leadout)

    // AR offsets: subtract 150-frame lead-in.
    let ar_offsets: Vec<u32> = toc_sectors.iter().map(|&s| s.saturating_sub(150)).collect();

    // id1 = sum of all AR offsets (tracks + leadout)
    let id1: u32 = ar_offsets.iter().fold(0u32, |acc, &o| acc.wrapping_add(o));

    // id2 = sum of max(offset, 1) * position (1-based)
    let id2: u32 = ar_offsets.iter().enumerate().fold(0u32, |acc, (i, &o)| {
        let pos = (i + 1) as u32;
        let val = if o == 0 { 1 } else { o };
        acc.wrapping_add(val.wrapping_mul(pos))
    });

    // CDDB disc ID from the TOC (with lead-in).
    let freedb_id = compute_freedb_id(&ar_offsets, n as usize);

    ArDiscId {
        track_count: n,
        id1,
        id2,
        freedb_id,
    }
}

/// Compute CDDB/FreeDB disc ID from frame offsets (without lead-in).
///
/// `offsets` includes tracks + leadout (n+1 entries).
/// Adds 150-frame lead-in internally for the CDDB algorithm.
fn compute_freedb_id(offsets: &[u32], n_tracks: usize) -> u32 {
    // CDDB uses offsets WITH 150-frame lead-in.
    let toc_offsets: Vec<u32> = offsets.iter().map(|&o| o + 150).collect();
    let leadout_toc = *toc_offsets.last().unwrap();

    // CDDB standard: divide THEN subtract, not subtract then divide.
    // Integer truncation makes these differ for some discs.
    let total_secs = leadout_toc / 75 - toc_offsets[0] / 75;

    let mut checksum = 0u32;
    for &off in &toc_offsets[..n_tracks] {
        let mut secs = off / 75;
        while secs > 0 {
            checksum += secs % 10;
            secs /= 10;
        }
    }
    checksum %= 255;

    (checksum << 24) | ((total_secs & 0xFFFF) << 8) | (n_tracks as u32 & 0xFF)
}

/// Probe a file and return its exact sample count.
///
/// Uses ffmpeg's stream duration in time_base units, which for FLAC
/// and other lossless formats is the exact sample count.
pub fn probe_sample_count(path: &std::path::Path) -> Result<(u64, u32), String> {
    // Try ffmpeg first (handles most formats).
    match probe_sample_count_ffmpeg(path) {
        Ok(result) => return Ok(result),
        Err(_) => {}
    }

    // Fallback: format-specific native tools for files ffmpeg can't handle.
    let ext = path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "wv" => probe_sample_count_wvunpack(path),
        _ => Err(format!("cannot probe {}", path.display())),
    }
}

/// Probe sample count via ffmpeg-next (in-process).
fn probe_sample_count_ffmpeg(path: &std::path::Path) -> Result<(u64, u32), String> {
    use ffmpeg_next as ffmpeg;

    crate::tui::probe::ensure_ffmpeg_init_pub();

    let ctx = ffmpeg::format::input(&path)
        .map_err(|e| format!("open failed: {}", e))?;

    let stream = ctx
        .streams()
        .best(ffmpeg::media::Type::Audio)
        .ok_or("no audio stream")?;

    let time_base = stream.time_base();
    let duration = stream.duration();

    let codec_params = stream.parameters();
    let codec_ctx = ffmpeg::codec::context::Context::from_parameters(codec_params)
        .map_err(|e| format!("codec params: {}", e))?;
    let audio = codec_ctx.decoder().audio()
        .map_err(|e| format!("decoder: {}", e))?;
    let sample_rate = audio.rate();

    if duration <= 0 {
        return Err("no duration in stream".into());
    }

    let samples = if time_base.denominator() == sample_rate as i32 && time_base.numerator() == 1 {
        duration as u64
    } else {
        (duration as f64 * time_base.numerator() as f64 / time_base.denominator() as f64
            * sample_rate as f64)
            .round() as u64
    };

    Ok((samples, sample_rate))
}

/// Probe sample count via wvunpack (fallback for WavPack files ffmpeg can't read).
///
/// Parses `wvunpack -q -s` output for duration and sample rate.
/// Example output:
/// ```text
/// source:            16-bit ints at 44100 Hz
/// duration:          0:39:58.41
/// ```
fn probe_sample_count_wvunpack(path: &std::path::Path) -> Result<(u64, u32), String> {
    let output = std::process::Command::new("wvunpack")
        .args(["-q", "-s"])
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("wvunpack failed: {}", e))?;

    if !output.status.success() {
        return Err("wvunpack returned error".into());
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut sample_rate: Option<u32> = None;
    let mut duration_secs: Option<f64> = None;

    for line in text.lines() {
        let trimmed = line.trim();
        // "source:            16-bit ints at 44100 Hz"
        if trimmed.starts_with("source:") {
            if let Some(hz_pos) = trimmed.find(" Hz") {
                let before_hz = &trimmed[..hz_pos];
                if let Some(at_pos) = before_hz.rfind("at ") {
                    if let Ok(sr) = before_hz[at_pos + 3..].trim().parse::<u32>() {
                        sample_rate = Some(sr);
                    }
                }
            }
        }
        // "duration:          0:39:58.41"
        if trimmed.starts_with("duration:") {
            let dur_str = trimmed.split(':').skip(1).collect::<Vec<&str>>().join(":");
            let dur_str = dur_str.trim();
            // Parse H:MM:SS.ff or M:SS.ff
            let parts: Vec<&str> = dur_str.split(':').collect();
            if parts.len() == 3 {
                // H:MM:SS.ff
                let h: f64 = parts[0].trim().parse().unwrap_or(0.0);
                let m: f64 = parts[1].trim().parse().unwrap_or(0.0);
                let s: f64 = parts[2].trim().parse().unwrap_or(0.0);
                duration_secs = Some(h * 3600.0 + m * 60.0 + s);
            } else if parts.len() == 2 {
                // M:SS.ff
                let m: f64 = parts[0].trim().parse().unwrap_or(0.0);
                let s: f64 = parts[1].trim().parse().unwrap_or(0.0);
                duration_secs = Some(m * 60.0 + s);
            }
        }
    }

    let sr = sample_rate.ok_or("could not parse sample rate from wvunpack")?;
    let dur = duration_secs.ok_or("could not parse duration from wvunpack")?;
    let samples = (dur * sr as f64).round() as u64;

    Ok((samples, sr))
}

/// Collect exact sample counts for a list of audio files.
///
/// Returns `(sample_counts, sample_rate)` or an error if any file
/// can't be probed or has a different sample rate.
pub fn collect_sample_counts(paths: &[PathBuf]) -> Result<(Vec<u64>, u32), String> {
    let mut counts = Vec::with_capacity(paths.len());
    let mut rate: Option<u32> = None;

    for path in paths {
        let (samples, sr) = probe_sample_count(path)?;
        if let Some(r) = rate {
            if r != sr {
                return Err(format!(
                    "mixed sample rates: {} vs {} in {}",
                    r, sr, path.display(),
                ));
            }
        } else {
            rate = Some(sr);
        }
        counts.push(samples);
    }

    Ok((counts, rate.unwrap_or(44100)))
}

// ── TOC extraction from log/CUE files ──────────────────────────────

/// Try to find the CD TOC (track start sectors) from a rip log or CUE
/// sheet alongside the audio files. Returns sector offsets WITH the
/// 150-frame lead-in (matching the MusicBrainz/CDDB convention).
///
/// Search order:
/// 1. EAC `.log` file in the same directory (has exact TOC)
/// 2. XLD `.log` file
/// 3. Single-image `.cue` file with INDEX 01 timestamps
///
/// Returns `None` if no TOC source is found.
pub fn find_toc_offsets(dir: &Path) -> Option<Vec<u32>> {
    // Look for .log files first.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ext == "log" {
                log::info!("AccurateRip: trying log file: {}", path.display());
                if let Some(offsets) = parse_eac_log_toc(&path) {
                    log::info!("AccurateRip: parsed TOC from log: {:?}", offsets);
                    return Some(offsets);
                } else {
                    log::info!("AccurateRip: no TOC found in log file");
                }
            }
        }
    } else {
        log::info!("AccurateRip: could not read directory {}", dir.display());
    }

    // Look for .cue files with INDEX 01 timestamps.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ext == "cue" {
                if let Some(offsets) = parse_cue_index_offsets(&path) {
                    return Some(offsets);
                }
            }
        }
    }

    None
}

/// Parse an EAC/XLD log file for the TOC table.
///
/// EAC format:
/// ```text
/// TOC of the extracted CD
///
///      Track |   Start  |  Length  | Start sector | End sector
///     ---------------------------------------------------------
///         1  |  0:02.49 |  4:01.43 |         187  |     18304
///         2  |  4:03.55 |  3:32.58 |       18305  |     34254
/// ```
///
/// Returns start sectors (WITH 150-frame lead-in) for each track,
/// plus the leadout (last track's end sector + 1).
fn parse_eac_log_toc(path: &Path) -> Option<Vec<u32>> {
    // EAC logs can be in various encodings (UTF-8, Windows-1251, ISO-8859-1,
    // UTF-16LE with BOM). Read raw bytes and decode lossily — we only need
    // ASCII content (TOC numbers, pipe characters) so replacing non-UTF8
    // bytes with '�' is fine.
    let raw = std::fs::read(path).ok()?;

    // Handle UTF-16LE BOM (0xFF 0xFE).
    let content = if raw.starts_with(&[0xFF, 0xFE]) {
        // UTF-16LE: decode pairs of bytes.
        let u16s: Vec<u16> = raw[2..].chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u16s)
    } else {
        String::from_utf8_lossy(&raw).into_owned()
    };
    let mut offsets: Vec<u32> = Vec::new();
    let mut last_end: u32 = 0;
    let mut in_toc = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Detect the start of the TOC section.
        // EAC logs can be in any language (English, Russian, German, etc.)
        // so we look for "TOC" at the start of a line, which is consistent
        // across localisations.
        if trimmed.starts_with("TOC ") {
            in_toc = true;
            continue;
        }

        if !in_toc {
            continue;
        }

        // Skip header/separator lines and blank lines.
        if trimmed.is_empty() || trimmed.starts_with("---") {
            continue;
        }

        // Skip lines that don't contain pipe separators (column headers
        // in any language, or non-TOC text).
        if !trimmed.contains('|') {
            // If we already have data, this is the end of the TOC section.
            if !offsets.is_empty() {
                break;
            }
            continue;
        }

        // Parse pipe-separated row. The track number is first, then
        // start time, length, start sector, end sector.
        // Format: "  1  |  0:02.49 |  4:01.43 |         187  |     18304"
        let cols: Vec<&str> = trimmed.split('|').collect();
        if cols.len() >= 5 {
            if let Ok(start_sector) = cols[3].trim().parse::<u32>() {
                offsets.push(start_sector);
                if let Ok(end_sector) = cols[4].trim().parse::<u32>() {
                    last_end = end_sector;
                }
            }
        }
    }

    if offsets.is_empty() {
        return None;
    }

    // EAC log sectors are LBA (0-based, without 150-frame lead-in).
    // Add 150 to convert to absolute sector positions.
    let offsets: Vec<u32> = offsets.iter().map(|&s| s + 150).collect();
    let leadout = last_end + 1 + 150;
    let mut result = offsets;
    result.push(leadout);
    Some(result)
}

/// Parse a CUE sheet for INDEX 01 timestamps and convert to sector offsets.
///
/// Only works for single-image CUE sheets (one FILE with multiple TRACKs).
/// Returns sector offsets WITH 150-frame lead-in.
fn parse_cue_index_offsets(path: &Path) -> Option<Vec<u32>> {
    let content = String::from_utf8_lossy(&std::fs::read(path).ok()?).into_owned();
    let mut offsets: Vec<u32> = Vec::new();
    let mut file_count = 0;
    let mut audio_file: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("FILE ") {
            file_count += 1;
            if file_count > 1 {
                // Multi-file CUE — INDEX timestamps are per-file, not useful.
                return None;
            }
            // Extract the filename: FILE "name.flac" WAVE
            // or FILE name.flac WAVE
            if let Some(name) = extract_cue_filename(trimmed) {
                audio_file = Some(name);
            }
        }

        // "INDEX 01 MM:SS:FF"
        if trimmed.starts_with("INDEX 01 ") {
            let ts = trimmed[9..].trim();
            if let Some(frames) = parse_cue_timestamp(ts) {
                // CUE timestamps are relative to the FILE start.
                // Add 150-frame lead-in to get absolute sector.
                offsets.push(frames + 150);
            }
        }
    }

    if offsets.is_empty() {
        return None;
    }

    // Derive the leadout from the referenced audio file's total duration.
    let cue_dir = path.parent()?;
    let audio_path = resolve_cue_file_reference(cue_dir, &audio_file?)?;
    let (samples, sample_rate) = probe_sample_count(&audio_path).ok()?;
    let samples_per_frame = (sample_rate / 75) as u64;
    let total_frames = (samples / samples_per_frame) as u32;
    // Leadout = total frames + 150-frame lead-in.
    offsets.push(total_frames + 150);

    Some(offsets)
}

/// Extract the filename from a CUE FILE directive.
///
/// Handles both quoted (`FILE "name.flac" WAVE`) and unquoted
/// (`FILE name.flac WAVE`) forms.
fn extract_cue_filename(line: &str) -> Option<String> {
    let after_file = line.strip_prefix("FILE ")?.trim();
    if after_file.starts_with('"') {
        // Quoted: find closing quote.
        let end = after_file[1..].find('"')?;
        Some(after_file[1..1 + end].to_string())
    } else {
        // Unquoted: take everything up to the last whitespace-separated token
        // (which is the format type like WAVE, BINARY, etc.).
        let last_space = after_file.rfind(' ')?;
        Some(after_file[..last_space].trim().to_string())
    }
}

// parse_cue_timestamp is in cue_parser.rs
use super::cue_parser::parse_cue_timestamp;

/// Resolve a CUE FILE reference to an actual file path.
///
/// CUE sheets often reference a filename with an extension that doesn't
/// match the actual file (e.g., `album.wav` when the file is `album.flac`).
/// Tries the original reference first, then common lossless extensions.
pub fn resolve_cue_file_reference(dir: &Path, filename: &str) -> Option<PathBuf> {
    let original = dir.join(filename);
    if original.exists() {
        return Some(original);
    }

    // Try alternative lossless extensions.
    let stem = Path::new(filename).file_stem()?.to_str()?;
    for ext in &["flac", "wav", "wave", "ape", "wv", "aiff", "aif", "m4a", "alac"] {
        let alt = dir.join(format!("{}.{}", stem, ext));
        if alt.exists() {
            return Some(alt);
        }
    }

    None
}

// ── Database URL construction ───────────────────────────────────────

/// Build the AccurateRip database URL for a disc.
///
/// Path components are the least significant hex digits of id1:
/// `accuraterip/{digit0}/{digit1}/{digit2}/dBAR-{n}-{id1}-{id2}-{freedb}.bin`
/// where digit0 is the least significant hex digit (rightmost).
pub fn ar_url(disc_id: &ArDiscId) -> String {
    let id1_hex = format!("{:08x}", disc_id.id1);
    let chars: Vec<char> = id1_hex.chars().collect();
    // chars[0] is MSB, chars[7] is LSB.
    // Path uses LSB digits: position 0 = chars[7], 1 = chars[6], 2 = chars[5].
    let c0 = chars[7]; // least significant hex digit
    let c1 = chars[6];
    let c2 = chars[5];

    format!(
        "http://www.accuraterip.com/accuraterip/{}/{}/{}/dBAR-{:03}-{:08x}-{:08x}-{:08x}.bin",
        c0, c1, c2,
        disc_id.track_count,
        disc_id.id1,
        disc_id.id2,
        disc_id.freedb_id,
    )
}

// ── Database fetch ──────────────────────────────────────────────────

/// Fetch the AccurateRip database entry for a disc.
/// Returns `None` if the disc is not in the database (HTTP 404).
pub async fn fetch_ar_data(disc_id: &ArDiscId) -> Result<Option<ArDiscResponse>, String> {
    let url = ar_url(disc_id);
    log::info!("AccurateRip fetch: {}", url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let resp = client.get(&url)
        .send()
        .await
        .map_err(|e| format!("AccurateRip fetch failed: {}", e))?;

    let status = resp.status();
    log::info!("AccurateRip response: HTTP {}", status);

    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(format!("AccurateRip HTTP {}", status));
    }

    let data = resp.bytes()
        .await
        .map_err(|e| format!("AccurateRip read failed: {}", e))?;

    log::info!("AccurateRip response: {} bytes, parsing...", data.len());
    parse_ar_response(&data).map(Some)
}

// ── Binary response parsing ─────────────────────────────────────────

/// Parse the binary AccurateRip .bin response.
///
/// Format: one or more "pressing" records, concatenated until EOF.
/// Each pressing: 13-byte header + 9 bytes per track.
pub fn parse_ar_response(data: &[u8]) -> Result<ArDiscResponse, String> {
    let mut pressings = Vec::new();
    let mut pos = 0;

    while pos < data.len() {
        // Header: 1 + 4 + 4 + 4 = 13 bytes
        if pos + 13 > data.len() {
            break; // Truncated — stop gracefully
        }

        let track_count = data[pos];
        let id1 = u32::from_le_bytes([data[pos+1], data[pos+2], data[pos+3], data[pos+4]]);
        let id2 = u32::from_le_bytes([data[pos+5], data[pos+6], data[pos+7], data[pos+8]]);
        let freedb_id = u32::from_le_bytes([data[pos+9], data[pos+10], data[pos+11], data[pos+12]]);
        pos += 13;

        // Per-track data: 9 bytes each (1 + 4 + 4)
        let track_data_len = track_count as usize * 9;
        if pos + track_data_len > data.len() {
            break; // Truncated
        }

        let mut tracks = Vec::with_capacity(track_count as usize);
        for _ in 0..track_count {
            let confidence = data[pos];
            let crc = u32::from_le_bytes([data[pos+1], data[pos+2], data[pos+3], data[pos+4]]);
            let offset_crc = u32::from_le_bytes([data[pos+5], data[pos+6], data[pos+7], data[pos+8]]);
            pos += 9;
            tracks.push(ArTrackEntry { confidence, crc, offset_crc });
        }

        pressings.push(ArPressing {
            track_count,
            id1,
            id2,
            freedb_id,
            tracks,
        });
    }

    if pressings.is_empty() {
        return Err("Empty AccurateRip response".into());
    }

    Ok(ArDiscResponse { pressings })
}

// ── CRC computation ─────────────────────────────────────────────────

/// Sectors to skip at the start of the first track and end of the last track.
const SKIP_SECTORS: usize = 5;
/// Samples per CD sector (588 stereo sample pairs = 1/75 second at 44100 Hz).
const SAMPLES_PER_SECTOR: usize = 588;
/// Number of DWORDs to skip (each DWORD = one stereo sample pair).
const SKIP_DWORDS: usize = SKIP_SECTORS * SAMPLES_PER_SECTOR; // 2940

/// Compute AccurateRip CRCv1 and CRCv2 for a buffer of stereo sample DWORDs.
///
/// `dwords` contains the raw interleaved 16-bit stereo PCM reinterpreted as
/// little-endian u32 values. `is_first` and `is_last` control boundary
/// sample skipping per the AR spec.
///
/// Returns `(crc_v1, crc_v2)`.
pub fn compute_ar_crcs(
    dwords: &[u32],
    is_first: bool,
    is_last: bool,
) -> (u32, u32) {
    let n = dwords.len();
    if n == 0 {
        return (0, 0);
    }

    // 1-indexed position range to include in the CRC.
    let check_from: u32 = if is_first { (SKIP_DWORDS + 1) as u32 } else { 1 };
    let check_to: u32 = if is_last {
        n.saturating_sub(SKIP_DWORDS) as u32
    } else {
        n as u32
    };

    let mut crc_v1: u32 = 0;
    let mut crc_v2: u32 = 0;

    for (i, &dw) in dwords.iter().enumerate() {
        let pos = (i + 1) as u32; // 1-indexed
        if pos < check_from || pos > check_to {
            continue;
        }

        // CRCv1: pos × dword, wrapping u32
        crc_v1 = crc_v1.wrapping_add(pos.wrapping_mul(dw));

        // CRCv2: (pos × dword) as u64, then fold high + low u32
        let product: u64 = (dw as u64).wrapping_mul(pos as u64);
        let lo = (product & 0xFFFF_FFFF) as u32;
        let hi = (product >> 32) as u32;
        crc_v2 = crc_v2.wrapping_add(lo).wrapping_add(hi);
    }

    (crc_v1, crc_v2)
}

/// Compute CRCs at a given sample offset from the base buffer.
///
/// `full_dwords` is the track's audio plus a margin of `max_offset`
/// samples on each side. `track_len` is the number of DWORDs in the
/// actual track (without margin). `margin` is the number of extra
/// DWORDs on each side. `offset` is the shift to apply (positive =
/// shift right = read earlier samples).
///
/// Returns `(crc_v1, crc_v2)` for the shifted window, or `None` if
/// the offset would exceed the buffer.
pub fn compute_ar_crcs_at_offset(
    full_dwords: &[u32],
    track_len: usize,
    margin: usize,
    offset: i32,
    is_first: bool,
    is_last: bool,
) -> Option<(u32, u32)> {
    // The track data starts at index `margin` in full_dwords.
    // A positive offset means the drive read samples `offset` positions late,
    // so we shift our window backward by `offset`.
    let start = margin as i64 - offset as i64;
    let end = start + track_len as i64;

    if start < 0 || end > full_dwords.len() as i64 {
        return None;
    }

    let window = &full_dwords[start as usize..end as usize];
    Some(compute_ar_crcs(window, is_first, is_last))
}

// ── Track PCM decoding ──────────────────────────────────────────────

/// Maximum offset in samples for the full scan range.
pub const MAX_OFFSET: usize = 1200;

/// Decode a track to raw interleaved 16-bit stereo PCM (i16 pairs).
///
/// For non-16-bit sources (24-bit FLAC, etc.), truncates to 16-bit
/// since AccurateRip only works with CD-quality audio. Returns
/// interleaved `[L, R, L, R, ...]` i16 values.
pub fn decode_track_to_raw_i16(path: &Path) -> Result<Vec<i16>, String> {
    use ffmpeg_next as ffmpeg;
    use ffmpeg_next::media::Type;
    use ffmpeg_next::util::format::sample::{Sample, Type as SampleType};

    crate::tui::probe::ensure_ffmpeg_init_pub();

    let mut ictx = ffmpeg::format::input(&path)
        .map_err(|e| format!("open failed: {}", e))?;

    let audio_stream = ictx
        .streams()
        .best(Type::Audio)
        .ok_or("no audio stream")?;
    let stream_idx = audio_stream.index();

    let codec_params = audio_stream.parameters();
    let codec_ctx = ffmpeg::codec::context::Context::from_parameters(codec_params)
        .map_err(|e| format!("codec params: {}", e))?;
    let mut decoder = codec_ctx.decoder().audio()
        .map_err(|e| format!("decoder: {}", e))?;

    let channels = decoder.channels() as usize;
    if channels != 2 {
        return Err(format!("AccurateRip requires stereo audio, got {} channels", channels));
    }

    let sample_fmt = decoder.format();
    let mut raw_i16: Vec<i16> = Vec::new();
    let mut decoded = ffmpeg::util::frame::Audio::empty();

    macro_rules! decode_frame {
        () => {
            let n = decoded.samples();
            if n == 0 { continue; }

            match sample_fmt {
                Sample::I16(SampleType::Planar) => {
                    let left = decoded.plane::<i16>(0);
                    let right = decoded.plane::<i16>(1);
                    for i in 0..n {
                        raw_i16.push(left[i]);
                        raw_i16.push(right[i]);
                    }
                }
                Sample::I16(SampleType::Packed) => {
                    let data = decoded.data(0);
                    let interleaved: &[i16] = unsafe {
                        std::slice::from_raw_parts(data.as_ptr() as *const i16, n * 2)
                    };
                    raw_i16.extend_from_slice(interleaved);
                }
                Sample::I32(SampleType::Planar) => {
                    let left = decoded.plane::<i32>(0);
                    let right = decoded.plane::<i32>(1);
                    for i in 0..n {
                        raw_i16.push((left[i] >> 16) as i16);
                        raw_i16.push((right[i] >> 16) as i16);
                    }
                }
                Sample::I32(SampleType::Packed) => {
                    let data = decoded.data(0);
                    let full: &[i32] = unsafe {
                        std::slice::from_raw_parts(data.as_ptr() as *const i32, n * 2)
                    };
                    for i in 0..n {
                        raw_i16.push((full[i * 2] >> 16) as i16);
                        raw_i16.push((full[i * 2 + 1] >> 16) as i16);
                    }
                }
                Sample::F32(SampleType::Planar) => {
                    let left = decoded.plane::<f32>(0);
                    let right = decoded.plane::<f32>(1);
                    for i in 0..n {
                        raw_i16.push(float_to_i16(left[i] as f64));
                        raw_i16.push(float_to_i16(right[i] as f64));
                    }
                }
                Sample::F32(SampleType::Packed) => {
                    let data = decoded.data(0);
                    let full: &[f32] = unsafe {
                        std::slice::from_raw_parts(data.as_ptr() as *const f32, n * 2)
                    };
                    for i in 0..n {
                        raw_i16.push(float_to_i16(full[i * 2] as f64));
                        raw_i16.push(float_to_i16(full[i * 2 + 1] as f64));
                    }
                }
                Sample::F64(SampleType::Planar) => {
                    let left = decoded.plane::<f64>(0);
                    let right = decoded.plane::<f64>(1);
                    for i in 0..n {
                        raw_i16.push(float_to_i16(left[i]));
                        raw_i16.push(float_to_i16(right[i]));
                    }
                }
                Sample::F64(SampleType::Packed) => {
                    let data = decoded.data(0);
                    let full: &[f64] = unsafe {
                        std::slice::from_raw_parts(data.as_ptr() as *const f64, n * 2)
                    };
                    for i in 0..n {
                        raw_i16.push(float_to_i16(full[i * 2]));
                        raw_i16.push(float_to_i16(full[i * 2 + 1]));
                    }
                }
                _ => {
                    return Err(format!("unsupported sample format: {:?}", sample_fmt));
                }
            }
        };
    }

    for (stream, packet) in ictx.packets() {
        if stream.index() != stream_idx {
            continue;
        }
        decoder.send_packet(&packet).map_err(|e| format!("send_packet: {}", e))?;
        while decoder.receive_frame(&mut decoded).is_ok() {
            decode_frame!();
        }
    }
    decoder.send_eof().map_err(|e| format!("send_eof: {}", e))?;
    while decoder.receive_frame(&mut decoded).is_ok() {
        decode_frame!();
    }

    Ok(raw_i16)
}

/// Decode a track to u32 DWORDs with margin, for AR CRC computation.
///
/// Wraps `decode_track_to_raw_i16` and converts to DWORDs with
/// leading/trailing silence margin for offset scanning.
pub fn decode_track_to_dwords(
    path: &Path,
    margin: usize,
) -> Result<(Vec<u32>, usize), String> {
    let raw_i16 = decode_track_to_raw_i16(path)?;

    let track_len = raw_i16.len() / 2; // number of stereo sample pairs
    let mut dwords = Vec::with_capacity(margin + track_len + margin);

    // Leading margin (silence)
    dwords.resize(margin, 0u32);

    // Track data: each DWORD = [L_lo, L_hi, R_lo, R_hi] as LE u32.
    for i in 0..track_len {
        let l = raw_i16[i * 2] as u16;
        let r = raw_i16[i * 2 + 1] as u16;
        let dw = (l as u32) | ((r as u32) << 16);
        dwords.push(dw);
    }

    // Trailing margin (silence)
    dwords.resize(margin + track_len + margin, 0u32);

    Ok((dwords, track_len))
}

/// Clamp float to i16 range.
fn float_to_i16(v: f64) -> i16 {
    let scaled = v * 32768.0;
    if scaled >= 32767.0 {
        32767
    } else if scaled <= -32768.0 {
        -32768
    } else {
        scaled as i16
    }
}

// ── Offset scanning ─────────────────────────────────────────────────

/// Common drive read offsets (in samples) covering >95% of drives.
/// Source: AccurateRip drive offset database, sorted by frequency.
pub static COMMON_OFFSETS: &[i32] = &[
    0,
    6, -6,
    12, -12,
    48, -48,
    97, -97,
    102, -102,
    103, -103,
    116, -116,
    120, -120,
    294, -294,
    355, -355,
    587, -587,
    588, -588,
    594, -594,
    667, -667,
    685, -685,
    691, -691,
    694, -694,
    738, -738,
    1164, -1164,
    1194, -1194,
];

/// Try to match a track's CRCs against the database at a set of offsets.
///
/// Returns `Some((offset, confidence))` on the first match, or `None`.
pub fn try_offsets(
    full_dwords: &[u32],
    track_len: usize,
    margin: usize,
    is_first: bool,
    is_last: bool,
    db_pressings: &[ArPressing],
    track_idx: usize,
    offsets: &[i32],
) -> Option<(i32, u8)> {
    for &offset in offsets {
        if let Some((v1, v2)) = compute_ar_crcs_at_offset(
            full_dwords, track_len, margin, offset, is_first, is_last,
        ) {
            // Check against all pressings.
            for pressing in db_pressings {
                if track_idx < pressing.tracks.len() {
                    let db_crc = pressing.tracks[track_idx].crc;
                    let conf = pressing.tracks[track_idx].confidence;
                    if v1 == db_crc || v2 == db_crc {
                        return Some((offset, conf));
                    }
                }
            }
        }
    }
    None
}

// ── Full verification orchestrator ──────────────────────────────────

/// Verify an album against the AccurateRip database.
///
/// `paths` must be sorted by track order. `sample_counts` are the
/// exact per-track sample counts used to compute the disc ID.
///
/// If `full_scan` is true, tries every offset from -1200 to +1200.
/// Otherwise, tries only the common offsets (~50 values).
pub async fn verify_album(
    paths: &[PathBuf],
    sample_counts: &[u64],
    sample_rate: u32,
    full_scan: bool,
) -> ArVerifyResult {
    let n = paths.len();

    // Try to find the original CD TOC from a log/CUE file.
    // If found, use exact sector offsets; otherwise, reconstruct from
    // sample counts (works when the pre-gap is included in track 1).
    let dir = paths[0].parent().unwrap_or(Path::new("."));
    log::info!("AccurateRip: looking for TOC in {}", dir.display());
    let toc = find_toc_offsets(dir);

    // Track 1 pre-gap: number of extra frames at the start of track 1's
    // file when the rip includes the pre-gap (EAC "Gap appended to
    // previous track"). This is the LBA offset of track 1's INDEX 01.
    // The pre-gap audio must be trimmed before CRC computation.
    //
    // We also compute the expected track 1 duration from the TOC so we
    // can detect whether the file actually includes the pre-gap or not.
    let track1_pregap_frames: usize;
    let track1_toc_duration_frames: usize;
    if let Some(ref sectors) = toc {
        // TOC sectors include 150-frame lead-in. LBA = sector - 150.
        let lba = sectors[0].saturating_sub(150) as usize;
        track1_pregap_frames = lba;
        // Track 1 TOC duration = track 2 start - track 1 start (in sectors).
        if sectors.len() >= 2 {
            let t1_start = sectors[0].saturating_sub(150) as usize;
            let t2_start = sectors[1].saturating_sub(150) as usize;
            track1_toc_duration_frames = t2_start - t1_start;
        } else {
            track1_toc_duration_frames = 0;
        }
    } else {
        track1_pregap_frames = 0;
        track1_toc_duration_frames = 0;
    };

    let disc_id = if let Some(ref toc_sectors) = toc {
        log::info!("AccurateRip: found TOC with {} entries: {:?}", toc_sectors.len(), toc_sectors);
        if toc_sectors.len() == n + 1 {
            compute_ar_disc_id_from_toc(toc_sectors)
        } else {
            log::warn!(
                "AccurateRip: TOC has {} entries but expected {} (tracks+leadout), falling back to sample counts",
                toc_sectors.len(), n + 1,
            );
            compute_ar_disc_id(sample_counts, sample_rate)
        }
    } else {
        compute_ar_disc_id(sample_counts, sample_rate)
    };
    let url = ar_url(&disc_id);
    let disc_id_str = format!(
        "{:08x}-{:08x}-{:08x}",
        disc_id.id1, disc_id.id2, disc_id.freedb_id,
    );

    // Fetch database.
    let db_response = match fetch_ar_data(&disc_id).await {
        Ok(Some(resp)) => resp,
        Ok(None) => {
            // Disc not in database.
            return ArVerifyResult {
                tracks: paths.iter().enumerate().map(|(i, p)| ArTrackResult {
                    path: p.clone(),
                    track_number: (i + 1) as u32,
                    status: ArTrackStatus::NoDiscInDatabase,
                    confidence: None,
                    offset: None,
                    crc_v1: 0,
                    crc_v2: 0,
                }).collect(),
                was_common_scan: !full_scan,
                disc_id_str,
                url,
            };
        }
        Err(e) => {
            return ArVerifyResult {
                tracks: paths.iter().enumerate().map(|(i, p)| ArTrackResult {
                    path: p.clone(),
                    track_number: (i + 1) as u32,
                    status: ArTrackStatus::Error(e.clone()),
                    confidence: None,
                    offset: None,
                    crc_v1: 0,
                    crc_v2: 0,
                }).collect(),
                was_common_scan: !full_scan,
                disc_id_str,
                url,
            };
        }
    };

    // Build offset list.
    let offsets: Vec<i32> = if full_scan {
        (-1200..=1200).collect()
    } else {
        COMMON_OFFSETS.to_vec()
    };

    let margin = if full_scan { MAX_OFFSET } else {
        // Margin must cover the largest absolute offset we'll try.
        COMMON_OFFSETS.iter().map(|o| o.unsigned_abs() as usize).max().unwrap_or(0)
    };

    // Decode all tracks in parallel. Decoding is CPU-bound (ffmpeg FFI),
    // so we spawn each track on the blocking thread pool. This is the
    // main bottleneck — parallelising it gives roughly Ncores× speedup.
    let pressings = db_response.pressings;

    let mut decode_handles = Vec::with_capacity(n);
    for (i, path) in paths.iter().enumerate() {
        let path_clone = path.clone();
        let margin_copy = margin;
        let handle = tokio::task::spawn_blocking(move || {
            (i, decode_track_to_dwords(&path_clone, margin_copy))
        });
        decode_handles.push(handle);
    }

    // Collect decoded results (preserving track order).
    let mut decoded_tracks: Vec<(usize, Result<(Vec<u32>, usize), String>)> = Vec::with_capacity(n);
    for handle in decode_handles {
        match handle.await {
            Ok(result) => decoded_tracks.push(result),
            Err(e) => {
                // JoinError — shouldn't happen, but handle gracefully.
                decoded_tracks.push((decoded_tracks.len(), Err(format!("decode task failed: {}", e))));
            }
        }
    }
    decoded_tracks.sort_by_key(|(i, _)| *i);

    // CRC computation and matching (fast, done sequentially).
    let mut results = Vec::with_capacity(n);
    for (i, decode_result) in decoded_tracks {
        let path = &paths[i];
        let is_first = i == 0;
        let is_last = i == n - 1;

        match decode_result {
            Err(e) => {
                results.push(ArTrackResult {
                    path: path.clone(),
                    track_number: (i + 1) as u32,
                    status: ArTrackStatus::Error(e),
                    confidence: None,
                    offset: None,
                    crc_v1: 0,
                    crc_v2: 0,
                });
            }
            Ok((full_dwords, track_len)) => {
                // For track 1: if the file includes a pre-gap (extra
                // frames at the start), trim them. The pre-gap samples
                // are not part of the AR CRC. We detect this by comparing
                // the file's decoded length against the TOC-derived track
                // duration. Only trim if the file is longer than expected.
                let (eff_margin, eff_track_len) = if is_first && track1_pregap_frames > 0 && track1_toc_duration_frames > 0 {
                    let pregap_dwords = track1_pregap_frames * SAMPLES_PER_SECTOR;
                    let toc_dwords = track1_toc_duration_frames * SAMPLES_PER_SECTOR;
                    let file_frames = track_len / SAMPLES_PER_SECTOR;
                    if file_frames > track1_toc_duration_frames {
                        // File includes pre-gap — trim it.
                        log::info!(
                            "AccurateRip: track 1 has {} frames, TOC says {}, trimming {} pre-gap frames",
                            file_frames, track1_toc_duration_frames, track1_pregap_frames,
                        );
                        (margin + pregap_dwords, toc_dwords)
                    } else {
                        // File matches or is shorter than TOC — no pre-gap to trim.
                        (margin, track_len)
                    }
                } else {
                    (margin, track_len)
                };

                // Compute CRCs at offset 0 for display.
                let (crc_v1_0, crc_v2_0) = compute_ar_crcs(
                    &full_dwords[eff_margin..eff_margin + eff_track_len],
                    is_first,
                    is_last,
                );

                // Try matching at all offsets.
                match try_offsets(
                    &full_dwords, eff_track_len, eff_margin,
                    is_first, is_last,
                    &pressings, i,
                    &offsets,
                ) {
                    Some((offset, confidence)) => {
                        results.push(ArTrackResult {
                            path: path.clone(),
                            track_number: (i + 1) as u32,
                            status: ArTrackStatus::Verified,
                            confidence: Some(confidence),
                            offset: Some(offset),
                            crc_v1: crc_v1_0,
                            crc_v2: crc_v2_0,
                        });
                    }
                    None => {
                        results.push(ArTrackResult {
                            path: path.clone(),
                            track_number: (i + 1) as u32,
                            status: ArTrackStatus::Mismatch,
                            confidence: None,
                            offset: None,
                            crc_v1: crc_v1_0,
                            crc_v2: crc_v2_0,
                        });
                    }
                }
            }
        }
    }

    ArVerifyResult {
        tracks: results,
        was_common_scan: !full_scan,
        disc_id_str,
        url,
    }
}

/// Summary string for display (e.g., "9/10 verified, confidence 14, offset +0").
pub fn format_summary(result: &ArVerifyResult) -> String {
    let total = result.tracks.len();
    let verified = result.tracks.iter()
        .filter(|t| t.status == ArTrackStatus::Verified)
        .count();

    if result.tracks.iter().any(|t| t.status == ArTrackStatus::NoDiscInDatabase) {
        return "Disc not in AccurateRip database".to_string();
    }

    if verified == 0 {
        return format!("0/{} tracks verified", total);
    }

    // Find the most common confidence and offset among verified tracks.
    let max_confidence = result.tracks.iter()
        .filter_map(|t| t.confidence)
        .max()
        .unwrap_or(0);

    let common_offset = result.tracks.iter()
        .filter_map(|t| t.offset)
        .next()
        .unwrap_or(0);

    let offset_str = if common_offset >= 0 {
        format!("+{}", common_offset)
    } else {
        format!("{}", common_offset)
    };

    format!(
        "{}/{} verified, confidence {}, offset {}",
        verified, total, max_confidence, offset_str,
    )
}

// ── Batch verification ──────────────────────────────────────────────

/// Batch-verify all albums under a directory tree.
///
/// Discovers albums by grouping audio files by parent directory,
/// verifies each album sequentially (to avoid hammering the AR server),
/// and returns a summary for all albums.
pub async fn batch_verify(
    scan_dir: &Path,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) -> Box<ArBatchResult> {
    // Discover all audio files recursively.
    let mut all_audio = super::browse::expand_paths_to_audio(&[scan_dir.to_path_buf()]);
    super::probe::sort_paths_by_track(&mut all_audio);

    if all_audio.is_empty() {
        return Box::new(ArBatchResult {
            albums: Vec::new(),
            scan_dir: scan_dir.to_path_buf(),
            report_path: None,
        });
    }

    // Group by parent directory (each directory = one album).
    let groups = super::gnudb::group_by_disc(&all_audio);
    let total_albums = groups.len();
    let mut albums: Vec<ArBatchAlbumResult> = Vec::with_capacity(total_albums);

    for (idx, (label, group_paths)) in groups.into_iter().enumerate() {
        let dir = group_paths[0].parent().unwrap_or(Path::new(".")).to_path_buf();
        let album_name = if !label.is_empty() {
            label.clone()
        } else {
            dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string()
        };

        let _ = tx.send(crate::tui::message::AppMessage::StatusMessage(
            format!("Batch AR: {}/{} — {}...", idx + 1, total_albums, album_name),
        )).await;

        // Check for single-image layout.
        let result = if let Some(info) = super::cue_parser::detect_single_image(&dir) {
            verify_single_image(&info, false).await
        } else {
            // Multi-file: collect sample counts and verify.
            match collect_sample_counts(&group_paths) {
                Ok((sample_counts, sample_rate)) => {
                    verify_album(&group_paths, &sample_counts, sample_rate, false).await
                }
                Err(e) => {
                    albums.push(ArBatchAlbumResult {
                        dir,
                        album_name,
                        total_tracks: group_paths.len(),
                        verified: 0,
                        mismatched: 0,
                        not_in_db: false,
                        confidence: None,
                        offset: None,
                        error: Some(e),
                    });
                    continue;
                }
            }
        };

        // Summarize this album.
        let verified = result.tracks.iter()
            .filter(|t| t.status == ArTrackStatus::Verified)
            .count();
        let mismatched = result.tracks.iter()
            .filter(|t| t.status == ArTrackStatus::Mismatch)
            .count();
        let not_in_db = result.tracks.iter()
            .any(|t| t.status == ArTrackStatus::NoDiscInDatabase);
        let max_conf = result.tracks.iter()
            .filter_map(|t| t.confidence)
            .max();
        let common_offset = result.tracks.iter()
            .filter_map(|t| t.offset)
            .next();

        albums.push(ArBatchAlbumResult {
            dir,
            album_name,
            total_tracks: result.tracks.len(),
            verified,
            mismatched,
            not_in_db,
            confidence: max_conf,
            offset: common_offset,
            error: None,
        });
    }

    // Generate report file.
    let report = format_batch_report(&albums, scan_dir);
    let report_path = scan_dir.join("accuraterip-report.txt");
    let report_path = match std::fs::write(&report_path, &report) {
        Ok(()) => Some(report_path),
        Err(e) => {
            log::warn!("Failed to write AR batch report: {}", e);
            None
        }
    };

    Box::new(ArBatchResult {
        albums,
        scan_dir: scan_dir.to_path_buf(),
        report_path,
    })
}

/// Format a text report from batch verification results.
fn format_batch_report(albums: &[ArBatchAlbumResult], scan_dir: &Path) -> String {
    let total = albums.len();
    let fully_verified = albums.iter().filter(|a| a.verified == a.total_tracks && a.total_tracks > 0 && !a.not_in_db).count();
    let partial = albums.iter().filter(|a| a.verified > 0 && a.verified < a.total_tracks && !a.not_in_db).count();
    let not_in_db = albums.iter().filter(|a| a.not_in_db).count();
    let mismatch_only = albums.iter().filter(|a| a.mismatched > 0 && a.verified == 0 && !a.not_in_db).count();
    let errors = albums.iter().filter(|a| a.error.is_some()).count();

    let mut out = String::new();
    out.push_str("AccurateRip Batch Verification Report\n");
    out.push_str(&format!("Generated: {}\n", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")));
    out.push_str(&format!("Directory: {}\n\n", scan_dir.display()));

    out.push_str(&format!("Summary:\n"));
    out.push_str(&format!("  Albums scanned:    {:>5}\n", total));
    out.push_str(&format!("  Fully verified:    {:>5} ({:.1}%)\n", fully_verified, fully_verified as f64 / total.max(1) as f64 * 100.0));
    out.push_str(&format!("  Partial match:     {:>5} ({:.1}%)\n", partial, partial as f64 / total.max(1) as f64 * 100.0));
    out.push_str(&format!("  Not in database:   {:>5} ({:.1}%)\n", not_in_db, not_in_db as f64 / total.max(1) as f64 * 100.0));
    out.push_str(&format!("  CRC mismatch:      {:>5} ({:.1}%)\n", mismatch_only, mismatch_only as f64 / total.max(1) as f64 * 100.0));
    if errors > 0 {
        out.push_str(&format!("  Errors:            {:>5}\n", errors));
    }
    out.push_str("\n");
    out.push_str(&"─".repeat(60));
    out.push_str("\n\n");

    for a in albums {
        let icon = if a.error.is_some() {
            "!"
        } else if a.not_in_db {
            "?"
        } else if a.verified == a.total_tracks && a.total_tracks > 0 {
            "✓"
        } else if a.mismatched > 0 {
            "✗"
        } else {
            "~"
        };

        out.push_str(&format!("{} {}\n", icon, a.album_name));
        if let Some(ref e) = a.error {
            out.push_str(&format!("  error: {}\n", e));
        } else if a.not_in_db {
            out.push_str("  Disc not in AccurateRip database\n");
        } else {
            let mut detail = format!("  {}/{} verified", a.verified, a.total_tracks);
            if let Some(conf) = a.confidence {
                detail.push_str(&format!(", confidence {}", conf));
            }
            if let Some(off) = a.offset {
                detail.push_str(&format!(", offset {:+}", off));
            }
            if a.mismatched > 0 {
                detail.push_str(&format!(", {} CRC mismatch", a.mismatched));
            }
            out.push_str(&detail);
            out.push('\n');
        }
        out.push('\n');
    }

    out
}

// ── Single-image verification ───────────────────────────────────────

/// Verify a single-image CUE album against AccurateRip.
///
/// Decodes the full image file once, slices by CUE track boundaries,
/// and computes per-track CRCs. Reuses the standard AR CRC algorithm
/// and database fetch.
pub async fn verify_single_image(
    info: &super::cue_parser::SingleImageInfo,
    full_scan: bool,
) -> ArVerifyResult {
    let n = info.track_boundaries.len();

    // Compute disc ID from the CUE's INDEX 01 timestamps.
    let disc_id = {
        let toc = find_toc_offsets(info.cue_path.parent().unwrap_or(Path::new(".")));
        if let Some(ref toc_sectors) = toc {
            if toc_sectors.len() == n + 1 {
                compute_ar_disc_id_from_toc(toc_sectors)
            } else {
                // Fallback: compute from track durations.
                let sample_counts: Vec<u64> = info.track_boundaries.iter()
                    .map(|&(_, count)| count)
                    .collect();
                compute_ar_disc_id(&sample_counts, info.sample_rate)
            }
        } else {
            let sample_counts: Vec<u64> = info.track_boundaries.iter()
                .map(|&(_, count)| count)
                .collect();
            compute_ar_disc_id(&sample_counts, info.sample_rate)
        }
    };
    let url = ar_url(&disc_id);
    let disc_id_str = format!(
        "{:08x}-{:08x}-{:08x}",
        disc_id.id1, disc_id.id2, disc_id.freedb_id,
    );

    // Fetch database.
    let db_response = match fetch_ar_data(&disc_id).await {
        Ok(Some(resp)) => resp,
        Ok(None) => {
            return ArVerifyResult {
                tracks: (0..n).map(|i| ArTrackResult {
                    path: info.audio_path.clone(),
                    track_number: (i + 1) as u32,
                    status: ArTrackStatus::NoDiscInDatabase,
                    confidence: None,
                    offset: None,
                    crc_v1: 0,
                    crc_v2: 0,
                }).collect(),
                was_common_scan: !full_scan,
                disc_id_str,
                url,
            };
        }
        Err(e) => {
            return ArVerifyResult {
                tracks: (0..n).map(|i| ArTrackResult {
                    path: info.audio_path.clone(),
                    track_number: (i + 1) as u32,
                    status: ArTrackStatus::Error(e.clone()),
                    confidence: None,
                    offset: None,
                    crc_v1: 0,
                    crc_v2: 0,
                }).collect(),
                was_common_scan: !full_scan,
                disc_id_str,
                url,
            };
        }
    };

    // Decode the full image to raw i16. Try ffmpeg first, fall back to
    // wvunpack for WavPack v4 files.
    let audio_path = info.audio_path.clone();
    let raw_result = tokio::task::spawn_blocking(move || {
        decode_track_to_raw_i16(&audio_path)
            .or_else(|_| decode_to_raw_i16_wvunpack(&audio_path))
    }).await;

    let raw_i16 = match raw_result {
        Ok(Ok(data)) => data,
        Ok(Err(e)) => {
            return ArVerifyResult {
                tracks: (0..n).map(|i| ArTrackResult {
                    path: info.audio_path.clone(),
                    track_number: (i + 1) as u32,
                    status: ArTrackStatus::Error(e.clone()),
                    confidence: None,
                    offset: None,
                    crc_v1: 0,
                    crc_v2: 0,
                }).collect(),
                was_common_scan: !full_scan,
                disc_id_str,
                url,
            };
        }
        Err(e) => {
            return ArVerifyResult {
                tracks: (0..n).map(|i| ArTrackResult {
                    path: info.audio_path.clone(),
                    track_number: (i + 1) as u32,
                    status: ArTrackStatus::Error(format!("decode task failed: {}", e)),
                    confidence: None,
                    offset: None,
                    crc_v1: 0,
                    crc_v2: 0,
                }).collect(),
                was_common_scan: !full_scan,
                disc_id_str,
                url,
            };
        }
    };

    // Convert full image to DWORDs.
    let total_dwords = raw_i16.len() / 2;
    let all_dwords: Vec<u32> = (0..total_dwords)
        .map(|i| {
            let l = raw_i16[i * 2] as u16;
            let r = raw_i16[i * 2 + 1] as u16;
            (l as u32) | ((r as u32) << 16)
        })
        .collect();
    drop(raw_i16); // free the i16 buffer

    // Build offset list and margin.
    let offsets: Vec<i32> = if full_scan {
        (-1200..=1200).collect()
    } else {
        COMMON_OFFSETS.to_vec()
    };
    let margin = if full_scan { MAX_OFFSET } else {
        COMMON_OFFSETS.iter().map(|o| o.unsigned_abs() as usize).max().unwrap_or(0)
    };

    // Verify each track by slicing the DWORDs buffer.
    let pressings = &db_response.pressings;
    let mut results = Vec::with_capacity(n);

    for (i, &(start_sample, sample_count)) in info.track_boundaries.iter().enumerate() {
        let is_first = i == 0;
        let is_last = i == n - 1;
        let start_dw = start_sample as usize;
        let count_dw = sample_count as usize;

        // Extend slice for offset scanning margin, using neighboring tracks'
        // audio as natural margin (since the image is one continuous buffer).
        let margin_start = start_dw.saturating_sub(margin);
        let margin_end = (start_dw + count_dw + margin).min(total_dwords);
        let eff_margin = start_dw - margin_start;

        let track_slice = &all_dwords[margin_start..margin_end];

        // CRC at offset 0 for display.
        let (crc_v1_0, crc_v2_0) = compute_ar_crcs(
            &track_slice[eff_margin..eff_margin + count_dw],
            is_first,
            is_last,
        );

        // Try matching at all offsets.
        match try_offsets(
            track_slice, count_dw, eff_margin,
            is_first, is_last,
            pressings, i,
            &offsets,
        ) {
            Some((offset, confidence)) => {
                results.push(ArTrackResult {
                    path: info.audio_path.clone(),
                    track_number: (i + 1) as u32,
                    status: ArTrackStatus::Verified,
                    confidence: Some(confidence),
                    offset: Some(offset),
                    crc_v1: crc_v1_0,
                    crc_v2: crc_v2_0,
                });
            }
            None => {
                results.push(ArTrackResult {
                    path: info.audio_path.clone(),
                    track_number: (i + 1) as u32,
                    status: ArTrackStatus::Mismatch,
                    confidence: None,
                    offset: None,
                    crc_v1: crc_v1_0,
                    crc_v2: crc_v2_0,
                });
            }
        }
    }

    ArVerifyResult {
        tracks: results,
        was_common_scan: !full_scan,
        disc_id_str,
        url,
    }
}

/// Decode a WavPack file to raw i16 via wvunpack (fallback for v4 files).
///
/// Runs `wvunpack -q -o - file.wv` which outputs raw PCM to stdout.
fn decode_to_raw_i16_wvunpack(path: &Path) -> Result<Vec<i16>, String> {
    let output = std::process::Command::new("wvunpack")
        .args(["-q", "-o", "-"])
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("wvunpack decode failed: {}", e))?;

    if !output.status.success() {
        return Err("wvunpack decode returned error".into());
    }

    // wvunpack -o - outputs WAV format (with header). We need to skip
    // the WAV header (44 bytes for standard PCM WAV) and read raw i16.
    let data = &output.stdout;
    if data.len() < 44 {
        return Err("wvunpack output too short".into());
    }

    // Find the "data" chunk in the WAV header.
    let data_offset = find_wav_data_offset(data).unwrap_or(44);
    let pcm_data = &data[data_offset..];

    // Reinterpret as i16 (little-endian, which is native on LE platforms).
    if pcm_data.len() % 2 != 0 {
        return Err("wvunpack output has odd byte count".into());
    }
    let samples: Vec<i16> = pcm_data.chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();

    Ok(samples)
}

/// Find the offset of the "data" chunk payload in a WAV file.
fn find_wav_data_offset(wav: &[u8]) -> Option<usize> {
    // Search for "data" marker followed by chunk size.
    let mut pos = 12; // skip RIFF header (12 bytes)
    while pos + 8 <= wav.len() {
        let chunk_id = &wav[pos..pos + 4];
        let chunk_size = u32::from_le_bytes([wav[pos+4], wav[pos+5], wav[pos+6], wav[pos+7]]) as usize;
        if chunk_id == b"data" {
            return Some(pos + 8); // payload starts after chunk header
        }
        pos += 8 + chunk_size;
    }
    None
}

// ── Offset correction ───────────────────────────────────────────────

/// Check if all tracks in a result verified at the same non-zero offset.
/// Returns `Some(offset)` if so, `None` otherwise.
pub fn detect_uniform_offset(result: &ArVerifyResult) -> Option<i32> {
    if result.tracks.is_empty() {
        return None;
    }
    let mut common_offset: Option<i32> = None;
    for t in &result.tracks {
        if t.status != ArTrackStatus::Verified {
            return None; // all must be verified
        }
        let off = t.offset?;
        if off == 0 {
            return None; // already at offset 0
        }
        if let Some(prev) = common_offset {
            if prev != off {
                return None; // mixed offsets
            }
        } else {
            common_offset = Some(off);
        }
    }
    common_offset
}

/// Apply offset correction to a set of tracks.
///
/// Decodes each track, shifts the audio by `-offset` samples (correcting
/// the drive read offset), re-encodes to FLAC, preserves metadata, then
/// verifies the corrected files at offset 0. Only replaces originals if
/// ALL tracks verify at offset 0.
///
/// Both positive and negative offsets are supported. Returns a summary
/// on success or an error message on failure.
pub async fn apply_offset_correction(
    paths: &[PathBuf],
    offset: i32,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) -> Result<String, String> {
    if offset == 0 {
        return Err("Offset is already 0 — no correction needed".into());
    }

    let n = paths.len();
    let abs_offset = offset.unsigned_abs() as usize;
    let sample_shift = abs_offset * 2; // i16 values (stereo pairs)

    // Create temp directory.
    let tmp_dir = std::env::temp_dir().join(format!("tonepoet-offset-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Failed to create temp dir: {}", e))?;

    // Run the pipeline; clean up temp dir regardless of outcome.
    let result = offset_correction_inner(paths, offset, abs_offset, sample_shift, n, &tmp_dir, &tx).await;

    // Always clean up temp dir.
    let _ = std::fs::remove_dir_all(&tmp_dir);

    result
}

/// Inner pipeline for offset correction. Separated so the caller can
/// always clean up the temp directory regardless of success or failure.
async fn offset_correction_inner(
    paths: &[PathBuf],
    offset: i32,
    abs_offset: usize,
    sample_shift: usize,
    n: usize,
    tmp_dir: &Path,
    tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) -> Result<String, String> {
    // Check required tools for the input format.
    let format_ext = paths[0].extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match format_ext.as_str() {
        "flac" => {
            if !tool_exists("metaflac") {
                return Err("metaflac not found — required for FLAC metadata".into());
            }
        }
        "ape" => {
            if !tool_exists("mac") {
                return Err("mac (Monkey's Audio) not found — required for APE encoding".into());
            }
        }
        "m4a" | "mp4" | "alac" | "wav" | "wave" | "aiff" | "aif" | "wv" => {} // ffmpeg handles these
        other => {
            return Err(format!("Offset correction not supported for .{} files", other));
        }
    }

    // Copy log + CUE file(s) to temp dir for TOC lookup during verification.
    let orig_dir = paths[0].parent().unwrap_or(Path::new("."));
    if let Ok(entries) = std::fs::read_dir(orig_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if ext == "log" || ext == "cue" {
                let dest = tmp_dir.join(p.file_name().unwrap());
                let _ = std::fs::copy(&p, &dest);
            }
        }
    }

    // Decode all tracks first (needed for both shift directions).
    let mut all_raw: Vec<Vec<i16>> = Vec::with_capacity(n);
    for (i, path) in paths.iter().enumerate() {
        let _ = tx.send(crate::tui::message::AppMessage::StatusMessage(
            format!("Offset correction: decoding track {}/{}...", i + 1, n),
        )).await;

        let path_clone = path.clone();
        let raw = tokio::task::spawn_blocking(move || {
            decode_track_to_raw_i16(&path_clone)
        }).await
            .map_err(|e| format!("decode task failed: {}", e))?
            .map_err(|e| format!("decode failed for {}: {}", path.display(), e))?;

        if raw.len() < sample_shift {
            return Err(format!(
                "Track {} too short ({} samples) for offset correction of {} samples",
                i + 1, raw.len() / 2, abs_offset,
            ));
        }
        all_raw.push(raw);
    }

    // Build corrected tracks.
    //
    // Positive AR offset: audio is shifted LEFT (drive read too early).
    //   Correction: shift RIGHT. Each track: prepend overflow from previous
    //   (silence for track 1), drop last N samples (they flow to next track).
    //
    // Negative AR offset: audio is shifted RIGHT (drive read too late).
    //   Correction: shift LEFT. Each track: drop first N samples (they
    //   belong to previous track), append first N of next track (silence
    //   for last track).
    for (i, raw) in all_raw.iter().enumerate() {
        let corrected = if offset > 0 {
            // Shift RIGHT: prepend from previous, drop end.
            let prefix = if i == 0 {
                vec![0i16; sample_shift] // silence before disc
            } else {
                let prev = &all_raw[i - 1];
                prev[prev.len() - sample_shift..].to_vec()
            };
            let mut c = Vec::with_capacity(raw.len());
            c.extend_from_slice(&prefix);
            c.extend_from_slice(&raw[..raw.len() - sample_shift]);
            c
        } else {
            // Shift LEFT: drop start, append from next.
            let suffix = if i + 1 < n {
                all_raw[i + 1][..sample_shift].to_vec()
            } else {
                vec![0i16; sample_shift] // silence after disc
            };
            let mut c = Vec::with_capacity(raw.len());
            c.extend_from_slice(&raw[sample_shift..]);
            c.extend_from_slice(&suffix);
            c
        };

        // Sanity check: corrected must have same length as original.
        if corrected.len() != raw.len() {
            return Err(format!(
                "Internal error: corrected track {} has {} samples, expected {}",
                i + 1, corrected.len() / 2, raw.len() / 2,
            ));
        }

        let path = &paths[i];
        let out_name = path.file_name().unwrap_or_default();
        let out_path = tmp_dir.join(out_name);

        let _ = tx.send(crate::tui::message::AppMessage::StatusMessage(
            format!("Offset correction: encoding track {}/{}...", i + 1, n),
        )).await;

        encode_corrected_track(&corrected, &out_path, path).await?;
        copy_metadata(path, &out_path).await?;
    }

    // 4. Verify corrected files at offset 0.
    let _ = tx.send(crate::tui::message::AppMessage::StatusMessage(
        "Offset correction: verifying corrected files...".into(),
    )).await;

    let corrected_paths: Vec<PathBuf> = paths.iter()
        .map(|p| tmp_dir.join(p.file_name().unwrap_or_default()))
        .collect();

    let (sample_counts, sample_rate) = collect_sample_counts(&corrected_paths)?;
    let verify_result = verify_album(&corrected_paths, &sample_counts, sample_rate, false).await;

    // Check all tracks verified at offset 0.
    for (i, t) in verify_result.tracks.iter().enumerate() {
        if t.status != ArTrackStatus::Verified {
            return Err(format!(
                "Verification failed: track {} — {}. Originals unchanged.",
                i + 1,
                match &t.status {
                    ArTrackStatus::Mismatch => "CRC mismatch at offset 0".to_string(),
                    ArTrackStatus::NoDiscInDatabase => "disc not in database".to_string(),
                    ArTrackStatus::Error(e) => format!("error: {}", e),
                    ArTrackStatus::Verified => unreachable!(),
                },
            ));
        }
        if t.offset != Some(0) {
            return Err(format!(
                "Verification failed: track {} verified at offset {:+}, expected +0. Originals unchanged.",
                i + 1, t.offset.unwrap_or(-999),
            ));
        }
    }

    // 5. All verified at offset 0 — replace originals.
    //
    // Safety: back up each original to .bak BEFORE overwriting. If any
    // copy fails mid-loop, restore all .bak files so the album is never
    // left in a partially-corrected state.
    let _ = tx.send(crate::tui::message::AppMessage::StatusMessage(
        "Offset correction: replacing originals...".into(),
    )).await;

    // Phase A: create backups.
    let mut backed_up: Vec<(PathBuf, PathBuf)> = Vec::new(); // (original, backup)
    for orig in paths.iter() {
        let bak = orig.with_extension(format!(
            "{}.bak",
            orig.extension().and_then(|e| e.to_str()).unwrap_or("flac"),
        ));
        if let Err(e) = std::fs::rename(orig, &bak) {
            // Restore any backups we already made.
            for (o, b) in &backed_up {
                let _ = std::fs::rename(b, o);
            }
            return Err(format!("Failed to back up {}: {}", orig.display(), e));
        }
        backed_up.push((orig.clone(), bak));
    }

    // Phase B: copy corrected files over originals.
    for (i, (orig, _bak)) in backed_up.iter().enumerate() {
        let corrected = &corrected_paths[i];
        if let Err(e) = std::fs::copy(corrected, orig) {
            // Restore ALL backups — undo everything.
            for (o, b) in &backed_up {
                let _ = std::fs::rename(b, o);
            }
            return Err(format!("Failed to write corrected {}: {}. All originals restored.", orig.display(), e));
        }
    }

    // Phase C: remove backups (all copies succeeded).
    for (_orig, bak) in &backed_up {
        let _ = std::fs::remove_file(bak);
    }

    Ok(format!(
        "Offset corrected: {} tracks shifted by {:+} samples, all verified at offset +0",
        n, -offset,
    ))
}

/// Check if a command exists on the PATH.
fn tool_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Encode corrected PCM to the target format, detected from src_path extension.
async fn encode_corrected_track(
    corrected: &[i16],
    out_path: &Path,
    src_path: &Path,
) -> Result<(), String> {
    use tokio::process::Command as TokioCommand;

    let raw_bytes: Vec<u8> = corrected.iter()
        .flat_map(|&s| s.to_le_bytes())
        .collect();

    let ext = src_path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "ape" => {
            // APE: mac doesn't support stdin. Write temp WAV, then encode.
            let tmp_wav = out_path.with_extension("tmp.wav");
            encode_via_ffmpeg(&raw_bytes, &tmp_wav, &["-c:a", "pcm_s16le"]).await?;
            let status = TokioCommand::new("mac")
                .arg(&tmp_wav)
                .arg(out_path)
                .arg("-c2000") // compression level: normal
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .status()
                .await
                .map_err(|e| format!("Failed to run mac: {}", e))?;
            let _ = std::fs::remove_file(&tmp_wav);
            if !status.success() {
                return Err(format!("mac encode failed for {}", out_path.display()));
            }
            Ok(())
        }
        "m4a" | "mp4" | "alac" => encode_via_ffmpeg(&raw_bytes, out_path, &["-c:a", "alac"]).await,
        "wv" => encode_via_ffmpeg(&raw_bytes, out_path, &["-c:a", "wavpack"]).await,
        "wav" | "wave" => encode_via_ffmpeg(&raw_bytes, out_path, &["-c:a", "pcm_s16le"]).await,
        "aiff" | "aif" => encode_via_ffmpeg(&raw_bytes, out_path, &["-c:a", "pcm_s16be"]).await,
        _ => encode_via_ffmpeg(&raw_bytes, out_path, &["-compression_level", "8"]).await, // FLAC default
    }
}

/// Encode raw PCM bytes to a file via ffmpeg with format-specific codec args.
async fn encode_via_ffmpeg(
    raw_bytes: &[u8],
    out_path: &Path,
    codec_args: &[&str],
) -> Result<(), String> {
    use tokio::process::Command as TokioCommand;

    let mut cmd = TokioCommand::new("ffmpeg");
    cmd.args(["-y", "-hide_banner", "-loglevel", "error",
              "-f", "s16le", "-ar", "44100", "-ac", "2",
              "-i", "pipe:0"]);
    for arg in codec_args {
        cmd.arg(arg);
    }
    cmd.arg(out_path);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()
        .map_err(|e| format!("Failed to spawn ffmpeg: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(raw_bytes).await
            .map_err(|e| format!("Failed to write to ffmpeg stdin: {}", e))?;
        drop(stdin);
    }

    let output = child.wait_with_output().await
        .map_err(|e| format!("ffmpeg failed: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg encode failed: {}", stderr));
    }
    Ok(())
}

/// Copy metadata from source to destination, format-aware.
async fn copy_metadata(src: &Path, dst: &Path) -> Result<(), String> {
    let ext = src.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "flac" => copy_metadata_metaflac(src, dst).await,
        "m4a" | "mp4" | "alac" | "wv" | "ape" => {
            let src = src.to_path_buf();
            let dst = dst.to_path_buf();
            tokio::task::spawn_blocking(move || copy_tags_via_lofty(&src, &dst))
                .await
                .map_err(|e| format!("metadata task failed: {}", e))?
        }
        _ => Ok(()), // WAV, AIFF — no metadata to copy
    }
}

/// Copy FLAC metadata via metaflac (tags + embedded pictures).
async fn copy_metadata_metaflac(src: &Path, dst: &Path) -> Result<(), String> {
    use tokio::process::Command as TokioCommand;

    // Export and import tags.
    let export = TokioCommand::new("metaflac")
        .arg("--export-tags-to=-")
        .arg(src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| format!("metaflac export failed: {}", e))?;

    if export.status.success() && !export.stdout.is_empty() {
        let mut child = TokioCommand::new("metaflac")
            .arg("--import-tags-from=-")
            .arg(dst)
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("metaflac import spawn failed: {}", e))?;

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(&export.stdout).await
                .map_err(|e| format!("metaflac import write failed: {}", e))?;
            drop(stdin);
        }
        let output = child.wait_with_output().await
            .map_err(|e| format!("metaflac import failed: {}", e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("metaflac tag import failed for {}: {}", dst.display(), stderr));
        }
    }

    // Export and import embedded picture (if any).
    let tmp_pic = std::env::temp_dir().join(format!(
        "tonepoet-pic-{}-{}.bin",
        std::process::id(),
        src.file_name().unwrap_or_default().to_string_lossy(),
    ));
    let pic_export = TokioCommand::new("metaflac")
        .arg(format!("--export-picture-to={}", tmp_pic.display()))
        .arg(src)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    if let Ok(status) = pic_export {
        if status.success() && tmp_pic.exists() {
            let _ = TokioCommand::new("metaflac")
                .arg(format!("--import-picture-from={}", tmp_pic.display()))
                .arg(dst)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;
            let _ = std::fs::remove_file(&tmp_pic);
        }
    }

    Ok(())
}

/// Copy tags and pictures via lofty (for WavPack and APE).
fn copy_tags_via_lofty(src: &Path, dst: &Path) -> Result<(), String> {
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::config::WriteOptions;

    let src_tagged = lofty::read_from_path(src)
        .map_err(|e| format!("Failed to read tags from {}: {}", src.display(), e))?;

    let src_tag = match src_tagged.primary_tag() {
        Some(t) => t,
        None => return Ok(()), // no tags to copy
    };

    let mut dst_tagged = lofty::read_from_path(dst)
        .map_err(|e| format!("Failed to read {}: {}", dst.display(), e))?;

    // Get or create the primary tag on the destination.
    let tag_type = src_tag.tag_type();
    if dst_tagged.tag(tag_type).is_none() {
        dst_tagged.insert_tag(lofty::tag::Tag::new(tag_type));
    }

    if let Some(dst_tag) = dst_tagged.tag_mut(tag_type) {
        // Remove existing items that we'll replace, then copy from source.
        let keys: Vec<_> = src_tag.items().map(|item| item.key().clone()).collect();
        for key in &keys {
            dst_tag.remove_key(key);
        }
        for item in src_tag.items() {
            dst_tag.push(item.clone());
        }
        for pic in src_tag.pictures() {
            dst_tag.push_picture(pic.clone());
        }
    }

    dst_tagged.save_to_path(dst, WriteOptions::default())
        .map_err(|e| format!("Failed to write tags to {}: {}", dst.display(), e))?;

    Ok(())
}
