//! Blu-ray backend metadata to unified disc-browser model mapping.
//!
//! This mirrors the DVD-Video mapper: it turns authored playlist/chapter/audio
//! stream data into curated `DiscPresentation` values, carries suppressed
//! candidates with reasons, and orders presentations deterministically for the
//! browser and convert view.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::Path;

use super::bluray_backend::{
    BluRayAudioCoding, BluRayAudioStreamKind, BlurayAudioStreamInfo, BlurayBackend,
    BlurayChapterInfo, BlurayDisplayAngle, BlurayTitleInfo,
    BlurayUnsupportedStreamDiagnostic, ProbeDepth,
};
use super::model::*;

const MIN_PLAYLIST_DURATION_SECS: f64 = 30.0;

/// Build unified disc contents for a parsed Blu-ray source.
pub fn map_bluray_disc<B: BlurayBackend>(
    disc: &B::Disc,
    source_path: &Path,
) -> Result<DiscContents, String> {
    let label = blu_ray_disc_label::<B>(disc, source_path);
    let titles = B::titles(disc)?;
    let mut presentations = Vec::new();
    let mut suppressed = Vec::new();
    let mut signatures: HashMap<PlaylistSignature, u32> = HashMap::new();
    let protection_status = B::protection_status(disc);

    for title in titles {
        let playlist_number = title.playlist_number;
        let max_angle = B::max_angle(disc, title.key)
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(title.angle_count.max(1));
        let display_angles: Vec<BlurayDisplayAngle> = (1..=max_angle)
            .filter_map(|angle| BlurayDisplayAngle::new(angle).ok())
            .collect();
        let first_angle = display_angles
            .first()
            .copied()
            .unwrap_or_else(BlurayDisplayAngle::first);

        let chapters = match chapters_for_angle::<B>(disc, &title, first_angle) {
            Ok(chapters) => chapters,
            Err(err) => {
                suppressed.push(suppressed_title(
                    playlist_number,
                    0,
                    0,
                    first_angle,
                    0,
                    0.0,
                    format!("chapter metadata failed: {err}"),
                    Some(format!("playlist {playlist_number:05}")),
                ));
                continue;
            }
        };

        if chapters.is_empty() {
            suppressed.push(suppressed_title(
                playlist_number,
                0,
                0,
                first_angle,
                0,
                0.0,
                "title contains no chapters".to_string(),
                Some(format!("playlist {playlist_number:05}")),
            ));
            continue;
        }

        let metadata_enumeration = match B::stream_enumeration_with_probe_policy(
            disc,
            title.key,
            ProbeDepth::None,
        ) {
            Ok(enumeration) => enumeration,
            Err(err) => {
                suppressed.push(suppressed_title(
                    playlist_number,
                    0,
                    0,
                    first_angle,
                    chapters.len(),
                    title_duration_secs(&title, &chapters),
                    format!("audio stream metadata failed: {err}"),
                    Some(format!("playlist {playlist_number:05}")),
                ));
                continue;
            }
        };

        let stream_enumeration = if protection_status.may_read_media_for_probe() {
            match B::stream_enumeration_with_probe_policy(
                disc,
                title.key,
                ProbeDepth::bounded_default(),
            ) {
                Ok(enumeration) => enumeration,
                Err(err) => {
                    suppressed.push(suppressed_title(
                        playlist_number,
                        0,
                        0,
                        first_angle,
                        chapters.len(),
                        title_duration_secs(&title, &chapters),
                        format!(
                            "bounded LPCM probe failed; using metadata-only stream metadata: {err}"
                        ),
                        Some(format!("playlist {playlist_number:05}")),
                    ));
                    metadata_enumeration
                }
            }
        } else {
            metadata_enumeration
        };

        let title_total_duration_secs = title_duration_secs(&title, &chapters);
        for diagnostic in &stream_enumeration.stream_diagnostics {
            suppressed.push(suppressed_stream_diagnostic(
                playlist_number,
                first_angle,
                chapters.len(),
                title_total_duration_secs,
                diagnostic,
            ));
        }

        let mut streams = stream_enumeration.supported_streams;
        streams.retain(is_supported_browse_stream);

        if streams.is_empty() {
            suppressed.push(suppressed_title(
                playlist_number,
                0,
                0,
                first_angle,
                chapters.len(),
                title_duration_secs(&title, &chapters),
                "title declares no supported primary audio streams".to_string(),
                Some(format!("playlist {playlist_number:05}")),
            ));
            continue;
        }

        let total_duration_secs = title_duration_secs(&title, &chapters);
        if total_duration_secs > 0.0 && total_duration_secs < MIN_PLAYLIST_DURATION_SECS {
            let stream = &streams[0];
            suppressed.push(suppressed_title(
                playlist_number,
                stream.pid,
                stream.stream_index,
                first_angle,
                chapters.len(),
                total_duration_secs,
                format!(
                    "title is shorter than {:.0} seconds and is likely a menu or intro",
                    MIN_PLAYLIST_DURATION_SECS
                ),
                Some(format!("playlist {playlist_number:05}")),
            ));
            continue;
        }

        let signature = playlist_signature(&chapters, total_duration_secs, &streams);
        if let Some(original_playlist) = signatures.get(&signature) {
            let stream = &streams[0];
            suppressed.push(suppressed_title(
                playlist_number,
                stream.pid,
                stream.stream_index,
                first_angle,
                chapters.len(),
                total_duration_secs,
                format!("duplicate playlist of {original_playlist:05}"),
                Some(format!("playlist {playlist_number:05}")),
            ));
            continue;
        }
        signatures.insert(signature, playlist_number);

        for display_angle in display_angles {
            let angle_chapters = if display_angle == first_angle {
                chapters.clone()
            } else {
                match chapters_for_angle::<B>(disc, &title, display_angle) {
                    Ok(chapters) => chapters,
                    Err(err) => {
                        for stream in &streams {
                            suppressed.push(suppressed_title(
                                playlist_number,
                                stream.pid,
                                stream.stream_index,
                                display_angle,
                                0,
                                0.0,
                                format!("angle {} chapter metadata failed: {err}", display_angle.get()),
                                Some(format!("playlist {playlist_number:05} angle {}", display_angle.get())),
                            ));
                        }
                        continue;
                    }
                }
            };

            if angle_chapters.is_empty() {
                for stream in &streams {
                    suppressed.push(suppressed_title(
                        playlist_number,
                        stream.pid,
                        stream.stream_index,
                        display_angle,
                        0,
                        0.0,
                        "angle contains no chapters".to_string(),
                        Some(format!("playlist {playlist_number:05} angle {}", display_angle.get())),
                    ));
                }
                continue;
            }

            for stream in &streams {
                let format = format_for_stream(stream);
                let tracks = tracks_for_chapters(&angle_chapters, max_angle, display_angle.get());
                let total_duration_secs = tracks_total_duration_secs(&tracks)
                    .unwrap_or_else(|| title_duration_secs(&title, &angle_chapters));
                let mut presentation = DiscPresentation {
                    id: PresentationId::blu_ray_title(
                        playlist_number,
                        stream.pid,
                        stream.stream_index,
                        display_angle,
                    ),
                    label: String::new(),
                    format,
                    tracks,
                    total_duration_secs,
                    album_title: None,
                    album_artist: None,
                    genre: None,
                    year: None,
                };
                presentation.label = presentation_label(&presentation, stream.coding, max_angle);
                presentations.push(presentation);
            }
        }
    }

    presentations.sort_by(|a, b| score_bluray_presentation(b).cmp(&score_bluray_presentation(a)));

    Ok(DiscContents {
        format: DiscFormat::BluRay,
        label,
        source_path: source_path.to_path_buf(),
        presentations,
        suppressed,
        copy_protection: CopyProtectionSummary {
            description: protection_status.summary(),
        },
        diagnostics: Vec::new(),
        album_title: None,
        album_artist: None,
        genre: None,
        year: None,
    })
}

