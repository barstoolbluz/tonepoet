//! CUE layout detection and probe-derived track geometry for the TUI.
//!
//! The CUE parsing core lives in `crate::convert::cue_parser` (re-exported
//! here for existing call sites). This module keeps the detection helpers
//! that are coupled to TUI services: locating a CUE in a directory
//! (`gnudb`), resolving file references and probing sample counts
//! (`accuraterip`).
pub use crate::convert::cue_parser::*;

use std::path::{Path, PathBuf};

/// Returns true for a CUE file that should participate in sidecar and
/// split-CUE surface discovery. Dot-prefixed `.cue` files are treated as
/// editor scratch buffers, not user sidecars, so a rejected embedded-CUESHEET
/// edit cannot poison the next folder open or make sidecar detection ambiguous.
pub fn is_user_visible_cue_path(path: &Path) -> bool {
    // Single shared definition: the convert-layer classifier owns the
    // hidden-cue policy so the TUI and planner cannot drift.
    crate::convert::classify::is_cue_sheet_path(path)
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

/// A native multi-FILE CUE admitted through the same membership policy used
/// by queue expansion and the metadata editor. `track_audio_paths` is aligned
/// one-for-one with `sheet.tracks` and therefore preserves each track's FILE
/// ownership without re-resolving user-controlled references in the TUI.
#[derive(Debug, Clone)]
pub struct MultiFileCueLayout {
    pub cue_path: PathBuf,
    pub sheet: CueSheet,
    pub audio_paths: Vec<PathBuf>,
    pub track_audio_paths: Vec<PathBuf>,
}

/// One track's exact CUE-frame geometry within its owning FILE.
///
/// `end_frame == Some(_)` is an authored INDEX 01 boundary and must remain in
/// the CUE's native 75-frames-per-second domain. `None` means this is the last
/// track owned by the FILE; only that boundary is derived from physical EOF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultiFileTrackBoundary {
    pub start_frame: u32,
    pub end_frame: Option<u32>,
    pub file_total_samples: u64,
    pub sample_rate: u32,
}

/// A native multi-FILE CUE plus probe-derived per-track boundaries.
#[derive(Debug, Clone)]
pub struct MultiFileCueInfo {
    pub layout: MultiFileCueLayout,
    pub track_boundaries: Vec<MultiFileTrackBoundary>,
}

/// Current front-end default for cue-source resolution. All metadata-editor
/// and lookup entry points accept an explicit `CueSidecarPolicy`; this constant
/// is only the no-config default. A future folder-level preference replaces
/// the value supplied at those entry points without changing their behavior.
pub(crate) const DEFAULT_FRONTEND_CUE_POLICY: crate::convert::pipeline::CueSidecarPolicy =
    crate::convert::pipeline::CueSidecarPolicy::PreferSidecar;

fn parsed_sheet_is_native_multi_file_candidate(sheet: &CueSheet) -> bool {
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for track in &sheet.tracks {
        let Some(file_ref) = track.file.as_deref() else {
            return false;
        };
        *counts.entry(file_ref).or_insert(0) += 1;
    }
    counts.len() >= 2 && counts.values().any(|count| *count > 1)
}

/// Resolve a native multi-FILE CUE layout while preserving admission errors.
/// `Ok(None)` means this is not the native multi-FILE album shape; `Err` means
/// it is that shape but the shared editor/planner admission policy rejected it.
pub fn multi_file_cue_layout_for_cue(
    cue_path: &Path,
) -> Result<Option<MultiFileCueLayout>, String> {
    multi_file_cue_layout_for_cue_with_policy(cue_path, DEFAULT_FRONTEND_CUE_POLICY)
}

