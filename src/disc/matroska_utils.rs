//! File-backed Matroska presentation mapper for #91 Phase B.
//!
//! A media stream is one independently selectable presentation. A Matroska
//! chapter table is shared by the file, not owned by one audio stream; the
//! chapter boundaries are normalized against each stream's own sample clock.
//! Neither this probe nor the Browse menu decodes media or creates files.

use std::path::Path;

use super::diagnostics::{DiagnosticScope, DiagnosticSeverity, DiscDiagnostic};
use super::model::{
    AudioPresentationFormat, CopyProtectionSummary, DiscContents, DiscExtractTagFields,
    DiscFormat, DiscPresentation, DiscTrack, FormatProvenance, PresentationExtractInfo,
    PresentationId,
};
use crate::convert::chapter_structure::{normalize_embedded_chapters, read_embedded_chapters};

/// The inexpensive menu predicate. All I/O is deferred to the background probe.
pub fn is_matroska_path(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext|
        ext.eq_ignore_ascii_case("mkv") || ext.eq_ignore_ascii_case("mka"))
}

fn tag(metadata: &ffmpeg_next::DictionaryRef<'_>, key: &str) -> Option<String> {
    metadata.get(key).map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned)
}

fn disc_tags(metadata: &ffmpeg_next::DictionaryRef<'_>) -> DiscExtractTagFields {
    DiscExtractTagFields {
        title: tag(metadata, "title"),
        artist: tag(metadata, "artist"),
        album: tag(metadata, "album"),
        genre: tag(metadata, "genre"),
        year: tag(metadata, "date").or_else(|| tag(metadata, "year")),
        composer: tag(metadata, "composer"),
        performer: tag(metadata, "performer"),
        catalog: tag(metadata, "catalog"),
        publisher: tag(metadata, "publisher"),
    }
}

fn audio_is_lossless(codec: &str) -> bool {
    codec.starts_with("pcm_") || matches!(codec,
        "flac" | "alac" | "wavpack" | "ape" | "tta" | "truehd" | "mlp" | "shorten")
}

fn reported_depth(codec: &str) -> Option<u32> {
    if codec.starts_with("pcm_") {
        if codec.contains("s8") || codec.contains("u8") { Some(8) }
        else if codec.contains("s16") || codec.contains("u16") { Some(16) }
        else if codec.contains("s24") || codec.contains("u24") { Some(24) }
        else if codec.contains("s32") || codec.contains("u32") || codec.contains("f32") { Some(32) }
        else if codec.contains("s64") || codec.contains("f64") { Some(64) }
        else { None }
    } else { None }
}

fn stream_duration(duration: i64, num: i32, den: i32) -> Option<f64> {
    (duration > 0 && num > 0 && den > 0)
        .then(|| duration as f64 * num as f64 / den as f64)
        .filter(|value| value.is_finite() && *value > 0.0)
}