/// Ranking key for Blu-ray presentation auto-selection. Higher values rank
/// ahead, except playlist/stream/angle fields which use `Reverse` for stable
/// lower-number tiebreakers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlurayPresentationScore {
    has_metadata: u8,
    has_chapter_durations: u8,
    chapter_count: usize,
    total_duration_millis: u64,
    is_stereo: u8,
    is_lossless: u8,
    codec_rank: u8,
    sample_rate: u32,
    bit_depth: u32,
    playlist_number: Reverse<u32>,
    audio_stream_index: Reverse<u8>,
    display_angle: Reverse<u8>,
}

#[must_use]
pub fn score_bluray_presentation(presentation: &DiscPresentation) -> BlurayPresentationScore {
    let (playlist_number, _pid, audio_stream_index, display_angle) = presentation
        .id
        .blu_ray_parts()
        .unwrap_or((u32::MAX, u16::MAX, u8::MAX, u8::MAX));

    BlurayPresentationScore {
        has_metadata: presentation_has_metadata(presentation) as u8,
        has_chapter_durations: has_computable_chapter_durations(presentation) as u8,
        chapter_count: presentation.tracks.len(),
        total_duration_millis: duration_millis(presentation.total_duration_secs),
        is_stereo: presentation_is_stereo(presentation) as u8,
        is_lossless: presentation.format.lossless as u8,
        codec_rank: codec_rank_from_format(&presentation.format),
        sample_rate: presentation.format.sample_rate.unwrap_or(0),
        bit_depth: presentation.format.bit_depth.unwrap_or(0),
        playlist_number: Reverse(playlist_number),
        audio_stream_index: Reverse(audio_stream_index),
        display_angle: Reverse(display_angle),
    }
}

#[must_use]
pub fn best_bluray_presentation_index(contents: &DiscContents) -> Option<usize> {
    contents
        .presentations
        .iter()
        .enumerate()
        .max_by_key(|(_, presentation)| score_bluray_presentation(presentation))
        .map(|(index, _)| index)
}

