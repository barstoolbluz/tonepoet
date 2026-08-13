//! Standalone CUE sheet generation from tagged audio files in the browse view.
//! Separate from the conversion pipeline's generator in tonepoet-features.

use std::path::Path;
use std::time::Duration;

/// Album-level metadata for CUE header.
#[derive(Debug, Clone)]
pub struct CueAlbumInfo {
    pub title: String,
    pub artist: String,
    pub year: Option<String>,
    pub genre: Option<String>,
    /// 13-digit UPC/EAN, emitted as the `CATALOG` line. Sourced from
    /// MusicBrainz `barcode` for `:cue-mb`, or from a tag's
    /// `CATALOGNUMBER` if it parses cleanly.
    pub catalog: Option<String>,
}

/// Per-track metadata for a CUE entry.
#[derive(Debug, Clone)]
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
    /// CD ISRC code (12 alphanumerics, no separators), if present in tags.
    pub isrc: Option<String>,
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
            push_isrc_line(&mut cue, t.isrc.as_deref());
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
                    push_isrc_line(&mut cue, next.isrc.as_deref());
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
        push_isrc_line(&mut cue, t.isrc.as_deref());

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

/// Build a CUE string from a MusicBrainz release for embedding as a
/// `CUESHEET` tag on a single-image rip. The single image's filename
/// (basename, not path) and its format extension drive the FILE line.
///
/// Refuses to generate when *any* track is missing `length_ms`: the
/// timestamps would be wrong and silent corruption is worse than a
/// caller-visible error.
pub fn cue_from_mb_release(
    release: &super::musicbrainz::MbRelease,
    image_filename: &str,
    image_ext: &str,
) -> Result<String, String> {
    if release.tracks.is_empty() {
        return Err("MB release has no tracks".to_string());
    }
    let mut tracks_sorted: Vec<&super::musicbrainz::MbTrack> = release.tracks.iter().collect();
    tracks_sorted.sort_by_key(|t| t.position);

    let mut cue_tracks: Vec<CueTrackInfo> = Vec::with_capacity(tracks_sorted.len());
    for t in &tracks_sorted {
        let length_ms = t.length_ms.ok_or_else(|| {
            format!(
                "MB track {} ({:?}) has no length; cannot generate timestamps",
                t.position, t.title,
            )
        })?;
        cue_tracks.push(CueTrackInfo {
            filename: image_filename.to_string(),
            title: t.title.clone(),
            artist: if t.artist.is_empty() {
                release.artist.clone()
            } else {
                t.artist.join("; ")
            },
            track_number: t.position,
            duration: Duration::from_millis(length_ms as u64),
            format_tag: cue_format_tag(image_ext).to_string(),
            pregap_frames: None,
            isrc: t.isrc.clone(),
        });
    }

    let album = CueAlbumInfo {
        title: release.title.clone(),
        artist: release.artist.clone(),
        year: release.year.clone(),
        genre: None,
        catalog: release
            .catalog
            .clone()
            .or_else(|| release.barcode.clone())
            .filter(|s| !s.is_empty()),
    };
    let format_tag = cue_format_tag(image_ext);
    Ok(generate_single_image_cue(
        &album,
        &cue_tracks,
        image_filename,
        format_tag,
    ))
}

/// Regenerate a CUE sheet from a parsed `CueSheet`, applying per-track
/// field overrides (TITLE, PERFORMER, ISRC). Preserves original
/// `index01_frames` / `index00_frames` so timestamps don't drift through
/// a duration-recompute round-trip. Emits standard CUE syntax; LOSSY
/// for any custom REM lines or comments not surfaced as parsed fields
/// — caller should warn the user (and back up the original) before
/// overwriting.
///
/// `track_overrides[i]` aligns with `parsed.tracks[i]` (assumed sorted
/// by track number; the editor sorts at populate time).
pub struct TrackOverride {
    pub title: Option<String>,
    pub performer: Option<String>,
    pub isrc: Option<String>,
}

