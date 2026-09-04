//! Embedded chapter structure, normalized at the conversion boundary.
//!
//! This module deliberately does **not** create a second CUE model. CUE remains
//! frame-native and authoring-capable in `convert::cue_parser`,
//! `tui::CueAlbumTrackSource`, and `tui::cue_generate`. Embedded container
//! chapters are read-only source facts. Materializers immediately normalize
//! them to sample-domain boundaries and then bridge them into the existing
//! `PreparedTrack` / sample-bounded carrier path used by CUE conversion.
//!
//! That bridge is the rule: source-specific representations stop here;
//! downstream conversion sees ordered tracks with sample boundaries and titles,
//! regardless of whether those facts originated in a CUE or a container.

use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEmbeddedChapter {
    pub title: Option<String>,
    pub start: i64,
    pub end: i64,
    pub time_base_num: i32,
    pub time_base_den: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramTrackBoundary {
    pub start_sample: u64,
    pub samples: u64,
    /// True only for the final structural track. Lossy sources use this bit to
    /// preserve the existing open-ended tail policy: decode EOF becomes fact.
    pub is_program_tail: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedChapterTrack {
    pub ordinal: u32,
    pub title: Option<String>,
    pub boundary: ProgramTrackBoundary,
}

fn ensure_ffmpeg_initialized() -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| ffmpeg_next::init().map_err(|error| error.to_string()))
        .clone()
}

/// Extensions for which Tonepoet should cheaply inspect embedded chapters
/// before choosing the ordinary one-track single-file fast path.
#[must_use]
pub fn chapter_capable_source_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("m4a" | "m4b" | "mp4")
    )
}

#[must_use]
pub fn is_m4b_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("m4b"))
}

/// Read the container-owned chapter table without mutating global FFmpeg log
/// state. The caller decides whether an empty table is acceptable for the
/// source type.
pub fn read_embedded_chapters(path: &Path) -> Result<Vec<RawEmbeddedChapter>, String> {
    ensure_ffmpeg_initialized()?;
    let input = ffmpeg_next::format::input(&path)
        .map_err(|error| format!("cannot open {} for chapter inspection: {error}", path.display()))?;

    let mut chapters = Vec::with_capacity(input.nb_chapters() as usize);
    for chapter in input.chapters() {
        let time_base = chapter.time_base();
        let title = chapter
            .metadata()
            .get("title")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        chapters.push(RawEmbeddedChapter {
            title,
            start: chapter.start(),
            end: chapter.end(),
            time_base_num: time_base.numerator(),
            time_base_den: time_base.denominator(),
        });
    }
    Ok(chapters)
}

fn timestamp_to_sample(
    timestamp: i64,
    time_base_num: i32,
    time_base_den: i32,
    sample_rate: u32,
) -> Result<u64, String> {
    if timestamp < 0 {
        return Err(format!("negative chapter timestamp {timestamp}"));
    }
    if time_base_num <= 0 || time_base_den <= 0 {
        return Err(format!(
            "invalid chapter time base {time_base_num}/{time_base_den}"
        ));
    }
    if sample_rate == 0 {
        return Err("chapter normalization requires a non-zero sample rate".to_string());
    }

    let numerator = i128::from(timestamp)
        .checked_mul(i128::from(time_base_num))
        .and_then(|value| value.checked_mul(i128::from(sample_rate)))
        .ok_or_else(|| "chapter timestamp overflow while converting to samples".to_string())?;
    let denominator = i128::from(time_base_den);
    // Container chapter clocks are often millisecond- or movie-timescale based.
    // Round to the nearest decoded sample; adjacent end/start timestamps are
    // reconciled below so the resulting track partition has no one-sample seam.
    let rounded = numerator
        .checked_add(denominator / 2)
        .ok_or_else(|| "chapter timestamp overflow while rounding to samples".to_string())?
        / denominator;
    u64::try_from(rounded).map_err(|_| "chapter timestamp exceeds u64 sample range".to_string())
}