/// Rebuild labels after a future sidecar overlay populates album/track fields.
pub fn refresh_bluray_presentation_labels(contents: &mut DiscContents) {
    for presentation in &mut contents.presentations {
        let max_angle = max_angle_from_tracks(presentation).unwrap_or(1);
        let coding = coding_from_format(&presentation.format).unwrap_or(BluRayAudioCoding::Ac3);
        presentation.label = presentation_label(presentation, coding, max_angle);
    }
    contents
        .presentations
        .sort_by(|a, b| score_bluray_presentation(b).cmp(&score_bluray_presentation(a)));
}

fn blu_ray_disc_label<B: BlurayBackend>(disc: &B::Disc, source_path: &Path) -> String {
    B::disc_label(disc, source_path)
        .filter(|label| !label.trim().is_empty())
        .or_else(|| {
            source_path
                .file_stem()
                .and_then(|value| value.to_str())
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "Blu-ray Disc".to_string())
}

fn chapters_for_angle<B: BlurayBackend>(
    disc: &B::Disc,
    title: &BlurayTitleInfo,
    display_angle: BlurayDisplayAngle,
) -> Result<Vec<BlurayChapterInfo>, String> {
    B::chapters(disc, title.key, display_angle)
}

fn is_supported_browse_stream(stream: &BlurayAudioStreamInfo) -> bool {
    stream.kind == BluRayAudioStreamKind::Primary
}

fn format_for_stream(stream: &BlurayAudioStreamInfo) -> AudioPresentationFormat {
    AudioPresentationFormat {
        codec: Some(stream.coding.label().to_string()),
        sample_rate: stream.sample_rate,
        bit_depth: match stream.coding {
            BluRayAudioCoding::Lpcm => stream.bit_depth.bit_depth(),
            _ => None,
        },
        channels: stream.channels,
        channel_layout: stream
            .channel_layout
            .as_deref()
            .map(display_channel_layout)
            .or_else(|| stream.channels.map(channel_layout_from_count)),
        lossless: stream.coding.is_lossless(),
        provenance: FormatProvenance::Unknown,
    }
}

fn tracks_for_chapters(
    chapters: &[BlurayChapterInfo],
    max_angle: u8,
    display_angle: u8,
) -> Vec<DiscTrack> {
    chapters
        .iter()
        .enumerate()
        .map(|(index, chapter)| DiscTrack {
            number: chapter.chapter_number,
            title: Some(format!("Chapter {}", chapter.chapter_number)),
            performer: None,
            duration_secs: chapter_duration_secs(chapter)
                .or_else(|| inferred_chapter_duration_secs(chapters, index)),
            format_note: (max_angle > 1).then(|| format!("Angle {display_angle}/{max_angle}")),
        })
        .collect()
}

fn chapter_duration_secs(chapter: &BlurayChapterInfo) -> Option<f64> {
    chapter.duration_pts_90k.map(pts_90k_to_secs).or_else(|| {
        chapter
            .end_pts_90k
            .and_then(|end| end.checked_sub(chapter.start_pts_90k))
            .map(pts_90k_to_secs)
    })
}

fn inferred_chapter_duration_secs(chapters: &[BlurayChapterInfo], index: usize) -> Option<f64> {
    let current = chapters.get(index)?;
    let next = chapters.get(index + 1)?;
    next.start_pts_90k
        .checked_sub(current.start_pts_90k)
        .map(pts_90k_to_secs)
}

fn title_duration_secs(title: &BlurayTitleInfo, chapters: &[BlurayChapterInfo]) -> f64 {
    if title.duration_pts_90k > 0 {
        return title.duration_secs();
    }
    chapters
        .iter()
        .filter_map(chapter_duration_secs)
        .fold(0.0, |acc, value| acc + value)
}

fn tracks_total_duration_secs(tracks: &[DiscTrack]) -> Option<f64> {
    let mut total = 0.0;
    for track in tracks {
        total += track.duration_secs?;
    }
    Some(total)
}

fn pts_90k_to_secs(value: u64) -> f64 {
    value as f64 / 90_000.0
}

/// Build the format-aware Blu-ray label used by disc browsing and convert view.
///
/// `PresentationId::display_label()` intentionally stays identity-only because
/// codec, rate, depth, and channel layout live on `DiscPresentation::format`.
pub fn format_aware_bluray_presentation_label(presentation: &DiscPresentation) -> String {
    let max_angle = max_angle_from_tracks(presentation).unwrap_or(1);
    let coding = coding_from_format(&presentation.format).unwrap_or(BluRayAudioCoding::Ac3);
    presentation_label(presentation, coding, max_angle)
}

fn presentation_label(
    presentation: &DiscPresentation,
    coding: BluRayAudioCoding,
    max_angle: u8,
) -> String {
    let base = bluray_identity_and_format_label(presentation, coding, max_angle);
    if let Some(album_title) = presentation
        .album_title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return format!("{album_title} · {base}");
    }
    base
}