pub fn multi_file_cue_layout_for_cue_with_policy(
    cue_path: &Path,
    policy: crate::convert::pipeline::CueSidecarPolicy,
) -> Result<Option<MultiFileCueLayout>, String> {
    if !is_user_visible_cue_path(cue_path) {
        return Ok(None);
    }
    if policy == crate::convert::pipeline::CueSidecarPolicy::IgnoreCue {
        return Ok(None);
    }
    let parsed = parse_cue_file(cue_path)
        .map_err(|err| format!("multi-FILE CUE parse failed: {}: {err}", cue_path.display()))?;
    if !parsed_sheet_is_native_multi_file_candidate(&parsed) {
        return Ok(None);
    }
    if policy == crate::convert::pipeline::CueSidecarPolicy::EmbeddedOnly {
        return Err(
            "native multi-FILE CUE albums require a sidecar CUE source under EmbeddedOnly"
                .to_string(),
        );
    }
    let member = crate::convert::split_cue_album::admit_split_cue_member(cue_path).map_err(
        |err| {
            format!(
                "multi-FILE CUE admission failed: {}: {err}",
                cue_path.display()
            )
        },
    )?;
    if !member.contributes_synthetic_album_part() || member.referenced_audio.len() < 2 {
        return Ok(None);
    }
    Ok(Some(MultiFileCueLayout {
        cue_path: member.cue_path,
        sheet: member.sheet,
        audio_paths: member.referenced_audio,
        track_audio_paths: member.track_audio_paths,
    }))
}

fn multi_file_probe_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn cue_frame_position_within_file(
    frame: u32,
    total_samples: u64,
    sample_rate: u32,
) -> Result<std::cmp::Ordering, String> {
    if sample_rate == 0 {
        return Err("audio probe reported a zero sample rate".to_string());
    }
    let cue_position = (frame as u128)
        .checked_mul(sample_rate as u128)
        .ok_or_else(|| "CUE frame position overflowed".to_string())?;
    let physical_end = (total_samples as u128)
        .checked_mul(75)
        .ok_or_else(|| "audio EOF position overflowed".to_string())?;
    Ok(cue_position.cmp(&physical_end))
}

fn multi_file_track_boundaries_from_probes(
    layout: &MultiFileCueLayout,
    probes: &std::collections::BTreeMap<PathBuf, (u64, u32)>,
) -> Result<Vec<MultiFileTrackBoundary>, String> {
    let track_count = layout.sheet.tracks.len();
    if track_count == 0 || track_count != layout.track_audio_paths.len() {
        return Err("multi-FILE CUE track-to-image mapping is inconsistent".to_string());
    }
    if track_count > 99 {
        return Err(format!(
            "multi-FILE CUE has {track_count} tracks; CD TOCs and two-digit CUE \
             TRACK numbers are limited to 99"
        ));
    }

    let track_keys: Vec<PathBuf> = layout
        .track_audio_paths
        .iter()
        .map(|path| multi_file_probe_key(path))
        .collect();
    let mut boundaries = Vec::with_capacity(track_count);

    for (index, track) in layout.sheet.tracks.iter().enumerate() {
        let audio_path = &layout.track_audio_paths[index];
        let audio_key = &track_keys[index];
        let (total_samples, sample_rate) = probes.get(audio_key).copied().ok_or_else(|| {
            format!(
                "no audio probe is available for multi-FILE CUE member {}",
                audio_path.display()
            )
        })?;
        let index01 = track.index01_frames.ok_or_else(|| {
            format!(
                "multi-FILE CUE track {} has no INDEX 01",
                track.number
            )
        })?;
        if cue_frame_position_within_file(index01, total_samples, sample_rate)?
            != std::cmp::Ordering::Less
        {
            return Err(format!(
                "multi-FILE CUE track {} starts at or beyond the end of {}",
                track.number,
                audio_path.display()
            ));
        }
        let next_same_file = ((index + 1)..track_count)
            .find(|next| track_keys[*next].as_path() == audio_key.as_path());
        let end_frame = if let Some(next) = next_same_file {
            let next_track = &layout.sheet.tracks[next];
            let next_index01 = next_track.index01_frames.ok_or_else(|| {
                format!(
                    "multi-FILE CUE track {} has no INDEX 01",
                    next_track.number
                )
            })?;
            if next_index01 <= index01 {
                return Err(format!(
                    "multi-FILE CUE track {} has a zero-length or overlapping boundary in {}",
                    track.number,
                    audio_path.display()
                ));
            }
            if cue_frame_position_within_file(next_index01, total_samples, sample_rate)?
                == std::cmp::Ordering::Greater
            {
                return Err(format!(
                    "multi-FILE CUE track {} ends beyond the end of {}",
                    track.number,
                    audio_path.display()
                ));
            }
            Some(next_index01)
        } else {
            None
        };

        boundaries.push(MultiFileTrackBoundary {
            start_frame: index01,
            end_frame,
            file_total_samples: total_samples,
            sample_rate,
        });
    }

    Ok(boundaries)
}

