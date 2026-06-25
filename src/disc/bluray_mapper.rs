//! Blu-ray backend metadata to unified disc-browser model mapping.
//!
//! This mirrors the DVD-Video mapper: it turns authored playlist/chapter/audio
//! stream data into curated `DiscPresentation` values, carries suppressed
//! candidates with reasons, and orders presentations deterministically for the
//! browser and convert view.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::bluray_backend::{
    bluray_audio_stream_display_number, BluRayAudioCoding, BluRayAudioStreamKind,
    BlurayAudioStreamInfo, BlurayBackend,
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
    let probe = FfprobeBlurayAudioProbe::default();
    let control = BlurayProbeControl::bounded_default();
    map_bluray_disc_with_audio_probe::<B, _>(disc, source_path, &probe, &control)
}

pub(crate) fn map_bluray_disc_with_audio_probe<B, P>(
    disc: &B::Disc,
    source_path: &Path,
    compressed_audio_probe: &P,
    probe_control: &BlurayProbeControl<'_>,
) -> Result<DiscContents, String>
where
    B: BlurayBackend,
    P: BlurayCompressedAudioProbe + ?Sized,
{
    let label = blu_ray_disc_label::<B>(disc, source_path);
    let titles = B::titles(disc)?;
    let mut presentations = Vec::new();
    let mut suppressed = Vec::new();
    let mut signatures: HashMap<PlaylistSignature, u32> = HashMap::new();
    let protection_status = B::protection_status(disc);
    if let Some(guidance) = protection_status.user_guidance() {
        log::warn!("{guidance}");
    }

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
        if protection_status.may_read_media_for_probe() {
            probe_compressed_bluray_streams_for_playlist_with_probe(
                source_path,
                playlist_number,
                &mut streams,
                compressed_audio_probe,
                probe_control,
            );
        } else if streams
            .iter()
            .any(|stream| stream.kind == BluRayAudioStreamKind::Primary && stream.coding.is_compressed())
        {
            log::debug!(
                "skipping Blu-ray ffprobe decoded-audio probe for playlist {playlist_number:05}: {}",
                protection_status.summary()
            );
        }

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
        bit_depth: stream.decoded_bit_depth(),
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BlurayProbedStreamFacts {
    pub ffprobe_index: Option<u32>,
    pub audio_ordinal: Option<u8>,
    pub pid: Option<u16>,
    pub codec_name: Option<String>,
    pub codec_profile: Option<String>,
    pub bit_depth: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
}

#[derive(Debug, Clone)]
pub(crate) struct BlurayAudioProbeRequest<'a> {
    pub source_path: &'a Path,
    pub playlist_number: u32,
}

pub(crate) trait BlurayCompressedAudioProbe {
    fn probe_bluray_audio_streams(
        &self,
        request: &BlurayAudioProbeRequest<'_>,
        control: &BlurayProbeControl<'_>,
    ) -> Result<Vec<BlurayProbedStreamFacts>, String>;
}

pub(crate) struct BlurayProbeControl<'a> {
    timeout: Duration,
    is_cancelled: Option<&'a dyn Fn() -> bool>,
}

impl<'a> BlurayProbeControl<'a> {
    #[must_use]
    pub fn bounded_default() -> Self {
        Self {
            timeout: ProbeDepth::DEFAULT_MAX_DURATION,
            is_cancelled: None,
        }
    }

