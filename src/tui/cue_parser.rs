//! Single-image CUE detection for the TUI.
//!
//! The CUE parsing core lives in `crate::convert::cue_parser` (re-exported
//! here for existing call sites). This module keeps the detection helpers
//! that are coupled to TUI services: locating a CUE in a directory
//! (`gnudb`), resolving file references and probing sample counts
//! (`accuraterip`).
pub use crate::convert::cue_parser::*;

use std::path::{Path, PathBuf};


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

/// Locate the sidecar CUE that actually references `audio_path` as a
/// single-image album.
///
/// Unlike `find_sidecar_cue`, this is deliberately not lexicographic. It parses
/// every `.cue` in the audio file's directory, keeps only multi-track CUEs whose
/// audio tracks all share one FILE reference, resolves that reference with the
/// same extension-mismatch semantics used elsewhere in the TUI, and returns the
/// CUE only when exactly one candidate resolves to the edited audio file. This
/// prevents metadata save write-back from touching an unrelated or ambiguous CUE
/// in a multi-CUE directory.
pub fn find_sidecar_cue_for_audio_image(audio_path: &Path) -> Option<PathBuf> {
    let parent = audio_path.parent()?;
    let entries = std::fs::read_dir(parent).ok()?;
    let mut cues: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("cue"))
                .unwrap_or(false)
        })
        .collect();
    cues.sort();

    let matches: Vec<PathBuf> = cues
        .into_iter()
        .filter(|cue_path| {
            let Ok(sheet) = parse_cue_file(cue_path) else {
                return false;
            };
            if sheet.tracks.len() < 2
                || !sheet.tracks.iter().all(|track| track.index01_frames.is_some())
            {
                return false;
            }
            let Some(first_file) = sheet.tracks.first().and_then(|track| track.file.as_deref()) else {
                return false;
            };
            if !sheet
                .tracks
                .iter()
                .all(|track| track.file.as_deref() == Some(first_file))
            {
                return false;
            }
            let Some(resolved) = crate::tui::accuraterip::resolve_cue_file_reference(parent, first_file) else {
                return false;
            };
            paths_refer_to_same_file(&resolved, audio_path)
        })
        .collect();

    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

fn paths_refer_to_same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => absolutize_lossy(a) == absolutize_lossy(b),
    }
}

fn absolutize_lossy(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

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