/// Probe every distinct member image once and compute per-track boundaries
/// using INDEX 01 times local to each FILE. The last track owned by a FILE runs
/// to that FILE's physical end, even when later tracks belong to other FILEs.
pub fn probe_multi_file_cue(layout: MultiFileCueLayout) -> Result<MultiFileCueInfo, String> {
    if layout.sheet.tracks.len() > 99 {
        return Err(format!(
            "multi-FILE CUE has {} tracks; CD TOCs and two-digit CUE TRACK numbers \
             are limited to 99",
            layout.sheet.tracks.len()
        ));
    }
    let mut probes = std::collections::BTreeMap::new();
    for audio_path in &layout.audio_paths {
        let key = multi_file_probe_key(audio_path);
        if probes.contains_key(&key) {
            continue;
        }
        let (total_samples, sample_rate) =
            crate::tui::accuraterip::probe_sample_count(audio_path).map_err(|err| {
                format!(
                    "could not probe multi-FILE CUE member {}: {err}",
                    audio_path.display()
                )
            })?;
        if total_samples == 0 {
            return Err(format!(
                "multi-FILE CUE member {} contains no audio samples",
                audio_path.display()
            ));
        }
        if sample_rate == 0 {
            return Err(format!(
                "multi-FILE CUE member {} has an invalid zero sample rate",
                audio_path.display()
            ));
        }
        probes.insert(key, (total_samples, sample_rate));
    }

    let track_boundaries = multi_file_track_boundaries_from_probes(&layout, &probes)?;
    Ok(MultiFileCueInfo {
        layout,
        track_boundaries,
    })
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
        .filter(|path| is_user_visible_cue_path(path))
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
    let mut infos = detect_single_image_cues(dir);
    if infos.len() == 1 {
        infos.pop()
    } else {
        None
    }
}

/// Detect every materializable single-image CUE in `dir`, preserving
/// deterministic CUE-file order. This is the multi-cue album path used for
/// split-side/split-disc folders; each CUE is paired to the FILE reference it
/// actually declares rather than by directory sort order.
pub fn detect_single_image_cues(dir: &Path) -> Vec<SingleImageInfo> {
    crate::tui::gnudb::find_cues_in_dir(dir)
        .into_iter()
        .filter(|cue_path| is_user_visible_cue_path(cue_path))
        .filter_map(|cue_path| single_image_info_for_cue(&cue_path))
        .collect()
}

/// Detect a materializable single-image CUE from an explicit `.cue` path.
pub fn detect_single_image_cue(cue_path: &Path) -> Option<SingleImageInfo> {
    if !is_user_visible_cue_path(cue_path) {
        return None;
    }
    single_image_info_for_cue(cue_path)
}