/// Map exactly the audio streams and (if structurally valid) their chapters.
/// A malformed chapter table does not discard valid whole-stream copy: it is
/// surfaced as a warning, and only the whole-stream selection is exposed.
pub fn map_matroska_file(path: &Path) -> Result<DiscContents, String> {
    if !is_matroska_path(path) || !path.is_file() {
        return Err(format!("Not a regular MKV/MKA file: {}", path.display()));
    }
    let raw_chapters = read_embedded_chapters(path)?;
    let input = ffmpeg_next::format::input(path)
        .map_err(|error| format!("Cannot inspect Matroska streams in '{}': {error}", path.display()))?;
    let metadata = input.metadata();
    let tags = disc_tags(&metadata);
    let container_duration = stream_duration(input.duration(), 1, 1_000_000);
    let mut presentations = Vec::new();
    let mut diagnostics = Vec::new();

    for stream in input.streams().filter(|s| s.parameters().medium() == ffmpeg_next::media::Type::Audio) {
        let stream_index = u32::try_from(stream.index())
            .map_err(|_| "Matroska stream index exceeds u32".to_string())?;
        let audio_ordinal = u32::try_from(presentations.len() + 1)
            .map_err(|_| "Matroska audio stream count exceeds u32".to_string())?;
        let id = PresentationId::MatroskaStream {
            stream_index, audio_ordinal,
            chapter_count: 0, // finalized after per-stream chapter normalization
        };
        // An unknown or unsupported decoder must not prevent a byte-exact
        // packet copy. The elementary codec ID belongs to stream parameters;
        // decoded sample-clock facts are optional until chapter splitting.
        let params = stream.parameters();
        let codec = params.id().name().to_owned();
        // Codec parameters carry the original integer precision for FLAC and
        // other lossless compressed streams. Decoder sample_fmt can be s32 for
        // both 16- and 24-bit inputs; it is not the source bit depth.
        let (declared_depth, profile) = unsafe {
            let p = params.as_ptr();
            ((*p).bits_per_raw_sample, (*p).profile)
        };
        let decoded_info = ffmpeg_next::codec::context::Context::from_parameters(params)
            .ok().and_then(|context| context.decoder().audio().ok());
        let rate = decoded_info.as_ref().map(|audio| audio.rate()).filter(|r| *r > 0);
        let channels = decoded_info.as_ref()
            .and_then(|audio| u8::try_from(audio.channels()).ok()).filter(|c| *c > 0);
        // A decoder-reported native speaker assignment is evidence; channel
        // count alone is not. Retain its exact WAVE speaker mask, including
        // the distinction between rear and side 5.1 surrounds.
        let channel_layout = decoded_info.as_ref().and_then(|audio| {
            let layout = audio.channel_layout();
            let mask = u32::try_from(layout.bits()).ok()?;
            if mask == 0 || mask.count_ones() != audio.channels() as u32 {
                return None;
            }
            Some(match mask {
                0x4 => "mono".to_owned(),
                0x3 => "stereo".to_owned(),
                0x3f => "5.1(back)".to_owned(),
                0x60f => "5.1(side)".to_owned(),
                0x63f => "7.1(side)".to_owned(),
                0xff => "7.1(wide)".to_owned(),
                _ => format!("0x{mask:x}"),
            })
        });
        let stream_metadata = stream.metadata();
        let stream_tags = disc_tags(&stream_metadata);
        let title = stream_tags.title.clone()
            .unwrap_or_else(|| format!("Audio stream {audio_ordinal}"));
        let native_duration = stream_duration(stream.duration(),
            stream.time_base().numerator(), stream.time_base().denominator());
        let duration = native_duration.or(container_duration);
        let normalized = if raw_chapters.is_empty() { None }
            else { rate.map(|r| normalize_embedded_chapters(&raw_chapters, r)) };
        let chapters = match normalized {
            Some(Ok(chapters)) => chapters,
            Some(Err(error)) => {
                diagnostics.push(DiscDiagnostic { severity: DiagnosticSeverity::Warning,
                    scope: DiagnosticScope::Presentation(id),
                    message: format!("Audio stream {audio_ordinal}: chapters cannot be split ({error}); whole-stream copy remains available") });
                Vec::new()
            }
            None => {
                if !raw_chapters.is_empty() {
                    diagnostics.push(DiscDiagnostic { severity: DiagnosticSeverity::Warning,
                        scope: DiagnosticScope::Presentation(id),
                        message: format!("Audio stream {audio_ordinal}: sample rate unavailable; whole-stream copy only") });
                }
                Vec::new()
            }
        };
        let id = PresentationId::MatroskaStream {
            stream_index, audio_ordinal,
            chapter_count: u32::try_from(chapters.len())
                .map_err(|_| "Matroska chapter count exceeds u32".to_string())?,
        };
        let mut authored_track_tags = Vec::new();
        let tracks = if chapters.is_empty() {
            authored_track_tags.push(DiscExtractTagFields::default());
            vec![DiscTrack { number: 1, title: Some("Whole stream".into()),
                performer: stream_tags.performer.clone(), duration_secs: duration,
                format_note: Some(if raw_chapters.is_empty() { "No embedded chapters" }
                    else { "Whole stream only (chapter split unavailable)" }.into()) }]
        } else {
            chapters.iter().map(|chapter| {
                let title = chapter.title.as_ref().filter(|t| !t.trim().is_empty()).cloned();
                authored_track_tags.push(DiscExtractTagFields { title: title.clone(),
                    ..Default::default() });
                DiscTrack { number: chapter.ordinal, title: title.clone()
                    .or_else(|| Some(format!("Chapter {}", chapter.ordinal))),
                    performer: stream_tags.performer.clone(),
                    duration_secs: rate.map(|r| chapter.boundary.samples as f64 / f64::from(r)),
                    format_note: None }
            }).collect()
        };
        let label = if let Some(language) = tag(&stream_metadata, "language") {
            format!("{title} ({language})")
        } else { title };
        // FF_PROFILE_DTS_HD_MA == 60; other DTS profiles may be lossy.
        let lossless = audio_is_lossless(&codec) || (codec == "dts" && profile == 60);
        let bit_depth = (declared_depth > 0).then_some(declared_depth as u32)
            .or_else(||reported_depth(&codec))
            .or_else(|| lossless.then(|| decoded_info.as_ref()
                .map(|audio| (audio.format().bytes() as u32) * 8)).flatten());
        presentations.push(DiscPresentation {
            id, label, format: AudioPresentationFormat {
                codec: Some(codec.clone()), sample_rate: rate,
                bit_depth, channels, channel_layout,
                lossless, provenance: FormatProvenance::Unknown,
            }, tracks, total_duration_secs: duration.unwrap_or(0.0),
            extract: PresentationExtractInfo {
                authored_album_tags: stream_tags.prioritized(&tags),
                authored_track_tags, ..Default::default()
            }, album_title: tags.album.clone(),
            album_artist: tags.artist.clone(), genre: tags.genre.clone(), year: tags.year.clone(),
        });
    }
    if presentations.is_empty() {
        return Err(format!("{} contains no audio streams", path.display()));
    }
    let label = path.file_stem().map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Matroska file".into());
    Ok(DiscContents {
        format: DiscFormat::Matroska, label, source_path: path.to_path_buf(),
        presentations, suppressed: Vec::new(), copy_protection: CopyProtectionSummary {
            description: "Not applicable (file source)".into(),
        }, diagnostics, album_title: tags.album, album_artist: tags.artist,
        genre: tags.genre, year: tags.year,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn admission_is_only_mkv_mka() {
        for file in ["audio.mka", "MOVIE.MKV", "part.MkV"] {
            assert!(is_matroska_path(Path::new(file)));
        }
        for file in ["orphan.m2ts", "file.vob", "movie.webm", "album.cue"] {
            assert!(!is_matroska_path(Path::new(file)));
        }
    }

    #[test]
    fn multiple_audio_streams_and_embedded_chapters_are_discoverable() {
        if Command::new("ffmpeg").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        let chapters = temp.path().join("chapters.txt");
        std::fs::write(&chapters, ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=First\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=500\nEND=1000\ntitle=Second\n").unwrap();
        let file = temp.path().join("two-stream.MKA");
        let status = Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=1:sample_rate=48000",
            "-f", "lavfi", "-i", "sine=frequency=550:duration=1:sample_rate=48000",
            "-f", "ffmetadata", "-i"]).arg(&chapters)
            .args(["-map", "0:a:0", "-map", "1:a:0", "-map_chapters", "2", "-c:a", "flac"])
            .arg(&file).stdin(std::process::Stdio::null()).status().unwrap();
        assert!(status.success(), "FFmpeg fixture failed");
        let info = map_matroska_file(&file).unwrap();
        assert_eq!(info.format, DiscFormat::Matroska);
        assert_eq!(info.presentations.len(), 2);
        assert_eq!(info.presentations[0].tracks.len(), 2);
        assert_eq!(info.presentations[0].tracks[0].title.as_deref(), Some("First"));
        assert_eq!(info.presentations[1].tracks[1].title.as_deref(), Some("Second"));
        assert_eq!(info.presentations[0].id, PresentationId::MatroskaStream {
            stream_index: 0, audio_ordinal: 1, chapter_count: 2,
        });
        assert_eq!(info.presentations[1].id, PresentationId::MatroskaStream {
            stream_index: 1, audio_ordinal: 2, chapter_count: 2,
        });
    }
}
