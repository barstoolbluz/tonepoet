//! Blu-ray source materializer.
//!
//! This stage turns one selected Blu-ray playlist/audio-stream/display-angle
//! presentation into per-chapter `BluRayTrack` references. It does not demux or
//! decode media bytes. Phase 3/4 realization will consume the typed source
//! references created here.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::convert::TryFrom;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::disc::bluray_backend::{
    bluray_audio_stream_display_number, validate_bluray_streams_for_materialization,
    BluRayAudioCoding, BluRayAudioStreamKind, BlurayAudioStreamInfo, BlurayBackend,
    BlurayChapterInfo, BlurayDisplayAngle, BlurayTitleInfo, ProbeDepth,
};
use crate::disc::bluray_backend_libbluray::BlurayBackendLibbluray;
use crate::disc::model::PresentationId;

use super::errors::{MaterializeError, SourceDetectError};
use super::reporter::PipelineReporter;
use super::stages::Materializer;
use super::tool::{ToolBinary, ToolRunner};
use super::types::*;

pub struct BlurayMaterializer;

#[async_trait]
impl Materializer for BlurayMaterializer {
    async fn materialize(
        &self,
        req: &PipelineRequest,
        staging: &StagingDir,
        runner: &dyn ToolRunner,
        _reporter: Option<&dyn PipelineReporter>,
        _tool_paths: &HashMap<String, PathBuf>,
        cancel: &CancellationToken,
    ) -> Result<PreparedSource, MaterializeError> {
        std::fs::create_dir_all(&staging.root).map_err(|err| {
            MaterializeError::Extraction(format!(
                "failed to create staging directory '{}': {err}",
                staging.root.display()
            ))
        })?;
        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }

        let source = crate::disc::bluray_utils::bluray_source_path_for_backend(&req.container)
            .map_err(MaterializeError::Parse)?;
        let disc = BlurayBackendLibbluray::open(&source).map_err(|err| {
            MaterializeError::Parse(format!(
                "Blu-ray open failed for '{}': {err}",
                source.display()
            ))
        })?;
        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }

        let mut selection = select_bluray_program::<BlurayBackendLibbluray>(
            &disc,
            &source,
            &req.source,
        )?;
        selection.stream =
            selected_stream_with_materialization_facts::<BlurayBackendLibbluray>(&disc, &selection)?;

        build_prepared_bluray_source(
            &req.container,
            &source,
            &req.source.track_selection,
            &selection,
            runner,
            Some(cancel),
        )
    }
}

/// Route Blu-ray sources after SACD, DVD-Audio, and DVD-Video first-refusal.
pub(crate) fn is_bluray_candidate(req: &PipelineRequest) -> Result<bool, SourceDetectError> {
    if req.container.is_file() {
        return Ok(crate::disc::bluray_utils::is_bluray_iso(&req.container)
            || (req.source.explicit_bluray_requested() && has_extension(&req.container, "iso")));
    }
    if req.container.is_dir() {
        return Ok(crate::disc::bluray_utils::is_bluray_directory(&req.container)
            || req.source.explicit_bluray_requested());
    }
    Ok(false)
}

#[derive(Debug, Clone)]
struct BlurayProgramSelection {
    title: BlurayTitleInfo,
    display_angle: BlurayDisplayAngle,
    max_angle: u8,
    chapters: Vec<BlurayChapterInfo>,
    stream: BlurayAudioStreamInfo,
}

#[derive(Debug, Clone, Copy)]
struct BlurayPresentationRequest {
    playlist_number: Option<u32>,
    audio_pid: Option<u16>,
    audio_stream_index: Option<u8>,
    display_angle: Option<u8>,
}

impl BlurayPresentationRequest {
    fn from_source_options(options: &SourceOptions) -> Self {
        Self {
            playlist_number: options.bluray_playlist,
            audio_pid: options.bluray_audio_pid,
            audio_stream_index: options.bluray_audio_stream,
            display_angle: options.bluray_angle,
        }
    }

    fn exact(
        playlist_number: u32,
        audio_pid: u16,
        audio_stream_index: u8,
        display_angle: u8,
    ) -> Self {
        Self {
            playlist_number: Some(playlist_number),
            audio_pid: Some(audio_pid),
            audio_stream_index: Some(audio_stream_index),
            display_angle: Some(display_angle),
        }
    }
}

fn select_bluray_program<B: BlurayBackend>(
    disc: &B::Disc,
    source: &Path,
    options: &SourceOptions,
) -> Result<BlurayProgramSelection, MaterializeError> {
    if options.explicit_bluray_requested() {
        let request = BlurayPresentationRequest::from_source_options(options);
        if request.playlist_number.is_some() {
            return select_bluray_program_direct::<B>(disc, request);
        }
        return select_filtered_bluray_program_from_mapper::<B>(disc, source, request);
    }

    let contents = crate::disc::bluray_mapper::map_bluray_disc::<B>(disc, source)
        .map_err(MaterializeError::Parse)?;
    let index = crate::disc::bluray_mapper::best_bluray_presentation_index(&contents)
        .ok_or_else(|| {
            MaterializeError::InvalidTrackSelection(format!(
                "Blu-ray source '{}' contains no supported audio presentation",
                source.display()
            ))
        })?;
    let id = contents.presentations[index].id;
    let (playlist_number, audio_pid, audio_stream_index, display_angle) = id
        .blu_ray_parts()
        .ok_or_else(|| MaterializeError::Parse("best Blu-ray presentation has non-Blu-ray identity".to_string()))?;
    select_bluray_program_direct::<B>(
        disc,
        BlurayPresentationRequest::exact(
            playlist_number,
            audio_pid,
            audio_stream_index,
            display_angle,
        ),
    )
}

fn select_filtered_bluray_program_from_mapper<B: BlurayBackend>(
    disc: &B::Disc,
    source: &Path,
    request: BlurayPresentationRequest,
) -> Result<BlurayProgramSelection, MaterializeError> {
    let contents = crate::disc::bluray_mapper::map_bluray_disc::<B>(disc, source)
        .map_err(MaterializeError::Parse)?;
    let presentation = contents
        .presentations
        .iter()
        .filter(|presentation| presentation_matches_request(presentation.id, request))
        .max_by_key(|presentation| crate::disc::bluray_mapper::score_bluray_presentation(presentation))
        .ok_or_else(|| {
            MaterializeError::InvalidTrackSelection(format!(
                "Blu-ray source '{}' contains no supported presentation matching playlist {}, PID {}, stream {}, angle {}",
                source.display(),
                display_option_u32(request.playlist_number),
                display_option_pid(request.audio_pid),
                display_option_u8(request.audio_stream_index),
                display_option_u8(request.display_angle),
            ))
        })?;
    let (playlist_number, audio_pid, audio_stream_index, display_angle) = presentation
        .id
        .blu_ray_parts()
        .ok_or_else(|| MaterializeError::Parse("matched Blu-ray presentation has non-Blu-ray identity".to_string()))?;
    select_bluray_program_direct::<B>(
        disc,
        BlurayPresentationRequest::exact(
            playlist_number,
            audio_pid,
            audio_stream_index,
            display_angle,
        ),
    )
}

fn presentation_matches_request(id: PresentationId, request: BlurayPresentationRequest) -> bool {
    let Some((playlist_number, audio_pid, audio_stream_index, display_angle)) = id.blu_ray_parts()
    else {
        return false;
    };
    request
        .playlist_number
        .map_or(true, |wanted| wanted == playlist_number)
        && request.audio_pid.map_or(true, |wanted| wanted == audio_pid)
        && request
            .audio_stream_index
            .map_or(true, |wanted| wanted == audio_stream_index)
        && request
            .display_angle
            .map_or(true, |wanted| wanted == display_angle)
}

