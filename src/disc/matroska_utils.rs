//! File-backed audio stream mapping for Matroska and orphan MPEG transports.
//!
//! A media stream is one independently selectable presentation. A Matroska
//! chapter table is shared by the file, not owned by one audio stream; the
//! chapter boundaries are normalized against each stream's own sample clock.
//! Neither this probe nor the Browse menu decodes media or creates files.
//! Orphaned M2TS/VOB files have no disc navigation: they expose only whole streams.

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

/// Estimate the audio *output*, never the video-bearing transport file. Use
/// the audio stream's own duration, not the container's potentially longer
/// video duration. VBR and unknown-rate streams remain unbudgeted; the worker
/// handles a real disk-full error rather than an invented hard blocker.
fn transport_audio_bytes(codec: &str, duration: Option<f64>, bit_rate: i64,
    sample_rate: Option<u32>, channels: Option<u8>, bit_depth: Option<u32>) -> Option<u64> {
    let duration = duration?;
    let bytes_per_second = if matches!(codec, "pcm_dvd" | "pcm_bluray") {
        let depth = bit_depth?;
        if !(1..=32).contains(&depth) { return None; }
        f64::from(sample_rate?) * f64::from(channels?) * f64::from(depth.div_ceil(8))
    } else if matches!(codec, "ac3" | "mp2") && bit_rate > 0 {
        bit_rate as f64 / 8.0
    } else { return None; };
    let bytes = duration * bytes_per_second;
    (bytes.is_finite() && bytes > 0.0 && bytes <= u64::MAX as f64)
        .then_some(bytes.ceil() as u64)
}

/// Map exactly the Matroska audio streams and their validated chapters.
/// A malformed chapter table does not discard a valid whole-stream copy.
pub fn map_matroska_file(path: &Path) -> Result<DiscContents, String> {
    map_file_streams(path, FileStreamKind::Matroska)
}

