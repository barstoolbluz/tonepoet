//! CUE sheet parser for metadata import and bulk rename.
//!
//! Extracts album/track metadata (title, performer, date, genre) and
//! file references from a CUE sheet. Handles both single-image (one
//! FILE + many TRACKs) and track-by-track (one FILE per TRACK) layouts.

use std::path::{Path, PathBuf};

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
}

/// Parse a CUE sheet from a file path.
pub fn parse_cue_file(path: &Path) -> Result<CueSheet, String> {
    let raw = std::fs::read(path)
        .map_err(|e| format!("failed to read CUE file: {}", e))?;
    let content = String::from_utf8_lossy(&raw);
    Ok(parse_cue(&content))
}

/// Parse CUE sheet content from a string.
pub fn parse_cue(content: &str) -> CueSheet {
    let mut sheet = CueSheet::default();

    // State: are we inside a TRACK block?
    let mut current_track: Option<CueTrack> = None;
    // The most recent FILE line (used for both single-image and per-track).
    let mut current_file: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        // FILE "filename.ext" WAVE
        if let Some(file) = parse_file_line(trimmed) {
            current_file = Some(file);
            // If we're inside a track that doesn't have a file yet,
            // associate it. Otherwise it'll be picked up by the next TRACK.
            if let Some(ref mut track) = current_track {
                if track.file.is_none() {
                    track.file = current_file.clone();
                }
            }
            continue;
        }

        // TRACK NN AUDIO
        if let Some(num) = parse_track_line(trimmed) {
            // Commit the previous track (if any).
            if let Some(track) = current_track.take() {
                sheet.tracks.push(track);
            }
            current_track = Some(CueTrack {
                number: num,
                file: current_file.clone(),
                ..Default::default()
            });
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
            if let Some(val) = parse_rem_field(trimmed, "DATE")
                .or_else(|| parse_rem_field(trimmed, "YEAR"))
            {
                sheet.date = Some(val);
                continue;
            }
            if let Some(val) = parse_rem_field(trimmed, "GENRE") {
                sheet.genre = Some(val);
                continue;
            }
        }

        // INDEX 01 MM:SS:FF (track start position).
        if let Some(ref mut track) = current_track {
            if trimmed.starts_with("INDEX 01 ") {
                let ts = trimmed[9..].trim();
                track.index01_frames = parse_cue_timestamp(ts);
                continue;
            }
        }

        // Other lines (INDEX 00, FLAGS, etc.) are ignored.
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
    let line = line.strip_prefix("FILE")?;
    let line = line.trim_start();
    extract_quoted(line)
}

/// Parse a `TRACK NN AUDIO` line, returning the track number.
fn parse_track_line(line: &str) -> Option<u32> {
    let rest = line.strip_prefix("TRACK")?.trim_start();
    let num_end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..num_end].parse().ok()
}

/// Parse a line like `TITLE "Some Title"` or `PERFORMER "Name"`.
fn parse_quoted_field(line: &str, keyword: &str) -> Option<String> {
    let rest = line.strip_prefix(keyword)?.trim_start();
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
    let rest = line.strip_prefix("REM")?.trim_start();
    let rest = rest.strip_prefix(field)?.trim_start();
    // Handle both quoted and unquoted values.
    if rest.starts_with('"') {
        extract_quoted(rest)
    } else {
        let val = rest.trim();
        if val.is_empty() { None } else { Some(val.to_string()) }
    }
}

/// Parse a CUE "MM:SS:FF" timestamp to a frame count (75 frames/second).
pub fn parse_cue_timestamp(ts: &str) -> Option<u32> {
    let parts: Vec<&str> = ts.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let mm: u32 = parts[0].parse().ok()?;
    let ss: u32 = parts[1].parse().ok()?;
    let ff: u32 = parts[2].parse().ok()?;
    Some(mm * 60 * 75 + ss * 75 + ff)
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
    let all_same_file = sheet.tracks.iter()
        .all(|t| t.file.as_ref() == Some(first_file));
    if !all_same_file {
        return None;
    }

    // Resolve the audio file (handles extension mismatches).
    let audio_path = crate::tui::accuraterip::resolve_cue_file_reference(dir, first_file)?;

    // Probe for sample rate and total samples.
    let (total_samples, sample_rate) = crate::tui::accuraterip::probe_sample_count(&audio_path).ok()?;
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
                "-y", "-hide_banner", "-loglevel", "error",
                "-ss", &format!("{:.6}", start_secs),
                "-t", &format!("{:.6}", duration_secs),
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
        assert_eq!(parse_track_line("TRACK 12 AUDIO"), Some(12));
        assert_eq!(parse_track_line("TRACK 1 AUDIO"), Some(1));
        assert_eq!(parse_track_line("  TRACK 05 AUDIO"), None); // leading whitespace stripped by caller
    }

    #[test]
    fn quoted_extraction() {
        assert_eq!(extract_quoted("\"hello world\" extra"), Some("hello world".to_string()));
        assert_eq!(extract_quoted("no quotes"), None);
        assert_eq!(extract_quoted("\"\""), Some("".to_string()));
    }
}