fn select_bluray_program_direct<B: BlurayBackend>(
    disc: &B::Disc,
    request: BlurayPresentationRequest,
) -> Result<BlurayProgramSelection, MaterializeError> {
    let titles = B::titles(disc).map_err(MaterializeError::Parse)?;
    let candidate_titles = requested_titles(&titles, request.playlist_number)?;
    let mut best: Option<(BlurayDirectSelectionScore, BlurayProgramSelection)> = None;

    for title in candidate_titles {
        let max_angle = B::max_angle(disc, title.key)
            .map_err(MaterializeError::Parse)?
            .max(title.angle_count)
            .max(1);
        let display_angle = select_display_angle(title.playlist_number, max_angle, request.display_angle)?;
        let chapters = B::chapters(disc, title.key, display_angle)
            .map_err(MaterializeError::Parse)?;
        if chapters.is_empty() {
            if request.playlist_number.is_some() {
                return Err(MaterializeError::InvalidTrackSelection(format!(
                    "Blu-ray playlist {:05} angle {} contains no chapters",
                    title.playlist_number,
                    display_angle.get()
                )));
            }
            continue;
        }
        validate_chapter_pts(&title, &chapters)?;

        let streams = B::streams(disc, title.key).map_err(MaterializeError::Parse)?;
        let streams = supported_primary_streams(streams);
        validate_explicit_stream_pair(title.playlist_number, &streams, request)?;
        let matching_streams = streams_matching_request(&streams, request);
        if matching_streams.is_empty() {
            if request.playlist_number.is_some() {
                return Err(no_matching_stream_error(title.playlist_number, request));
            }
            continue;
        }

        for stream in matching_streams {
            let selection = BlurayProgramSelection {
                title: title.clone(),
                display_angle,
                max_angle,
                chapters: chapters.clone(),
                stream: stream.clone(),
            };
            let score = direct_selection_score(&selection);
            let should_replace = match best.as_ref() {
                Some((best_score, _)) => score > *best_score,
                None => true,
            };
            if should_replace {
                best = Some((score, selection));
            }
        }
    }

    best.map(|(_, selection)| selection).ok_or_else(|| {
        MaterializeError::InvalidTrackSelection(format!(
            "Blu-ray source contains no supported presentation matching playlist {}, PID {}, stream {}, angle {}",
            display_option_u32(request.playlist_number),
            display_option_pid(request.audio_pid),
            display_option_u8(request.audio_stream_index),
            display_option_u8(request.display_angle),
        ))
    })
}

fn requested_titles<'a>(
    titles: &'a [BlurayTitleInfo],
    playlist_number: Option<u32>,
) -> Result<Vec<&'a BlurayTitleInfo>, MaterializeError> {
    if let Some(playlist_number) = playlist_number {
        let title = titles
            .iter()
            .find(|title| title.playlist_number == playlist_number)
            .ok_or_else(|| {
                MaterializeError::InvalidTrackSelection(format!(
                    "Blu-ray playlist {playlist_number:05} not found"
                ))
            })?;
        return Ok(vec![title]);
    }
    Ok(titles.iter().collect())
}

fn select_display_angle(
    playlist_number: u32,
    max_angle: u8,
    requested_angle: Option<u8>,
) -> Result<BlurayDisplayAngle, MaterializeError> {
    let requested_angle = requested_angle.unwrap_or(1);
    let display_angle = BlurayDisplayAngle::new(requested_angle).map_err(|err| {
        MaterializeError::InvalidTrackSelection(format!(
            "Blu-ray playlist {playlist_number:05} angle {requested_angle} is invalid: {err}"
        ))
    })?;
    if display_angle.get() > max_angle {
        return Err(MaterializeError::InvalidTrackSelection(format!(
            "Blu-ray playlist {playlist_number:05} has {max_angle} angle(s); supported angle range is 1..={max_angle}, cannot select angle {}",
            display_angle.get()
        )));
    }
    Ok(display_angle)
}

fn supported_primary_streams(streams: Vec<BlurayAudioStreamInfo>) -> Vec<BlurayAudioStreamInfo> {
    streams
        .into_iter()
        .filter(|stream| stream.kind == BluRayAudioStreamKind::Primary)
        .collect()
}

fn validate_explicit_stream_pair(
    playlist_number: u32,
    streams: &[BlurayAudioStreamInfo],
    request: BlurayPresentationRequest,
) -> Result<(), MaterializeError> {
    let (Some(audio_pid), Some(audio_stream_index)) = (request.audio_pid, request.audio_stream_index)
    else {
        return Ok(());
    };

    if let Some(stream) = streams
        .iter()
        .find(|stream| stream.stream_index == audio_stream_index)
    {
        if stream.pid != audio_pid {
            return Err(MaterializeError::InvalidTrackSelection(format!(
                "Blu-ray playlist {:05} audio stream {} is PID 0x{:04x}, not requested PID 0x{audio_pid:04x}",
                playlist_number,
                bluray_audio_stream_display_number(audio_stream_index),
                stream.pid
            )));
        }
    }
    Ok(())
}

fn streams_matching_request<'a>(
    streams: &'a [BlurayAudioStreamInfo],
    request: BlurayPresentationRequest,
) -> Vec<&'a BlurayAudioStreamInfo> {
    streams
        .iter()
        .filter(|stream| request.audio_pid.map_or(true, |pid| pid == stream.pid))
        .filter(|stream| {
            request
                .audio_stream_index
                .map_or(true, |index| index == stream.stream_index)
        })
        .collect()
}