pub fn regenerate_cue_with_overrides(
    parsed: &super::cue_parser::CueSheet,
    track_overrides: &[TrackOverride],
    image_filename: &str,
    image_format_tag: &str,
) -> String {
    let mut cue = String::new();

    // Album-level header — preserve what the parsed sheet had.
    if let Some(genre) = parsed.genre.as_deref().filter(|s| !s.is_empty()) {
        cue.push_str(&format!("REM GENRE \"{}\"\n", escape(genre)));
    }
    if let Some(date) = parsed.date.as_deref().filter(|s| !s.is_empty()) {
        cue.push_str(&format!("REM DATE \"{}\"\n", escape(date)));
    }
    if let Some(catalog) = parsed.catalog.as_deref().filter(|s| !s.is_empty()) {
        cue.push_str(&format!("CATALOG {}\n", escape(catalog)));
    }
    if let Some(title) = parsed.title.as_deref().filter(|s| !s.is_empty()) {
        cue.push_str(&format!("TITLE \"{}\"\n", escape(title)));
    }
    if let Some(performer) = parsed.performer.as_deref().filter(|s| !s.is_empty()) {
        cue.push_str(&format!("PERFORMER \"{}\"\n", escape(performer)));
    }

    cue.push_str(&format!(
        "FILE \"{}\" {}\n",
        escape(image_filename),
        image_format_tag,
    ));

    for (i, track) in parsed.tracks.iter().enumerate() {
        cue.push_str(&format!("  TRACK {:02} AUDIO\n", track.number));
        // Title: override > parsed > nothing
        let title = track_overrides
            .get(i)
            .and_then(|o| o.title.as_deref())
            .or(track.title.as_deref())
            .filter(|s| !s.is_empty());
        if let Some(t) = title {
            cue.push_str(&format!("    TITLE \"{}\"\n", escape(t)));
        }
        let performer = track_overrides
            .get(i)
            .and_then(|o| o.performer.as_deref())
            .or(track.performer.as_deref())
            .filter(|s| !s.is_empty());
        if let Some(p) = performer {
            cue.push_str(&format!("    PERFORMER \"{}\"\n", escape(p)));
        }
        let isrc = track_overrides
            .get(i)
            .and_then(|o| o.isrc.as_deref())
            .or(track.isrc.as_deref())
            .filter(|s| !s.is_empty());
        if let Some(c) = isrc {
            cue.push_str(&format!("    ISRC {}\n", escape(c)));
        }
        for directive in &track.directives {
            let directive = directive.trim();
            if !directive.is_empty() {
                cue.push_str("    ");
                cue.push_str(directive);
                cue.push('\n');
            }
        }
        if let Some(idx00) = track.index00_frames {
            cue.push_str(&format!(
                "    INDEX 00 {}\n",
                frames_to_cue_timestamp(idx00)
            ));
        }
        if let Some(idx01) = track.index01_frames {
            cue.push_str(&format!(
                "    INDEX 01 {}\n",
                frames_to_cue_timestamp(idx01)
            ));
        }
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
    if let Some(catalog) = album.catalog.as_deref().filter(|s| !s.trim().is_empty()) {
        cue.push_str(&format!("CATALOG {}\n", catalog.trim()));
    }
    cue.push_str(&format!("PERFORMER \"{}\"\n", escape(&album.artist)));
    cue.push_str(&format!("TITLE \"{}\"\n", escape(&album.title)));
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Emit an `ISRC <code>` line under a TRACK block when the tag holds a
/// non-empty value. Trim surrounding whitespace; emit nothing on empty.
fn push_isrc_line(cue: &mut String, isrc: Option<&str>) {
    let Some(raw) = isrc else { return };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return;
    }
    cue.push_str(&format!("    ISRC {}\n", trimmed));
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

/// Blocking worker helper: build album + track info by probing files and reading tags.
///
/// This calls `read_metadata()` and `probe_audio()` for each input file. Do not
/// call it from TUI reducers or key handlers; wrap it in `spawn_blocking` and
/// send the result back through `AppMessage`.
///
/// Falls back to filename parsing when tags are missing. `cue_dir` is the
/// directory where the CUE file will be written; FILE references are made
/// relative to it.
pub(crate) fn gather_cue_info_blocking(
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

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("wav");
        let format_tag = cue_format_tag(ext).to_string();

        // FILE reference: relative to CUE output directory.
        let filename = path
            .strip_prefix(cue_dir)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

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

        let isrc = meta.as_ref().and_then(|m| m.isrc.clone());

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
            isrc,
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
        catalog: None,
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

/// Blocking worker helper: bridge a parsed CueSheet + colocated audio paths
/// into the (album, tracks) shape used by the generators, with pregaps
/// reconstructed from `INDEX 00` and durations probed from the audio files.
///
/// This calls `probe_audio()` for the referenced audio files. Do not call it
/// from TUI reducers or key handlers; wrap it in `spawn_blocking` and send the
/// result back through `AppMessage`.
///
/// Used by `:cue-fill` so re-emission preserves the user's track boundaries
/// without depending on a separate EAC log. `audio_paths` must be sorted by
/// track order and have the same length as `sheet.tracks` (multi-file) or len
/// == 1 (single-image). Returns `Err` on length mismatch.
pub(crate) fn cue_sheet_to_track_info_blocking(
    sheet: &super::cue_parser::CueSheet,
    audio_paths: &[std::path::PathBuf],
    cue_dir: &std::path::Path,
) -> Result<(CueAlbumInfo, Vec<CueTrackInfo>), String> {
    if sheet.tracks.is_empty() {
        return Err("parsed CUE has no tracks".to_string());
    }
    let single_image = audio_paths.len() == 1 && sheet.tracks.len() > 1;

    // Probe each audio file once for duration. For single-image, this is
    // the one image file; for multi-file, it's each track file in order.
    let durations: Vec<Duration> = audio_paths
        .iter()
        .map(|p| {
            super::probe::probe_audio(p)
                .map(|info| Duration::from_secs_f64(info.duration_secs))
                .unwrap_or(Duration::ZERO)
        })
        .collect();

    let mut tracks = Vec::with_capacity(sheet.tracks.len());

    for (i, ct) in sheet.tracks.iter().enumerate() {
        // Resolve the audio file we'll reference from the CUE. For
        // multi-file we take the i-th selected audio path; for single-image
        // every track shares audio_paths[0].
        let audio_path: &std::path::PathBuf = if single_image {
            &audio_paths[0]
        } else {
            audio_paths.get(i).ok_or_else(|| {
                format!(
                    "audio path missing for track {}; expected {} files, got {}",
                    ct.number,
                    sheet.tracks.len(),
                    audio_paths.len(),
                )
            })?
        };

        // Per-track duration. Multi-file: probe of this file. Single-image:
        // INDEX 01 delta (or remaining file length for the final track).
        let duration = if single_image {
            let total = durations.first().copied().unwrap_or_default();
            let total_frames = (total.as_secs_f64() * 75.0).round() as u32;
            let this_idx01 = ct.index01_frames.unwrap_or(0);
            let next_idx01 = sheet
                .tracks
                .get(i + 1)
                .and_then(|n| n.index01_frames)
                .unwrap_or(total_frames);
            let frames = next_idx01.saturating_sub(this_idx01);
            Duration::from_secs_f64(frames as f64 / 75.0)
        } else {
            durations[i]
        };

        // Pregap reconstruction.
        let pregap_frames = ct
            .index00_frames
            .and_then(|idx00| {
                if single_image {
                    // Absolute cumulative: pregap = INDEX 01 - INDEX 00
                    ct.index01_frames.and_then(|idx01| idx01.checked_sub(idx00))
                } else if i == 0 {
                    // Track 1's INDEX 00 is the lead-in pregap (not in rip).
                    None
                } else {
                    // Multi-file noncompliant: INDEX 00 is in prev FILE.
                    // pregap = (prev track length frames) - INDEX 00 position
                    let prev_dur = durations.get(i - 1).copied().unwrap_or_default();
                    let prev_frames = (prev_dur.as_secs_f64() * 75.0).round() as u32;
                    prev_frames.checked_sub(idx00)
                }
            })
            .filter(|&n| n > 0);

        let filename = audio_path
            .strip_prefix(cue_dir)
            .unwrap_or(audio_path)
            .to_string_lossy()
            .to_string();
        let ext = audio_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("flac");
        let format_tag = cue_format_tag(ext).to_string();

        tracks.push(CueTrackInfo {
            filename,
            title: ct.title.clone().unwrap_or_default(),
            artist: ct.performer.clone().unwrap_or_default(),
            track_number: ct.number,
            duration,
            format_tag,
            pregap_frames,
            isrc: ct.isrc.clone(),
        });
    }

    let album = CueAlbumInfo {
        title: sheet.title.clone().unwrap_or_default(),
        artist: sheet.performer.clone().unwrap_or_default(),
        year: sheet.date.clone(),
        genre: sheet.genre.clone(),
        catalog: sheet.catalog.clone(),
    };
    Ok((album, tracks))
}

/// Sanity-check a CUE content string before writing. Returns `Ok(())`
/// when the parsed sheet has at least one track, every track has an
/// `INDEX 01`, and at least one FILE reference is present. Used by the
/// preview overlay's save action so a user-edited CUE can't accidentally
/// be written in a structurally broken state.
pub fn validate_cue_content(content: &str) -> Result<(), String> {
    let sheet = super::cue_parser::parse_cue(content);
    if sheet.tracks.is_empty() {
        return Err("CUE has no TRACK declarations".to_string());
    }
    let any_file = sheet.tracks.iter().any(|t| t.file.is_some());
    if !any_file {
        return Err("CUE has no FILE references".to_string());
    }
    for t in &sheet.tracks {
        if t.index01_frames.is_none() {
            return Err(format!("track {:02} is missing INDEX 01", t.number));
        }
    }
    Ok(())
}

/// Counts of fields actually changed by `fill_cue_with_mb`, for status messaging.
#[derive(Debug, Clone, Default)]
pub struct FillStats {
    pub titles_filled: usize,
    pub artists_filled: usize,
    pub isrcs_filled: usize,
    pub year_filled: bool,
    pub catalog_filled: bool,
}

impl FillStats {
    /// True when no field was filled — caller can short-circuit the write.
    pub fn is_empty(&self) -> bool {
        self.titles_filled == 0
            && self.artists_filled == 0
            && self.isrcs_filled == 0
            && !self.year_filled
            && !self.catalog_filled
    }
}

/// Fill *only* empty/absent fields on `album` and `tracks` from a MusicBrainz
/// release. Used by `:cue-fill` (enrich semantics): user-typed values are
/// preserved verbatim; only fields where the existing value is empty/absent
/// get the MB value. Returns counts of what was changed.
///
/// Distinct from `apply_mb_overrides` which has overwrite semantics.
pub fn fill_cue_with_mb(
    album: &mut CueAlbumInfo,
    tracks: &mut [CueTrackInfo],
    mb: &super::musicbrainz::MbRelease,
) -> FillStats {
    let mut stats = FillStats::default();

    fn fill_string(field: &mut String, candidate: &str) -> bool {
        if !field.trim().is_empty() {
            return false;
        }
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            return false;
        }
        *field = trimmed.to_string();
        true
    }
    fn fill_opt(field: &mut Option<String>, candidate: Option<&str>) -> bool {
        if field
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            return false;
        }
        if let Some(c) = candidate.map(str::trim).filter(|s| !s.is_empty()) {
            *field = Some(c.to_string());
            return true;
        }
        false
    }

    if fill_string(&mut album.title, &mb.title) {
        stats.titles_filled += 1;
    }
    if fill_string(&mut album.artist, &mb.artist) {
        stats.artists_filled += 1;
    }
    if fill_opt(&mut album.year, mb.year.as_deref()) {
        stats.year_filled = true;
    }
    if fill_opt(&mut album.catalog, mb.barcode.as_deref()) {
        stats.catalog_filled = true;
    }

    for t in tracks.iter_mut() {
        if let Some(mt) = mb.tracks.iter().find(|m| m.position == t.track_number) {
            if fill_string(&mut t.title, &mt.title) {
                stats.titles_filled += 1;
            }
            let artist = mt.artist.join("; ");
            if fill_string(&mut t.artist, &artist) {
                stats.artists_filled += 1;
            }
            if fill_opt(&mut t.isrc, mt.isrc.as_deref()) {
                stats.isrcs_filled += 1;
            }
        }
    }

    stats
}

/// Overlay MusicBrainz data on a tag-derived album + tracks.
///
/// MB values win when present and non-empty; the existing tag-derived
/// values stay as fall-back. Tracks match by `track_number ↔ MbTrack.position`.
/// Used by `:cue-mb` (overwrite mode).
pub fn apply_mb_overrides(
    album: &mut CueAlbumInfo,
    tracks: &mut [CueTrackInfo],
    mb: &super::musicbrainz::MbRelease,
) {
    fn replace_if_better(field: &mut String, candidate: &str) {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() {
            *field = trimmed.to_string();
        }
    }
    fn set_opt_if_better(field: &mut Option<String>, candidate: Option<&str>) {
        if let Some(c) = candidate.map(str::trim).filter(|s| !s.is_empty()) {
            *field = Some(c.to_string());
        }
    }

    replace_if_better(&mut album.title, &mb.title);
    replace_if_better(&mut album.artist, &mb.artist);
    set_opt_if_better(&mut album.year, mb.year.as_deref());
    set_opt_if_better(&mut album.catalog, mb.barcode.as_deref());

    for t in tracks.iter_mut() {
        if let Some(mt) = mb.tracks.iter().find(|m| m.position == t.track_number) {
            replace_if_better(&mut t.title, &mt.title);
            let artist = mt.artist.join("; ");
            replace_if_better(&mut t.artist, &artist);
            set_opt_if_better(&mut t.isrc, mt.isrc.as_deref());
        }
    }
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
            isrc: None,
        }
    }

    fn track_with_isrc(num: u32, duration: Duration, isrc: &str) -> CueTrackInfo {
        let mut t = track(num, duration, None);
        t.isrc = Some(isrc.to_string());
        t
    }

    fn album() -> CueAlbumInfo {
        CueAlbumInfo {
            title: "Album".to_string(),
            artist: "Artist".to_string(),
            year: None,
            genre: None,
            catalog: None,
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

        assert!(
            !cue.contains("INDEX 00"),
            "pregap exceeding prev track must be skipped"
        );
        assert_eq!(cue.matches("  TRACK 02 AUDIO").count(), 1);
    }

    #[test]
    fn multifile_cue_emits_isrc_line_when_present() {
        let tracks = vec![
            track_with_isrc(1, Duration::from_secs(240), "USRC17607839"),
            track(2, Duration::from_secs(180), None),
        ];
        let cue = generate_multifile_cue(&album(), &tracks);
        assert!(cue.contains("    ISRC USRC17607839\n"));
        // No ISRC line for track 2.
        assert_eq!(cue.matches("ISRC ").count(), 1);
    }

    #[test]
    fn isrc_line_skipped_for_empty_or_whitespace_isrc() {
        let mut t = track(1, Duration::from_secs(60), None);
        t.isrc = Some("   ".to_string());
        let cue = generate_multifile_cue(&album(), &[t]);
        assert!(!cue.contains("ISRC"));
    }

    #[test]
    fn isrc_emitted_in_pregap_injected_track_block() {
        // Track 2's ISRC must appear inside track 1's FILE block (where its
        // TRACK declaration was emitted because of pregap), not in track 2's
        // FILE block.
        let t1 = track(1, Duration::from_secs(240), None);
        let mut t2 = track(2, Duration::from_secs(180), Some(75));
        t2.isrc = Some("USRC17607840".to_string());
        let cue = generate_multifile_cue(&album(), &[t1, t2]);

        let file1_pos = cue.find("FILE \"01 - Track.flac\"").unwrap();
        let file2_pos = cue.find("FILE \"02 - Track.flac\"").unwrap();
        let isrc_pos = cue.find("ISRC USRC17607840").unwrap();
        assert!(
            isrc_pos > file1_pos && isrc_pos < file2_pos,
            "ISRC for track 2 should sit inside track 1's FILE block (pregap injection)"
        );
    }

    #[test]
    fn single_image_cue_emits_isrc_line() {
        let tracks = vec![
            track_with_isrc(1, Duration::from_secs(240), "USRC17607839"),
            track_with_isrc(2, Duration::from_secs(180), "USRC17607840"),
        ];
        let cue = generate_single_image_cue(&album(), &tracks, "image.flac", "FLAC");
        assert!(cue.contains("    ISRC USRC17607839\n"));
        assert!(cue.contains("    ISRC USRC17607840\n"));
    }

    #[test]
    fn header_emits_catalog_line_when_album_has_value() {
        let mut a = album();
        a.catalog = Some("0044007735428".to_string());
        let cue = generate_multifile_cue(&a, &[track(1, Duration::from_secs(60), None)]);
        assert!(cue.contains("CATALOG 0044007735428\n"));
    }

    #[test]
    fn header_skips_catalog_line_when_album_has_none() {
        let cue = generate_multifile_cue(&album(), &[track(1, Duration::from_secs(60), None)]);
        assert!(!cue.contains("CATALOG"));
    }

    #[test]
    fn validate_cue_accepts_minimal_valid_content() {
        let cue = "FILE \"x.flac\" FLAC\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n";
        assert!(validate_cue_content(cue).is_ok());
    }

    #[test]
    fn validate_cue_rejects_empty_content() {
        assert!(validate_cue_content("").is_err());
    }

    #[test]
    fn validate_cue_rejects_no_tracks() {
        let cue = "FILE \"x.flac\" FLAC\nTITLE \"Album\"\n";
        let err = validate_cue_content(cue).unwrap_err();
        assert!(err.contains("no TRACK"));
    }

    #[test]
    fn validate_cue_rejects_track_missing_index01() {
        let cue = "FILE \"x.flac\" FLAC\n  TRACK 01 AUDIO\n    TITLE \"x\"\n";
        let err = validate_cue_content(cue).unwrap_err();
        assert!(err.contains("INDEX 01"));
    }

    #[test]
    fn validate_cue_rejects_no_file() {
        // Track without a FILE line — none associated.
        let cue = "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n";
        let err = validate_cue_content(cue).unwrap_err();
        assert!(err.contains("no FILE"));
    }

    #[test]
    fn fill_cue_only_changes_empty_fields() {
        use super::super::musicbrainz::{MbRelease, MbTrack};

        let mut album = CueAlbumInfo {
            title: "User Title".to_string(),
            artist: "".to_string(), // empty → fill from MB
            year: None,             // absent → fill from MB
            genre: None,
            catalog: Some("USER-CAT".to_string()), // present → keep
        };
        let mut tracks = vec![
            // ISRC absent → fill; title present → keep
            CueTrackInfo {
                filename: "01.flac".to_string(),
                title: "User Track 1".to_string(),
                artist: "".to_string(),
                track_number: 1,
                duration: Duration::from_secs(60),
                format_tag: "FLAC".to_string(),
                pregap_frames: None,
                isrc: None,
            },
        ];
        let mb = MbRelease {
            release_id: "x".to_string(),
            title: "MB Title".to_string(),
            artist_values: vec!["MB Artist".to_string()],
            artist: "MB Artist".to_string(),
            year: Some("1971".to_string()),
            barcode: Some("MB-BARCODE".to_string()),
            tracks: vec![MbTrack {
                position: 1,
                title: "MB Track 1".to_string(),
                artist: vec!["MB Performer".to_string()],
                isrc: Some("USRC17607839".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let stats = fill_cue_with_mb(&mut album, &mut tracks, &mb);

        // Album: title preserved, artist filled, year filled, catalog preserved
        assert_eq!(album.title, "User Title");
        assert_eq!(album.artist, "MB Artist");
        assert_eq!(album.year.as_deref(), Some("1971"));
        assert_eq!(album.catalog.as_deref(), Some("USER-CAT"));

        // Track 1: title preserved, artist filled, isrc filled
        assert_eq!(tracks[0].title, "User Track 1");
        assert_eq!(tracks[0].artist, "MB Performer");
        assert_eq!(tracks[0].isrc.as_deref(), Some("USRC17607839"));

        // Stats: artist+isrc per track, year at album level. titles_filled
        // counts only what was actually filled.
        assert_eq!(stats.titles_filled, 0);
        assert_eq!(stats.artists_filled, 2); // album + 1 track
        assert_eq!(stats.isrcs_filled, 1);
        assert!(stats.year_filled);
        assert!(!stats.catalog_filled);
        assert!(!stats.is_empty());
    }

    #[test]
    fn fill_cue_is_empty_when_nothing_to_fill() {
        use super::super::musicbrainz::MbRelease;

        let mut album = CueAlbumInfo {
            title: "T".to_string(),
            artist: "A".to_string(),
            year: Some("1971".to_string()),
            genre: None,
            catalog: Some("C".to_string()),
        };
        let mut tracks: Vec<CueTrackInfo> = vec![];
        let mb = MbRelease {
            release_id: "x".to_string(),
            title: "MB".to_string(),
            artist_values: vec!["MB".to_string()],
            artist: "MB".to_string(),
            year: Some("1972".to_string()),
            barcode: Some("X".to_string()),
            ..Default::default()
        };
        let stats = fill_cue_with_mb(&mut album, &mut tracks, &mb);
        assert!(stats.is_empty());
        assert_eq!(album.title, "T"); // unchanged
    }

    #[test]
    fn apply_mb_overrides_replaces_when_mb_has_data_keeps_when_empty() {
        use super::super::musicbrainz::{MbRelease, MbTrack};

        let mut album = CueAlbumInfo {
            title: "Tag Title".to_string(),
            artist: "Tag Artist".to_string(),
            year: Some("1969".to_string()),
            genre: None,
            catalog: None,
        };
        let mut tracks = vec![
            track(1, Duration::from_secs(60), None),
            track(2, Duration::from_secs(60), None),
        ];

        let mb = MbRelease {
            release_id: "x".to_string(),
            title: "MB Title".to_string(),
            artist_values: vec!["MB Artist".to_string()],
            artist: "MB Artist".to_string(),
            year: Some("1971".to_string()),
            barcode: Some("0044007735428".to_string()),
            tracks: vec![
                MbTrack {
                    position: 1,
                    title: "MB Track 1".to_string(),
                    artist: vec!["Track Artist 1".to_string()],
                    isrc: Some("USRC17607839".to_string()),
                    ..Default::default()
                },
                MbTrack {
                    // Empty title from MB → must NOT clobber tag value.
                    position: 2,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        apply_mb_overrides(&mut album, &mut tracks, &mb);

        assert_eq!(album.title, "MB Title");
        assert_eq!(album.artist, "MB Artist");
        assert_eq!(album.year.as_deref(), Some("1971"));
        assert_eq!(album.catalog.as_deref(), Some("0044007735428"));
        assert_eq!(tracks[0].title, "MB Track 1");
        assert_eq!(tracks[0].artist, "Track Artist 1");
        assert_eq!(tracks[0].isrc.as_deref(), Some("USRC17607839"));
        // Track 2: MB had empty values → tag values kept.
        assert_eq!(tracks[1].title, "Track 2");
        assert_eq!(tracks[1].artist, "Artist");
        assert_eq!(tracks[1].isrc, None);
    }

    fn mb_track(
        position: u32,
        title: &str,
        length_ms: Option<u32>,
        isrc: Option<&str>,
    ) -> super::super::musicbrainz::MbTrack {
        super::super::musicbrainz::MbTrack {
            position,
            track_id: None,
            recording_id: None,
            artist_id: None,
            title: title.into(),
            artist: Vec::new(),
            composer: Vec::new(),
            isrc: isrc.map(String::from),
            length_ms,
        }
    }

    #[test]
    fn cue_from_mb_release_produces_valid_cue_with_cumulative_timestamps() {
        let release = super::super::musicbrainz::MbRelease {
            release_id: "rid".into(),
            release_group_id: None,
            artist_id: None,
            title: "Album".into(),
            artist_values: vec!["Artist".into()],
            artist: "Artist".into(),
            year: Some("1970".into()),
            original_date: None,
            country: None,
            catalog: Some("CAT-1".into()),
            barcode: None,
            disc_count: 1,
            tracks: vec![
                mb_track(1, "Track 1", Some(240_000), Some("USRC17607839")), // 4:00
                mb_track(2, "Track 2", Some(180_000), None),                 // 3:00
                mb_track(3, "Track 3", Some(120_000), None),                 // 2:00
            ],
            relationship_projection_complete: true,
            track_parse_error: None,
        };
        let cue = cue_from_mb_release(&release, "image.flac", "flac")
            .expect("cue generation should succeed");
        // Validates structurally.
        validate_cue_content(&cue).expect("CUE must validate");
        // FILE line carries the right name + format tag.
        assert!(cue.contains("FILE \"image.flac\" FLAC"));
        // Track 1 starts at 00:00:00, Track 2 at 04:00:00, Track 3 at 07:00:00.
        assert!(
            cue.contains("    INDEX 01 00:00:00"),
            "track 1 INDEX 01\n{}",
            cue
        );
        assert!(
            cue.contains("    INDEX 01 04:00:00"),
            "track 2 INDEX 01\n{}",
            cue
        );
        assert!(
            cue.contains("    INDEX 01 07:00:00"),
            "track 3 INDEX 01\n{}",
            cue
        );
        // ISRC carried per-track when MB provides it.
        assert!(cue.contains("USRC17607839"));
        // Catalog from release.
        assert!(cue.contains("CATALOG"));
    }

    #[test]
    fn cue_from_mb_release_refuses_when_lengths_missing() {
        let release = super::super::musicbrainz::MbRelease {
            release_id: "rid".into(),
            release_group_id: None,
            artist_id: None,
            title: "Album".into(),
            artist_values: vec!["Artist".into()],
            artist: "Artist".into(),
            year: None,
            original_date: None,
            country: None,
            catalog: None,
            barcode: None,
            disc_count: 1,
            tracks: vec![
                mb_track(1, "Track 1", Some(240_000), None),
                mb_track(2, "Track 2", None, None),
            ],
            relationship_projection_complete: true,
            track_parse_error: None,
        };
        let err = cue_from_mb_release(&release, "image.flac", "flac")
            .expect_err("must refuse when a track has no length");
        assert!(
            err.contains("length"),
            "error should mention length: {}",
            err
        );
    }

    #[test]
    fn cue_from_mb_release_refuses_on_empty_tracks() {
        let release = super::super::musicbrainz::MbRelease {
            release_id: "rid".into(),
            release_group_id: None,
            artist_id: None,
            title: "Album".into(),
            artist_values: vec!["Artist".into()],
            artist: "Artist".into(),
            year: None,
            original_date: None,
            country: None,
            catalog: None,
            barcode: None,
            disc_count: 1,
            tracks: vec![],
            relationship_projection_complete: true,
            track_parse_error: None,
        };
        assert!(cue_from_mb_release(&release, "image.flac", "flac").is_err());
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

    #[test]
    fn regenerate_with_overrides_preserves_index_and_applies_overrides() {
        use super::super::cue_parser::{CueSheet, CueTrack};
        let parsed = CueSheet {
            title: Some("Old Album".into()),
            performer: Some("Old Artist".into()),
            date: Some("1977".into()),
            genre: None,
            catalog: None,
            tracks: vec![
                CueTrack {
                    number: 1,
                    title: Some("Old Track 1".into()),
                    performer: Some("Old Artist".into()),
                    file: Some("image.flac".into()),
                    index01_frames: Some(0),
                    index00_frames: None,
                    isrc: Some("USRC0000001".into()),
                    directives: Vec::new(),
                },
                CueTrack {
                    number: 2,
                    title: Some("Old Track 2".into()),
                    performer: Some("Old Artist".into()),
                    file: Some("image.flac".into()),
                    index01_frames: Some(18000), // 4:00:00
                    index00_frames: Some(17925), // 3:59:00 (75-frame pregap)
                    isrc: None,
                    directives: Vec::new(),
                },
            ],
        };
        let overrides = vec![
            TrackOverride {
                title: Some("New Track 1".to_string()),
                performer: None,
                isrc: None,
            },
            TrackOverride {
                title: Some("New Track 2".to_string()),
                performer: Some("New Performer".to_string()),
                isrc: Some("USRC0000002".to_string()),
            },
        ];
        let cue = regenerate_cue_with_overrides(&parsed, &overrides, "image.flac", "FLAC");
        // Album-level preserved.
        assert!(cue.contains("TITLE \"Old Album\""));
        assert!(cue.contains("PERFORMER \"Old Artist\""));
        assert!(cue.contains("REM DATE \"1977\""));
        // FILE line.
        assert!(cue.contains("FILE \"image.flac\" FLAC"));
        // Track 1: title overridden, performer falls back to parsed, ISRC from parsed.
        assert!(cue.contains("    TITLE \"New Track 1\""));
        assert!(cue.contains("    ISRC USRC0000001"));
        // Track 2: title + performer + ISRC all overridden.
        assert!(cue.contains("    TITLE \"New Track 2\""));
        assert!(cue.contains("    PERFORMER \"New Performer\""));
        assert!(cue.contains("    ISRC USRC0000002"));
        // INDEX timestamps preserved from parsed.
        assert!(cue.contains("    INDEX 01 00:00:00"));
        assert!(cue.contains("    INDEX 00 03:59:00"));
        assert!(cue.contains("    INDEX 01 04:00:00"));
    }

    #[test]
    fn regenerate_round_trips_track_flags_and_rem_directives_byte_stably() {
        let original = concat!(
            "REM GENRE \"Rock\"\n",
            "REM DATE \"1983\"\n",
            "TITLE \"Album\"\n",
            "PERFORMER \"Artist\"\n",
            "FILE \"image.flac\" FLAC\n",
            "  TRACK 01 AUDIO\n",
            "    TITLE \"One\"\n",
            "    PERFORMER \"Artist\"\n",
            "    ISRC USABC8300001\n",
            "    FLAGS PRE DCP\n",
            "    REM COMPOSER \"Composer One\"\n",
            "    REM COMMENT \"preserve ordering\"\n",
            "    INDEX 00 00:00:00\n",
            "    INDEX 01 00:00:10\n",
        );
        let parsed = super::super::cue_parser::parse_cue(original);
        assert_eq!(
            parsed.tracks[0].directives,
            vec![
                "FLAGS PRE DCP".to_string(),
                "REM COMPOSER \"Composer One\"".to_string(),
                "REM COMMENT \"preserve ordering\"".to_string(),
            ]
        );
        let regenerated = regenerate_cue_with_overrides(
            &parsed,
            &[TrackOverride {
                title: None,
                performer: None,
                isrc: None,
            }],
            "image.flac",
            "FLAC",
        );
        assert_eq!(regenerated, original);
    }

    #[test]
    fn regenerate_with_no_overrides_preserves_parsed_track_titles() {
        use super::super::cue_parser::{CueSheet, CueTrack};
        let parsed = CueSheet {
            title: None,
            performer: None,
            date: None,
            genre: None,
            catalog: None,
            tracks: vec![CueTrack {
                number: 1,
                title: Some("Original Title".into()),
                performer: None,
                file: Some("a.flac".into()),
                index01_frames: Some(0),
                index00_frames: None,
                isrc: None,
                directives: Vec::new(),
            }],
        };
        let overrides = vec![TrackOverride {
            title: None,
            performer: None,
            isrc: None,
        }];
        let cue = regenerate_cue_with_overrides(&parsed, &overrides, "a.flac", "FLAC");
        assert!(cue.contains("    TITLE \"Original Title\""));
    }
}