fn bluray_identity_and_format_label(
    presentation: &DiscPresentation,
    coding: BluRayAudioCoding,
    max_angle: u8,
) -> String {
    let (playlist_number, pid, stream_index, display_angle) = presentation
        .id
        .blu_ray_parts()
        .unwrap_or((0, 0, 0, 1));
    let angle = if max_angle > 1 {
        format!(" · Angle {display_angle}/{max_angle}")
    } else {
        String::new()
    };

    format!(
        "Blu-ray Playlist {playlist_number:05} Stream {} PID 0x{pid:04x} · {}{angle}",
        blu_ray_audio_stream_display_number(stream_index),
        format_details_from_presentation(presentation, coding)
    )
}

fn format_details_from_presentation(
    presentation: &DiscPresentation,
    coding: BluRayAudioCoding,
) -> String {
    let mut details = Vec::new();
    if let Some(rate) = presentation.format.sample_rate {
        details.push(sample_rate_display(rate));
    }
    if let Some(bit_depth) = presentation.format.bit_depth {
        details.push(format!("{bit_depth}-bit"));
    }
    if let Some(layout) = presentation.format.channel_layout.as_deref() {
        details.push(layout.to_string());
    }

    let codec_label = presentation
        .format
        .codec
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| coding.label());

    if details.is_empty() {
        codec_label.to_string()
    } else {
        format!("{codec_label} {}", details.join(" / "))
    }
}

fn sample_rate_display(sample_rate: u32) -> String {
    let khz = sample_rate as f64 / 1000.0;
    if sample_rate % 1000 == 0 {
        format!("{khz:.0} kHz")
    } else {
        format!("{khz:.1} kHz")
    }
}

fn display_channel_layout(layout: &str) -> String {
    match layout.trim().to_ascii_lowercase().as_str() {
        "mono" => "Mono".to_string(),
        "stereo" => "Stereo".to_string(),
        "5.0" => "5.0".to_string(),
        "5.1" => "5.1".to_string(),
        "7.0" => "7.0".to_string(),
        "7.1" => "7.1".to_string(),
        other if other.ends_with("ch") => other.to_string(),
        _ => layout.to_string(),
    }
}

fn channel_layout_from_count(channels: u8) -> String {
    match channels {
        1 => "Mono".to_string(),
        2 => "Stereo".to_string(),
        6 => "5.1".to_string(),
        8 => "7.1".to_string(),
        n => format!("{n}ch"),
    }
}

fn presentation_has_metadata(presentation: &DiscPresentation) -> bool {
    presentation.album_title.is_some()
        || presentation.album_artist.is_some()
        || presentation.genre.is_some()
        || presentation.year.is_some()
        || presentation.tracks.iter().any(|track| {
            track
                .title
                .as_deref()
                .is_some_and(|title| !title.starts_with("Chapter "))
                || track.performer.is_some()
        })
}

fn has_computable_chapter_durations(presentation: &DiscPresentation) -> bool {
    !presentation.tracks.is_empty()
        && presentation
            .tracks
            .iter()
            .all(|track| track.duration_secs.is_some())
}

fn presentation_is_stereo(presentation: &DiscPresentation) -> bool {
    presentation.format.channels == Some(2)
        || presentation
            .format
            .channel_layout
            .as_deref()
            .is_some_and(|layout| layout.eq_ignore_ascii_case("stereo"))
}

fn codec_rank_from_format(format: &AudioPresentationFormat) -> u8 {
    format
        .codec
        .as_deref()
        .and_then(coding_from_label)
        .map(BluRayAudioCoding::codec_rank)
        .unwrap_or(0)
}

fn coding_from_format(format: &AudioPresentationFormat) -> Option<BluRayAudioCoding> {
    format.codec.as_deref().and_then(coding_from_label)
}

fn coding_from_label(label: &str) -> Option<BluRayAudioCoding> {
    match label.trim().to_ascii_lowercase().as_str() {
        "lpcm" => Some(BluRayAudioCoding::Lpcm),
        "ac-3" | "ac3" => Some(BluRayAudioCoding::Ac3),
        "e-ac-3" | "eac3" => Some(BluRayAudioCoding::Eac3),
        "dts" => Some(BluRayAudioCoding::Dts),
        "truehd" | "true hd" => Some(BluRayAudioCoding::TrueHd),
        "dts-hd hr" | "dts-hd" => Some(BluRayAudioCoding::DtsHd),
        "dts-hd ma" | "dts-hd master" => Some(BluRayAudioCoding::DtsHdMaster),
        _ => None,
    }
}

fn max_angle_from_tracks(presentation: &DiscPresentation) -> Option<u8> {
    presentation.tracks.iter().find_map(|track| {
        let note = track.format_note.as_deref()?;
        let (_, denominator) = note.rsplit_once('/')?;
        denominator.trim().parse().ok()
    })
}