    #[must_use]
    pub fn with_cancellation(mut self, is_cancelled: &'a dyn Fn() -> bool) -> Self {
        self.is_cancelled = Some(is_cancelled);
        self
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn is_cancelled(&self) -> bool {
        self.is_cancelled.map_or(false, |is_cancelled| is_cancelled())
    }
}

impl<'a> Default for BlurayProbeControl<'a> {
    fn default() -> Self {
        Self::bounded_default()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FfprobeBlurayAudioProbe {
    ffprobe_path: PathBuf,
}

impl FfprobeBlurayAudioProbe {
    #[must_use]
    pub fn new(ffprobe_path: PathBuf) -> Self {
        Self { ffprobe_path }
    }

    #[must_use]
    pub fn from_tool_paths(tool_paths: &HashMap<String, PathBuf>) -> Self {
        tool_paths
            .get("ffprobe")
            .or_else(|| tool_paths.get("FFPROBE"))
            .or_else(|| tool_paths.get("Ffprobe"))
            .cloned()
            .map(Self::new)
            .unwrap_or_default()
    }
}

impl Default for FfprobeBlurayAudioProbe {
    fn default() -> Self {
        Self::new(PathBuf::from("ffprobe"))
    }
}

impl BlurayCompressedAudioProbe for FfprobeBlurayAudioProbe {
    fn probe_bluray_audio_streams(
        &self,
        request: &BlurayAudioProbeRequest<'_>,
        control: &BlurayProbeControl<'_>,
    ) -> Result<Vec<BlurayProbedStreamFacts>, String> {
        ffprobe_bluray_playlist_audio_streams(
            &self.ffprobe_path,
            request.source_path,
            request.playlist_number,
            control,
        )
    }
}

pub(crate) fn probe_compressed_bluray_streams_for_playlist_with_probe<P>(
    source_path: &Path,
    playlist_number: u32,
    streams: &mut [BlurayAudioStreamInfo],
    probe: &P,
    control: &BlurayProbeControl<'_>,
) where
    P: BlurayCompressedAudioProbe + ?Sized,
{
    if !streams.iter().any(is_primary_compressed_stream) {
        return;
    }

    if !source_path.exists() {
        log::debug!(
            "skipping Blu-ray ffprobe decoded-audio probe for missing source path '{}'",
            source_path.display()
        );
        return;
    }

    let facts = match probe_bluray_playlist_audio_streams_uncached(
        source_path,
        playlist_number,
        probe,
        control,
    ) {
        Ok(facts) => facts,
        Err(err) => {
            log::warn!(
                "Blu-ray playlist {playlist_number:05} ffprobe decoded-audio probe failed: {err}"
            );
            return;
        }
    };

    for stream in streams.iter_mut().filter(|stream| is_primary_compressed_stream(stream)) {
        if let Err(err) = apply_matching_bluray_audio_probe_facts(playlist_number, stream, &facts) {
            log::warn!(
                "Blu-ray playlist {playlist_number:05} stream {} PID 0x{:04x} ffprobe decoded-audio probe did not match usable decoded facts: {err}",
                bluray_audio_stream_display_number(stream.stream_index),
                stream.pid
            );
        }
    }
}

pub(crate) fn probe_compressed_bluray_stream_for_playlist_with_probe<P>(
    source_path: &Path,
    playlist_number: u32,
    stream: &mut BlurayAudioStreamInfo,
    probe: &P,
    control: &BlurayProbeControl<'_>,
) -> Result<(), String>
where
    P: BlurayCompressedAudioProbe + ?Sized,
{
    if !is_primary_compressed_stream(stream) {
        return Ok(());
    }

    if !source_path.exists() {
        return Err(format!(
            "source path '{}' does not exist",
            source_path.display()
        ));
    }

    let facts = probe_bluray_playlist_audio_streams_uncached(
        source_path,
        playlist_number,
        probe,
        control,
    )?;
    apply_matching_bluray_audio_probe_facts(playlist_number, stream, &facts)
}

fn is_primary_compressed_stream(stream: &BlurayAudioStreamInfo) -> bool {
    stream.kind == BluRayAudioStreamKind::Primary && stream.coding.is_compressed()
}

// Do not keep a process-global cache for ffprobe playlist facts. Even successful
// probes can be incomplete or probe-dependent: bits_per_raw_sample may be missing
// with one ffprobe binary but present with another, and a stable mount path can
// later point at different media. The caller already probes each playlist once
// per mapping operation, which provides the important performance win without
// leaking stale or incomplete facts across browse and materialization runs.
fn probe_bluray_playlist_audio_streams_uncached<P>(
    source_path: &Path,
    playlist_number: u32,
    probe: &P,
    control: &BlurayProbeControl<'_>,
) -> Result<Vec<BlurayProbedStreamFacts>, String>
where
    P: BlurayCompressedAudioProbe + ?Sized,
{
    if control.is_cancelled() {
        return Err("cancelled before ffprobe decoded-audio probe started".to_string());
    }

    let request = BlurayAudioProbeRequest {
        source_path,
        playlist_number,
    };
    probe.probe_bluray_audio_streams(&request, control)
}

fn apply_matching_bluray_audio_probe_facts(
    playlist_number: u32,
    stream: &mut BlurayAudioStreamInfo,
    facts: &[BlurayProbedStreamFacts],
) -> Result<(), String> {
    let facts = match_bluray_audio_probe_facts(stream, facts)?;
    apply_bluray_audio_probe_facts(playlist_number, stream, facts)
}

fn match_bluray_audio_probe_facts<'a>(
    stream: &BlurayAudioStreamInfo,
    facts: &'a [BlurayProbedStreamFacts],
) -> Result<&'a BlurayProbedStreamFacts, String> {
    let mut candidates: Vec<&BlurayProbedStreamFacts> = facts
        .iter()
        .filter(|facts| facts.pid == Some(stream.pid))
        .collect();

    if candidates.is_empty() {
        candidates = facts.iter().filter(|facts| facts.pid.is_none()).collect();
    }

    if candidates.is_empty() {
        return Err(format!(
            "ffprobe returned no audio stream with PID 0x{:04x}; candidates: {}",
            stream.pid,
            summarize_probe_candidates(facts)
        ));
    }

    let codec_matches: Vec<&BlurayProbedStreamFacts> = candidates
        .iter()
        .copied()
        .filter(|facts| ffprobe_codec_matches_bluray_coding(
            stream.coding,
            facts.codec_name.as_deref(),
            facts.codec_profile.as_deref(),
        ))
        .collect();

    let candidates = if codec_matches.is_empty() {
        if candidates.len() == 1 && candidates[0].pid == Some(stream.pid) {
            log::debug!(
                "using sole ffprobe PID match for Blu-ray stream {} PID 0x{:04x} despite codec mismatch: {}",
                bluray_audio_stream_display_number(stream.stream_index),
                stream.pid,
                describe_probe_candidate(candidates[0])
            );
            candidates
        } else {
            return Err(format!(
                "ffprobe returned no {}-compatible audio stream for PID 0x{:04x}; candidates: {}",
                stream.coding.label(),
                stream.pid,
                summarize_probe_candidates(facts)
            ));
        }
    } else {
        codec_matches
    };

    let mut scored: Vec<(&BlurayProbedStreamFacts, i32)> = candidates
        .into_iter()
        .map(|facts| (facts, probe_match_score(stream, facts)))
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1));

    let Some((best, best_score)) = scored.first().copied() else {
        return Err("ffprobe returned no match candidates".to_string());
    };

    if scored
        .get(1)
        .is_some_and(|(_, score)| *score == best_score)
    {
        return Err(format!(
            "ambiguous ffprobe match for {} stream {} PID 0x{:04x}; candidates: {}",
            stream.coding.label(),
            bluray_audio_stream_display_number(stream.stream_index),
            stream.pid,
            summarize_probe_candidates(facts)
        ));
    }

    Ok(best)
}