/// A transport outside authored Blu-ray/DVD-Video navigation has independent
/// audio streams, but no trustworthy chapter boundaries. Never invent them
/// from timestamps, stream duration or MPEG packet boundaries.
pub(crate) fn map_orphan_transport_file(path: &Path) -> Result<DiscContents, String> {
    map_file_streams(path, FileStreamKind::OrphanTransport)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FileStreamKind { Matroska, OrphanTransport }

fn map_file_streams(path: &Path, kind: FileStreamKind) -> Result<DiscContents, String> {
    let supported = match kind {
        FileStreamKind::Matroska => is_matroska_path(path),
        FileStreamKind::OrphanTransport => super::on_disc_transport::is_transport_path(path)
            && !super::on_disc_transport::has_disc_location(path),
    };
    if !supported || !path.is_file() {
        return Err(format!("Not a regular supported standalone media file: {}", path.display()));
    }
    let raw_chapters = if kind == FileStreamKind::Matroska {
        read_embedded_chapters(path)?
    } else { Vec::new() };
    let input = ffmpeg_next::format::input(path)
        .map_err(|error| format!("Cannot inspect audio streams in '{}': {error}", path.display()))?;
    let metadata = input.metadata();
    let tags = disc_tags(&metadata);
    let container_duration = stream_duration(input.duration(), 1, 1_000_000);
    let mut presentations = Vec::new();
    let mut diagnostics = Vec::new();
    if kind == FileStreamKind::OrphanTransport {
        diagnostics.push(DiscDiagnostic {
            severity: DiagnosticSeverity::Info, scope: DiagnosticScope::Disc,
            message: "No chapters found: this standalone file lacks Blu-ray/DVD-Video navigation; only complete audio streams can be extracted".into(),
        });
    }

    for stream in input.streams().filter(|s| s.parameters().medium() == ffmpeg_next::media::Type::Audio) {
        let stream_index = u32::try_from(stream.index())
            .map_err(|_| "Matroska stream index exceeds u32".to_string())?;
        let audio_ordinal = u32::try_from(presentations.len() + 1)
            .map_err(|_| "Matroska audio stream count exceeds u32".to_string())?;
        let id = match kind {
            FileStreamKind::Matroska => PresentationId::MatroskaStream {
                stream_index, audio_ordinal,
                chapter_count: 0, // finalized after per-stream chapter normalization
            },
            FileStreamKind::OrphanTransport => PresentationId::TransportStream {
                stream_index, audio_ordinal,
            },
        };
        // An unknown or unsupported decoder must not prevent a byte-exact
        // packet copy. The elementary codec ID belongs to stream parameters;
        // decoded sample-clock facts are optional until chapter splitting.
        let params = stream.parameters();
        let codec = params.id().name().to_owned();
        // Codec parameters carry the original integer precision for FLAC and
        // other lossless compressed streams. Decoder sample_fmt can be s32 for
        // both 16- and 24-bit inputs; it is not the source bit depth.
        let (declared_depth, profile, bit_rate) = unsafe {
            let p = params.as_ptr();
            ((*p).bits_per_raw_sample, (*p).profile, (*p).bit_rate)
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
        let id = if kind == FileStreamKind::Matroska {
            PresentationId::MatroskaStream {
                stream_index, audio_ordinal,
                chapter_count: u32::try_from(chapters.len())
                    .map_err(|_| "Matroska chapter count exceeds u32".to_string())?,
            }
        } else { id };
        let mut authored_track_tags = Vec::new();
        let tracks = if chapters.is_empty() {
            authored_track_tags.push(DiscExtractTagFields::default());
            vec![DiscTrack { number: 1, title: Some("Whole stream".into()),
                performer: stream_tags.performer.clone(), duration_secs: duration,
                format_note: Some(if kind == FileStreamKind::OrphanTransport {
                    "No chapters: standalone file has no disc navigation"
                } else if raw_chapters.is_empty() { "No embedded chapters" }
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
        let estimated_audio_bytes = if kind == FileStreamKind::OrphanTransport {
            transport_audio_bytes(&codec, native_duration, bit_rate, rate, channels, bit_depth)
        } else { None };
        presentations.push(DiscPresentation {
            id, label, format: AudioPresentationFormat {
                codec: Some(codec.clone()), sample_rate: rate,
                bit_depth, channels, channel_layout,
                lossless, provenance: FormatProvenance::Unknown,
            }, tracks, total_duration_secs: duration.unwrap_or(0.0),
            extract: PresentationExtractInfo {
                transport_audio_bytes: estimated_audio_bytes,
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
        .unwrap_or_else(|| "Audio file".into());
    Ok(DiscContents {
        format: match kind { FileStreamKind::Matroska => DiscFormat::Matroska,
            FileStreamKind::OrphanTransport => DiscFormat::TransportFile },
        label, source_path: path.to_path_buf(),
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
    fn transport_size_is_from_audio_clock_not_container_video() {
        assert_eq!(transport_audio_bytes("ac3", Some(100.0), 192_000,
            None, None, None), Some(2_400_000));
        assert_eq!(transport_audio_bytes("pcm_dvd", Some(100.0), 0,
            Some(48_000), Some(2), Some(16)), Some(19_200_000));
        assert_eq!(transport_audio_bytes("pcm_bluray", Some(100.0), 0,
            Some(96_000), Some(6), Some(24)), Some(172_800_000));
        assert_eq!(transport_audio_bytes("ac3", None, 192_000,
            None, None, None), None);
        assert_eq!(transport_audio_bytes("ac3", Some(100.0), 0,
            None, None, None), None);
        assert_eq!(transport_audio_bytes("truehd", Some(100.0), 2_000_000,
            None, None, None), None); // variable bitrate is not a hard size fact
    }

    #[test]
    fn transport_video_payload_does_not_inflate_audio_capacity() {
        if Command::new("ffmpeg").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        for (extension, muxer) in [("m2ts", "mpegts"), ("vob", "vob")] {
            let source = temp.path().join(format!("movie.{extension}"));
            let generated = Command::new("ffmpeg").args([
                "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
                "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25:duration=5",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=5:sample_rate=48000",
                "-map", "0:v:0", "-map", "1:a:0", "-c:v", "mpeg2video", "-b:v", "4M",
                "-c:a", "ac3", "-b:a", "192k", "-f", muxer,
            ]).arg(&source).output().unwrap();
            assert!(generated.status.success(), "fixture: {}",
                String::from_utf8_lossy(&generated.stderr));
            let contents = map_orphan_transport_file(&source).unwrap();
            assert_eq!(contents.presentations.len(), 1);
            let presentation = &contents.presentations[0];
            let expected = presentation.extract.transport_audio_bytes.unwrap();
            let stream_index = match presentation.id {
                PresentationId::TransportStream { stream_index, .. } => stream_index,
                _ => panic!("not a transport stream"),
            };
            assert_eq!(stream_index, 1, "video is stream 0");
            let audio = temp.path().join(format!("audio-{extension}.ac3"));
            let copied = Command::new("ffmpeg").args([
                "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
                "-i",
            ]).arg(&source).args([
                "-map", "0:1", "-vn", "-sn", "-dn", "-c:a", "copy", "-f", "ac3",
            ]).arg(&audio).output().unwrap();
            assert!(copied.status.success());
            let actual = std::fs::metadata(&audio).unwrap().len();
            assert!(std::fs::metadata(&source).unwrap().len() > actual * 3);
            assert!(expected >= actual * 9 / 10 && expected <= actual * 11 / 10,
                "{extension}: estimated {expected}, actual {actual}");
            let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents, source);
            state.tags = false;
            state.extraction_log = false;
            state.destination = temp.path().join(format!("output-{extension}"));
            assert_eq!(state.selected_item_bytes().unwrap().0, expected);
            let reserve = state.native_required_volume_bytes(true).unwrap();
            assert!(reserve >= actual * 2, "private and publication copies must fit");
            assert!(reserve < 2 * 1024 * 1024, "video must not enter reserve: {reserve}");
        }
    }

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

    #[test]
    fn orphan_transport_probes_whole_streams_without_authored_chapters() {
        if Command::new("ffmpeg").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        for (extension, muxer) in [("M2TS", "mpegts"), ("vOb", "vob")] {
            let file = temp.path().join(format!("copied.{extension}"));
            let output = Command::new("ffmpeg").args([
                "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
                "-f", "lavfi", "-i", "sine=frequency=420:duration=0.3:sample_rate=48000",
                "-f", "lavfi", "-i", "sine=frequency=630:duration=0.3:sample_rate=48000",
                "-map", "0:a:0", "-map", "1:a:0", "-c:a", "ac3", "-b:a", "192k",
                "-f", muxer,
            ]).arg(&file).output().unwrap();
            assert!(output.status.success(), "{}: {}", extension,
                String::from_utf8_lossy(&output.stderr));
            let contents = super::super::on_disc_transport::probe_on_disc_transport(&file).unwrap();
            assert_eq!(contents.format, DiscFormat::TransportFile);
            assert_eq!(contents.presentations.len(), 2);
            assert!(contents.diagnostics.iter().any(|d| d.message.contains("No chapters found")
                && d.message.contains("navigation")));
            for (index, presentation) in contents.presentations.iter().enumerate() {
                assert_eq!(presentation.id, PresentationId::TransportStream {
                    stream_index: index as u32, audio_ordinal: index as u32 + 1,
                });
                assert_eq!(presentation.tracks.len(), 1);
                assert_eq!(presentation.tracks[0].title.as_deref(), Some("Whole stream"));
                assert!(presentation.tracks[0].format_note.as_deref().unwrap().contains("no disc navigation"));
                assert_eq!(presentation.format.codec.as_deref(), Some("ac3"));
            }
            let state = crate::tui::disc_browser::DiscBrowserState::new(contents, file);
            assert!(state.file_per_program);
            assert!(!state.cue);
            assert!(state.selected.iter().all(|yes| *yes));
            assert!(state.output_rows().contains(&"Folder"));
            assert!(state.output_rows().contains(&"Naming"));
            assert!(state.output_rows().contains(&"Tags"));
            assert!(state.output_rows().contains(&"Overwrite"));
            assert!(!state.output_rows().contains(&"CUE"));
        }
    }

}
