//! Standalone CUE sheet generation from tagged audio files in the browse view.
//! Separate from the conversion pipeline's generator in tonepoet-features.

use std::path::Path;
use std::time::Duration;

/// Album-level metadata for CUE header.
pub struct CueAlbumInfo {
    pub title: String,
    pub artist: String,
    pub year: Option<String>,
    pub genre: Option<String>,
}

/// Per-track metadata for a CUE entry.
pub struct CueTrackInfo {
    pub filename: String,
    pub title: String,
    pub artist: String,
    pub track_number: u32,
    pub duration: Duration,
    pub format_tag: String,
    /// Pregap length in CD frames (75/sec), populated from a colocated EAC
    /// `.log`. `None` means no pregap data is available; `Some(n)` means
    /// the noncompliant CUE form should emit `INDEX 00` in the previous
    /// FILE block at offset `(prev_track_length - n)`.
    pub pregap_frames: Option<u32>,
}

/// Generate a multi-file CUE sheet.
///
/// When a track N≥2 carries `pregap_frames`, the EAC default "append pregap
/// to previous track" convention applies: the pregap audio physically lives
/// at the end of track N−1's FILE, so we emit track N's `TRACK …` and
/// `INDEX 00` lines inside track N−1's FILE block at offset
/// `(prev_track_length − pregap)`. This is the "noncompliant" CUE form
/// (multiple TRACK declarations per FILE) that EAC and CUETools both expect
/// when a log is present. Without pregap data we emit the simpler one-track-
/// per-FILE form with `INDEX 01 00:00:00`.
pub fn generate_multifile_cue(album: &CueAlbumInfo, tracks: &[CueTrackInfo]) -> String {
    let mut cue = String::new();
    write_header(&mut cue, album);

    for (i, t) in tracks.iter().enumerate() {
        cue.push_str(&format!(
            "FILE \"{}\" {}\n",
            escape(&t.filename),
            t.format_tag,
        ));

        // Track 1's TRACK header always opens a fresh FILE; subsequent tracks
        // open the FILE with their own INDEX 01 unless their TRACK header was
        // already emitted in the previous FILE (because of pregap).
        let track_header_already_emitted = i > 0
            && tracks[i].pregap_frames.is_some_and(|p| {
                let prev = duration_to_frames(&tracks[i - 1].duration);
                p > 0 && p < prev
            });

        if !track_header_already_emitted {
            cue.push_str(&format!("  TRACK {:02} AUDIO\n", t.track_number));
            cue.push_str(&format!("    TITLE \"{}\"\n", escape(&t.title)));
            cue.push_str(&format!("    PERFORMER \"{}\"\n", escape(&t.artist)));
        }
        cue.push_str("    INDEX 01 00:00:00\n");

        // If the *next* track has a pregap that fits inside this FILE,
        // emit its TRACK + INDEX 00 here.
        if let Some(next) = tracks.get(i + 1) {
            if let Some(pregap) = next.pregap_frames {
                let this_frames = duration_to_frames(&t.duration);
                if pregap > 0 && pregap < this_frames {
                    let index00 = this_frames - pregap;
                    cue.push_str(&format!("  TRACK {:02} AUDIO\n", next.track_number));
                    cue.push_str(&format!("    TITLE \"{}\"\n", escape(&next.title)));
                    cue.push_str(&format!("    PERFORMER \"{}\"\n", escape(&next.artist)));
                    cue.push_str(&format!(
                        "    INDEX 00 {}\n",
                        frames_to_cue_timestamp(index00)
                    ));
                }
            }
        }
    }

    cue
}

/// Generate a single-image CUE sheet (one FILE, cumulative timestamps).
pub fn generate_single_image_cue(
    album: &CueAlbumInfo,
    tracks: &[CueTrackInfo],
    image_filename: &str,
    image_format_tag: &str,
) -> String {
    let mut cue = String::new();
    write_header(&mut cue, album);

    cue.push_str(&format!(
        "FILE \"{}\" {}\n",
        escape(image_filename),
        image_format_tag,
    ));

    let mut cumulative = Duration::ZERO;
    for (i, t) in tracks.iter().enumerate() {
        cue.push_str(&format!("  TRACK {:02} AUDIO\n", t.track_number));
        cue.push_str(&format!("    TITLE \"{}\"\n", escape(&t.title)));
        cue.push_str(&format!("    PERFORMER \"{}\"\n", escape(&t.artist)));

        let cumulative_frames = duration_to_frames(&cumulative);
        if i > 0 {
            if let Some(pregap) = t.pregap_frames {
                if pregap > 0 && pregap <= cumulative_frames {
                    let index00 = cumulative_frames - pregap;
                    cue.push_str(&format!(
                        "    INDEX 00 {}\n",
                        frames_to_cue_timestamp(index00)
                    ));
                }
            }
        }
        cue.push_str(&format!("    INDEX 01 {}\n", format_timestamp(&cumulative)));
        cumulative += t.duration;
    }

    cue
}