fn probe_match_score(stream: &BlurayAudioStreamInfo, facts: &BlurayProbedStreamFacts) -> i32 {
    let mut score = 0;
    if facts.pid == Some(stream.pid) {
        score += 100;
    }
    if ffprobe_codec_matches_bluray_coding(
        stream.coding,
        facts.codec_name.as_deref(),
        facts.codec_profile.as_deref(),
    ) {
        score += 40;
    }
    if facts.channels.is_some() && facts.channels == stream.channels {
        score += 12;
    }
    if facts.sample_rate.is_some() && facts.sample_rate == stream.sample_rate {
        score += 12;
    }
    if facts.audio_ordinal == Some(stream.stream_index) {
        score += 8;
    }
    if facts.bit_depth.is_some() {
        score += 4;
    }
    score
}

fn ffprobe_codec_matches_bluray_coding(
    coding: BluRayAudioCoding,
    codec_name: Option<&str>,
    codec_profile: Option<&str>,
) -> bool {
    let Some(codec_name) = codec_name else {
        return false;
    };
    let codec = codec_name.trim().to_ascii_lowercase();
    let profile = codec_profile
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    match coding {
        BluRayAudioCoding::Lpcm => codec == "pcm_bluray",
        BluRayAudioCoding::Ac3 => codec == "ac3",
        BluRayAudioCoding::Eac3 => codec == "eac3",
        BluRayAudioCoding::TrueHd => codec == "truehd",
        BluRayAudioCoding::Dts => codec == "dts" && !profile.contains("dts-hd"),
        BluRayAudioCoding::DtsHd => {
            codec == "dts"
                && (profile.is_empty()
                    || (profile.contains("dts-hd")
                        && !profile.contains("ma")
                        && !profile.contains("master")))
        }
        BluRayAudioCoding::DtsHdMaster => {
            codec == "dts"
                && (profile.is_empty()
                    || profile.contains("ma")
                    || profile.contains("master"))
        }
    }
}

fn apply_bluray_audio_probe_facts(
    playlist_number: u32,
    stream: &mut BlurayAudioStreamInfo,
    facts: &BlurayProbedStreamFacts,
) -> Result<(), String> {
    if stream.coding.is_lossless() && facts.bit_depth.is_none() {
        return Err(format!(
            "ffprobe did not report bits_per_raw_sample for matched {} stream {}",
            stream.coding.label(),
            describe_probe_candidate(facts)
        ));
    }

    let mut updated = stream.clone();
    if let Some(sample_rate) = facts.sample_rate {
        if let Some(clpi_rate) = updated.sample_rate {
            if clpi_rate != sample_rate {
                log::warn!(
                    "Blu-ray playlist {playlist_number:05} stream {} PID 0x{:04x} CLPI sample rate {} Hz differs from ffprobe decoded sample rate {} Hz; using ffprobe value",
                    bluray_audio_stream_display_number(updated.stream_index),
                    updated.pid,
                    clpi_rate,
                    sample_rate
                );
            }
        }
        updated.sample_rate = Some(sample_rate);
    }
    if let Some(bit_depth) = facts.bit_depth {
        updated.probed_bit_depth = Some(bit_depth);
    }
    if let Some(channels) = facts.channels {
        updated.channels = Some(channels);
        updated.channel_layout = Some(channel_layout_from_count(channels));
    }

    *stream = updated;
    Ok(())
}

fn ffprobe_bluray_playlist_audio_streams(
    ffprobe_path: &Path,
    source_path: &Path,
    playlist_number: u32,
    control: &BlurayProbeControl<'_>,
) -> Result<Vec<BlurayProbedStreamFacts>, String> {
    if control.is_cancelled() {
        return Err("cancelled before ffprobe decoded-audio probe started".to_string());
    }

    let input = format!("bluray:{}", source_path.display());
    let playlist_arg = playlist_number.to_string();
    let child = Command::new(ffprobe_path)
        .args([
            "-v",
            "error",
            "-playlist",
            playlist_arg.as_str(),
            "-i",
            input.as_str(),
            "-select_streams",
            "a",
            "-show_entries",
            "stream=index,id,codec_name,profile,bits_per_raw_sample,sample_rate,channels",
            "-of",
            "default=noprint_wrappers=0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to start ffprobe '{}': {err}", ffprobe_path.display()))?;

    let output = wait_for_ffprobe_output(child, control)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "ffprobe exited with status {}{}",
            output.status,
            ffprobe_stderr_suffix(&stderr)
        ));
    }

    parse_ffprobe_playlist_audio_facts(&output.stdout)
}

fn wait_for_ffprobe_output(
    mut child: std::process::Child,
    control: &BlurayProbeControl<'_>,
) -> Result<Output, String> {
    let started = Instant::now();
    let poll_interval = Duration::from_millis(25);

    loop {
        if control.is_cancelled() {
            terminate_ffprobe_child(child);
            return Err("cancelled while ffprobe decoded-audio probe was running".to_string());
        }

        match child.try_wait() {
            Ok(Some(_status)) => {
                return child
                    .wait_with_output()
                    .map_err(|err| format!("failed to collect ffprobe output: {err}"));
            }
            Ok(None) => {}
            Err(err) => {
                terminate_ffprobe_child(child);
                return Err(format!("failed while waiting for ffprobe: {err}"));
            }
        }

        if started.elapsed() >= control.timeout {
            terminate_ffprobe_child(child);
            return Err(format!(
                "ffprobe decoded-audio probe timed out after {:.1}s",
                control.timeout.as_secs_f64()
            ));
        }

        thread::sleep(poll_interval);
    }
}