fn single_image_info_for_cue(cue_path: &Path) -> Option<SingleImageInfo> {
    let dir = cue_path.parent()?;
    let sheet = parse_cue_file(cue_path).ok()?;

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
        let start_frames = sheet.tracks[i].index01_frames? as u64;
        let start_sample = start_frames * samples_per_frame;
        let end_sample = if i + 1 < n {
            let next_frames = sheet.tracks[i + 1].index01_frames? as u64;
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
        cue_path: cue_path.to_path_buf(),
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
            .stdin(std::process::Stdio::null())
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
            .stdin(std::process::Stdio::null())
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
        .stdin(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admitted_multi_file_layout(
        cue_text: &str,
        audio_names: &[&str],
    ) -> (tempfile::TempDir, MultiFileCueLayout) {
        let temp = tempfile::tempdir().expect("tempdir");
        for name in audio_names {
            std::fs::write(temp.path().join(name), b"audio fixture")
                .expect("audio fixture");
        }
        let cue_path = temp.path().join("album.cue");
        std::fs::write(&cue_path, cue_text).expect("cue fixture");
        let layout = multi_file_cue_layout_for_cue(&cue_path)
            .expect("native multi-FILE CUE admission")
            .expect("native multi-FILE CUE should admit");
        (temp, layout)
    }

    #[test]
    fn dot_prefixed_cue_files_do_not_make_sidecar_detection_ambiguous() {
        let temp = tempfile::tempdir().expect("tempdir");
        let audio = temp.path().join("album.flac");
        std::fs::write(&audio, b"fixture").expect("audio fixture");
        let visible = temp.path().join("album.cue");
        let hidden = temp.path().join(".album.tonepoet-embedded-cuesheet-rejected.cue");
        let cue_text = "FILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n";
        std::fs::write(&visible, cue_text).expect("visible cue");
        std::fs::write(&hidden, cue_text).expect("hidden cue");

        assert!(is_user_visible_cue_path(&visible));
        assert!(!is_user_visible_cue_path(&hidden));
        assert_eq!(find_sidecar_cue_for_audio_image(&audio).as_deref(), Some(visible.as_path()));
    }

    #[test]
    fn multi_file_boundaries_use_index_times_local_to_each_member() {
        let cue = concat!(
            "TITLE \"Album\"\n",
            "FILE \"a.flac\" FLAC\n",
            "  TRACK 01 AUDIO\n",
            "    INDEX 01 00:00:00\n",
            "  TRACK 02 AUDIO\n",
            "    INDEX 01 00:05:00\n",
            "FILE \"b.flac\" FLAC\n",
            "  TRACK 03 AUDIO\n",
            "    INDEX 01 00:00:00\n",
            "  TRACK 04 AUDIO\n",
            "    INDEX 01 00:04:00\n",
            "  TRACK 05 AUDIO\n",
            "    INDEX 01 00:09:00\n",
        );
        let (_temp, layout) = admitted_multi_file_layout(cue, &["a.flac", "b.flac"]);
        let mut probes = std::collections::BTreeMap::new();
        probes.insert(multi_file_probe_key(&layout.audio_paths[0]), (441_000, 44_100));
        probes.insert(multi_file_probe_key(&layout.audio_paths[1]), (529_200, 44_100));

        let boundaries = multi_file_track_boundaries_from_probes(&layout, &probes)
            .expect("file-local boundaries");
        assert_eq!(
            boundaries,
            vec![
                MultiFileTrackBoundary {
                    start_frame: 0,
                    end_frame: Some(375),
                    file_total_samples: 441_000,
                    sample_rate: 44_100,
                },
                MultiFileTrackBoundary {
                    start_frame: 375,
                    end_frame: None,
                    file_total_samples: 441_000,
                    sample_rate: 44_100,
                },
                MultiFileTrackBoundary {
                    start_frame: 0,
                    end_frame: Some(300),
                    file_total_samples: 529_200,
                    sample_rate: 44_100,
                },
                MultiFileTrackBoundary {
                    start_frame: 300,
                    end_frame: Some(675),
                    file_total_samples: 529_200,
                    sample_rate: 44_100,
                },
                MultiFileTrackBoundary {
                    start_frame: 675,
                    end_frame: None,
                    file_total_samples: 529_200,
                    sample_rate: 44_100,
                },
            ]
        );

        let sectors = crate::tui::command::multi_file_cue_info_to_cd_sectors(
            &MultiFileCueInfo {
                layout,
                track_boundaries: boundaries,
            },
        )
        .expect("continuous TOC");
        assert_eq!(sectors, vec![150, 525, 900, 1200, 1575, 1800]);
        assert!(crate::tui::musicbrainz::build_mb_toc(&sectors).is_some());
    }

    #[test]
    fn non_divisible_sample_rate_preserves_authored_cue_frame_durations() {
        let cue = concat!(
            "FILE \"a.flac\" FLAC\n",
            "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
            "  TRACK 02 AUDIO\n    INDEX 01 00:00:01\n",
            "FILE \"b.flac\" FLAC\n",
            "  TRACK 03 AUDIO\n    INDEX 01 00:00:00\n",
            "  TRACK 04 AUDIO\n    INDEX 01 00:01:25\n",
        );
        let (_temp, layout) = admitted_multi_file_layout(cue, &["a.flac", "b.flac"]);
        let mut probes = std::collections::BTreeMap::new();
        // 43_094 samples at 32 kHz floor to an EOF position of exactly 101
        // CUE frames. The first authored duration is exactly one frame and must
        // not be rounded through samples.
        probes.insert(multi_file_probe_key(&layout.audio_paths[0]), (43_094, 32_000));
        // 85_334 samples floor to 200 CUE frames.
        probes.insert(multi_file_probe_key(&layout.audio_paths[1]), (85_334, 32_000));

        let boundaries = multi_file_track_boundaries_from_probes(&layout, &probes)
            .expect("32 kHz boundaries");
        let sectors = crate::tui::command::multi_file_cue_info_to_cd_sectors(
            &MultiFileCueInfo {
                layout,
                track_boundaries: boundaries,
            },
        )
        .expect("exact CUE-frame TOC");

        assert_eq!(sectors, vec![150, 151, 251, 351, 451]);
        assert!(crate::tui::musicbrainz::build_mb_toc(&sectors).is_some());
        let disc_id = crate::tui::gnudb::compute_disc_id_from_sectors(&sectors)
            .expect("exact GNUDB sector geometry");
        assert_eq!(disc_id.disc_id, "0b000404");
    }

    #[test]
    fn three_file_toc_preserves_file_local_index_geometry() {
        let cue = concat!(
            "FILE \"a.flac\" FLAC\n",
            "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
            "  TRACK 02 AUDIO\n    INDEX 01 00:01:00\n",
            "FILE \"b.flac\" FLAC\n",
            "  TRACK 03 AUDIO\n    INDEX 01 00:00:00\n",
            "  TRACK 04 AUDIO\n    INDEX 01 00:02:00\n",
            "  TRACK 05 AUDIO\n    INDEX 01 00:03:00\n",
            "FILE \"c.flac\" FLAC\n",
            "  TRACK 06 AUDIO\n    INDEX 01 00:00:00\n",
            "  TRACK 07 AUDIO\n    INDEX 01 00:01:00\n",
        );
        let (_temp, layout) = admitted_multi_file_layout(cue, &["a.flac", "b.flac", "c.flac"]);
        let mut probes = std::collections::BTreeMap::new();
        probes.insert(multi_file_probe_key(&layout.audio_paths[0]), (88_200, 44_100));
        probes.insert(multi_file_probe_key(&layout.audio_paths[1]), (128_000, 32_000));
        probes.insert(multi_file_probe_key(&layout.audio_paths[2]), (132_300, 44_100));

        let boundaries = multi_file_track_boundaries_from_probes(&layout, &probes)
            .expect("three-file boundaries");
        let sectors = crate::tui::command::multi_file_cue_info_to_cd_sectors(
            &MultiFileCueInfo {
                layout,
                track_boundaries: boundaries,
            },
        )
        .expect("three-file TOC");

        assert_eq!(sectors, vec![150, 225, 300, 450, 525, 600, 675, 825]);
        assert!(crate::tui::musicbrainz::build_mb_toc(&sectors).is_some());
    }

    #[test]
    fn four_file_boundaries_support_different_rates_and_track_counts() {
        let cue = concat!(
            "FILE \"a.flac\" FLAC\n",
            "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
            "  TRACK 02 AUDIO\n    INDEX 01 00:02:00\n",
            "FILE \"b.flac\" FLAC\n",
            "  TRACK 03 AUDIO\n    INDEX 01 00:00:00\n",
            "  TRACK 04 AUDIO\n    INDEX 01 00:03:00\n",
            "FILE \"c.flac\" FLAC\n",
            "  TRACK 05 AUDIO\n    INDEX 01 00:00:00\n",
            "  TRACK 06 AUDIO\n    INDEX 01 00:04:00\n",
            "FILE \"d.flac\" FLAC\n",
            "  TRACK 07 AUDIO\n    INDEX 01 00:00:00\n",
            "  TRACK 08 AUDIO\n    INDEX 01 00:05:00\n",
        );
        let (_temp, layout) =
            admitted_multi_file_layout(cue, &["a.flac", "b.flac", "c.flac", "d.flac"]);
        let specs = [
            (6_u64, 44_100_u32),
            (7_u64, 48_000_u32),
            (8_u64, 88_200_u32),
            (9_u64, 96_000_u32),
        ];
        let mut probes = std::collections::BTreeMap::new();
        for (path, (seconds, rate)) in layout.audio_paths.iter().zip(specs) {
            probes.insert(
                multi_file_probe_key(path),
                (seconds * rate as u64, rate),
            );
        }
        let boundaries = multi_file_track_boundaries_from_probes(&layout, &probes)
            .expect("four-file boundaries");
        let sectors = crate::tui::command::multi_file_cue_info_to_cd_sectors(
            &MultiFileCueInfo {
                layout,
                track_boundaries: boundaries,
            },
        )
        .expect("four-file TOC");
        assert_eq!(
            sectors,
            vec![150, 300, 600, 825, 1125, 1425, 1725, 2100, 2400]
        );
        assert!(crate::tui::musicbrainz::build_mb_toc(&sectors).is_some());
    }

    #[test]
    fn mixed_flac_wavpack_multi_file_cue_builds_deterministic_track_geometry() {
        let cue = concat!(
            "TITLE \"Mixed\"\n",
            "FILE \"side-a.flac\" FLAC\n",
            "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
            "  TRACK 02 AUDIO\n    INDEX 01 00:01:00\n",
            "FILE \"side-b.wv\" WAVE\n",
            "  TRACK 03 AUDIO\n    INDEX 01 00:00:00\n",
            "  TRACK 04 AUDIO\n    INDEX 01 00:02:00\n",
        );
        let (_temp, layout) =
            admitted_multi_file_layout(cue, &["side-a.flac", "side-b.wv"]);
        assert_eq!(
            layout
                .audio_paths
                .iter()
                .filter_map(|path| path.extension().and_then(|ext| ext.to_str()))
                .collect::<Vec<_>>(),
            vec!["flac", "wv"]
        );

        let mut probes = std::collections::BTreeMap::new();
        probes.insert(
            multi_file_probe_key(&layout.audio_paths[0]),
            (132_300, 44_100),
        );
        probes.insert(
            multi_file_probe_key(&layout.audio_paths[1]),
            (192_000, 48_000),
        );
        let boundaries = multi_file_track_boundaries_from_probes(&layout, &probes)
            .expect("mixed-codec boundaries");
        assert_eq!(
            boundaries
                .iter()
                .map(|boundary| boundary.sample_rate)
                .collect::<Vec<_>>(),
            vec![44_100, 44_100, 48_000, 48_000]
        );
        let sectors = crate::tui::command::multi_file_cue_info_to_cd_sectors(
            &MultiFileCueInfo {
                layout,
                track_boundaries: boundaries,
            },
        )
        .expect("mixed-codec TOC");
        assert_eq!(sectors, vec![150, 225, 375, 525, 675]);
    }

    #[test]
    fn native_multi_file_admission_obeys_cue_sidecar_policy() {
        let cue = r#"FILE "a.flac" FLAC
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 00:01:00
FILE "b.flac" FLAC
  TRACK 03 AUDIO
    INDEX 01 00:00:00
  TRACK 04 AUDIO
    INDEX 01 00:01:00
"#;
        let (_temp, layout) = admitted_multi_file_layout(cue, &["a.flac", "b.flac"]);
        let cue_path = layout.cue_path;

        assert!(multi_file_cue_layout_for_cue_with_policy(
            &cue_path,
            crate::convert::pipeline::CueSidecarPolicy::SidecarOnly,
        )
        .expect("SidecarOnly admission")
        .is_some());
        assert!(multi_file_cue_layout_for_cue_with_policy(
            &cue_path,
            crate::convert::pipeline::CueSidecarPolicy::PreferEmbedded,
        )
        .expect("PreferEmbedded falls back to the sidecar for a queued CUE source")
        .is_some());
        assert!(multi_file_cue_layout_for_cue_with_policy(
            &cue_path,
            crate::convert::pipeline::CueSidecarPolicy::IgnoreCue,
        )
        .expect("IgnoreCue result")
        .is_none());
        let error = multi_file_cue_layout_for_cue_with_policy(
            &cue_path,
            crate::convert::pipeline::CueSidecarPolicy::EmbeddedOnly,
        )
        .expect_err("EmbeddedOnly cannot resolve a native multi-FILE sidecar album");
        assert!(error.contains("require a sidecar CUE source"));
    }

    #[test]
    fn embedded_only_does_not_misclassify_an_unrelated_single_file_cue() {
        let temp = tempfile::tempdir().expect("tempdir");
        let audio = temp.path().join("album.flac");
        std::fs::write(&audio, b"audio").expect("audio fixture");
        let cue_path = temp.path().join("album.cue");
        std::fs::write(
            &cue_path,
            concat!(
                "FILE \"album.flac\" FLAC\n",
                "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
                "  TRACK 02 AUDIO\n    INDEX 01 00:01:00\n",
            ),
        )
        .expect("single-file CUE fixture");

        assert!(multi_file_cue_layout_for_cue_with_policy(
            &cue_path,
            crate::convert::pipeline::CueSidecarPolicy::EmbeddedOnly,
        )
        .expect("non-multi-FILE classification")
        .is_none());
    }

    #[test]
    fn rejected_multi_file_membership_is_not_silently_treated_as_plain_audio() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("side-a.flac"), b"audio").expect("side A");
        let cue_path = temp.path().join("album.cue");
        std::fs::write(
            &cue_path,
            concat!(
                "FILE \"side-a.flac\" FLAC\n",
                "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
                "  TRACK 02 AUDIO\n    INDEX 01 00:01:00\n",
                "FILE \"missing-side-b.flac\" FLAC\n",
                "  TRACK 03 AUDIO\n    INDEX 01 00:00:00\n",
                "  TRACK 04 AUDIO\n    INDEX 01 00:01:00\n",
            ),
        )
        .expect("cue fixture");

        let error = multi_file_cue_layout_for_cue(&cue_path)
            .expect_err("missing member must remain a visible admission failure");
        assert!(error.contains("member image missing"), "unexpected error: {error}");
    }

    #[test]
    fn multi_file_toc_fails_closed_above_ninety_nine_tracks() {
        let cue = concat!(
            "FILE \"a.flac\" FLAC\n",
            "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
            "  TRACK 02 AUDIO\n    INDEX 01 00:01:00\n",
            "FILE \"b.flac\" FLAC\n",
            "  TRACK 03 AUDIO\n    INDEX 01 00:00:00\n",
            "  TRACK 04 AUDIO\n    INDEX 01 00:01:00\n",
        );
        let (_temp, mut layout) = admitted_multi_file_layout(cue, &["a.flac", "b.flac"]);
        let template = layout.sheet.tracks.last().cloned().expect("track template");
        while layout.sheet.tracks.len() < 100 {
            let mut track = template.clone();
            track.number = layout.sheet.tracks.len() as u32 + 1;
            layout.sheet.tracks.push(track);
        }
        let boundaries = vec![
            MultiFileTrackBoundary {
                start_frame: 0,
                end_frame: Some(75),
                file_total_samples: 44_100,
                sample_rate: 44_100,
            };
            100
        ];
        let error = crate::tui::command::multi_file_cue_info_to_cd_sectors(
            &MultiFileCueInfo {
                layout,
                track_boundaries: boundaries,
            },
        )
        .expect_err("100-track TOC must fail closed");
        assert!(error.contains("limited to 99"), "unexpected error: {error}");
    }
}