/// Convert raw chapter timestamps to one contiguous, sample-domain program
/// partition. We reject structural gaps/overlaps instead of silently dropping
/// or duplicating audio. A one-sample disagreement is accepted as rational
/// timestamp rounding and reconciled to the following chapter's start.
pub fn normalize_embedded_chapters(
    chapters: &[RawEmbeddedChapter],
    sample_rate: u32,
) -> Result<Vec<EmbeddedChapterTrack>, String> {
    if chapters.is_empty() {
        return Ok(Vec::new());
    }

    let mut starts = Vec::with_capacity(chapters.len());
    let mut ends = Vec::with_capacity(chapters.len());
    for (index, chapter) in chapters.iter().enumerate() {
        let start = timestamp_to_sample(
            chapter.start,
            chapter.time_base_num,
            chapter.time_base_den,
            sample_rate,
        )?;
        let end = timestamp_to_sample(
            chapter.end,
            chapter.time_base_num,
            chapter.time_base_den,
            sample_rate,
        )?;
        if end <= start {
            return Err(format!(
                "embedded chapter {} has a non-positive duration ({}..{} samples)",
                index + 1,
                start,
                end
            ));
        }
        starts.push(start);
        ends.push(end);
    }

    if starts[0] > 1 {
        return Err(format!(
            "embedded chapters leave {} leading samples outside chapter structure",
            starts[0]
        ));
    }
    starts[0] = 0;

    for index in 0..chapters.len().saturating_sub(1) {
        if starts[index + 1] <= starts[index] {
            return Err(format!(
                "embedded chapter {} does not start after chapter {}",
                index + 2,
                index + 1
            ));
        }
        let end = ends[index];
        let next = starts[index + 1];
        let seam_error = end.abs_diff(next);
        if seam_error > 1 {
            let relation = if end < next { "gap" } else { "overlap" };
            return Err(format!(
                "embedded chapters {} and {} have a {relation} of {seam_error} samples",
                index + 1,
                index + 2
            ));
        }
        ends[index] = next;
    }

    let mut result = Vec::with_capacity(chapters.len());
    for index in 0..chapters.len() {
        let start = starts[index];
        let end = ends[index];
        let samples = end.checked_sub(start).ok_or_else(|| {
            format!("embedded chapter {} sample range underflowed", index + 1)
        })?;
        if samples == 0 {
            return Err(format!("embedded chapter {} contains no samples", index + 1));
        }
        result.push(EmbeddedChapterTrack {
            ordinal: u32::try_from(index + 1)
                .map_err(|_| "embedded chapter count exceeds u32".to_string())?,
            title: chapters[index].title.clone(),
            boundary: ProgramTrackBoundary {
                start_sample: start,
                samples,
                is_program_tail: index + 1 == chapters.len(),
            },
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter(title: &str, start_ms: i64, end_ms: i64) -> RawEmbeddedChapter {
        RawEmbeddedChapter {
            title: Some(title.to_string()),
            start: start_ms,
            end: end_ms,
            time_base_num: 1,
            time_base_den: 1_000,
        }
    }

    #[test]
    fn mp4_family_chapter_admission_is_case_insensitive() {
        assert!(chapter_capable_source_extension(Path::new("book.m4b")));
        assert!(chapter_capable_source_extension(Path::new("book.M4B")));
        assert!(chapter_capable_source_extension(Path::new("album.m4a")));
        assert!(chapter_capable_source_extension(Path::new("movie.mp4")));
        assert!(!chapter_capable_source_extension(Path::new("track.flac")));
        assert!(is_m4b_path(Path::new("book.M4B")));
    }

    #[test]
    fn millisecond_chapters_become_contiguous_sample_boundaries() {
        let tracks = normalize_embedded_chapters(
            &[
                chapter("Opening Credits", 0, 23_700),
                chapter("Prologue", 23_700, 1_004_120),
                chapter("One", 1_004_120, 1_845_340),
            ],
            22_050,
        )
        .expect("valid chapter table");

        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[0].ordinal, 1);
        assert_eq!(tracks[0].title.as_deref(), Some("Opening Credits"));
        assert_eq!(tracks[0].boundary.start_sample, 0);
        assert_eq!(
            tracks[0].boundary.start_sample + tracks[0].boundary.samples,
            tracks[1].boundary.start_sample
        );
        assert!(tracks[2].boundary.is_program_tail);
    }

    #[test]
    fn one_sample_clock_rounding_seam_is_reconciled() {
        let chapters = vec![
            RawEmbeddedChapter {
                title: Some("A".into()),
                start: 0,
                end: 1_001,
                time_base_num: 1,
                time_base_den: 1_000,
            },
            RawEmbeddedChapter {
                title: Some("B".into()),
                start: 1_000,
                end: 2_000,
                time_base_num: 1,
                time_base_den: 1_000,
            },
        ];
        let tracks = normalize_embedded_chapters(&chapters, 1_000).expect("one-sample seam");
        assert_eq!(tracks[0].boundary.samples, 1_000);
        assert_eq!(tracks[1].boundary.start_sample, 1_000);
    }

    #[test]
    fn real_gap_is_rejected_instead_of_dropping_audio() {
        let error = normalize_embedded_chapters(
            &[chapter("A", 0, 1_000), chapter("B", 1_100, 2_000)],
            48_000,
        )
        .expect_err("100 ms gap is not a usable partition");
        assert!(error.contains("gap"), "{error}");
    }

    #[test]
    fn leading_unstructured_audio_is_rejected() {
        let error = normalize_embedded_chapters(&[chapter("A", 100, 1_000)], 48_000)
            .expect_err("leading audio cannot be silently discarded");
        assert!(error.contains("leading samples"), "{error}");
    }
}