fn no_matching_stream_error(
    playlist_number: u32,
    request: BlurayPresentationRequest,
) -> MaterializeError {
    match (request.audio_pid, request.audio_stream_index) {
        (Some(pid), Some(index)) => MaterializeError::InvalidTrackSelection(format!(
            "Blu-ray playlist {playlist_number:05} has no supported primary audio stream {} with requested PID {pid} (0x{pid:04x})",
            bluray_audio_stream_display_number(index)
        )),
        (Some(pid), None) => MaterializeError::InvalidTrackSelection(format!(
            "Blu-ray playlist {playlist_number:05} has no supported primary audio stream with requested PID {pid} (0x{pid:04x})"
        )),
        (None, Some(index)) => MaterializeError::InvalidTrackSelection(format!(
            "Blu-ray playlist {playlist_number:05} has no supported primary audio stream {}",
            bluray_audio_stream_display_number(index)
        )),
        (None, None) => MaterializeError::InvalidTrackSelection(format!(
            "Blu-ray playlist {playlist_number:05} has no supported primary audio streams"
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BlurayDirectSelectionScore {
    has_chapter_durations: bool,
    chapter_count: usize,
    duration_pts_90k: u64,
    is_stereo: bool,
    is_lossless: bool,
    codec_rank: u8,
    sample_rate: u32,
    bit_depth: u32,
    reverse_identity: ReverseBlurayIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ReverseBlurayIdentity {
    playlist_number: Reverse<u32>,
    audio_stream_index: Reverse<u8>,
    display_angle: Reverse<u8>,
}

fn direct_selection_score(selection: &BlurayProgramSelection) -> BlurayDirectSelectionScore {
    BlurayDirectSelectionScore {
        has_chapter_durations: has_chapter_durations(&selection.chapters),
        chapter_count: selection.chapters.len(),
        duration_pts_90k: selection.title.duration_pts_90k,
        is_stereo: selection.stream.channels == Some(2)
            || selection
                .stream
                .channel_layout
                .as_deref()
                .is_some_and(|layout| layout.eq_ignore_ascii_case("stereo")),
        is_lossless: selection.stream.coding.is_lossless(),
        codec_rank: selection.stream.coding.codec_rank(),
        sample_rate: selection.stream.sample_rate.unwrap_or(0),
        bit_depth: selection.stream.bit_depth.bit_depth().unwrap_or(0),
        reverse_identity: ReverseBlurayIdentity {
            playlist_number: Reverse(selection.title.playlist_number),
            audio_stream_index: Reverse(selection.stream.stream_index),
            display_angle: Reverse(selection.display_angle.get()),
        },
    }
}

fn has_chapter_durations(chapters: &[BlurayChapterInfo]) -> bool {
    !chapters.is_empty()
        && chapters.iter().all(|chapter| {
            chapter.duration_pts_90k.is_some()
                || chapter
                    .end_pts_90k
                    .and_then(|end| end.checked_sub(chapter.start_pts_90k))
                    .is_some()
        })
}

fn selected_stream_with_materialization_facts<B: BlurayBackend>(
    disc: &B::Disc,
    selection: &BlurayProgramSelection,
) -> Result<BlurayAudioStreamInfo, MaterializeError> {
    if selection.stream.coding != BluRayAudioCoding::Lpcm {
        return Ok(selection.stream.clone());
    }

    let streams = B::streams_with_probe_policy(
        disc,
        selection.title.key,
        ProbeDepth::bounded_default(),
    )
    .map_err(|err| {
        MaterializeError::Parse(format!(
            "Blu-ray playlist {:05} LPCM bit-depth probe failed for PID 0x{:04x}: {err}",
            selection.title.playlist_number, selection.stream.pid
        ))
    })?;

    let stream = streams
        .into_iter()
        .find(|stream| {
            stream.kind == BluRayAudioStreamKind::Primary
                && stream.pid == selection.stream.pid
                && stream.stream_index == selection.stream.stream_index
        })
        .ok_or_else(|| {
            MaterializeError::Parse(format!(
                "Blu-ray playlist {:05} LPCM bit-depth probe did not return selected stream {} PID 0x{:04x}",
                selection.title.playlist_number,
                bluray_audio_stream_display_number(selection.stream.stream_index),
                selection.stream.pid
            ))
        })?;

    validate_bluray_streams_for_materialization(std::slice::from_ref(&stream)).map_err(|err| {
        MaterializeError::Parse(format!(
            "Blu-ray playlist {:05} LPCM bit-depth probe did not produce a materializable stream: {err}",
            selection.title.playlist_number
        ))
    })?;

    if stream.bit_depth.is_probed() {
        Ok(stream)
    } else {
        Err(MaterializeError::Parse(format!(
            "Blu-ray playlist {:05} LPCM stream {} PID 0x{:04x} is missing probed bit depth after validation",
            selection.title.playlist_number,
            bluray_audio_stream_display_number(selection.stream.stream_index),
            selection.stream.pid
        )))
    }
}

fn bluray_track_bit_depth(stream: &BlurayAudioStreamInfo) -> Option<u32> {
    match stream.coding {
        BluRayAudioCoding::Lpcm => stream.bit_depth.bit_depth(),
        BluRayAudioCoding::Ac3
        | BluRayAudioCoding::Eac3
        | BluRayAudioCoding::Dts
        | BluRayAudioCoding::TrueHd
        | BluRayAudioCoding::DtsHd
        | BluRayAudioCoding::DtsHdMaster => None,
    }
}

fn source_audio_coding(coding: BluRayAudioCoding) -> Option<SourceAudioCoding> {
    match coding {
        BluRayAudioCoding::Lpcm => Some(SourceAudioCoding::Pcm),
        BluRayAudioCoding::Ac3
        | BluRayAudioCoding::Eac3
        | BluRayAudioCoding::Dts
        | BluRayAudioCoding::TrueHd
        | BluRayAudioCoding::DtsHd
        | BluRayAudioCoding::DtsHdMaster => Some(SourceAudioCoding::Unknown),
    }
}


fn build_prepared_bluray_source(
    container: &Path,
    source: &Path,
    track_selection: &TrackSelection,
    selection: &BlurayProgramSelection,
    runner: &dyn ToolRunner,
    cancel: Option<&CancellationToken>,
) -> Result<PreparedSource, MaterializeError> {
    validate_bluray_streams_for_materialization(std::slice::from_ref(&selection.stream)).map_err(
        |err| {
            MaterializeError::Parse(format!(
                "Blu-ray playlist {:05} stream {} PID 0x{:04x} cannot be materialized: {err}",
                selection.title.playlist_number,
                bluray_audio_stream_display_number(selection.stream.stream_index),
                selection.stream.pid
            ))
        },
    )?;

    let title_chapter_count = bluray_chapter_count_for_selection(selection)?;
    let selected_chapters = selected_chapter_ordinals(title_chapter_count, track_selection)?;
    let sidecar_identity = bluray_sidecar_identity(selection, title_chapter_count);

    let mut tracks = Vec::with_capacity(selected_chapters.len());
    for (chapter_index, chapter) in selection.chapters.iter().enumerate() {
        if !selected_chapters.contains(&chapter.chapter_number) {
            continue;
        }
        if let Some(cancel) = cancel {
            if cancel.is_cancelled() {
                return Err(MaterializeError::Cancelled);
            }
        }

        let bit_depth = bluray_track_bit_depth(&selection.stream);
        let source_ordinal = chapter.chapter_number;
        let output_track_number = bluray_output_track_number(tracks.len());
        tracks.push(PreparedTrack {
            id: TrackId {
                source_ordinal,
                disc_number: None,
                track_number: output_track_number,
            },
            source_ref: TrackSourceRef::BluRayTrack {
                source: container.to_path_buf(),
                playlist_number: selection.title.playlist_number,
                title_index: selection.title.key.title_index() as usize,
                angle_number: selection.display_angle.get(),
                chapter_number: chapter.chapter_number,
                chapter_start_pts_90k: chapter.start_pts_90k,
                chapter_end_pts_90k: chapter_end_pts_90k(
                    &selection.title,
                    &selection.chapters,
                    chapter_index,
                )?,
                audio_pid: selection.stream.pid,
                audio_stream_index: selection.stream.stream_index,
                audio_coding: selection.stream.coding,
                sample_rate: selection.stream.sample_rate,
                bit_depth,
                channels: selection.stream.channels,
                channel_layout: selection.stream.channel_layout.clone(),
            },
            metadata: overlay_bluray_sidecar_metadata_stub(
                track_metadata(selection, chapter.chapter_number, output_track_number),
                &sidecar_identity,
            ),
            expected_samples: None,
            sample_rate: selection.stream.sample_rate,
            source_audio: SourceAudioDescriptor::from_scalar(
                selection.stream.sample_rate,
                bit_depth,
                source_audio_coding(selection.stream.coding),
            ),
            bit_depth,
        });
    }

    if tracks.is_empty() {
        return Err(MaterializeError::InvalidTrackSelection(format!(
            "Blu-ray playlist {:05} selection {:?} did not match any chapter",
            selection.title.playlist_number, track_selection
        )));
    }

    let album_metadata = overlay_bluray_sidecar_metadata_stub(
        base_album_metadata(source, selection, title_chapter_count),
        &sidecar_identity,
    );
    let tool_versions = bluray_tool_versions(runner, selection.stream.coding);

    Ok(PreparedSource {
        container: container.to_path_buf(),
        kind: SourceKind::BluRay,
        tracks,
        album_metadata,
        provenance: ExtractionProvenance {
            source_kind: SourceKind::BluRay,
            source_sha256: None,
            tool_versions,
            extracted_at: chrono::Utc::now(),
        },
    })
}

fn bluray_chapter_count_for_selection(
    selection: &BlurayProgramSelection,
) -> Result<u32, MaterializeError> {
    if selection.chapters.is_empty() {
        return Err(MaterializeError::InvalidTrackSelection(format!(
            "Blu-ray playlist {:05} angle {} contains no chapters",
            selection.title.playlist_number,
            selection.display_angle.get()
        )));
    }
    u32::try_from(selection.chapters.len()).map_err(|_| {
        MaterializeError::Parse(format!(
            "Blu-ray playlist {:05} has too many chapters to address as u32",
            selection.title.playlist_number
        ))
    })
}

fn validate_chapter_pts(
    title: &BlurayTitleInfo,
    chapters: &[BlurayChapterInfo],
) -> Result<(), MaterializeError> {
    for pair in chapters.windows(2) {
        let current = &pair[0];
        let next = &pair[1];
        if next.start_pts_90k < current.start_pts_90k {
            return Err(MaterializeError::Parse(format!(
                "Blu-ray playlist {:05} chapter {} starts at {}, but chapter {} starts earlier at {}",
                title.playlist_number,
                current.chapter_number,
                current.start_pts_90k,
                next.chapter_number,
                next.start_pts_90k
            )));
        }
    }

    if let Some(last) = chapters.last() {
        if title.duration_pts_90k > 0 && title.duration_pts_90k < last.start_pts_90k {
            return Err(MaterializeError::Parse(format!(
                "Blu-ray playlist {:05} duration {} is before final chapter {} start {}",
                title.playlist_number,
                title.duration_pts_90k,
                last.chapter_number,
                last.start_pts_90k
            )));
        }
    }
    Ok(())
}

fn chapter_end_pts_90k(
    title: &BlurayTitleInfo,
    chapters: &[BlurayChapterInfo],
    chapter_index: usize,
) -> Result<Option<u64>, MaterializeError> {
    let chapter = chapters.get(chapter_index).ok_or_else(|| {
        MaterializeError::Parse(format!(
            "Blu-ray playlist {:05} chapter index {} is outside chapter table",
            title.playlist_number, chapter_index
        ))
    })?;

    if let Some(next) = chapters.get(chapter_index.saturating_add(1)) {
        if next.start_pts_90k < chapter.start_pts_90k {
            return Err(MaterializeError::Parse(format!(
                "Blu-ray playlist {:05} chapter {} end would precede its start",
                title.playlist_number, chapter.chapter_number
            )));
        }
        return Ok(Some(next.start_pts_90k));
    }

    if title.duration_pts_90k == 0 {
        return Ok(None);
    }
    if title.duration_pts_90k < chapter.start_pts_90k {
        return Err(MaterializeError::Parse(format!(
            "Blu-ray playlist {:05} final chapter {} starts after title duration",
            title.playlist_number, chapter.chapter_number
        )));
    }
    Ok(Some(title.duration_pts_90k))
}

fn selected_chapter_ordinals(
    max_ordinal: u32,
    selection: &TrackSelection,
) -> Result<BTreeSet<u32>, MaterializeError> {
    match selection {
        TrackSelection::All => Ok((1..=max_ordinal).collect()),
        TrackSelection::Range { start, end } => {
            if *start == 0 || *end == 0 || start > end {
                return Err(MaterializeError::InvalidTrackSelection(format!(
                    "invalid range {start}-{end}"
                )));
            }
            if *start > max_ordinal {
                return Err(MaterializeError::InvalidTrackSelection(format!(
                    "range start {start} exceeds track count {max_ordinal}"
                )));
            }
            Ok((*start..=(*end).min(max_ordinal)).collect())
        }
        TrackSelection::Set(indices) => {
            validate_track_set(indices, max_ordinal)?;
            Ok(indices.iter().copied().collect())
        }
    }
}

fn validate_track_set(indices: &BTreeSet<u32>, max_ordinal: u32) -> Result<(), MaterializeError> {
    if indices.is_empty() {
        return Err(MaterializeError::InvalidTrackSelection("empty track set".to_string()));
    }
    for &idx in indices {
        if idx == 0 || idx > max_ordinal {
            return Err(MaterializeError::InvalidTrackSelection(format!(
                "track {idx} outside valid range 1-{max_ordinal}"
            )));
        }
    }
    Ok(())
}

fn base_album_metadata(
    source: &Path,
    selection: &BlurayProgramSelection,
    authored_chapter_count: u32,
) -> AlbumMetadata {
    let mut extra = BTreeMap::new();
    extra.insert(
        "bluray_playlist".to_string(),
        format!("{:05}", selection.title.playlist_number),
    );
    extra.insert(
        "bluray_title_index".to_string(),
        selection.title.key.title_index().to_string(),
    );
    extra.insert(
        "bluray_angle".to_string(),
        selection.display_angle.get().to_string(),
    );
    extra.insert(
        "bluray_max_angle".to_string(),
        selection.max_angle.to_string(),
    );
    extra.insert(
        "bluray_audio_pid".to_string(),
        format!("0x{:04x}", selection.stream.pid),
    );
    extra.insert(
        "bluray_audio_stream".to_string(),
        selection.stream.stream_index.to_string(),
    );
    extra.insert(
        "bluray_audio_coding".to_string(),
        selection.stream.coding.label().to_string(),
    );

    AlbumMetadata {
        album: source
            .file_stem()
            .and_then(|value| value.to_str())
            .map(str::to_string),
        album_artist: None,
        genre: None,
        date: None,
        total_tracks: bluray_album_total_tracks(authored_chapter_count),
        total_discs: None,
        disc_number: None,
        extra,
    }
}

fn track_metadata(
    selection: &BlurayProgramSelection,
    chapter_number: u32,
    output_track_number: u32,
) -> TrackMetadata {
    let mut extra = BTreeMap::new();
    extra.insert(
        "bluray_playlist".to_string(),
        format!("{:05}", selection.title.playlist_number),
    );
    extra.insert("bluray_chapter_number".to_string(), chapter_number.to_string());
    extra.insert(
        "bluray_audio_pid".to_string(),
        format!("0x{:04x}", selection.stream.pid),
    );
    extra.insert(
        "bluray_audio_stream".to_string(),
        selection.stream.stream_index.to_string(),
    );
    extra.insert(
        "bluray_angle".to_string(),
        selection.display_angle.get().to_string(),
    );

    TrackMetadata {
        title: Some(format!("Chapter {chapter_number}")),
        artist: None,
        album_artist: None,
        composer: None,
        performer: None,
        genre: None,
        date: None,
        track_number: Some(output_track_number),
        disc_number: None,
        isrc: None,
        publisher: None,
        copyright: None,
        comment: None,
        pre_emphasis: false,
        extra,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BluraySidecarIdentity {
    playlist_number: u32,
    audio_pid: u16,
    audio_stream_index: u8,
    angle_number: u8,
    chapter_count: u32,
    duration_fingerprint: String,
}

fn bluray_sidecar_identity(
    selection: &BlurayProgramSelection,
    chapter_count: u32,
) -> BluraySidecarIdentity {
    BluraySidecarIdentity {
        playlist_number: selection.title.playlist_number,
        audio_pid: selection.stream.pid,
        audio_stream_index: selection.stream.stream_index,
        angle_number: selection.display_angle.get(),
        chapter_count,
        duration_fingerprint: bluray_duration_fingerprint(selection),
    }
}

fn bluray_duration_fingerprint(selection: &BlurayProgramSelection) -> String {
    let mut fingerprint = format!("duration_pts_90k={};", selection.title.duration_pts_90k);
    for chapter in &selection.chapters {
        fingerprint.push_str(&format!(
            "chapter={}:start={}:end={:?}:duration={:?};",
            chapter.chapter_number,
            chapter.start_pts_90k,
            chapter.end_pts_90k,
            chapter.duration_pts_90k
        ));
    }
    fingerprint
}

/// Phase 2 placeholder. Phase 5 will load and apply Blu-ray sidecars.
/// This function intentionally returns the input metadata unchanged.
/// TODO(Phase 5): implement Blu-ray sidecar load/save/overlay keyed by
/// playlist, PID, stream index, angle, chapter count, and duration fingerprint.
fn overlay_bluray_sidecar_metadata_stub<T>(
    metadata: T,
    identity: &BluraySidecarIdentity,
) -> T {
    let _ = (
        identity.playlist_number,
        identity.audio_pid,
        identity.audio_stream_index,
        identity.angle_number,
        identity.chapter_count,
        identity.duration_fingerprint.as_str(),
    );
    #[cfg(test)]
    BLURAY_SIDECAR_OVERLAY_STUB_CALLS.with(|calls| calls.set(calls.get() + 1));
    metadata
}

#[cfg(test)]
thread_local! {
    static BLURAY_SIDECAR_OVERLAY_STUB_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_bluray_sidecar_overlay_stub_call_count() {
    BLURAY_SIDECAR_OVERLAY_STUB_CALLS.with(|calls| calls.set(0));
}

#[cfg(test)]
fn bluray_sidecar_overlay_stub_call_count() -> usize {
    BLURAY_SIDECAR_OVERLAY_STUB_CALLS.with(|calls| calls.get())
}

fn bluray_tool_versions(
    runner: &dyn ToolRunner,
    selected_coding: BluRayAudioCoding,
) -> BTreeMap<String, String> {
    let mut tool_versions = BTreeMap::new();
    tool_versions.insert("libbluray".to_string(), bluray_backend_provenance_value());
    if selected_coding != BluRayAudioCoding::Lpcm {
        if let Some(version) = runner.tool_version(ToolBinary::Ffmpeg) {
            tool_versions.insert("ffmpeg".to_string(), version);
        }
    }
    tool_versions
}

fn bluray_backend_provenance_value() -> String {
    bluray_linked_library_version_from_build_metadata()
        .map(|version| format!("linked libbluray {version}"))
        .unwrap_or_else(|| {
            "in-process libbluray backend; linked library version not exposed by build metadata"
                .to_string()
        })
}

fn bluray_linked_library_version_from_build_metadata() -> Option<&'static str> {
    option_env!("DEP_LIBBLURAY_VERSION")
        .or_else(|| option_env!("LIBBLURAY_VERSION"))
        .or_else(|| option_env!("LIBBLURAY_SYS_VERSION"))
        .filter(|version| !version.trim().is_empty())
}

fn bluray_album_total_tracks(authored_chapter_count: u32) -> u32 {
    authored_chapter_count
}

fn bluray_output_track_number(output_index_zero_based: usize) -> u32 {
    output_index_zero_based
        .saturating_add(1)
        .min(u32::MAX as usize) as u32
}

fn display_option_u32(value: Option<u32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "<auto>".to_string())
}

fn display_option_u8(value: Option<u8>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "<auto>".to_string())
}

fn display_option_pid(value: Option<u16>) -> String {
    value
        .map(|value| format!("0x{value:04x}"))
        .unwrap_or_else(|| "<auto>".to_string())
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(ext))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::super::errors::{MaterializeError, ToolRunnerError};
    use super::super::tool::{ToolBinary, ToolCommand, ToolRunner};
    use super::super::types::{
        AlbumMetadata, CueSidecarPolicy, DvdaDownmixPolicy, DvdaGroupSelection, PreparedSource,
        SacdArea, SourceAudioCoding, SourceKind, SourceOptions, TrackMetadata, TrackSelection,
        TrackSourceRef,
    };
    use super::{
        bluray_sidecar_overlay_stub_call_count, bluray_track_bit_depth,
        build_prepared_bluray_source, chapter_end_pts_90k, reset_bluray_sidecar_overlay_stub_call_count,
        select_bluray_program, selected_chapter_ordinals, selected_stream_with_materialization_facts,
        validate_chapter_pts, validate_explicit_stream_pair, BlurayPresentationRequest,
    };
    use crate::disc::bluray_backend::{
        BluRayAudioCoding, BluRayAudioStreamKind, BlurayAudioStreamInfo,
        BlurayBackend, BlurayBackendCapability, BlurayChapterInfo, BlurayDisplayAngle,
        BlurayLpcmBitDepth, BlurayLpcmNotProbedReason, BlurayPtsContinuitySegment,
        BlurayStreamDecryptor, BlurayTitleInfo, BlurayTitleKey, ProbeDepth,
    };
    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use tokio_util::sync::CancellationToken;

    struct VersionOnlyRunner(HashMap<ToolBinary, String>);

    #[async_trait::async_trait]
    impl ToolRunner for VersionOnlyRunner {
        async fn run(
            &self,
            _cmd: ToolCommand,
            _cancel: &CancellationToken,
        ) -> Result<super::super::tool::ToolOutput, ToolRunnerError> {
            panic!("VersionOnlyRunner must not execute commands")
        }

        fn tool_version(&self, binary: ToolBinary) -> Option<String> {
            self.0.get(&binary).cloned()
        }
    }

    #[derive(Debug, Clone)]
    struct FakeTitle {
        info: BlurayTitleInfo,
        max_angle: u8,
        chapters_by_angle: BTreeMap<u8, Vec<BlurayChapterInfo>>,
        streams: Vec<BlurayAudioStreamInfo>,
        probed_streams: Option<Vec<BlurayAudioStreamInfo>>,
    }

    #[derive(Debug)]
    struct FakeDisc {
        label: Option<String>,
        titles: Vec<FakeTitle>,
        probe_calls: Cell<usize>,
    }

    struct FakeBackend;

    impl BlurayBackend for FakeBackend {
        type Disc = FakeDisc;
        type TitleSource = Cursor<Vec<u8>>;

        fn open(_path: &Path) -> Result<Self::Disc, String> {
            Err("FakeBackend::open is not used by materializer unit tests".to_string())
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
            fake_title_by_playlist(disc, playlist_number).map(|title| title.info.key)
        }

        fn chapters(
            disc: &Self::Disc,
            title: BlurayTitleKey,
            display_angle: BlurayDisplayAngle,
        ) -> Result<Vec<BlurayChapterInfo>, String> {
            let title = fake_title_by_key(disc, title)?;
            Ok(title
                .chapters_by_angle
                .get(&display_angle.get())
                .cloned()
                .unwrap_or_default())
        }

        fn streams(
            disc: &Self::Disc,
            title: BlurayTitleKey,
        ) -> Result<Vec<BlurayAudioStreamInfo>, String> {
            Ok(fake_title_by_key(disc, title)?.streams.clone())
        }

        fn streams_with_probe_policy(
            disc: &Self::Disc,
            title: BlurayTitleKey,
            policy: ProbeDepth,
        ) -> Result<Vec<BlurayAudioStreamInfo>, String> {
            let title = fake_title_by_key(disc, title)?;
            if policy != ProbeDepth::None {
                disc.probe_calls.set(disc.probe_calls.get() + 1);
            }
            Ok(title
                .probed_streams
                .clone()
                .unwrap_or_else(|| title.streams.clone()))
        }

        fn max_angle(disc: &Self::Disc, title: BlurayTitleKey) -> Result<u8, String> {
            Ok(fake_title_by_key(disc, title)?.max_angle)
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
            Ok(BlurayBackendCapability::unsupported(
                "FakeBackend does not expose continuity segments",
            ))
        }
    }

    fn fake_title_by_key<'a>(
        disc: &'a FakeDisc,
        key: BlurayTitleKey,
    ) -> Result<&'a FakeTitle, String> {
        disc.titles
            .iter()
            .find(|title| title.info.key == key)
            .ok_or_else(|| format!("title key {:?} not found", key))
    }

    fn fake_title_by_playlist<'a>(
        disc: &'a FakeDisc,
        playlist_number: u32,
    ) -> Result<&'a FakeTitle, String> {
        disc.titles
            .iter()
            .find(|title| title.info.playlist_number == playlist_number)
            .ok_or_else(|| format!("playlist {playlist_number:05} not found"))
    }

    fn source_options() -> SourceOptions {
        SourceOptions {
            archive_password: None,
            sacd_area: None::<SacdArea>,
            dvda_group_selection: DvdaGroupSelection::Default,
            dvda_group: None,
            dvda_assume_decrypted: false,
            dvda_downmix_policy: DvdaDownmixPolicy::Auto,
            dvdv_vts: None,
            dvdv_title: None,
            dvdv_audio_stream: None,
            dvdv_angle: None,
            bluray_playlist: None,
            bluray_audio_pid: None,
            bluray_audio_stream: None,
            bluray_angle: None,
            cue_sidecar: CueSidecarPolicy::PreferSidecar,
            track_selection: TrackSelection::All,
        }
    }

    fn explicit_options(
        playlist_number: u32,
        audio_pid: u16,
        audio_stream_index: u8,
        display_angle: u8,
    ) -> SourceOptions {
        let mut options = source_options();
        options.bluray_playlist = Some(playlist_number);
        options.bluray_audio_pid = Some(audio_pid);
        options.bluray_audio_stream = Some(audio_stream_index);
        options.bluray_angle = Some(display_angle);
        options
    }

    fn fake_disc(titles: Vec<FakeTitle>) -> FakeDisc {
        FakeDisc {
            label: Some("Fake Blu-ray".to_string()),
            titles,
            probe_calls: Cell::new(0),
        }
    }

    fn fake_title(
        playlist_number: u32,
        title_index: u32,
        duration_pts_90k: u64,
        max_angle: u8,
        chapter_starts: &[u64],
        streams: Vec<BlurayAudioStreamInfo>,
    ) -> FakeTitle {
        let chapters = chapter_starts
            .iter()
            .enumerate()
            .map(|(index, start)| BlurayChapterInfo {
                chapter_number: u32::try_from(index).unwrap() + 1,
                start_pts_90k: *start,
                end_pts_90k: None,
                duration_pts_90k: chapter_starts
                    .get(index + 1)
                    .copied()
                    .or(Some(duration_pts_90k))
                    .and_then(|end| end.checked_sub(*start)),
                byte_offset: None,
                clip_ref: None,
            })
            .collect();
        let key = BlurayTitleKey::from_libbluray(title_index, playlist_number);
        FakeTitle {
            info: BlurayTitleInfo {
                key,
                playlist_number,
                duration_pts_90k,
                angle_count: max_angle,
                chapter_count: u32::try_from(chapter_starts.len()).unwrap(),
                clip_count: 1,
            },
            max_angle,
            chapters_by_angle: BTreeMap::from([(1, chapters.clone()), (2, chapters)]),
            streams,
            probed_streams: None,
        }
    }

    fn title(duration_pts_90k: u64) -> BlurayTitleInfo {
        BlurayTitleInfo {
            key: BlurayTitleKey::from_libbluray(7, 12),
            playlist_number: 12,
            duration_pts_90k,
            angle_count: 1,
            chapter_count: 2,
            clip_count: 1,
        }
    }

    fn chapter(chapter_number: u32, start_pts_90k: u64) -> BlurayChapterInfo {
        BlurayChapterInfo {
            chapter_number,
            start_pts_90k,
            end_pts_90k: None,
            duration_pts_90k: None,
            byte_offset: None,
            clip_ref: None,
        }
    }

    fn audio_stream(
        pid: u16,
        stream_index: u8,
        coding: BluRayAudioCoding,
        bit_depth: BlurayLpcmBitDepth,
    ) -> BlurayAudioStreamInfo {
        BlurayAudioStreamInfo {
            kind: BluRayAudioStreamKind::Primary,
            pid,
            stream_index,
            coding,
            sample_rate: Some(96_000),
            bit_depth,
            channels: Some(2),
            channel_layout: Some("stereo".to_string()),
            language: Some("eng".to_string()),
        }
    }

    fn compressed_stream(
        pid: u16,
        stream_index: u8,
        coding: BluRayAudioCoding,
        sample_rate: Option<u32>,
        channels: Option<u8>,
    ) -> BlurayAudioStreamInfo {
        BlurayAudioStreamInfo {
            kind: BluRayAudioStreamKind::Primary,
            pid,
            stream_index,
            coding,
            sample_rate,
            bit_depth: BlurayLpcmBitDepth::NotApplicable,
            channels,
            channel_layout: channels.map(|channels| match channels {
                2 => "stereo".to_string(),
                6 => "5.1".to_string(),
                8 => "7.1".to_string(),
                other => format!("{other}ch"),
            }),
            language: Some("eng".to_string()),
        }
    }

    fn unprobed_lpcm_stream(pid: u16, stream_index: u8) -> BlurayAudioStreamInfo {
        audio_stream(
            pid,
            stream_index,
            BluRayAudioCoding::Lpcm,
            BlurayLpcmBitDepth::NotProbed {
                reason: BlurayLpcmNotProbedReason::ProbePolicyNone,
            },
        )
    }

    fn probed_lpcm_stream(pid: u16, stream_index: u8, bit_depth: u32) -> BlurayAudioStreamInfo {
        audio_stream(
            pid,
            stream_index,
            BluRayAudioCoding::Lpcm,
            BlurayLpcmBitDepth::Probed {
                bit_depth,
                scanned_bytes: 188,
            },
        )
    }

    fn assert_album_metadata_eq(left: &AlbumMetadata, right: &AlbumMetadata) {
        assert_eq!(&left.album, &right.album);
        assert_eq!(&left.album_artist, &right.album_artist);
        assert_eq!(&left.genre, &right.genre);
        assert_eq!(&left.date, &right.date);
        assert_eq!(left.total_tracks, right.total_tracks);
        assert_eq!(&left.total_discs, &right.total_discs);
        assert_eq!(&left.disc_number, &right.disc_number);
        assert_eq!(&left.extra, &right.extra);
    }

    fn assert_track_metadata_eq(left: &TrackMetadata, right: &TrackMetadata) {
        assert_eq!(&left.title, &right.title);
        assert_eq!(&left.artist, &right.artist);
        assert_eq!(&left.album_artist, &right.album_artist);
        assert_eq!(&left.composer, &right.composer);
        assert_eq!(&left.performer, &right.performer);
        assert_eq!(&left.genre, &right.genre);
        assert_eq!(&left.date, &right.date);
        assert_eq!(&left.track_number, &right.track_number);
        assert_eq!(&left.disc_number, &right.disc_number);
        assert_eq!(&left.isrc, &right.isrc);
        assert_eq!(&left.publisher, &right.publisher);
        assert_eq!(&left.copyright, &right.copyright);
        assert_eq!(&left.comment, &right.comment);
        assert_eq!(left.pre_emphasis, right.pre_emphasis);
        assert_eq!(&left.extra, &right.extra);
    }

    fn assert_prepared_sources_eq_ignoring_extracted_at(
        left: &PreparedSource,
        right: &PreparedSource,
    ) {
        assert_eq!(&left.container, &right.container);
        assert_eq!(left.kind, right.kind);
        assert_album_metadata_eq(&left.album_metadata, &right.album_metadata);
        assert_eq!(left.tracks.len(), right.tracks.len());
        for (left_track, right_track) in left.tracks.iter().zip(&right.tracks) {
            assert_eq!(&left_track.id, &right_track.id);
            assert_eq!(&left_track.source_ref, &right_track.source_ref);
            assert_track_metadata_eq(&left_track.metadata, &right_track.metadata);
            assert_eq!(left_track.expected_samples, right_track.expected_samples);
            assert_eq!(left_track.sample_rate, right_track.sample_rate);
            assert_eq!(&left_track.source_audio, &right_track.source_audio);
            assert_eq!(left_track.bit_depth, right_track.bit_depth);
        }
        assert_eq!(left.provenance.source_kind, right.provenance.source_kind);
        assert_eq!(&left.provenance.source_sha256, &right.provenance.source_sha256);
        assert_eq!(&left.provenance.tool_versions, &right.provenance.tool_versions);
        // The timestamp is intentionally excluded. Repeated materialization can
        // stamp a different extraction time while all source-derived facts remain
        // stable.
    }

    fn version_runner() -> VersionOnlyRunner {
        VersionOnlyRunner(HashMap::from([(
            ToolBinary::Ffmpeg,
            "ffmpeg 7.1.3".to_string(),
        )]))
    }

    #[test]
    fn materializer_bluray_explicit_selection_creates_one_track_per_chapter() {
        let disc = fake_disc(vec![fake_title(
            12,
            0,
            270_000,
            2,
            &[0, 90_000, 180_000],
            vec![compressed_stream(0x1100, 0, BluRayAudioCoding::Dts, Some(48_000), Some(6))],
        )]);
        let options = explicit_options(12, 0x1100, 0, 2);

        let selection = select_bluray_program::<FakeBackend>(
            &disc,
            Path::new("/fixtures/Concert.iso"),
            &options,
        )
        .expect("explicit Blu-ray selection");
        let prepared = build_prepared_bluray_source(
            Path::new("/fixtures/Concert.iso"),
            Path::new("/fixtures/Concert.iso"),
            &options.track_selection,
            &selection,
            &version_runner(),
            None,
        )
        .expect("prepared Blu-ray source");

        assert_eq!(selection.title.playlist_number, 12);
        assert_eq!(selection.stream.pid, 0x1100);
        assert_eq!(selection.display_angle.get(), 2);
        assert_eq!(prepared.kind, SourceKind::BluRay);
        assert_eq!(prepared.tracks.len(), 3);
        for (index, track) in prepared.tracks.iter().enumerate() {
            let authored_chapter = u32::try_from(index).unwrap() + 1;
            assert_eq!(track.id.track_number, authored_chapter);
            assert_eq!(track.id.source_ordinal, authored_chapter);
            assert_eq!(track.metadata.track_number, Some(authored_chapter));
            assert_eq!(track.sample_rate, Some(48_000));
            assert_eq!(track.bit_depth, None);
            match &track.source_ref {
                TrackSourceRef::BluRayTrack {
                    source,
                    playlist_number,
                    title_index,
                    angle_number,
                    chapter_number,
                    chapter_start_pts_90k,
                    chapter_end_pts_90k,
                    audio_pid,
                    audio_stream_index,
                    audio_coding,
                    sample_rate,
                    bit_depth,
                    channels,
                    channel_layout,
                } => {
                    assert_eq!(source, &PathBuf::from("/fixtures/Concert.iso"));
                    assert_eq!(*playlist_number, 12);
                    assert_eq!(*title_index, 0);
                    assert_eq!(*angle_number, 2);
                    assert_eq!(*chapter_number, authored_chapter);
                    assert_eq!(*chapter_start_pts_90k, [0, 90_000, 180_000][index]);
                    assert_eq!(*audio_pid, 0x1100);
                    assert_eq!(*audio_stream_index, 0);
                    assert_eq!(*audio_coding, BluRayAudioCoding::Dts);
                    assert_eq!(*sample_rate, Some(48_000));
                    assert_eq!(*bit_depth, None);
                    assert_eq!(*channels, Some(6));
                    assert_eq!(channel_layout.as_deref(), Some("5.1"));
                    assert_eq!(
                        *chapter_end_pts_90k,
                        Some([90_000, 180_000, 270_000][index])
                    );
                }
                other => panic!("unexpected source reference: {other:?}"),
            }
        }
    }

    #[test]
    fn materializer_bluray_explicit_pid_stream_index_mismatch_fails() {
        let disc = fake_disc(vec![fake_title(
            12,
            0,
            90_000,
            1,
            &[0],
            vec![compressed_stream(0x1100, 0, BluRayAudioCoding::Ac3, Some(48_000), Some(2))],
        )]);
        let options = explicit_options(12, 0x1101, 0, 1);

        let err = select_bluray_program::<FakeBackend>(&disc, Path::new("/disc.iso"), &options)
            .unwrap_err();

        assert!(matches!(err, MaterializeError::InvalidTrackSelection(_)));
        let msg = err.to_string();
        assert!(msg.contains("audio stream 1"));
        assert!(msg.contains("PID 0x1100"));
        assert!(msg.contains("requested PID 0x1101"));
    }

    #[test]
    fn materializer_bluray_missing_playlist_fails_without_default_fallback() {
        let disc = fake_disc(vec![fake_title(
            12,
            0,
            90_000,
            1,
            &[0],
            vec![compressed_stream(0x1100, 0, BluRayAudioCoding::Ac3, Some(48_000), Some(2))],
        )]);
        let options = explicit_options(99_999, 0x1100, 0, 1);

        let err = select_bluray_program::<FakeBackend>(&disc, Path::new("/disc.iso"), &options)
            .unwrap_err();

        assert!(matches!(err, MaterializeError::InvalidTrackSelection(_)));
        assert!(err.to_string().contains("playlist 99999 not found"));
    }

    #[test]
    fn materializer_bluray_missing_pid_reports_decimal_and_hex() {
        let disc = fake_disc(vec![fake_title(
            12,
            0,
            90_000,
            1,
            &[0],
            vec![compressed_stream(0x1100, 0, BluRayAudioCoding::Ac3, Some(48_000), Some(2))],
        )]);
        let mut options = explicit_options(12, 0x1200, 0, 1);
        options.bluray_audio_stream = None;

        let err = select_bluray_program::<FakeBackend>(&disc, Path::new("/disc.iso"), &options)
            .unwrap_err();

        assert!(matches!(err, MaterializeError::InvalidTrackSelection(_)));
        let msg = err.to_string();
        assert!(msg.contains("requested PID 4608"));
        assert!(msg.contains("0x1200"));
    }

    #[test]
    fn materializer_bluray_out_of_range_angle_reports_one_based_range() {
        let disc = fake_disc(vec![fake_title(
            12,
            0,
            90_000,
            2,
            &[0],
            vec![compressed_stream(0x1100, 0, BluRayAudioCoding::Ac3, Some(48_000), Some(2))],
        )]);

        let zero = select_bluray_program::<FakeBackend>(
            &disc,
            Path::new("/disc.iso"),
            &explicit_options(12, 0x1100, 0, 0),
        )
        .unwrap_err();
        assert!(zero.to_string().contains("one-based"));

        let too_high = select_bluray_program::<FakeBackend>(
            &disc,
            Path::new("/disc.iso"),
            &explicit_options(12, 0x1100, 0, 3),
        )
        .unwrap_err();
        let msg = too_high.to_string();
        assert!(msg.contains("supported angle range is 1..=2"));
        assert!(msg.contains("cannot select angle 3"));
    }

    #[test]
    fn materializer_bluray_selected_title_with_no_chapters_fails() {
        let disc = fake_disc(vec![fake_title(
            12,
            0,
            90_000,
            1,
            &[],
            vec![compressed_stream(0x1100, 0, BluRayAudioCoding::Ac3, Some(48_000), Some(2))],
        )]);
        let options = explicit_options(12, 0x1100, 0, 1);

        let err = select_bluray_program::<FakeBackend>(&disc, Path::new("/disc.iso"), &options)
            .unwrap_err();

        assert!(matches!(err, MaterializeError::InvalidTrackSelection(_)));
        assert!(err.to_string().contains("contains no chapters"));
    }

    #[test]
    fn materializer_bluray_lpcm_probe_success_populates_bit_depth() {
        let mut title = fake_title(
            12,
            0,
            270_000,
            1,
            &[0, 90_000, 180_000],
            vec![unprobed_lpcm_stream(0x1100, 0)],
        );
        title.probed_streams = Some(vec![probed_lpcm_stream(0x1100, 0, 24)]);
        let disc = fake_disc(vec![title]);
        let options = explicit_options(12, 0x1100, 0, 1);
        let mut selection = select_bluray_program::<FakeBackend>(
            &disc,
            Path::new("/fixtures/Album.iso"),
            &options,
        )
        .expect("select LPCM stream");

        selection.stream = selected_stream_with_materialization_facts::<FakeBackend>(
            &disc,
            &selection,
        )
        .expect("bounded LPCM probe");
        let prepared = build_prepared_bluray_source(
            Path::new("/fixtures/Album.iso"),
            Path::new("/fixtures/Album.iso"),
            &options.track_selection,
            &selection,
            &version_runner(),
            None,
        )
        .expect("materialize probed LPCM stream");

        assert_eq!(disc.probe_calls.get(), 1);
        assert_eq!(prepared.tracks.len(), 3);
        for track in &prepared.tracks {
            assert_eq!(track.bit_depth, Some(24));
            assert_eq!(track.sample_rate, Some(96_000));
            assert_eq!(track.source_audio.coding, Some(SourceAudioCoding::Pcm));
            assert_eq!(track.source_audio.primary_sample_rate, Some(96_000));
            assert_eq!(track.source_audio.bit_depth, Some(24));
            match &track.source_ref {
                TrackSourceRef::BluRayTrack {
                    bit_depth,
                    sample_rate,
                    channels,
                    audio_coding,
                    ..
                } => {
                    assert_eq!(*audio_coding, BluRayAudioCoding::Lpcm);
                    assert_eq!(*bit_depth, Some(24));
                    assert_eq!(*sample_rate, Some(96_000));
                    assert_eq!(*channels, Some(2));
                }
                other => panic!("unexpected source reference: {other:?}"),
            }
        }
    }

    #[test]
    fn materializer_bluray_lpcm_probe_failure_blocks_track_creation() {
        let mut title = fake_title(
            12,
            0,
            270_000,
            1,
            &[0, 90_000, 180_000],
            vec![unprobed_lpcm_stream(0x1100, 0)],
        );
        title.probed_streams = Some(vec![unprobed_lpcm_stream(0x1100, 0)]);
        let disc = fake_disc(vec![title]);
        let options = explicit_options(12, 0x1100, 0, 1);
        let selection = select_bluray_program::<FakeBackend>(
            &disc,
            Path::new("/fixtures/Album.iso"),
            &options,
        )
        .expect("select LPCM stream");

        let probe_err = selected_stream_with_materialization_facts::<FakeBackend>(&disc, &selection)
            .unwrap_err();
        assert!(matches!(probe_err, MaterializeError::Parse(_)));
        assert!(probe_err.to_string().contains("bit-depth probe"));
        assert_eq!(disc.probe_calls.get(), 1);

        let materialize_err = build_prepared_bluray_source(
            Path::new("/fixtures/Album.iso"),
            Path::new("/fixtures/Album.iso"),
            &options.track_selection,
            &selection,
            &version_runner(),
            None,
        )
        .unwrap_err();
        assert!(matches!(materialize_err, MaterializeError::Parse(_)));
        assert!(materialize_err.to_string().contains("cannot be materialized"));
    }

    #[test]
    fn materializer_bluray_compressed_codec_materializes_without_bit_depth() {
        let disc = fake_disc(vec![fake_title(
            12,
            0,
            180_000,
            1,
            &[0, 90_000],
            vec![compressed_stream(
                0x1102,
                1,
                BluRayAudioCoding::DtsHdMaster,
                Some(96_000),
                Some(6),
            )],
        )]);
        let options = explicit_options(12, 0x1102, 1, 1);
        let selection = select_bluray_program::<FakeBackend>(
            &disc,
            Path::new("/fixtures/Album.iso"),
            &options,
        )
        .expect("select DTS-HD MA stream");
        let prepared = build_prepared_bluray_source(
            Path::new("/fixtures/Album.iso"),
            Path::new("/fixtures/Album.iso"),
            &options.track_selection,
            &selection,
            &version_runner(),
            None,
        )
        .expect("materialize compressed Blu-ray stream");

        assert_eq!(prepared.tracks.len(), 2);
        assert_eq!(prepared.provenance.source_kind, SourceKind::BluRay);
        assert!(prepared.provenance.tool_versions.contains_key("libbluray"));
        assert_eq!(
            prepared.provenance.tool_versions.get("ffmpeg").map(String::as_str),
            Some("ffmpeg 7.1.3")
        );
        for track in &prepared.tracks {
            assert_eq!(track.bit_depth, None);
            assert_eq!(track.sample_rate, Some(96_000));
            assert_eq!(track.source_audio.coding, Some(SourceAudioCoding::Unknown));
            match &track.source_ref {
                TrackSourceRef::BluRayTrack {
                    audio_coding,
                    bit_depth,
                    sample_rate,
                    channels,
                    audio_pid,
                    audio_stream_index,
                    ..
                } => {
                    assert_eq!(*audio_coding, BluRayAudioCoding::DtsHdMaster);
                    assert_eq!(*bit_depth, None);
                    assert_eq!(*sample_rate, Some(96_000));
                    assert_eq!(*channels, Some(6));
                    assert_eq!(*audio_pid, 0x1102);
                    assert_eq!(*audio_stream_index, 1);
                }
                other => panic!("unexpected source reference: {other:?}"),
            }
        }
    }

    #[test]
    fn materializer_bluray_default_scoring_matches_browser_scoring() {
        let disc = fake_disc(vec![
            fake_title(
                10,
                0,
                90_000,
                1,
                &[0],
                vec![compressed_stream(0x1100, 0, BluRayAudioCoding::Ac3, Some(48_000), Some(2))],
            ),
            fake_title(
                12,
                1,
                270_000,
                1,
                &[0, 90_000, 180_000],
                vec![compressed_stream(
                    0x1101,
                    1,
                    BluRayAudioCoding::DtsHdMaster,
                    Some(96_000),
                    Some(6),
                )],
            ),
            fake_title(
                14,
                2,
                270_000,
                1,
                &[0, 90_000, 180_000],
                vec![compressed_stream(
                    0x1102,
                    0,
                    BluRayAudioCoding::TrueHd,
                    Some(48_000),
                    Some(2),
                )],
            ),
        ]);
        let contents = crate::disc::bluray_mapper::map_bluray_disc::<FakeBackend>(
            &disc,
            Path::new("/fixtures/Scored.iso"),
        )
        .expect("map fake Blu-ray disc");
        let browser_best = crate::disc::bluray_mapper::best_bluray_presentation_index(&contents)
            .and_then(|index| contents.presentations[index].id.blu_ray_parts())
            .expect("browser default presentation");

        let selection = select_bluray_program::<FakeBackend>(
            &disc,
            Path::new("/fixtures/Scored.iso"),
            &source_options(),
        )
        .expect("materializer default selection");

        assert_eq!(
            browser_best,
            (
                selection.title.playlist_number,
                selection.stream.pid,
                selection.stream.stream_index,
                selection.display_angle.get(),
            )
        );
    }

    #[test]
    fn materializer_bluray_provenance_uses_build_metadata_path_without_wrapper_accessor() {
        let lpcm_versions = bluray_tool_versions(&version_runner(), BluRayAudioCoding::Lpcm);
        let libbluray = lpcm_versions
            .get("libbluray")
            .expect("libbluray provenance entry");
        assert!(
            libbluray.starts_with("linked libbluray ")
                || libbluray == "in-process libbluray backend; linked library version not exposed by build metadata"
        );
        assert!(!lpcm_versions.contains_key("ffmpeg"));

        let compressed_versions = bluray_tool_versions(&version_runner(), BluRayAudioCoding::Ac3);
        assert_eq!(compressed_versions.get("libbluray"), Some(libbluray));
        assert_eq!(
            compressed_versions.get("ffmpeg").map(String::as_str),
            Some("ffmpeg 7.1.3")
        );
    }

    #[test]
    fn materializer_bluray_prepared_source_metadata_defaults_to_source_stem() {
        let disc = fake_disc(vec![fake_title(
            12,
            0,
            270_000,
            1,
            &[0, 90_000, 180_000],
            vec![compressed_stream(0x1100, 0, BluRayAudioCoding::Ac3, Some(48_000), Some(2))],
        )]);
        let options = explicit_options(12, 0x1100, 0, 1);
        let selection = select_bluray_program::<FakeBackend>(
            &disc,
            Path::new("/music/Fake Album.iso"),
            &options,
        )
        .expect("select Blu-ray presentation");

        let prepared = build_prepared_bluray_source(
            Path::new("/music/Fake Album.iso"),
            Path::new("/music/Fake Album.iso"),
            &options.track_selection,
            &selection,
            &version_runner(),
            None,
        )
        .expect("prepared source");

        assert_eq!(prepared.album_metadata.album.as_deref(), Some("Fake Album"));
        assert_eq!(prepared.album_metadata.total_tracks, 3);
        assert_eq!(prepared.kind, SourceKind::BluRay);
        assert_eq!(prepared.provenance.source_kind, SourceKind::BluRay);
        assert!(prepared.provenance.tool_versions.contains_key("libbluray"));
    }

    #[test]
    fn materializer_bluray_album_total_tracks_uses_authored_chapter_count_for_subset() {
        let disc = fake_disc(vec![fake_title(
            12,
            0,
            360_000,
            1,
            &[0, 90_000, 180_000, 270_000],
            vec![compressed_stream(0x1100, 0, BluRayAudioCoding::Ac3, Some(48_000), Some(2))],
        )]);
        let mut options = explicit_options(12, 0x1100, 0, 1);
        options.track_selection = TrackSelection::Range { start: 2, end: 3 };
        let selection = select_bluray_program::<FakeBackend>(
            &disc,
            Path::new("/music/Subset Album.iso"),
            &options,
        )
        .expect("select Blu-ray presentation");

        let prepared = build_prepared_bluray_source(
            Path::new("/music/Subset Album.iso"),
            Path::new("/music/Subset Album.iso"),
            &options.track_selection,
            &selection,
            &version_runner(),
            None,
        )
        .expect("prepared source from chapter subset");

        assert_eq!(prepared.tracks.len(), 2);
        assert_eq!(prepared.album_metadata.total_tracks, 4);
        assert_eq!(prepared.tracks[0].id.source_ordinal, 2);
        assert_eq!(prepared.tracks[0].id.track_number, 1);
        assert_eq!(prepared.tracks[1].id.source_ordinal, 3);
        assert_eq!(prepared.tracks[1].id.track_number, 2);
    }


    #[test]
    fn materializer_bluray_repeated_materialization_is_idempotent_after_timestamp_normalization() {
        let disc = fake_disc(vec![fake_title(
            12,
            0,
            270_000,
            1,
            &[0, 90_000, 180_000],
            vec![compressed_stream(
                0x1100,
                0,
                BluRayAudioCoding::TrueHd,
                Some(96_000),
                Some(2),
            )],
        )]);
        let options = explicit_options(12, 0x1100, 0, 1);

        let first_selection = select_bluray_program::<FakeBackend>(
            &disc,
            Path::new("/music/Repeatable.iso"),
            &options,
        )
        .expect("first Blu-ray selection");
        let first = build_prepared_bluray_source(
            Path::new("/music/Repeatable.iso"),
            Path::new("/music/Repeatable.iso"),
            &options.track_selection,
            &first_selection,
            &version_runner(),
            None,
        )
        .expect("first prepared source");

        let second_selection = select_bluray_program::<FakeBackend>(
            &disc,
            Path::new("/music/Repeatable.iso"),
            &options,
        )
        .expect("second Blu-ray selection");
        let second = build_prepared_bluray_source(
            Path::new("/music/Repeatable.iso"),
            Path::new("/music/Repeatable.iso"),
            &options.track_selection,
            &second_selection,
            &version_runner(),
            None,
        )
        .expect("second prepared source");

        assert_prepared_sources_eq_ignoring_extracted_at(&first, &second);
    }

    #[test]
    fn materializer_bluray_calls_phase2_sidecar_overlay_stub() {
        reset_bluray_sidecar_overlay_stub_call_count();
        let disc = fake_disc(vec![fake_title(
            12,
            0,
            180_000,
            1,
            &[0, 90_000],
            vec![compressed_stream(0x1100, 0, BluRayAudioCoding::Ac3, Some(48_000), Some(2))],
        )]);
        let options = explicit_options(12, 0x1100, 0, 1);
        let selection = select_bluray_program::<FakeBackend>(
            &disc,
            Path::new("/music/Sidecar Stub.iso"),
            &options,
        )
        .expect("select Blu-ray presentation");

        let prepared = build_prepared_bluray_source(
            Path::new("/music/Sidecar Stub.iso"),
            Path::new("/music/Sidecar Stub.iso"),
            &options.track_selection,
            &selection,
            &version_runner(),
            None,
        )
        .expect("prepared source");

        assert_eq!(
            bluray_sidecar_overlay_stub_call_count(),
            prepared.tracks.len() + 1
        );
        assert_eq!(prepared.album_metadata.album.as_deref(), Some("Sidecar Stub"));
        assert_eq!(prepared.album_metadata.total_tracks, 2);
        assert_eq!(prepared.tracks[0].metadata.title.as_deref(), Some("Chapter 1"));
        assert_eq!(prepared.tracks[1].metadata.title.as_deref(), Some("Chapter 2"));
    }

    #[test]
    fn explicit_pid_stream_conflict_fails_before_materialization() {
        let streams = vec![audio_stream(
            0x1100,
            0,
            BluRayAudioCoding::Ac3,
            BlurayLpcmBitDepth::NotApplicable,
        )];
        let request = BlurayPresentationRequest {
            playlist_number: Some(12),
            audio_pid: Some(0x1101),
            audio_stream_index: Some(0),
            display_angle: Some(1),
        };

        let err = validate_explicit_stream_pair(12, &streams, request).unwrap_err();

        assert!(matches!(err, MaterializeError::InvalidTrackSelection(_)));
        assert!(err.to_string().contains("not requested PID"));
    }

    #[test]
    fn explicit_pid_stream_pair_accepts_matching_stream() {
        let streams = vec![audio_stream(
            0x1100,
            0,
            BluRayAudioCoding::Ac3,
            BlurayLpcmBitDepth::NotApplicable,
        )];
        let request = BlurayPresentationRequest {
            playlist_number: Some(12),
            audio_pid: Some(0x1100),
            audio_stream_index: Some(0),
            display_angle: Some(1),
        };

        validate_explicit_stream_pair(12, &streams, request).unwrap();
    }

    #[test]
    fn lpcm_bit_depth_only_materializes_after_probe() {
        let unprobed = unprobed_lpcm_stream(0x1100, 0);
        let probed = probed_lpcm_stream(0x1100, 0, 24);
        let compressed = audio_stream(
            0x1101,
            1,
            BluRayAudioCoding::Ac3,
            BlurayLpcmBitDepth::NotApplicable,
        );

        assert_eq!(bluray_track_bit_depth(&unprobed), None);
        assert_eq!(bluray_track_bit_depth(&probed), Some(24));
        assert_eq!(bluray_track_bit_depth(&compressed), None);
    }

    #[test]
    fn chapter_end_uses_next_start_then_title_duration() {
        let title = title(270_000);
        let chapters = vec![chapter(1, 0), chapter(2, 90_000)];

        assert_eq!(
            chapter_end_pts_90k(&title, &chapters, 0).unwrap(),
            Some(90_000)
        );
        assert_eq!(
            chapter_end_pts_90k(&title, &chapters, 1).unwrap(),
            Some(270_000)
        );
    }

    #[test]
    fn chapter_pts_validation_rejects_reversed_order() {
        let err = validate_chapter_pts(&title(270_000), &[chapter(1, 90_000), chapter(2, 0)])
            .unwrap_err();
        assert!(matches!(err, MaterializeError::Parse(_)));
    }

    #[test]
    fn selected_chapter_ordinals_supports_sparse_sets() {
        let set = BTreeSet::from([1, 3]);
        assert_eq!(
            selected_chapter_ordinals(4, &TrackSelection::Set(set)).unwrap(),
            BTreeSet::from([1, 3])
        );
    }
}