/// Map a file extension to the CUE FILE type keyword.
pub fn cue_format_tag(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "flac" => "FLAC",
        "mp3" => "MP3",
        "opus" => "OPUS",
        "m4a" | "aac" => "AAC",
        "wav" => "WAVE",
        "aiff" | "aif" => "AIFF",
        "wv" => "WAVPACK",
        _ => "WAVE",
    }
}

/// Derive a sanitised CUE filename from album metadata.
pub fn cue_output_filename(album: &CueAlbumInfo) -> String {
    let base = if album.artist.is_empty()
        || album.artist == "Unknown Artist"
        || album.artist == "Various Artists"
    {
        album.title.clone()
    } else {
        format!("{} - {}", album.artist, album.title)
    };
    let sanitised: String = base
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    format!("{}.cue", sanitised)
}

// ── internal helpers ────────────────────────────────────────────────

fn write_header(cue: &mut String, album: &CueAlbumInfo) {
    if let Some(genre) = &album.genre {
        cue.push_str(&format!("REM GENRE \"{}\"\n", escape(genre)));
    }
    if let Some(year) = &album.year {
        cue.push_str(&format!("REM DATE \"{}\"\n", escape(year)));
    }
    cue.push_str("REM COMMENT \"Generated by tonepoet\"\n");
    cue.push_str(&format!("PERFORMER \"{}\"\n", escape(&album.artist)));
    cue.push_str(&format!("TITLE \"{}\"\n", escape(&album.title)));
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn format_timestamp(d: &Duration) -> String {
    let total_secs = d.as_secs();
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    let frames = (d.subsec_millis() * 75) / 1000;
    format!("{:02}:{:02}:{:02}", minutes, seconds, frames)
}

/// Convert a CD frame count (75 frames/sec) to a CUE `MM:SS:FF` timestamp.
pub fn frames_to_cue_timestamp(frames: u32) -> String {
    let minutes = frames / (75 * 60);
    let seconds = (frames / 75) % 60;
    let frame_remainder = frames % 75;
    format!("{:02}:{:02}:{:02}", minutes, seconds, frame_remainder)
}

/// Convert a Duration to a CD frame count (75 frames/sec, rounded).
fn duration_to_frames(d: &Duration) -> u32 {
    (d.as_secs_f64() * 75.0).round() as u32
}

/// Build album + track info by probing files and reading tags.
/// Falls back to filename parsing when tags are missing.
/// `cue_dir` is the directory where the CUE file will be written;
/// FILE references are made relative to it.
pub fn gather_cue_info(
    paths: &[std::path::PathBuf],
    cue_dir: &Path,
) -> Result<(CueAlbumInfo, Vec<CueTrackInfo>), String> {
    if paths.is_empty() {
        return Err("No audio files".into());
    }

    let mut tracks = Vec::with_capacity(paths.len());

    // Read metadata + duration for each file.
    let mut album_title: Option<String> = None;
    let mut album_artist: Option<String> = None;
    let mut album_year: Option<String> = None;
    let mut album_genre: Option<String> = None;

    for path in paths {
        let meta = super::probe::read_metadata(path).ok();
        let info = super::probe::probe_audio(path).ok();

        let duration = info
            .as_ref()
            .map(|i| Duration::from_secs_f64(i.duration_secs))
            .unwrap_or(Duration::ZERO);

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("wav");
        let format_tag = cue_format_tag(ext).to_string();

        // FILE reference: relative to CUE output directory.
        let filename = path
            .strip_prefix(cue_dir)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        // Track number: prefer tag, fall back to filename.
        let track_number = meta
            .as_ref()
            .and_then(|m| m.track_number)
            .unwrap_or_else(|| super::probe::extract_track_from_filename(stem));

        // Title: prefer tag, fall back to filename parsing.
        let title = meta
            .as_ref()
            .and_then(|m| m.title.clone())
            .unwrap_or_else(|| {
                let (_, parsed) = super::probe::parse_title_from_filename(stem);
                parsed.unwrap_or_else(|| stem.to_string())
            });

        // Artist: prefer tag.
        let artist = meta
            .as_ref()
            .and_then(|m| m.artist.clone())
            .unwrap_or_default();

        // Collect album-level info from first file that has it.
        if album_title.is_none() {
            album_title = meta.as_ref().and_then(|m| m.album.clone());
        }
        if album_artist.is_none() {
            album_artist = meta.as_ref().and_then(|m| m.artist.clone());
        }
        if album_year.is_none() {
            album_year = meta.as_ref().and_then(|m| m.year.clone());
        }
        if album_genre.is_none() {
            album_genre = meta.as_ref().and_then(|m| m.genre.clone());
        }

        tracks.push(CueTrackInfo {
            filename,
            title,
            artist,
            track_number,
            duration,
            format_tag,
            pregap_frames: None,
        });
    }

    // If a colocated EAC log carries pregap data, annotate each track. We
    // index by track_number rather than by position in the input so
    // out-of-order paths still get the right pregap.
    if let Some(log_path) = super::accuraterip::find_eac_log(cue_dir) {
        if let Some(pregaps) = super::accuraterip::parse_eac_log_pregaps(&log_path) {
            for track in tracks.iter_mut() {
                if let Some(idx) = (track.track_number as usize).checked_sub(1) {
                    if let Some(Some(frames)) = pregaps.get(idx) {
                        track.pregap_frames = Some(*frames);
                    }
                }
            }
        }
    }

    // Fall back to CUE output directory name for album info.
    let dir_name = cue_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Unknown Album");

    let album = CueAlbumInfo {
        title: album_title.unwrap_or_else(|| dir_name.to_string()),
        artist: album_artist.unwrap_or_else(|| "Unknown Artist".to_string()),
        year: album_year,
        genre: album_genre,
    };

    Ok((album, tracks))
}

/// Derive the hypothetical single-image filename from album metadata and
/// the extension of the first source file.
pub fn derive_image_filename(album: &CueAlbumInfo, first_path: &Path) -> String {
    let ext = first_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("flac");
    let base = if album.artist.is_empty()
        || album.artist == "Unknown Artist"
        || album.artist == "Various Artists"
    {
        album.title.clone()
    } else {
        format!("{} - {}", album.artist, album.title)
    };
    let sanitised: String = base
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    format!("{}.{}", sanitised, ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(num: u32, duration: Duration, pregap: Option<u32>) -> CueTrackInfo {
        CueTrackInfo {
            filename: format!("{:02} - Track.flac", num),
            title: format!("Track {}", num),
            artist: "Artist".to_string(),
            track_number: num,
            duration,
            format_tag: "FLAC".to_string(),
            pregap_frames: pregap,
        }
    }

    fn album() -> CueAlbumInfo {
        CueAlbumInfo {
            title: "Album".to_string(),
            artist: "Artist".to_string(),
            year: None,
            genre: None,
        }
    }

    #[test]
    fn cue_timestamp_formatting() {
        assert_eq!(frames_to_cue_timestamp(0), "00:00:00");
        assert_eq!(frames_to_cue_timestamp(75), "00:01:00");
        assert_eq!(frames_to_cue_timestamp(150), "00:02:00");
        assert_eq!(frames_to_cue_timestamp(75 * 60), "01:00:00");
        // 3:43:37 = 3*60*75 + 43*75 + 37 = 13500 + 3225 + 37 = 16762
        assert_eq!(frames_to_cue_timestamp(16762), "03:43:37");
    }

    #[test]
    fn multifile_cue_emits_noncompliant_index00_for_pregap() {
        // Track 1 = 4 minutes (18000 frames). Track 2 has 75-frame pregap (1 sec).
        // INDEX 00 should be at track 1 frame (18000-75) = 17925 = 03:59:00.
        let tracks = vec![
            track(1, Duration::from_secs(240), None),
            track(2, Duration::from_secs(180), Some(75)),
        ];
        let cue = generate_multifile_cue(&album(), &tracks);

        assert!(cue.contains("FILE \"01 - Track.flac\" FLAC"));
        assert!(cue.contains("  TRACK 01 AUDIO"));
        assert!(cue.contains("    INDEX 01 00:00:00"));
        // Track 02's TRACK declaration appears inside file 01's block.
        assert!(cue.contains("    INDEX 00 03:59:00"));
        // Track 02 file block must NOT re-declare its TRACK header.
        let track_02_count = cue.matches("  TRACK 02 AUDIO").count();
        assert_eq!(track_02_count, 1, "TRACK 02 must appear exactly once");
    }

    #[test]
    fn multifile_cue_without_pregap_uses_compliant_form() {
        let tracks = vec![
            track(1, Duration::from_secs(240), None),
            track(2, Duration::from_secs(180), None),
        ];
        let cue = generate_multifile_cue(&album(), &tracks);

        assert!(!cue.contains("INDEX 00"));
        assert_eq!(cue.matches("  TRACK 01 AUDIO").count(), 1);
        assert_eq!(cue.matches("  TRACK 02 AUDIO").count(), 1);
    }

    #[test]
    fn multifile_cue_skips_pregap_longer_than_prev_track() {
        // Pregap of 99999 frames > track 1's 4 minutes (18000 frames). Skip.
        let tracks = vec![
            track(1, Duration::from_secs(240), None),
            track(2, Duration::from_secs(180), Some(99_999)),
        ];
        let cue = generate_multifile_cue(&album(), &tracks);

        assert!(!cue.contains("INDEX 00"), "pregap exceeding prev track must be skipped");
        assert_eq!(cue.matches("  TRACK 02 AUDIO").count(), 1);
    }

    #[test]
    fn single_image_cue_emits_index00_at_correct_offset() {
        // Track 1 = 4 min (18000 frames). Track 2 = 3 min, with 75-frame pregap.
        // Track 2 cumulative start = 18000. INDEX 00 = 18000 - 75 = 17925 = 03:59:00.
        // Track 2 INDEX 01 = 04:00:00.
        let tracks = vec![
            track(1, Duration::from_secs(240), None),
            track(2, Duration::from_secs(180), Some(75)),
        ];
        let cue = generate_single_image_cue(&album(), &tracks, "image.flac", "FLAC");

        assert!(cue.contains("    INDEX 00 03:59:00"));
        assert!(cue.contains("    INDEX 01 04:00:00"));
    }
}