fn terminate_ffprobe_child(mut child: std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
fn parse_ffprobe_stream_facts(stdout: &[u8]) -> Result<BlurayProbedStreamFacts, String> {
    parse_ffprobe_playlist_audio_facts(stdout)?
        .into_iter()
        .next()
        .ok_or_else(|| "ffprobe returned no audio stream fields".to_string())
}

fn parse_ffprobe_playlist_audio_facts(stdout: &[u8]) -> Result<Vec<BlurayProbedStreamFacts>, String> {
    let text = std::str::from_utf8(stdout)
        .map_err(|err| format!("ffprobe returned non-UTF-8 output: {err}"))?;
    let mut streams = Vec::new();
    let mut current: Option<BlurayProbedStreamFacts> = None;
    let mut saw_stream_field = false;

    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        match line {
            "[STREAM]" => {
                if let Some(facts) = current.take() {
                    streams.push(facts);
                }
                current = Some(BlurayProbedStreamFacts {
                    audio_ordinal: (streams.len() <= u8::MAX as usize).then_some(streams.len() as u8),
                    ..BlurayProbedStreamFacts::default()
                });
                continue;
            }
            "[/STREAM]" => {
                if let Some(facts) = current.take() {
                    streams.push(facts);
                }
                continue;
            }
            _ => {}
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let facts = current.get_or_insert_with(|| BlurayProbedStreamFacts {
            audio_ordinal: Some(0),
            ..BlurayProbedStreamFacts::default()
        });
        saw_stream_field = true;
        apply_ffprobe_stream_field(facts, key, value);
    }

    if let Some(facts) = current.take() {
        streams.push(facts);
    }

    streams.retain(|facts| {
        facts.ffprobe_index.is_some()
            || facts.pid.is_some()
            || facts.codec_name.is_some()
            || facts.sample_rate.is_some()
            || facts.channels.is_some()
            || facts.bit_depth.is_some()
    });

    if saw_stream_field && !streams.is_empty() {
        Ok(streams)
    } else {
        Err("ffprobe returned no audio stream fields".to_string())
    }
}

fn apply_ffprobe_stream_field(facts: &mut BlurayProbedStreamFacts, key: &str, value: &str) {
    match key {
        "index" => {
            facts.ffprobe_index = parse_u32_allow_zero(value);
        }
        "id" => {
            facts.pid = parse_u16_auto(value);
        }
        "codec_name" => {
            facts.codec_name = parse_nonempty_string(value);
        }
        "profile" => {
            facts.codec_profile = parse_nonempty_string(value);
        }
        "bits_per_raw_sample" => {
            facts.bit_depth = parse_positive_u32(value).filter(|depth| *depth <= 64);
        }
        "sample_rate" => {
            facts.sample_rate = parse_positive_u32(value);
        }
        "channels" => {
            facts.channels = parse_positive_u32(value)
                .filter(|value| *value <= u8::MAX as u32)
                .map(|value| value as u8);
        }
        _ => {}
    }
}

fn parse_nonempty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("N/A") {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_u32_allow_zero(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("N/A") {
        return None;
    }
    trimmed.parse::<u32>().ok()
}

fn parse_positive_u32(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("N/A") {
        return None;
    }
    let parsed = trimmed.parse::<u32>().ok()?;
    (parsed > 0).then_some(parsed)
}

fn parse_u16_auto(value: &str) -> Option<u16> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("N/A") {
        return None;
    }
    let parsed = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .map(|hex| u16::from_str_radix(hex, 16).ok())
        .unwrap_or_else(|| trimmed.parse::<u16>().ok())?;
    (parsed > 0).then_some(parsed)
}

fn summarize_probe_candidates(facts: &[BlurayProbedStreamFacts]) -> String {
    if facts.is_empty() {
        return "<none>".to_string();
    }
    facts
        .iter()
        .map(describe_probe_candidate)
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe_probe_candidate(facts: &BlurayProbedStreamFacts) -> String {
    let pid = facts
        .pid
        .map(|pid| format!("pid=0x{pid:04x}"))
        .unwrap_or_else(|| "pid=<unknown>".to_string());
    let codec = facts
        .codec_name
        .as_deref()
        .unwrap_or("<unknown-codec>");
    let profile = facts
        .codec_profile
        .as_deref()
        .filter(|profile| !profile.trim().is_empty())
        .map(|profile| format!(" profile={profile}"))
        .unwrap_or_default();
    let ordinal = facts
        .audio_ordinal
        .map(|ordinal| format!("a:{ordinal}"))
        .unwrap_or_else(|| "a:?".to_string());
    let index = facts
        .ffprobe_index
        .map(|index| format!("index={index}"))
        .unwrap_or_else(|| "index=?".to_string());
    let rate = facts
        .sample_rate
        .map(|rate| format!(" {rate}Hz"))
        .unwrap_or_default();
    let channels = facts
        .channels
        .map(|channels| format!(" {channels}ch"))
        .unwrap_or_default();
    let depth = facts
        .bit_depth
        .map(|depth| format!(" {depth}-bit"))
        .unwrap_or_default();

    format!("{ordinal} {index} {pid} codec={codec}{profile}{rate}{channels}{depth}")
}

fn ffprobe_stderr_suffix(stderr: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(": {trimmed}")
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
        bluray_audio_stream_display_number(stream_index),
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
                bit_depth: stream.decoded_bit_depth(),
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
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap as TestHashMap;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex as TestMutex, Once, OnceLock as TestOnceLock};

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

    #[derive(Debug)]
    struct FakeCompressedAudioProbe {
        facts: Vec<BlurayProbedStreamFacts>,
        error: Option<String>,
        calls: Cell<usize>,
        requests: RefCell<Vec<(PathBuf, u32)>>,
    }

    impl FakeCompressedAudioProbe {
        fn success(facts: Vec<BlurayProbedStreamFacts>) -> Self {
            Self {
                facts,
                error: None,
                calls: Cell::new(0),
                requests: RefCell::new(Vec::new()),
            }
        }

        fn failure(error: impl Into<String>) -> Self {
            Self {
                facts: Vec::new(),
                error: Some(error.into()),
                calls: Cell::new(0),
                requests: RefCell::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.get()
        }
    }

    impl BlurayCompressedAudioProbe for FakeCompressedAudioProbe {
        fn probe_bluray_audio_streams(
            &self,
            request: &BlurayAudioProbeRequest<'_>,
            _control: &BlurayProbeControl<'_>,
        ) -> Result<Vec<BlurayProbedStreamFacts>, String> {
            self.calls.set(self.calls.get() + 1);
            self.requests
                .borrow_mut()
                .push((request.source_path.to_path_buf(), request.playlist_number));
            if let Some(error) = &self.error {
                Err(error.clone())
            } else {
                Ok(self.facts.clone())
            }
        }
    }

    struct RecordingLogger;

    static TEST_LOGGER: RecordingLogger = RecordingLogger;
    static TEST_LOGGER_SET: Once = Once::new();
    static TEST_LOGGER_ACTIVE: AtomicBool = AtomicBool::new(false);
    static TEST_LOG_MESSAGES: TestOnceLock<TestMutex<Vec<String>>> = TestOnceLock::new();

    impl log::Log for RecordingLogger {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() <= log::Level::Warn
        }

        fn log(&self, record: &log::Record<'_>) {
            if !self.enabled(record.metadata()) {
                return;
            }
            if let Some(messages) = TEST_LOG_MESSAGES.get() {
                if let Ok(mut messages) = messages.lock() {
                    messages.push(record.args().to_string());
                }
            }
        }

        fn flush(&self) {}
    }

    fn begin_log_capture() -> bool {
        let messages = TEST_LOG_MESSAGES.get_or_init(|| TestMutex::new(Vec::new()));
        if let Ok(mut messages) = messages.lock() {
            messages.clear();
        }
        TEST_LOGGER_SET.call_once(|| {
            if log::set_logger(&TEST_LOGGER).is_ok() {
                log::set_max_level(log::LevelFilter::Trace);
                TEST_LOGGER_ACTIVE.store(true, Ordering::Relaxed);
            }
        });
        TEST_LOGGER_ACTIVE.load(Ordering::Relaxed)
    }

    fn captured_logs() -> Vec<String> {
        TEST_LOG_MESSAGES
            .get()
            .and_then(|messages| messages.lock().ok().map(|messages| messages.clone()))
            .unwrap_or_default()
    }

    impl BlurayProbedStreamFacts {
        fn test_fact(
            pid: Option<u16>,
            audio_ordinal: Option<u8>,
            codec_name: &str,
            profile: Option<&str>,
            sample_rate: Option<u32>,
            bit_depth: Option<u32>,
            channels: Option<u8>,
        ) -> Self {
            Self {
                ffprobe_index: audio_ordinal.map(u32::from),
                audio_ordinal,
                pid,
                codec_name: Some(codec_name.to_string()),
                codec_profile: profile.map(str::to_string),
                bit_depth,
                sample_rate,
                channels,
            }
        }
    }

    fn existing_source_path(test_name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "tonepoet_bluray_probe_{test_name}_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create fake Blu-ray source path");
        path
    }

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
            _source: &mut Self::TitleSource,
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
    fn mapper_uses_real_probe_result_for_compressed_bit_depth() {
        let disc = fake_disc(vec![fake_title(
            12,
            58 * 60 + 32,
            12,
            vec![stream(
                0x1100,
                0,
                BluRayAudioCoding::DtsHdMaster,
                Some(192_000),
                None,
                Some(2),
            )],
        )]);
        let source = existing_source_path("mapper_uses_real_probe_result_for_compressed_bit_depth");
        let probe = FakeCompressedAudioProbe::success(vec![BlurayProbedStreamFacts::test_fact(
            Some(0x1100),
            Some(0),
            "dts",
            Some("DTS-HD MA"),
            Some(192_000),
            Some(24),
            Some(2),
        )]);

        let contents = map_bluray_disc_with_audio_probe::<FakeBackend, _>(
            &disc,
            &source,
            &probe,
            &BlurayProbeControl::bounded_default(),
        )
        .unwrap();

        assert_eq!(probe.call_count(), 1);
        assert_eq!(contents.presentations.len(), 1);
        assert_eq!(contents.presentations[0].format.bit_depth, Some(24));
        assert_eq!(
            contents.presentations[0].label,
            "Blu-ray Playlist 00012 Stream 1 PID 0x1100 · DTS-HD MA 192 kHz / 24-bit / Stereo"
        );
    }

    #[test]
    fn parses_ffprobe_playlist_audio_stream_facts_with_pid_and_ordinals() {
        let facts = parse_ffprobe_playlist_audio_facts(
            b"[STREAM]
index=2
id=0x1100
codec_name=dts
profile=DTS-HD MA
sample_rate=192000
channels=2
bits_per_raw_sample=24
[/STREAM]
[STREAM]
index=3
id=0x1102
codec_name=truehd
profile=unknown
sample_rate=192000
channels=2
bits_per_raw_sample=24
[/STREAM]
",
        )
        .unwrap();

        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].ffprobe_index, Some(2));
        assert_eq!(facts[0].audio_ordinal, Some(0));
        assert_eq!(facts[0].pid, Some(0x1100));
        assert_eq!(facts[0].codec_name.as_deref(), Some("dts"));
        assert_eq!(facts[0].codec_profile.as_deref(), Some("DTS-HD MA"));
        assert_eq!(facts[0].sample_rate, Some(192_000));
        assert_eq!(facts[0].bit_depth, Some(24));
        assert_eq!(facts[0].channels, Some(2));
        assert_eq!(facts[1].audio_ordinal, Some(1));
        assert_eq!(facts[1].pid, Some(0x1102));
    }

    #[test]
    fn parses_ffprobe_default_stream_facts() {
        let facts = parse_ffprobe_stream_facts(
            b"codec_name=truehd
sample_rate=192000
channels=2
bits_per_raw_sample=24
",
        )
        .unwrap();

        assert_eq!(facts.sample_rate, Some(192_000));
        assert_eq!(facts.bit_depth, Some(24));
        assert_eq!(facts.channels, Some(2));
    }

    #[cfg(unix)]
    #[test]
    fn ffprobe_command_failure_reports_status_and_stderr() {
        use std::os::unix::fs::PermissionsExt;

        let dir = existing_source_path("ffprobe_command_failure_reports_status_and_stderr");
        let ffprobe = dir.join("ffprobe-fails.sh");
        std::fs::write(
            &ffprobe,
            "#!/bin/sh
echo synthetic ffprobe failure >&2
exit 42
",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&ffprobe).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&ffprobe, permissions).unwrap();

        let err = ffprobe_bluray_playlist_audio_streams(
            &ffprobe,
            &dir,
            12,
            &BlurayProbeControl::bounded_default(),
        )
        .unwrap_err();

        assert!(err.contains("ffprobe exited with status"));
        assert!(err.contains("synthetic ffprobe failure"));
    }

    #[cfg(unix)]
    #[test]
    fn ffprobe_command_uses_injected_path_and_playlist_wide_audio_entries() {
        use std::os::unix::fs::PermissionsExt;

        let dir = existing_source_path(
            "ffprobe_command_uses_injected_path_and_playlist_wide_audio_entries",
        );
        let ffprobe = dir.join("ffprobe-records-args.sh");
        let args_file = dir.join("args.txt");
        std::fs::write(
            &ffprobe,
            format!(
                "#!/bin/sh\nprintf '%s\n' \"$@\" > '{}'\ncat <<'EOF'\n[STREAM]\nindex=7\nid=0x1102\ncodec_name=truehd\nsample_rate=192000\nchannels=2\nbits_per_raw_sample=24\n[/STREAM]\nEOF\n",
                args_file.display()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&ffprobe).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&ffprobe, permissions).unwrap();

        let facts = ffprobe_bluray_playlist_audio_streams(
            &ffprobe,
            &dir,
            777,
            &BlurayProbeControl::bounded_default(),
        )
        .unwrap();

        let args = std::fs::read_to_string(args_file).unwrap();
        assert!(args.contains("-playlist\n777\n"));
        assert!(args.contains("-select_streams\na\n"));
        assert!(args.contains(
            "stream=index,id,codec_name,profile,bits_per_raw_sample,sample_rate,channels"
        ));
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].pid, Some(0x1102));
        assert_eq!(facts[0].codec_name.as_deref(), Some("truehd"));
        assert_eq!(facts[0].sample_rate, Some(192_000));
        assert_eq!(facts[0].channels, Some(2));
        assert_eq!(facts[0].bit_depth, Some(24));
    }

    #[test]
    fn ffprobe_probe_honors_pre_cancelled_control_before_spawning() {
        let cancelled = || true;
        let control = BlurayProbeControl::bounded_default().with_cancellation(&cancelled);
        let err = ffprobe_bluray_playlist_audio_streams(
            Path::new("/definitely/not/ffprobe"),
            Path::new("/definitely/not/a/disc"),
            12,
            &control,
        )
        .unwrap_err();

        assert!(err.contains("cancelled before ffprobe"));
    }

    #[cfg(unix)]
    #[test]
    fn ffprobe_probe_times_out_and_terminates_child() {
        use std::os::unix::fs::PermissionsExt;

        let dir = existing_source_path("ffprobe_probe_times_out_and_terminates_child");
        let ffprobe = dir.join("ffprobe-hangs.sh");
        std::fs::write(
            &ffprobe,
            "#!/bin/sh\nwhile :; do :; done\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&ffprobe).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&ffprobe, permissions).unwrap();

        let control = BlurayProbeControl::bounded_default()
            .with_timeout(Duration::from_millis(50));
        let err = ffprobe_bluray_playlist_audio_streams(&ffprobe, &dir, 12, &control)
            .unwrap_err();

        assert!(err.contains("timed out"));
    }

    #[test]
    fn ffprobe_probe_uses_configured_tool_path() {
        let configured = PathBuf::from("/opt/tonepoet/bin/ffprobe-custom");
        let mut tool_paths = TestHashMap::new();
        tool_paths.insert("ffprobe".to_string(), configured.clone());

        let probe = FfprobeBlurayAudioProbe::from_tool_paths(&tool_paths);

        assert_eq!(probe.ffprobe_path, configured);
    }

    #[test]
    fn missing_required_bit_depth_for_lossless_compressed_stream_is_atomic() {
        let source = existing_source_path(
            "missing_required_bit_depth_for_lossless_compressed_stream_is_atomic",
        );
        let mut stream = stream(
            0x1102,
            2,
            BluRayAudioCoding::TrueHd,
            Some(96_000),
            None,
            Some(2),
        );
        let original = stream.clone();
        let probe = FakeCompressedAudioProbe::success(vec![BlurayProbedStreamFacts::test_fact(
            Some(0x1102),
            Some(2),
            "truehd",
            None,
            Some(192_000),
            None,
            Some(6),
        )]);

        let err = probe_compressed_bluray_stream_for_playlist_with_probe(
            &source,
            12,
            &mut stream,
            &probe,
            &BlurayProbeControl::bounded_default(),
        )
        .unwrap_err();

        assert!(err.contains("bits_per_raw_sample"));
        assert_eq!(stream, original);
    }

    #[test]
    fn failed_probe_result_does_not_poison_later_success_for_same_source_playlist() {
        let source = existing_source_path(
            "failed_probe_result_does_not_poison_later_success_for_same_source_playlist",
        );
        let mut failed_stream = stream(
            0x1100,
            0,
            BluRayAudioCoding::DtsHdMaster,
            Some(192_000),
            None,
            Some(2),
        );
        let failing_probe = FakeCompressedAudioProbe::failure("ffprobe missing on PATH");

        let err = probe_compressed_bluray_stream_for_playlist_with_probe(
            &source,
            12,
            &mut failed_stream,
            &failing_probe,
            &BlurayProbeControl::bounded_default(),
        )
        .unwrap_err();

        assert!(err.contains("ffprobe missing on PATH"));
        assert_eq!(failing_probe.call_count(), 1);
        assert_eq!(failed_stream.probed_bit_depth, None);

        let mut successful_stream = stream(
            0x1100,
            0,
            BluRayAudioCoding::DtsHdMaster,
            Some(192_000),
            None,
            Some(2),
        );
        let succeeding_probe = FakeCompressedAudioProbe::success(vec![
            BlurayProbedStreamFacts::test_fact(
                Some(0x1100),
                Some(0),
                "dts",
                Some("DTS-HD MA"),
                Some(192_000),
                Some(24),
                Some(2),
            ),
        ]);

        probe_compressed_bluray_stream_for_playlist_with_probe(
            &source,
            12,
            &mut successful_stream,
            &succeeding_probe,
            &BlurayProbeControl::bounded_default(),
        )
        .unwrap();

        assert_eq!(succeeding_probe.call_count(), 1);
        assert_eq!(successful_stream.probed_bit_depth, Some(24));
    }

    #[test]
    fn incomplete_successful_probe_does_not_poison_later_complete_probe_for_same_source_playlist() {
        let source = existing_source_path(
            "incomplete_successful_probe_does_not_poison_later_complete_probe_for_same_source_playlist",
        );
        let mut incomplete_stream = stream(
            0x1102,
            2,
            BluRayAudioCoding::TrueHd,
            Some(192_000),
            None,
            Some(2),
        );
        let incomplete_probe = FakeCompressedAudioProbe::success(vec![
            BlurayProbedStreamFacts::test_fact(
                Some(0x1102),
                Some(2),
                "truehd",
                None,
                Some(192_000),
                None,
                Some(2),
            ),
        ]);

        let err = probe_compressed_bluray_stream_for_playlist_with_probe(
            &source,
            12,
            &mut incomplete_stream,
            &incomplete_probe,
            &BlurayProbeControl::bounded_default(),
        )
        .unwrap_err();

        assert!(err.contains("bits_per_raw_sample"));
        assert_eq!(incomplete_probe.call_count(), 1);
        assert_eq!(incomplete_stream.probed_bit_depth, None);

        let mut complete_stream = stream(
            0x1102,
            2,
            BluRayAudioCoding::TrueHd,
            Some(192_000),
            None,
            Some(2),
        );
        let complete_probe = FakeCompressedAudioProbe::success(vec![
            BlurayProbedStreamFacts::test_fact(
                Some(0x1102),
                Some(2),
                "truehd",
                None,
                Some(192_000),
                Some(24),
                Some(2),
            ),
        ]);

        probe_compressed_bluray_stream_for_playlist_with_probe(
            &source,
            12,
            &mut complete_stream,
            &complete_probe,
            &BlurayProbeControl::bounded_default(),
        )
        .unwrap();

        assert_eq!(complete_probe.call_count(), 1);
        assert_eq!(complete_stream.probed_bit_depth, Some(24));
    }

    #[test]
    fn browse_probe_failure_leaves_compressed_stream_unchanged() {
        let disc = fake_disc(vec![fake_title(
            12,
            3600,
            12,
            vec![stream(
                0x1100,
                0,
                BluRayAudioCoding::DtsHdMaster,
                Some(192_000),
                None,
                Some(2),
            )],
        )]);
        let source = existing_source_path("browse_probe_failure_leaves_compressed_stream_unchanged");
        let probe = FakeCompressedAudioProbe::failure("synthetic ffprobe failure");

        let contents = map_bluray_disc_with_audio_probe::<FakeBackend, _>(
            &disc,
            &source,
            &probe,
            &BlurayProbeControl::bounded_default(),
        )
        .unwrap();

        assert_eq!(probe.call_count(), 1);
        assert_eq!(contents.presentations.len(), 1);
        assert_eq!(contents.presentations[0].format.sample_rate, Some(192_000));
        assert_eq!(contents.presentations[0].format.bit_depth, None);
        assert_eq!(contents.presentations[0].format.channels, Some(2));
    }

    #[test]
    fn sample_rate_mismatch_logs_and_applies_only_after_required_bit_depth_present() {
        let captures_logs = begin_log_capture();
        let source = existing_source_path(
            "sample_rate_mismatch_applies_only_after_required_bit_depth_present",
        );
        let mut stream = stream(
            0x1102,
            2,
            BluRayAudioCoding::TrueHd,
            Some(96_000),
            None,
            Some(2),
        );
        let probe = FakeCompressedAudioProbe::success(vec![BlurayProbedStreamFacts::test_fact(
            Some(0x1102),
            Some(2),
            "truehd",
            None,
            Some(192_000),
            Some(24),
            Some(6),
        )]);

        probe_compressed_bluray_stream_for_playlist_with_probe(
            &source,
            12,
            &mut stream,
            &probe,
            &BlurayProbeControl::bounded_default(),
        )
        .unwrap();

        assert_eq!(stream.sample_rate, Some(192_000));
        assert_eq!(stream.probed_bit_depth, Some(24));
        assert_eq!(stream.channels, Some(6));
        assert_eq!(stream.channel_layout.as_deref(), Some("5.1"));
        if captures_logs {
            assert!(captured_logs().iter().any(|message| {
                message.contains("CLPI sample rate 96000 Hz differs")
                    && message.contains("ffprobe decoded sample rate 192000 Hz")
            }));
        }
    }

    #[test]
    fn protected_disc_skips_compressed_ffprobe_probe() {
        let disc = {
            let mut disc = fake_disc(vec![fake_title(
                12,
                3600,
                12,
                vec![stream(
                    0x1100,
                    0,
                    BluRayAudioCoding::DtsHdMaster,
                    Some(192_000),
                    None,
                    Some(2),
                )],
            )]);
            disc.protection_status = BlurayProtectionStatus::AacsDetectedNotHandled {
                details: BlurayAacsStatus {
                    handled: false,
                    libaacs_detected: true,
                    error_code: Some(1),
                    mkb_version: Some(78),
                },
            };
            disc
        };
        let source = existing_source_path("protected_disc_skips_compressed_ffprobe_probe");
        let probe = FakeCompressedAudioProbe::success(vec![BlurayProbedStreamFacts::test_fact(
            Some(0x1100),
            Some(0),
            "dts",
            Some("DTS-HD MA"),
            Some(192_000),
            Some(24),
            Some(2),
        )]);

        let contents = map_bluray_disc_with_audio_probe::<FakeBackend, _>(
            &disc,
            &source,
            &probe,
            &BlurayProbeControl::bounded_default(),
        )
        .unwrap();

        assert_eq!(probe.call_count(), 0);
        assert_eq!(contents.presentations.len(), 1);
        assert_eq!(contents.presentations[0].format.bit_depth, None);
    }

    #[test]
    fn stream_matching_prefers_pid_and_codec_over_audio_ordinal_for_truehd_core_pid() {
        let source = existing_source_path(
            "stream_matching_prefers_pid_and_codec_over_audio_ordinal_for_truehd_core_pid",
        );
        let mut stream = stream(
            0x1102,
            2,
            BluRayAudioCoding::TrueHd,
            Some(192_000),
            None,
            Some(2),
        );
        let probe = FakeCompressedAudioProbe::success(vec![
            BlurayProbedStreamFacts::test_fact(
                Some(0x1102),
                Some(2),
                "ac3",
                None,
                Some(48_000),
                None,
                Some(2),
            ),
            BlurayProbedStreamFacts::test_fact(
                Some(0x1102),
                Some(3),
                "truehd",
                None,
                Some(192_000),
                Some(24),
                Some(2),
            ),
        ]);

        probe_compressed_bluray_stream_for_playlist_with_probe(
            &source,
            12,
            &mut stream,
            &probe,
            &BlurayProbeControl::bounded_default(),
        )
        .unwrap();

        assert_eq!(stream.sample_rate, Some(192_000));
        assert_eq!(stream.probed_bit_depth, Some(24));
        assert_eq!(stream.channels, Some(2));
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
            bit_depth: if coding == BluRayAudioCoding::Lpcm {
                match bit_depth {
                    Some(bit_depth) => BlurayLpcmBitDepth::Probed {
                        bit_depth,
                        scanned_bytes: 188,
                    },
                    None => BlurayLpcmBitDepth::NotProbed {
                        reason: super::super::bluray_backend::BlurayLpcmNotProbedReason::ProbePolicyNone,
                    },
                }
            } else {
                BlurayLpcmBitDepth::NotApplicable
            },
            probed_bit_depth: if coding.is_compressed() { bit_depth } else { None },
            channels,
            channel_layout: channels.map(channel_layout_from_count),
            language: Some("eng".to_string()),
        }
    }
}