fn duration_millis(duration_secs: f64) -> u64 {
    if duration_secs.is_finite() && duration_secs > 0.0 {
        (duration_secs * 1000.0).round() as u64
    } else {
        0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PlaylistSignature {
    chapter_count: usize,
    total_duration_millis: u64,
    streams: Vec<StreamSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StreamSignature {
    kind: BluRayAudioStreamKind,
    stream_index: u8,
    coding: BluRayAudioCoding,
    sample_rate: Option<u32>,
    bit_depth: Option<u32>,
    channels: Option<u8>,
    channel_layout: Option<String>,
    language: Option<String>,
}

fn playlist_signature(
    chapters: &[BlurayChapterInfo],
    total_duration_secs: f64,
    streams: &[BlurayAudioStreamInfo],
) -> PlaylistSignature {
    PlaylistSignature {
        chapter_count: chapters.len(),
        total_duration_millis: duration_millis(total_duration_secs),
        streams: streams
            .iter()
            .map(|stream| StreamSignature {
                kind: stream.kind,
                stream_index: stream.stream_index,
                coding: stream.coding,
                sample_rate: stream.sample_rate,
                bit_depth: stream.bit_depth.bit_depth(),
                channels: stream.channels,
                channel_layout: stream.channel_layout.clone(),
                language: stream.language.clone(),
            })
            .collect(),
    }
}

fn suppressed_title(
    playlist_number: u32,
    audio_pid: u16,
    audio_stream_index: u8,
    display_angle: BlurayDisplayAngle,
    track_count: usize,
    duration_secs: f64,
    reason: String,
    native_detail: Option<String>,
) -> SuppressedPresentation {
    SuppressedPresentation {
        id: PresentationId::blu_ray_title(
            playlist_number,
            audio_pid,
            audio_stream_index,
            display_angle,
        ),
        reason,
        track_count,
        duration_secs,
        native_detail,
    }
}

fn suppressed_stream_diagnostic(
    playlist_number: u32,
    display_angle: BlurayDisplayAngle,
    track_count: usize,
    duration_secs: f64,
    diagnostic: &BlurayUnsupportedStreamDiagnostic,
) -> SuppressedPresentation {
    suppressed_title(
        playlist_number,
        diagnostic.pid.unwrap_or(0),
        diagnostic.stream_index.unwrap_or(0),
        display_angle,
        track_count,
        duration_secs,
        format!("unsupported audio stream ignored: {}", diagnostic.summary()),
        Some(format!("playlist {playlist_number:05}")),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap as TestHashMap;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::disc::bluray_backend::{
        BlurayAacsStatus, BlurayBackendCapability, BlurayLpcmBitDepth,
        BlurayProtectionStatus, BlurayPtsContinuitySegment, BlurayStreamDecryptor,
        BlurayStreamEnumeration, BlurayTitleKey,
    };

    #[derive(Debug, Clone)]
    struct FakeTitle {
        info: BlurayTitleInfo,
        chapters: Vec<BlurayChapterInfo>,
        chapters_by_display_angle: TestHashMap<u8, Vec<BlurayChapterInfo>>,
        streams: Vec<BlurayAudioStreamInfo>,
        stream_diagnostics: Vec<BlurayUnsupportedStreamDiagnostic>,
        bounded_probe_error: Option<String>,
    }

    #[derive(Debug, Clone)]
    struct FakeDisc {
        label: Option<String>,
        titles: Vec<FakeTitle>,
        protection_status: BlurayProtectionStatus,
    }

    struct FakeBackend;

    impl BlurayBackend for FakeBackend {
        type Disc = FakeDisc;
        type TitleSource = Cursor<Vec<u8>>;

        fn open(_path: &Path) -> Result<Self::Disc, String> {
            Err("not used by mapper unit tests".to_string())
        }

        fn disc_label(disc: &Self::Disc, _source: &Path) -> Option<String> {
            disc.label.clone()
        }

        fn titles(disc: &Self::Disc) -> Result<Vec<BlurayTitleInfo>, String> {
            Ok(disc.titles.iter().map(|title| title.info.clone()).collect())
        }

        fn title_by_playlist(
            disc: &Self::Disc,
            playlist_number: u32,
        ) -> Result<BlurayTitleKey, String> {
            disc.titles
                .iter()
                .find(|title| title.info.playlist_number == playlist_number)
                .map(|title| title.info.key)
                .ok_or_else(|| "missing playlist".to_string())
        }

        fn chapters(
            disc: &Self::Disc,
            title: BlurayTitleKey,
            display_angle: BlurayDisplayAngle,
        ) -> Result<Vec<BlurayChapterInfo>, String> {
            let title = find_title(disc, title)?;
            Ok(title
                .chapters_by_display_angle
                .get(&display_angle.get())
                .cloned()
                .unwrap_or_else(|| title.chapters.clone()))
        }

        fn streams_with_probe_policy(
            disc: &Self::Disc,
            title: BlurayTitleKey,
            _policy: ProbeDepth,
        ) -> Result<Vec<BlurayAudioStreamInfo>, String> {
            Ok(find_title(disc, title)?.streams.clone())
        }

        fn stream_enumeration_with_probe_policy(
            disc: &Self::Disc,
            title: BlurayTitleKey,
            policy: ProbeDepth,
        ) -> Result<BlurayStreamEnumeration, String> {
            let title = find_title(disc, title)?;
            if !matches!(policy, ProbeDepth::None) {
                if let Some(err) = &title.bounded_probe_error {
                    return Err(err.clone());
                }
            }
            Ok(BlurayStreamEnumeration {
                supported_streams: title.streams.clone(),
                stream_diagnostics: title.stream_diagnostics.clone(),
            })
        }

        fn protection_status(disc: &Self::Disc) -> BlurayProtectionStatus {
            disc.protection_status.clone()
        }

        fn max_angle(disc: &Self::Disc, title: BlurayTitleKey) -> Result<u8, String> {
            Ok(find_title(disc, title)?.info.angle_count.max(1))
        }

        fn open_title(
            _disc: &Self::Disc,
            _title: BlurayTitleKey,
            _display_angle: BlurayDisplayAngle,
            _decryptor: Option<&mut dyn BlurayStreamDecryptor>,
        ) -> Result<Self::TitleSource, String> {
            Ok(Cursor::new(Vec::new()))
        }

        fn pts_continuity_segments(
            _source: &Self::TitleSource,
        ) -> Result<BlurayBackendCapability<Vec<BlurayPtsContinuitySegment>>, String> {
            Ok(BlurayBackendCapability::unsupported("not used"))
        }
    }

    fn find_title(disc: &FakeDisc, key: BlurayTitleKey) -> Result<&FakeTitle, String> {
        disc.titles
            .iter()
            .find(|title| title.info.key == key)
            .ok_or_else(|| "missing title".to_string())
    }

    #[test]
    fn mapper_builds_plan_style_label_with_lpcm_bit_depth() {
        let disc = fake_disc(vec![fake_title(
            12,
            58 * 60 + 32,
            12,
            vec![stream(
                0x1100,
                0,
                BluRayAudioCoding::Lpcm,
                Some(96_000),
                Some(24),
                Some(2),
            )],
        )]);

        let contents = map_bluray_disc::<FakeBackend>(&disc, &PathBuf::from("/disc")).unwrap();

        assert_eq!(contents.format, DiscFormat::BluRay);
        assert_eq!(contents.copy_protection.description, "Unencrypted");
        assert_eq!(contents.presentations.len(), 1);
        assert_eq!(
            contents.presentations[0].label,
            "Blu-ray Playlist 00012 Stream 1 PID 0x1100 · LPCM 96 kHz / 24-bit / Stereo"
        );
        assert_eq!(contents.presentations[0].format.bit_depth, Some(24));
        assert_eq!(
            format_aware_bluray_presentation_label(&contents.presentations[0]),
            contents.presentations[0].label
        );
    }

    #[test]
    fn mapper_records_unsupported_stream_without_dropping_supported_stream() {
        let mut title = fake_title(
            12,
            3600,
            12,
            vec![stream(
                0x1100,
                0,
                BluRayAudioCoding::Lpcm,
                Some(96_000),
                Some(24),
                Some(2),
            )],
        );
        title.stream_diagnostics.push(BlurayUnsupportedStreamDiagnostic {
            kind: BluRayAudioStreamKind::Primary,
            clip_index: Some(0),
            stream_index: Some(1),
            pid: Some(0x1200),
            coding_type: Some(0x90),
            format: Some(0x03),
            rate: Some(0x01),
            language: Some("eng".to_string()),
            reason: "unsupported audio coding type".to_string(),
        });
        let disc = fake_disc(vec![title]);

        let contents = map_bluray_disc::<FakeBackend>(&disc, &PathBuf::from("/disc")).unwrap();

        assert_eq!(contents.presentations.len(), 1);
        assert_eq!(contents.suppressed.len(), 1);
        assert!(contents.suppressed[0]
            .reason
            .contains("unsupported audio stream ignored"));
        assert!(contents.suppressed[0].reason.contains("pid 0x1200"));
    }

    #[test]
    fn mapper_keeps_playlist_when_bounded_probe_fails_after_metadata_scan() {
        let mut title = fake_title(12, 3600, 12, vec![stream(
            0x1100,
            0,
            BluRayAudioCoding::Lpcm,
            Some(96_000),
            None,
            Some(2),
        )]);
        title.bounded_probe_error = Some("media-byte read failed".to_string());
        let disc = fake_disc(vec![title]);

        let contents = map_bluray_disc::<FakeBackend>(&disc, &PathBuf::from("/disc")).unwrap();

        assert_eq!(contents.presentations.len(), 1);
        assert!(contents
            .suppressed
            .iter()
            .any(|entry| entry.reason.contains("bounded LPCM probe failed")));
    }

    #[test]
    fn mapper_skips_bounded_probe_when_typed_protection_is_unhandled() {
        let mut title = fake_title(12, 3600, 12, vec![stream(
            0x1100,
            0,
            BluRayAudioCoding::Lpcm,
            Some(96_000),
            None,
            Some(2),
        )]);
        title.bounded_probe_error = Some("this must not be observed".to_string());
        let mut disc = fake_disc(vec![title]);
        disc.protection_status = BlurayProtectionStatus::AacsDetectedNotHandled {
            details: BlurayAacsStatus {
                handled: false,
                libaacs_detected: true,
                error_code: Some(1),
                mkb_version: Some(78),
            },
        };

        let contents = map_bluray_disc::<FakeBackend>(&disc, &PathBuf::from("/disc")).unwrap();

        assert_eq!(contents.copy_protection.description, "AACS detected / not handled (error code 1, MKB v78)");
        assert_eq!(contents.presentations.len(), 1);
        assert!(!contents
            .suppressed
            .iter()
            .any(|entry| entry.reason.contains("bounded LPCM probe failed")));
    }

    #[test]
    fn mapper_skips_bounded_probe_when_protection_status_is_unknown() {
        let mut title = fake_title(12, 3600, 12, vec![stream(
            0x1100,
            0,
            BluRayAudioCoding::Lpcm,
            Some(96_000),
            None,
            Some(2),
        )]);
        title.bounded_probe_error = Some("this must not be observed".to_string());
        let mut disc = fake_disc(vec![title]);
        disc.protection_status = BlurayProtectionStatus::Unknown {
            reason: "bd_get_disc_info returned NULL".to_string(),
        };

        let contents = map_bluray_disc::<FakeBackend>(&disc, &PathBuf::from("/disc")).unwrap();

        assert_eq!(
            contents.copy_protection.description,
            "Unknown / probe failed: bd_get_disc_info returned NULL"
        );
        assert_eq!(contents.presentations.len(), 1);
        assert!(!contents
            .suppressed
            .iter()
            .any(|entry| entry.reason.contains("bounded LPCM probe failed")));
    }

    #[test]
    fn mapper_uses_one_based_display_angles_for_all_presentations() {
        let mut title = fake_title(12, 3600, 2, vec![stream(
            0x1100,
            0,
            BluRayAudioCoding::Ac3,
            Some(48_000),
            None,
            Some(2),
        )]);
        title.info.angle_count = 2;
        let mut angle_two_chapters = title.chapters.clone();
        for chapter in &mut angle_two_chapters {
            chapter.byte_offset = Some(2);
        }
        title.chapters_by_display_angle.insert(2, angle_two_chapters);
        let disc = fake_disc(vec![title]);

        let contents = map_bluray_disc::<FakeBackend>(&disc, &PathBuf::from("/disc")).unwrap();

        let display_angles: Vec<u8> = contents
            .presentations
            .iter()
            .filter_map(|presentation| presentation.id.blu_ray_parts().map(|parts| parts.3))
            .collect();
        assert_eq!(display_angles, vec![1, 2]);
        assert!(contents.presentations[0].label.contains("Angle 1/2"));
        assert!(contents.presentations[1].label.contains("Angle 2/2"));
    }

    #[test]
    fn mapper_label_includes_one_based_stream_pid_format_and_angle() {
        let mut presentation = DiscPresentation {
            id: PresentationId::blu_ray_title(7, 0x1101, 1, BlurayDisplayAngle::first()),
            label: String::new(),
            format: AudioPresentationFormat {
                codec: Some("DTS-HD MA".to_string()),
                sample_rate: Some(96_000),
                bit_depth: None,
                channels: Some(6),
                channel_layout: Some("5.1".to_string()),
                lossless: true,
                provenance: FormatProvenance::Unknown,
            },
            tracks: vec![DiscTrack {
                number: 1,
                title: Some("Chapter 1".to_string()),
                performer: None,
                duration_secs: Some(120.0),
                format_note: Some("Angle 1/2".to_string()),
            }],
            total_duration_secs: 120.0,
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        };

        presentation.label = format_aware_bluray_presentation_label(&presentation);

        assert_eq!(
            presentation.label,
            "Blu-ray Playlist 00007 Stream 2 PID 0x1101 · DTS-HD MA 96 kHz / 5.1 · Angle 1/2"
        );
    }

    #[test]
    fn mapper_suppresses_short_and_duplicate_playlists() {
        let title_a = fake_title(
            12,
            3600,
            12,
            vec![stream(
                0x1100,
                0,
                BluRayAudioCoding::TrueHd,
                Some(48_000),
                None,
                Some(6),
            )],
        );
        let duplicate = fake_title(
            13,
            3600,
            12,
            vec![stream(
                0x1101,
                0,
                BluRayAudioCoding::TrueHd,
                Some(48_000),
                None,
                Some(6),
            )],
        );
        let short = fake_title(
            2,
            12,
            1,
            vec![stream(
                0x1100,
                0,
                BluRayAudioCoding::Ac3,
                Some(48_000),
                None,
                Some(2),
            )],
        );
        let disc = fake_disc(vec![title_a, duplicate, short]);

        let contents = map_bluray_disc::<FakeBackend>(&disc, &PathBuf::from("/disc")).unwrap();

        assert_eq!(contents.presentations.len(), 1);
        assert_eq!(contents.suppressed.len(), 2);
        assert!(contents
            .suppressed
            .iter()
            .any(|entry| entry.reason.contains("duplicate playlist")));
        assert!(contents
            .suppressed
            .iter()
            .any(|entry| entry.reason.contains("shorter than 30 seconds")));
    }

    #[test]
    fn scoring_prefers_sidecar_metadata_then_stereo_then_bit_depth() {
        let stereo = DiscPresentation {
            id: PresentationId::blu_ray_title(12, 0x1100, 0, BlurayDisplayAngle::first()),
            label: String::new(),
            format: AudioPresentationFormat {
                codec: Some("LPCM".to_string()),
                sample_rate: Some(96_000),
                bit_depth: Some(24),
                channels: Some(2),
                channel_layout: Some("Stereo".to_string()),
                lossless: true,
                provenance: FormatProvenance::Unknown,
            },
            tracks: vec![DiscTrack {
                number: 1,
                title: Some("Chapter 1".to_string()),
                performer: None,
                duration_secs: Some(120.0),
                format_note: None,
            }],
            total_duration_secs: 120.0,
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        };
        let mut tagged_multichannel = stereo.clone();
        tagged_multichannel.id = PresentationId::blu_ray_title(11, 0x1101, 1, BlurayDisplayAngle::first());
        tagged_multichannel.format.channels = Some(6);
        tagged_multichannel.format.channel_layout = Some("5.1".to_string());
        tagged_multichannel.album_title = Some("Tagged Album".to_string());

        let contents = DiscContents {
            format: DiscFormat::BluRay,
            label: "disc".to_string(),
            source_path: PathBuf::new(),
            presentations: vec![stereo, tagged_multichannel],
            suppressed: Vec::new(),
            copy_protection: CopyProtectionSummary {
                description: "Unencrypted".to_string(),
            },
            diagnostics: Vec::new(),
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        };

        assert_eq!(best_bluray_presentation_index(&contents), Some(1));
    }

    fn fake_disc(titles: Vec<FakeTitle>) -> FakeDisc {
        FakeDisc {
            label: Some("Fake Blu-ray".to_string()),
            titles,
            protection_status: BlurayProtectionStatus::Unencrypted,
        }
    }

    fn fake_title(
        playlist_number: u32,
        duration_secs: u64,
        chapter_count: u32,
        streams: Vec<BlurayAudioStreamInfo>,
    ) -> FakeTitle {
        let duration_pts_90k = duration_secs * 90_000;
        let chapter_duration = duration_pts_90k / u64::from(chapter_count.max(1));
        let chapters = (0..chapter_count)
            .map(|index| BlurayChapterInfo {
                chapter_number: index + 1,
                start_pts_90k: u64::from(index) * chapter_duration,
                end_pts_90k: Some(u64::from(index + 1) * chapter_duration),
                duration_pts_90k: Some(chapter_duration),
                byte_offset: None,
                clip_ref: None,
            })
            .collect();
        FakeTitle {
            info: BlurayTitleInfo {
                key: BlurayTitleKey::from_libbluray(playlist_number, playlist_number),
                playlist_number,
                duration_pts_90k,
                angle_count: 1,
                chapter_count,
                clip_count: 1,
            },
            chapters,
            chapters_by_display_angle: TestHashMap::new(),
            streams,
            stream_diagnostics: Vec::new(),
            bounded_probe_error: None,
        }
    }

    fn stream(
        pid: u16,
        stream_index: u8,
        coding: BluRayAudioCoding,
        sample_rate: Option<u32>,
        bit_depth: Option<u32>,
        channels: Option<u8>,
    ) -> BlurayAudioStreamInfo {
        BlurayAudioStreamInfo {
            kind: BluRayAudioStreamKind::Primary,
            pid,
            stream_index,
            coding,
            sample_rate,
            bit_depth: match bit_depth {
                Some(bit_depth) => BlurayLpcmBitDepth::Probed {
                    bit_depth,
                    scanned_bytes: 188,
                },
                None if coding == BluRayAudioCoding::Lpcm => BlurayLpcmBitDepth::NotProbed {
                    reason: super::super::bluray_backend::BlurayLpcmNotProbedReason::ProbePolicyNone,
                },
                None => BlurayLpcmBitDepth::NotApplicable,
            },
            channels,
            channel_layout: channels.map(channel_layout_from_count),
            language: Some("eng".to_string()),
        }
    }
}
