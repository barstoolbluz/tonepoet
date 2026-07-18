//! DVD-Video source materializer.
//!
//! This stage parses the DVD-Video IFO model into per-chapter `DvdVideoTrack`
//! references. It does not decode audio; realization later reads the selected
//! chapter sectors through the DVD-Video VOB file inventory and routes LPCM to
//! the in-process DVD-Video demuxer while compressed codecs go to ffmpeg.
//!
//! Cell ranges are emitted in authored PGC playback order. Multi-angle titles
//! are filtered to one explicit/default angle path. NAV/ILVU-interleaved angle
//! blocks are rejected fail-safe until the realizer grows ILVU-aware VOBU
//! navigation; extracting their raw sector span would mix other angles.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use dvdvideo::disc::{DvdFile, DvdFileKind};
use dvdvideo::ifo::{CellBlockMode, CellBlockType, CellPlaybackInfo, FrameRate, PgcTime};
use dvdvideo::{AudioCodingMode, AudioStreamAttr, DvdChapter, DvdDisc, Pgc, VtsIfo};
use tokio_util::sync::CancellationToken;

use super::errors::{MaterializeError, SourceDetectError};
use super::reporter::PipelineReporter;
use super::stages::Materializer;
use super::tool::{ToolBinary, ToolRunner};
use super::types::*;

pub struct DvdVideoMaterializer;

const DVD_SECTOR_BYTES: u64 = 2048;
const DVD_SECTOR_BYTES_USIZE: usize = DVD_SECTOR_BYTES as usize;
const DVDV_CSS_PREFLIGHT_MAX_SECTORS_PER_CHAPTER: u32 = 512;

#[async_trait]
impl Materializer for DvdVideoMaterializer {
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
        if crate::disc::dvda_utils::is_dvda_source(&req.container) {
            return Err(MaterializeError::Parse(format!(
                "{} is a hybrid/DVD-Audio source; DVD-Audio materialization must handle it",
                req.container.display()
            )));
        }

        let disc = crate::disc::dvdv_utils::open_dvdv_source(&req.container)
            .map_err(MaterializeError::Parse)?;
        let vts_ifos = crate::disc::dvdv_utils::parse_vts_ifos_for_source(&req.container, &disc)
            .map_err(MaterializeError::Parse)?
            .into_iter()
            .map(|(_, vts)| vts)
            .collect::<Vec<_>>();
        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }

        let selection = select_dvdv_program(
            &vts_ifos,
            req.source.dvdv_vts,
            req.source.dvdv_title,
            req.source.dvdv_audio_stream,
            req.source.dvdv_angle,
        )?;
        let vts = selection.vts;
        let title = selection.title;
        let stream = selection.stream;
        let angle_number = selection.angle_number;
        let vob_files = title_vob_inventory(&disc, &req.container, vts.vts_number)?;
        let audio_coding = dvdv_coding(stream).ok_or_else(|| {
            MaterializeError::InvalidTrackSelection(format!(
                "DVD-Video VTS {} stream {} uses unsupported audio coding {:?}",
                vts.vts_number, stream.stream_index, stream.coding_mode
            ))
        })?;
        let selected_chapters = selected_chapter_ordinals(
            title.chapter_count as u32,
            &req.source.track_selection,
        )?;
        let presentation_identity = dvdv_presentation_identity(vts, title, stream, angle_number);
        let supported_presentation_count = dvdv_supported_audio_presentation_count(&vts_ifos);
        let dvdv_sidecar = match load_dvdv_metadata_sidecars(&req.container)? {
            Some(sidecars) => {
                let mut matching = sidecars.into_iter().filter(|sidecar| {
                    dvdv_sidecar_matches_selection(
                        sidecar,
                        &presentation_identity,
                        title.angle_count,
                        title.chapters.len(),
                        selected_chapters.len(),
                        supported_presentation_count,
                    )
                });
                let first = matching.next();
                if matching.next().is_some() {
                    log::warn!(
                        "ignoring DVD-Video metadata sidecar for {} because multiple presentations match {:?}",
                        req.container.display(),
                        presentation_identity
                    );
                    None
                } else if first.is_none() {
                    log::warn!(
                        "ignoring DVD-Video metadata sidecar for {} because no presentation matches {:?}",
                        req.container.display(),
                        presentation_identity
                    );
                    None
                } else {
                    first
                }
            }
            None => None,
        };

        let mut tracks = Vec::with_capacity(selected_chapters.len());
        for chapter in title
            .chapters
            .iter()
            .filter(|chapter| selected_chapters.contains(&u32::from(chapter.number)))
        {
            if cancel.is_cancelled() {
                return Err(MaterializeError::Cancelled);
            }
            let cell_sectors = chapter_cell_sectors(vts, title, chapter, angle_number)?;
            if cell_sectors.is_empty() {
                return Err(MaterializeError::Parse(format!(
                    "DVD-Video VTS {} title {} chapter {} angle {} resolves to no cells",
                    vts.vts_number, title.number, chapter.number, angle_number
                )));
            }
            validate_cell_sector_coverage(
                vts.vts_number,
                title.number,
                chapter.number,
                &cell_sectors,
                &vob_files,
            )?;
            preflight_no_css_scrambling_flags(
                &req.container,
                vts.vts_number,
                title.number,
                chapter.number,
                &vob_files,
                &cell_sectors,
                cancel,
            )?;
            let source_ordinal = u32::from(chapter.number);
            let output_track_number = dvdv_output_track_number(tracks.len());
            tracks.push(PreparedTrack {
                id: TrackId {
                    source_ordinal,
                    disc_number: None,
                    track_number: output_track_number,
                },
                source_ref: TrackSourceRef::DvdVideoTrack {
                    source: req.container.clone(),
                    vts_number: vts.vts_number,
                    title_number: title.number,
                    angle_number,
                    chapter_number: chapter.number,
                    audio_stream_index: stream.stream_index,
                    audio_coding,
                    cell_sectors,
                    vob_files: vob_files.clone(),
                    sample_rate: stream.sample_frequency,
                    bit_depth: stream.bit_depth,
                    channels: Some(stream.channels),
                },
                metadata: track_metadata(
                    title.number,
                    chapter.number,
                    output_track_number,
                    dvdv_sidecar.as_ref(),
                ),
                expected_samples: None,
                sample_rate: stream.sample_frequency,
                source_audio: SourceAudioDescriptor::from_scalar(
                    stream.sample_frequency,
                    stream.bit_depth,
                    source_audio_coding(audio_coding),
                ),
                bit_depth: stream.bit_depth,
                warnings: Vec::new(),
            });
        }

        if tracks.is_empty() {
            return Err(MaterializeError::InvalidTrackSelection(format!(
                "DVD-Video VTS {} title {} selection {:?} did not match any chapter",
                vts.vts_number, title.number, req.source.track_selection
            )));
        }

        let mut extra = BTreeMap::new();
        extra.insert("dvdv_vts".to_string(), vts.vts_number.to_string());
        extra.insert("dvdv_title".to_string(), title.number.to_string());
        extra.insert("dvdv_angle".to_string(), angle_number.to_string());
        extra.insert("dvdv_audio_stream".to_string(), stream.stream_index.to_string());
        extra.insert("dvdv_audio_coding".to_string(), audio_coding.label().to_string());
        extra.insert(
            "dvdv_default_selection".to_string(),
            "scored-main-program-track-count-duration-then-stream".to_string(),
        );

        let tool_versions = dvdv_tool_versions(runner);
        let album_metadata = overlay_dvdv_album_metadata(
            AlbumMetadata {
                album: req
                    .container
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .map(str::to_string),
                album_artist: None,
                genre: None,
                date: None,
                total_tracks: dvdv_album_total_tracks(tracks.len()),
                total_discs: None,
                disc_number: None,
                extra,
            },
            dvdv_sidecar.as_ref(),
        );

        Ok(PreparedSource {
            container: req.container.clone(),
            kind: SourceKind::DvdVideo,
            tracks,
            album_metadata,
            provenance: ExtractionProvenance {
                source_kind: SourceKind::DvdVideo,
                source_sha256: None,
                tool_versions,
                extracted_at: chrono::Utc::now(),
            },
        })
    }
}


fn dvdv_tool_versions(runner: &dyn ToolRunner) -> BTreeMap<String, String> {
    let mut tool_versions = BTreeMap::new();
    tool_versions.insert("dvdvideo".to_string(), "in-process".to_string());
    if let Some(version) = runner.tool_version(ToolBinary::Ffmpeg) {
        tool_versions.insert("ffmpeg".to_string(), version);
    }
    tool_versions
}

/// Route DVD-Video sources after DVD-Audio has had first refusal.
pub(crate) fn is_dvdv_candidate(req: &PipelineRequest) -> Result<bool, SourceDetectError> {
    if crate::disc::dvda_utils::is_dvda_source(&req.container) {
        return Ok(false);
    }
    if req.container.is_file() {
        return Ok(crate::disc::dvdv_utils::is_dvdv_iso(&req.container)
            || (req.source.explicit_dvdv_requested() && has_extension(&req.container, "iso")));
    }
    if req.container.is_dir() {
        return Ok(crate::disc::dvdv_utils::is_dvdv_directory(&req.container)
            || req.source.explicit_dvdv_requested());
    }
    Ok(false)
}

#[derive(Debug, Clone, Copy)]
struct DvdvProgramSelection<'a> {
    vts: &'a VtsIfo,
    title: &'a dvdvideo::DvdTitle,
    stream: &'a AudioStreamAttr,
    /// 1-based DVD angle number selected for duration scoring and extraction.
    angle_number: u8,
}

fn select_dvdv_program<'a>(
    vts_ifos: &'a [VtsIfo],
    requested_vts: Option<u8>,
    requested_title: Option<u8>,
    requested_stream: Option<u8>,
    requested_angle: Option<u8>,
) -> Result<DvdvProgramSelection<'a>, MaterializeError> {
    let mut best: Option<(DvdvDefaultProgramScore, DvdvProgramSelection<'a>)> = None;

    for vts in vts_ifos {
        if requested_vts.is_some_and(|wanted| wanted != vts.vts_number) {
            continue;
        }
        for title in &vts.titles {
            if requested_title.is_some_and(|wanted| wanted != title.number) {
                continue;
            }
            if title.chapters.is_empty() {
                continue;
            }
            for stream in &vts.audio_streams {
                if requested_stream.is_some_and(|wanted| wanted != stream.stream_index) {
                    continue;
                }
                if dvdv_coding(stream).is_none() {
                    continue;
                }

                let Ok(angle_number) = select_angle(title, requested_angle) else {
                    continue;
                };
                let selection = DvdvProgramSelection {
                    vts,
                    title,
                    stream,
                    angle_number,
                };
                let score = dvdv_default_program_score(selection);
                let should_replace = match best.as_ref() {
                    Some((best_score, _)) => score > *best_score,
                    None => true,
                };
                if should_replace {
                    best = Some((score, selection));
                }
            }
        }
    }

    best.map(|(_, selection)| selection).ok_or_else(|| {
        MaterializeError::InvalidTrackSelection(format!(
            "DVD-Video source contains no supported audio program matching VTS {}, title {}, stream {}, angle {}",
            requested_vts
                .map(|value| value.to_string())
                .unwrap_or_else(|| "<auto>".to_string()),
            requested_title
                .map(|value| value.to_string())
                .unwrap_or_else(|| "<auto>".to_string()),
            requested_stream
                .map(|value| value.to_string())
                .unwrap_or_else(|| "<auto>".to_string()),
            requested_angle
                .map(|value| value.to_string())
                .unwrap_or_else(|| "1".to_string())
        ))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct DvdvDefaultProgramScore {
    /// Prefer programs whose selected-angle playback path has computable
    /// chapter durations; unsafe ILVU/interleaved paths should not outrank
    /// extractable programs merely because they have more authored chapters.
    playback_path_has_duration: bool,
    /// Main-program signal: concert titles usually have chaptered song tracks.
    track_count: usize,
    /// Main-program signal: prefer the long feature over menus, intro clips, and bonus shorts.
    duration_frames: u64,
    /// First audio preference once the likely main program is identified.
    stereo: bool,
    /// Prefer lossless audio over compressed audio when program and channel count tie.
    lossless: bool,
    /// Codec/sample detail tie-breakers; these must not outrank main-program selection.
    coding_rank: u8,
    sample_rate: u32,
    bit_depth: u32,
    /// Inverted so `max` deterministically prefers lower authored numbers at equal score.
    reverse_identity: ReverseDvdvIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ReverseDvdvIdentity {
    vts_number: u8,
    title_number: u8,
    audio_stream_index: u8,
}

fn dvdv_default_program_score(selection: DvdvProgramSelection<'_>) -> DvdvDefaultProgramScore {
    let coding = dvdv_coding(selection.stream);
    let duration_secs = title_duration_secs(selection.vts, selection.title, selection.angle_number);
    DvdvDefaultProgramScore {
        playback_path_has_duration: duration_secs.is_some(),
        track_count: selection.title.chapters.len(),
        duration_frames: seconds_to_cd_frames(duration_secs.unwrap_or(0.0)),
        stereo: selection.stream.channels == 2,
        lossless: coding.is_some_and(|coding| coding.is_lossless()),
        coding_rank: match coding {
            Some(DvdVideoAudioCoding::Lpcm) => 4,
            Some(DvdVideoAudioCoding::Dts) => 3,
            Some(DvdVideoAudioCoding::Ac3) => 2,
            Some(DvdVideoAudioCoding::Mpeg) => 1,
            None => 0,
        },
        sample_rate: selection.stream.sample_frequency.unwrap_or(0),
        bit_depth: selection.stream.bit_depth.unwrap_or(0),
        reverse_identity: ReverseDvdvIdentity {
            vts_number: u8::MAX.saturating_sub(selection.vts.vts_number),
            title_number: u8::MAX.saturating_sub(selection.title.number),
            audio_stream_index: u8::MAX.saturating_sub(selection.stream.stream_index),
        },
    }
}

fn title_duration_secs(
    vts: &VtsIfo,
    title: &dvdvideo::DvdTitle,
    angle_number: u8,
) -> Option<f64> {
    let mut total = 0.0;
    for chapter in &title.chapters {
        total += chapter_duration_secs(vts, title, chapter, angle_number)?;
    }
    Some(total)
}

fn chapter_duration_secs(
    vts: &VtsIfo,
    title: &dvdvideo::DvdTitle,
    chapter: &DvdChapter,
    angle_number: u8,
) -> Option<f64> {
    let pgc = vts.pgcs.get(usize::from(chapter.pgcn.checked_sub(1)?))?;
    if chapter.start_cell == 0 || chapter.end_cell < chapter.start_cell {
        return None;
    }

    if !pgc.cells.is_empty() {
        return chapter_duration_from_selected_angle_cells(vts, title, chapter, pgc, angle_number);
    }

    if chapter.start_cell == 1 && chapter.end_cell == pgc.number_of_cells {
        Some(pgc_time_secs(pgc.playback_time))
    } else {
        None
    }
}

fn chapter_duration_from_selected_angle_cells(
    vts: &VtsIfo,
    title: &dvdvideo::DvdTitle,
    chapter: &DvdChapter,
    pgc: &Pgc,
    angle_number: u8,
) -> Option<f64> {
    let mut total = 0.0;
    let mut cell_nr = chapter.start_cell;
    while cell_nr <= chapter.end_cell {
        let cell = pgc_cell(vts, chapter, pgc, cell_nr).ok()?;
        match classify_cell_for_extraction(cell) {
            CellExtractionKind::Plain => {
                total += pgc_time_secs(cell.playback_time);
                if cell_nr == chapter.end_cell {
                    break;
                }
                cell_nr += 1;
            }
            CellExtractionKind::AngleBlockStart => {
                let block = collect_angle_block(vts, title, chapter, pgc, cell_nr).ok()?;
                let selected_offset = usize::from(angle_number.saturating_sub(1));
                let selected_cell_nr = *block.cell_numbers.get(selected_offset)?;
                let selected_cell = pgc_cell(vts, chapter, pgc, selected_cell_nr).ok()?;
                total += pgc_time_secs(selected_cell.playback_time);
                if block.end_cell >= chapter.end_cell {
                    break;
                }
                cell_nr = block.end_cell + 1;
            }
            CellExtractionKind::UnsafeInterleaved
            | CellExtractionKind::UnexpectedBlockMember
            | CellExtractionKind::ReservedBlockType => return None,
        }
    }
    Some(total)
}

fn select_angle(
    title: &dvdvideo::DvdTitle,
    requested_angle: Option<u8>,
) -> Result<u8, MaterializeError> {
    let angle_number = requested_angle.unwrap_or(1);
    if angle_number == 0 {
        return Err(MaterializeError::InvalidTrackSelection(
            "DVD-Video angles are 1-based; angle 0 is invalid".to_string(),
        ));
    }
    let max_angle = title.angle_count.max(1);
    if angle_number > max_angle {
        return Err(MaterializeError::InvalidTrackSelection(format!(
            "DVD-Video title {} has {} angle(s), cannot select angle {}",
            title.number, max_angle, angle_number
        )));
    }
    Ok(angle_number)
}

fn pgc_time_secs(time: PgcTime) -> f64 {
    let base = f64::from(time.hours) * 3600.0
        + f64::from(time.minutes) * 60.0
        + f64::from(time.seconds);
    let fps = match time.frame_rate {
        FrameRate::Pal25 => 25.0,
        FrameRate::Ntsc30 => 30.0,
        FrameRate::Illegal | FrameRate::Reserved => 0.0,
    };
    if fps > 0.0 {
        base + f64::from(time.frames) / fps
    } else {
        base
    }
}

fn seconds_to_cd_frames(seconds: f64) -> u64 {
    if seconds.is_finite() && seconds > 0.0 {
        (seconds * 75.0).round() as u64
    } else {
        0
    }
}

fn title_vob_inventory(disc: &DvdDisc, source: &Path, vts_number: u8) -> Result<Vec<DvdVideoVobFileRef>, MaterializeError> {
    let mut files: Vec<&DvdFile> = disc
        .video_ts_files
        .iter()
        .filter(|file| matches!(file.kind, DvdFileKind::VtsTitle { ts, .. } if ts == vts_number))
        .collect();
    files.sort_by_key(|file| file.vob_index);

    if files.is_empty() {
        return Err(MaterializeError::Parse(format!(
            "DVD-Video VTS {vts_number} has no title VOB files in VIDEO_TS inventory"
        )));
    }

    let mut next_block = 0u32;
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let block_count_u64 = file.size.div_ceil(DVD_SECTOR_BYTES);
        if block_count_u64 == 0 {
            continue;
        }
        let block_count = u32::try_from(block_count_u64).map_err(|_| {
            MaterializeError::Parse(format!(
                "DVD-Video VOB '{}' is too large to address as 2048-byte sectors",
                file.name
            ))
        })?;
        let block_first = next_block;
        let block_last = block_first
            .checked_add(block_count)
            .and_then(|value| value.checked_sub(1))
            .ok_or_else(|| {
                MaterializeError::Parse(format!(
                    "DVD-Video VOB '{}' sector range overflow",
                    file.name
                ))
            })?;
        out.push(DvdVideoVobFileRef {
            vts_number,
            vob_index: file.vob_index,
            file_name: file.name.clone(),
            path: crate::disc::dvdv_utils::directory_video_ts_file_path(source, &file.name),
            lba: file.lba,
            byte_len: file.size,
            block_first,
            block_last,
        });
        next_block = block_last.checked_add(1).ok_or_else(|| {
            MaterializeError::Parse(format!(
                "DVD-Video VTS {vts_number} title VOB address space overflow"
            ))
        })?;
    }

    if out.is_empty() {
        return Err(MaterializeError::Parse(format!(
            "DVD-Video VTS {vts_number} title VOB inventory contains only empty files"
        )));
    }
    Ok(out)
}

fn validate_cell_sector_coverage(
    vts_number: u8,
    title_number: u8,
    chapter_number: u16,
    cell_sectors: &[(u32, u32)],
    vob_files: &[DvdVideoVobFileRef],
) -> Result<(), MaterializeError> {
    for &(first, last) in cell_sectors {
        if last < first {
            return Err(MaterializeError::Parse(format!(
                "DVD-Video VTS {vts_number} title {title_number} chapter {chapter_number} has invalid sector range {first}..{last}"
            )));
        }
        for &edge in &[first, last] {
            if !vob_files.iter().any(|file| file.contains(edge)) {
                return Err(MaterializeError::Parse(format!(
                    "DVD-Video VTS {vts_number} title {title_number} chapter {chapter_number} sector {edge} is outside the title VOB inventory"
                )));
            }
        }
    }
    Ok(())
}

fn preflight_no_css_scrambling_flags(
    source: &Path,
    vts_number: u8,
    title_number: u8,
    chapter_number: u16,
    vob_files: &[DvdVideoVobFileRef],
    cell_sectors: &[(u32, u32)],
    cancel: &CancellationToken,
) -> Result<(), MaterializeError> {
    let mut reader = DvdvCssPreflightSectorReader::open(source, vob_files)?;
    let mut sector = [0u8; DVD_SECTOR_BYTES_USIZE];
    let mut scanned = 0u32;

    for &(first, last) in cell_sectors {
        if last < first {
            return Err(MaterializeError::Parse(format!(
                "DVD-Video VTS {vts_number} title {title_number} chapter {chapter_number} has invalid sector range {first}..{last}"
            )));
        }
        let mut relative_sector = first;
        while relative_sector <= last && scanned < DVDV_CSS_PREFLIGHT_MAX_SECTORS_PER_CHAPTER {
            if cancel.is_cancelled() {
                return Err(MaterializeError::Cancelled);
            }
            reader.read_sector(vob_files, relative_sector, &mut sector)?;
            if let Some(evidence) = scrambled_pes_in_sector(&sector) {
                let vob_label = vob_files
                    .iter()
                    .find(|vob| vob.contains(relative_sector))
                    .map(|vob| vob.file_name.as_str())
                    .unwrap_or("<unknown VOB>");
                return Err(MaterializeError::Parse(format!(
                    "DVD-Video VTS {vts_number} title {title_number} chapter {chapter_number} appears CSS/encrypted: MPEG PES scrambling_control={} on stream 0x{:02X} at relative sector {} in {}. This build detects and explains likely CSS encryption but does not decrypt it; provide an unencrypted DVD-Video source and retry.",
                    evidence.scrambling_control, evidence.stream_id, relative_sector, vob_label
                )));
            }
            scanned = scanned.saturating_add(1);
            relative_sector = relative_sector.checked_add(1).ok_or_else(|| {
                MaterializeError::Parse("DVD-Video CSS preflight sector iterator overflow".to_string())
            })?;
        }
        if scanned >= DVDV_CSS_PREFLIGHT_MAX_SECTORS_PER_CHAPTER {
            break;
        }
    }
    Ok(())
}

struct DvdvCssPreflightSectorReader {
    iso: Option<File>,
    directory_vobs: BTreeMap<u8, File>,
}

impl DvdvCssPreflightSectorReader {
    fn open(source: &Path, vob_files: &[DvdVideoVobFileRef]) -> Result<Self, MaterializeError> {
        let directory_backed = vob_files.iter().any(|file| file.path.is_some());
        if directory_backed {
            let mut directory_vobs = BTreeMap::new();
            for vob in vob_files {
                let path = vob.path.as_ref().ok_or_else(|| {
                    MaterializeError::Parse(format!(
                        "DVD-Video VOB '{}' has no filesystem path in a directory-backed source",
                        vob.file_name
                    ))
                })?;
                let file = File::open(path).map_err(|err| {
                    MaterializeError::Extraction(format!(
                        "failed to open DVD-Video VOB '{}' for CSS preflight: {err}",
                        path.display()
                    ))
                })?;
                directory_vobs.insert(vob.vob_index, file);
            }
            Ok(Self {
                iso: None,
                directory_vobs,
            })
        } else {
            let iso = File::open(source).map_err(|err| {
                MaterializeError::Extraction(format!(
                    "failed to open DVD-Video ISO '{}' for CSS preflight: {err}",
                    source.display()
                ))
            })?;
            Ok(Self {
                iso: Some(iso),
                directory_vobs: BTreeMap::new(),
            })
        }
    }

    fn read_sector(
        &mut self,
        vob_files: &[DvdVideoVobFileRef],
        relative_sector: u32,
        out: &mut [u8; DVD_SECTOR_BYTES_USIZE],
    ) -> Result<(), MaterializeError> {
        let vob = vob_files
            .iter()
            .find(|file| file.contains(relative_sector))
            .ok_or_else(|| {
                MaterializeError::Parse(format!(
                    "DVD-Video CSS preflight sector {relative_sector} is outside the selected VTS title VOB inventory"
                ))
            })?;
        let offset_in_vob = relative_sector.checked_sub(vob.block_first).ok_or_else(|| {
            MaterializeError::Parse("DVD-Video CSS preflight VOB-relative sector underflow".to_string())
        })?;
        if vob.path.is_some() {
            let file = self.directory_vobs.get_mut(&vob.vob_index).ok_or_else(|| {
                MaterializeError::Extraction(format!(
                    "DVD-Video directory VOB '{}' was not opened for CSS preflight",
                    vob.file_name
                ))
            })?;
            let byte_offset = u64::from(offset_in_vob)
                .checked_mul(DVD_SECTOR_BYTES)
                .ok_or_else(|| MaterializeError::Parse("DVD-Video CSS preflight byte offset overflow".to_string()))?;
            file.seek(SeekFrom::Start(byte_offset)).map_err(|err| {
                MaterializeError::Extraction(format!(
                    "failed to seek {} during CSS preflight at byte {byte_offset}: {err}",
                    vob.file_name
                ))
            })?;
            file.read_exact(out).map_err(|err| {
                MaterializeError::Extraction(format!(
                    "failed to read {} during CSS preflight at relative sector {relative_sector}: {err}",
                    vob.file_name
                ))
            })
        } else {
            let iso = self.iso.as_mut().ok_or_else(|| {
                MaterializeError::Extraction("DVD-Video ISO reader is missing during CSS preflight".to_string())
            })?;
            let absolute_lba = u64::from(vob.lba)
                .checked_add(u64::from(offset_in_vob))
                .ok_or_else(|| MaterializeError::Parse("DVD-Video CSS preflight sector address overflow".to_string()))?;
            let byte_offset = absolute_lba
                .checked_mul(DVD_SECTOR_BYTES)
                .ok_or_else(|| MaterializeError::Parse("DVD-Video CSS preflight byte offset overflow".to_string()))?;
            iso.seek(SeekFrom::Start(byte_offset)).map_err(|err| {
                MaterializeError::Extraction(format!(
                    "failed to seek ISO sector {absolute_lba} during CSS preflight: {err}"
                ))
            })?;
            iso.read_exact(out).map_err(|err| {
                MaterializeError::Extraction(format!(
                    "failed to read ISO sector {absolute_lba} during CSS preflight: {err}"
                ))
            })
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CssScramblingEvidence {
    stream_id: u8,
    scrambling_control: u8,
}

fn scrambled_pes_in_sector(sector: &[u8]) -> Option<CssScramblingEvidence> {
    let mut i = 0usize;
    while i + 4 <= sector.len() {
        if !sector[i..].starts_with(&[0, 0, 1]) {
            i += 1;
            continue;
        }

        let stream_id = sector[i + 3];
        match ps_start_code_extent_for_css_probe(sector, i, stream_id) {
            CssProbeStartCodeExtent::Skip { end } => {
                i = end.max(i + 4);
            }
            CssProbeStartCodeExtent::Pes { packet_end } => {
                if packet_end > i + 8
                    && !is_unstructured_pes_stream_for_css_probe(stream_id)
                    && (sector[i + 6] & 0xC0) == 0x80
                {
                    let scrambling_control = (sector[i + 6] >> 4) & 0x03;
                    if scrambling_control != 0 {
                        return Some(CssScramblingEvidence {
                            stream_id,
                            scrambling_control,
                        });
                    }
                }
                i = packet_end.max(i + 4);
            }
            CssProbeStartCodeExtent::Malformed => {
                i += 4;
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CssProbeStartCodeExtent {
    Skip { end: usize },
    Pes { packet_end: usize },
    Malformed,
}

fn ps_start_code_extent_for_css_probe(
    sector: &[u8],
    start: usize,
    stream_id: u8,
) -> CssProbeStartCodeExtent {
    match stream_id {
        0xBA => pack_header_end_for_css_probe(sector, start)
            .map(|end| CssProbeStartCodeExtent::Skip { end })
            .unwrap_or(CssProbeStartCodeExtent::Malformed),
        0xBB => length_prefixed_start_code_end_for_css_probe(sector, start)
            .map(|end| CssProbeStartCodeExtent::Skip { end })
            .unwrap_or(CssProbeStartCodeExtent::Malformed),
        _ => {
            let Some(data_start) = start.checked_add(6) else {
                return CssProbeStartCodeExtent::Malformed;
            };
            if data_start > sector.len() {
                return CssProbeStartCodeExtent::Malformed;
            }
            let Some(packet_len) = pes_packet_length_for_css_probe(sector, start) else {
                return CssProbeStartCodeExtent::Malformed;
            };
            let packet_end = if packet_len == 0 {
                find_next_start_code_for_css_probe(sector, data_start).unwrap_or(sector.len())
            } else {
                let Some(end) = data_start.checked_add(packet_len).filter(|&end| end <= sector.len()) else {
                    return CssProbeStartCodeExtent::Malformed;
                };
                end
            };
            CssProbeStartCodeExtent::Pes { packet_end }
        }
    }
}

fn pes_packet_length_for_css_probe(buf: &[u8], start: usize) -> Option<usize> {
    let len_start = start.checked_add(4)?;
    let len_end = start.checked_add(6)?;
    if len_end > buf.len() {
        return None;
    }
    Some(u16::from_be_bytes([buf[len_start], buf[len_start + 1]]) as usize)
}

fn length_prefixed_start_code_end_for_css_probe(buf: &[u8], start: usize) -> Option<usize> {
    let len_end = start.checked_add(6)?;
    let packet_len = pes_packet_length_for_css_probe(buf, start)?;
    len_end.checked_add(packet_len).filter(|&end| end <= buf.len())
}

fn pack_header_end_for_css_probe(buf: &[u8], start: usize) -> Option<usize> {
    let marker = *buf.get(start.checked_add(4)?)?;
    if (marker & 0xC0) == 0x40 {
        let stuffing_index = start.checked_add(13)?;
        let stuffing = (*buf.get(stuffing_index)? & 0x07) as usize;
        start.checked_add(14)?.checked_add(stuffing).filter(|&end| end <= buf.len())
    } else if (marker & 0xF0) == 0x20 {
        start.checked_add(12).filter(|&end| end <= buf.len())
    } else {
        None
    }
}

fn find_next_start_code_for_css_probe(buf: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 3 <= buf.len() {
        if buf[i..].starts_with(&[0, 0, 1]) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn is_unstructured_pes_stream_for_css_probe(stream_id: u8) -> bool {
    matches!(
        stream_id,
        0xBC | 0xBE | 0xBF | 0xF0 | 0xF1 | 0xF2 | 0xF8 | 0xFF
    )
}

fn chapter_cell_sectors(
    vts: &VtsIfo,
    title: &dvdvideo::DvdTitle,
    chapter: &DvdChapter,
    angle_number: u8,
) -> Result<Vec<(u32, u32)>, MaterializeError> {
    if chapter.start_cell == 0 || chapter.end_cell < chapter.start_cell {
        return Err(MaterializeError::Parse(format!(
            "DVD-Video VTS {} title {} chapter {} has invalid cell span {}..{}",
            vts.vts_number, title.number, chapter.number, chapter.start_cell, chapter.end_cell
        )));
    }

    let pgc = vts
        .pgcs
        .get(usize::from(chapter.pgcn.saturating_sub(1)))
        .ok_or_else(|| {
            MaterializeError::Parse(format!(
                "DVD-Video VTS {} chapter {} references missing PGC {}",
                vts.vts_number, chapter.number, chapter.pgcn
            ))
        })?;

    let mut ranges = Vec::new();
    let mut cell_nr = chapter.start_cell;
    while cell_nr <= chapter.end_cell {
        let cell = pgc_cell(vts, chapter, pgc, cell_nr)?;
        match classify_cell_for_extraction(cell) {
            CellExtractionKind::Plain => {
                ranges.push(cell_sector_range(vts, chapter, pgc, cell_nr)?);
                if cell_nr == chapter.end_cell {
                    break;
                }
                cell_nr += 1;
            }
            CellExtractionKind::AngleBlockStart => {
                let block = collect_angle_block(vts, title, chapter, pgc, cell_nr)?;
                let selected_offset = usize::from(angle_number.saturating_sub(1));
                let Some(&selected_cell_nr) = block.cell_numbers.get(selected_offset) else {
                    return Err(MaterializeError::InvalidTrackSelection(format!(
                        "DVD-Video VTS {} title {} chapter {} angle {} is not present in authored angle block with {} cells",
                        vts.vts_number,
                        title.number,
                        chapter.number,
                        angle_number,
                        block.cell_numbers.len()
                    )));
                };
                ranges.push(cell_sector_range(vts, chapter, pgc, selected_cell_nr)?);
                if block.end_cell >= chapter.end_cell {
                    break;
                }
                cell_nr = block.end_cell + 1;
            }
            CellExtractionKind::UnsafeInterleaved => {
                return Err(unsupported_interleaved_error(vts, title, chapter, cell_nr));
            }
            CellExtractionKind::UnexpectedBlockMember => {
                return Err(MaterializeError::Parse(format!(
                    "DVD-Video VTS {} title {} chapter {} starts inside an angle/block sequence at PGC cell {}; refusing to guess playback path",
                    vts.vts_number, title.number, chapter.number, cell_nr
                )));
            }
            CellExtractionKind::ReservedBlockType => {
                return Err(MaterializeError::Parse(format!(
                    "DVD-Video VTS {} title {} chapter {} PGC cell {} uses reserved cell block type {:?}",
                    vts.vts_number,
                    title.number,
                    chapter.number,
                    cell_nr,
                    cell.block_type()
                )));
            }
        }
    }

    Ok(coalesce_authored_adjacent_ranges(ranges))
}

fn pgc_cell<'a>(
    vts: &VtsIfo,
    chapter: &DvdChapter,
    pgc: &'a Pgc,
    cell_nr: u8,
) -> Result<&'a CellPlaybackInfo, MaterializeError> {
    pgc.cells
        .get(usize::from(cell_nr.saturating_sub(1)))
        .ok_or_else(|| {
            MaterializeError::Parse(format!(
                "DVD-Video VTS {} chapter {} references missing cell playback {}",
                vts.vts_number, chapter.number, cell_nr
            ))
        })
}

fn cell_sector_range(
    vts: &VtsIfo,
    chapter: &DvdChapter,
    pgc: &Pgc,
    cell_nr: u8,
) -> Result<(u32, u32), MaterializeError> {
    let pos = pgc
        .cell_positions
        .get(usize::from(cell_nr.saturating_sub(1)))
        .ok_or_else(|| {
            MaterializeError::Parse(format!(
                "DVD-Video VTS {} chapter {} references missing cell position {}",
                vts.vts_number, chapter.number, cell_nr
            ))
        })?;
    let (first, last) = vts.cell_adt.lookup(pos.vob_id, pos.cell_id).ok_or_else(|| {
        MaterializeError::Parse(format!(
            "DVD-Video VTS {} chapter {} cell {}/{} has no VTS_C_ADT range",
            vts.vts_number, chapter.number, pos.vob_id, pos.cell_id
        ))
    })?;
    if last < first {
        return Err(MaterializeError::Parse(format!(
            "DVD-Video VTS {} chapter {} cell {}/{} has invalid sector range {}..{}",
            vts.vts_number, chapter.number, pos.vob_id, pos.cell_id, first, last
        )));
    }
    Ok((first, last))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellExtractionKind {
    Plain,
    AngleBlockStart,
    UnsafeInterleaved,
    UnexpectedBlockMember,
    ReservedBlockType,
}

fn classify_cell_for_extraction(cell: &CellPlaybackInfo) -> CellExtractionKind {
    if cell.interleaved() {
        return CellExtractionKind::UnsafeInterleaved;
    }
    match (cell.block_type(), cell.block_mode()) {
        (CellBlockType::Normal, CellBlockMode::NotInBlock) => CellExtractionKind::Plain,
        (CellBlockType::Angle, CellBlockMode::FirstCellInBlock) => CellExtractionKind::AngleBlockStart,
        (CellBlockType::Angle, CellBlockMode::CellInBlock | CellBlockMode::LastCellInBlock) => {
            CellExtractionKind::UnexpectedBlockMember
        }
        (CellBlockType::Angle, CellBlockMode::NotInBlock) => CellExtractionKind::UnexpectedBlockMember,
        (CellBlockType::Normal, _) => CellExtractionKind::UnexpectedBlockMember,
        (CellBlockType::Reserved2 | CellBlockType::Reserved3, _) => CellExtractionKind::ReservedBlockType,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AngleBlock {
    cell_numbers: Vec<u8>,
    end_cell: u8,
}

fn collect_angle_block(
    vts: &VtsIfo,
    title: &dvdvideo::DvdTitle,
    chapter: &DvdChapter,
    pgc: &Pgc,
    start_cell: u8,
) -> Result<AngleBlock, MaterializeError> {
    let mut cell_numbers = Vec::new();
    let mut cell_nr = start_cell;

    loop {
        if cell_nr > chapter.end_cell {
            return Err(MaterializeError::Parse(format!(
                "DVD-Video VTS {} title {} chapter {} angle block starting at PGC cell {} has no last-cell marker inside the chapter",
                vts.vts_number, title.number, chapter.number, start_cell
            )));
        }
        let cell = pgc_cell(vts, chapter, pgc, cell_nr)?;
        if cell.interleaved() {
            return Err(unsupported_interleaved_error(vts, title, chapter, cell_nr));
        }
        if cell.block_type() != CellBlockType::Angle {
            return Err(MaterializeError::Parse(format!(
                "DVD-Video VTS {} title {} chapter {} angle block at PGC cell {} contains non-angle cell {}",
                vts.vts_number, title.number, chapter.number, start_cell, cell_nr
            )));
        }
        cell_numbers.push(cell_nr);
        match cell.block_mode() {
            CellBlockMode::FirstCellInBlock if cell_nr == start_cell => {}
            CellBlockMode::CellInBlock => {}
            CellBlockMode::LastCellInBlock => {
                let angle_count = usize::from(title.angle_count.max(1));
                if cell_numbers.len() != angle_count {
                    log::warn!(
                        "DVD-Video VTS {} title {} chapter {} angle block has {} cells but TT_SRPT declares {} angles",
                        vts.vts_number,
                        title.number,
                        chapter.number,
                        cell_numbers.len(),
                        angle_count
                    );
                }
                return Ok(AngleBlock {
                    cell_numbers,
                    end_cell: cell_nr,
                });
            }
            _ => {
                return Err(MaterializeError::Parse(format!(
                    "DVD-Video VTS {} title {} chapter {} has invalid angle block mode {:?} at PGC cell {}",
                    vts.vts_number,
                    title.number,
                    chapter.number,
                    cell.block_mode(),
                    cell_nr
                )));
            }
        }
        if cell_nr == chapter.end_cell {
            return Err(MaterializeError::Parse(format!(
                "DVD-Video VTS {} title {} chapter {} angle block starting at PGC cell {} has no last-cell marker inside the chapter",
                vts.vts_number, title.number, chapter.number, start_cell
            )));
        }
        cell_nr += 1;
    }
}

fn unsupported_interleaved_error(
    vts: &VtsIfo,
    title: &dvdvideo::DvdTitle,
    chapter: &DvdChapter,
    cell_nr: u8,
) -> MaterializeError {
    MaterializeError::Parse(format!(
        "DVD-Video VTS {} title {} chapter {} PGC cell {} is ILVU/interleaved; safe extraction requires NAV/ILVU-aware angle following, so this build refuses to extract it rather than mixing angle blocks",
        vts.vts_number, title.number, chapter.number, cell_nr
    ))
}

/// Preserve the authored PGC/cell playback order while reducing syscalls for
/// physically contiguous cells. DVD-Video playback order is defined by the PGC
/// cell sequence, not by ascending sector order; sorting here can corrupt
/// seamless branching, angle blocks, interleaved content, or any title whose
/// logical playback order differs from physical layout.
fn coalesce_authored_adjacent_ranges(ranges: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for (first, last) in ranges {
        if let Some((_, prev_last)) = out.last_mut() {
            if first == prev_last.saturating_add(1) {
                *prev_last = last;
                continue;
            }
        }
        out.push((first, last));
    }
    out
}

fn dvdv_coding(attr: &AudioStreamAttr) -> Option<DvdVideoAudioCoding> {
    match attr.coding_mode {
        AudioCodingMode::Lpcm => Some(DvdVideoAudioCoding::Lpcm),
        AudioCodingMode::Ac3 => Some(DvdVideoAudioCoding::Ac3),
        AudioCodingMode::Dts => Some(DvdVideoAudioCoding::Dts),
        AudioCodingMode::Mpeg1 | AudioCodingMode::Mpeg2Ext => Some(DvdVideoAudioCoding::Mpeg),
        AudioCodingMode::Unknown(_) => None,
    }
}

fn source_audio_coding(coding: DvdVideoAudioCoding) -> Option<SourceAudioCoding> {
    match coding {
        DvdVideoAudioCoding::Lpcm => Some(SourceAudioCoding::Pcm),
        DvdVideoAudioCoding::Ac3 | DvdVideoAudioCoding::Dts | DvdVideoAudioCoding::Mpeg => {
            Some(SourceAudioCoding::Lossy)
        }
    }
}

fn load_dvdv_metadata_sidecars(
    source: &Path,
) -> Result<Option<Vec<crate::tui::command::DvdVideoMetadataSidecar>>, MaterializeError> {
    crate::tui::command::load_dvdv_metadata_sidecar_presentations(source)
        .map(|loaded| loaded.map(|(_, sidecars)| sidecars))
        .map_err(MaterializeError::Parse)
}


fn dvdv_presentation_identity(
    vts: &VtsIfo,
    title: &dvdvideo::DvdTitle,
    stream: &AudioStreamAttr,
    angle_number: u8,
) -> crate::tui::command::DvdVideoPresentationIdentity {
    let durations = dvdv_presentation_track_durations(vts, title, angle_number);
    crate::tui::command::DvdVideoPresentationIdentity {
        vts_number: vts.vts_number,
        title_number: title.number,
        audio_stream_index: stream.stream_index,
        angle_number: (title.angle_count.max(1) > 1).then_some(angle_number),
        track_count: Some(title.chapters.len()),
        duration_fingerprint: durations
            .as_deref()
            .map(crate::tui::command::dvdv_track_duration_fingerprint_from_secs),
    }
}

fn dvdv_presentation_track_durations(
    vts: &VtsIfo,
    title: &dvdvideo::DvdTitle,
    angle_number: u8,
) -> Option<Vec<f64>> {
    title
        .chapters
        .iter()
        .map(|chapter| chapter_duration_secs(vts, title, chapter, angle_number))
        .collect()
}

fn dvdv_supported_audio_presentation_count(vts_ifos: &[VtsIfo]) -> usize {
    let mut count = 0usize;
    for vts in vts_ifos {
        let supported_streams = vts
            .audio_streams
            .iter()
            .filter(|stream| dvdv_coding(stream).is_some())
            .count();
        if supported_streams == 0 {
            continue;
        }
        for title in &vts.titles {
            if title.chapters.is_empty() {
                continue;
            }
            let angle_count = usize::from(title.angle_count.max(1));
            count = count.saturating_add(supported_streams.saturating_mul(angle_count));
        }
    }
    count
}

fn dvdv_sidecar_matches_selection(
    sidecar: &crate::tui::command::DvdVideoMetadataSidecar,
    current: &crate::tui::command::DvdVideoPresentationIdentity,
    title_angle_count: u8,
    full_track_count: usize,
    selected_track_count: usize,
    supported_presentation_count: usize,
) -> bool {
    if sidecar.source.sidecar_kind != "dvd_video" {
        return false;
    }

    match sidecar.source.presentation.as_ref() {
        Some(stored) => {
            crate::tui::command::dvdv_presentation_identity_compatible(Some(stored), Some(current))
                // Sparse angle identity keeps ordinary single-angle TOML readable.
                // Multi-angle selections carry `Some(angle)`, so the shared
                // compatibility check rejects angle-less sidecar presentations.
                && (stored.angle_number.is_some() || title_angle_count <= 1)
        }
        None => {
            supported_presentation_count == 1
                && selected_track_count == full_track_count
                && (sidecar.tracks.is_empty() || sidecar.tracks.len() == full_track_count)
        }
    }
}

fn dvdv_album_total_tracks(materialized_track_count: usize) -> u32 {
    materialized_track_count.min(u32::MAX as usize) as u32
}

fn dvdv_output_track_number(output_index_zero_based: usize) -> u32 {
    output_index_zero_based
        .saturating_add(1)
        .min(u32::MAX as usize) as u32
}

fn overlay_dvdv_album_metadata(
    mut base: AlbumMetadata,
    sidecar: Option<&crate::tui::command::DvdVideoMetadataSidecar>,
) -> AlbumMetadata {
    let Some(sidecar) = sidecar else {
        return base;
    };

    if let Some(value) = dvdv_album_value(sidecar, &["ALBUM"]) {
        base.album = Some(value);
    }
    if let Some(value) = dvdv_album_value(sidecar, &["ALBUMARTIST", "ARTIST"]) {
        base.album_artist = Some(value);
    }
    if let Some(value) = dvdv_album_value(sidecar, &["GENRE"]) {
        base.genre = Some(value);
    }
    if let Some(value) = dvdv_album_value(sidecar, &["DATE", "YEAR"]) {
        base.date = Some(value);
    }
    if let Some(value) = dvdv_album_value(sidecar, &["DISCNUMBER"]).and_then(|v| parse_positive_u32(&v)) {
        base.disc_number = Some(value);
    }

    for (key, value) in &sidecar.album {
        if !is_dvdv_standard_album_key(key) {
            insert_nonempty(&mut base.extra, &metadata_extra_key(key), value.clone());
        }
    }
    if sidecar.schema_version != 0 {
        insert_nonempty(
            &mut base.extra,
            "dvdv_metadata_schema_version",
            sidecar.schema_version.to_string(),
        );
    }
    insert_nonempty(
        &mut base.extra,
        "dvdv_metadata_sidecar_kind",
        sidecar.source.sidecar_kind.clone(),
    );
    base
}

fn track_metadata(
    title_number: u8,
    chapter_number: u16,
    output_track_number: u32,
    sidecar: Option<&crate::tui::command::DvdVideoMetadataSidecar>,
) -> TrackMetadata {
    let sidecar_track = sidecar.and_then(|sidecar| {
        dvdv_sidecar_track_for_chapter(sidecar, title_number, chapter_number, output_track_number)
    });
    let mut extra = BTreeMap::new();
    insert_nonempty(&mut extra, "dvdv_title_number", title_number.to_string());
    insert_nonempty(&mut extra, "dvdv_chapter_number", chapter_number.to_string());

    if let Some(track) = sidecar_track {
        for (key, value) in &track.tags {
            if !is_dvdv_standard_track_key(key) {
                insert_nonempty(&mut extra, &metadata_extra_key(key), value.clone());
            }
        }
        if let Some(sidecar_number) = dvdv_track_value(Some(track), &["TRACKNUMBER"]) {
            if parse_positive_u32(&sidecar_number) != Some(output_track_number) {
                insert_nonempty(&mut extra, "dvdv_sidecar_track_number", sidecar_number);
            }
        }
    }

    let synthetic_title = format!("Title {} Chapter {}", title_number, chapter_number);
    let artist = dvdv_track_value(sidecar_track, &["ARTIST"]);
    let performer = dvdv_track_value(sidecar_track, &["PERFORMER"]).or_else(|| artist.clone());
    TrackMetadata {
        title: dvdv_track_value(sidecar_track, &["TITLE"]).or(Some(synthetic_title)),
        artist: artist.clone(),
        album_artist: dvdv_track_value(sidecar_track, &["ALBUMARTIST"]),
        composer: dvdv_track_value(sidecar_track, &["COMPOSER"]),
        performer,
        genre: dvdv_track_value(sidecar_track, &["GENRE"]),
        date: dvdv_track_value(sidecar_track, &["DATE", "YEAR"]),
        track_number: Some(output_track_number),
        disc_number: dvdv_track_value(sidecar_track, &["DISCNUMBER"])
            .and_then(|value| parse_positive_u32(&value)),
        isrc: dvdv_track_value(sidecar_track, &["ISRC"]),
        publisher: dvdv_track_value(sidecar_track, &["PUBLISHER"]),
        copyright: dvdv_track_value(sidecar_track, &["COPYRIGHT"]),
        comment: dvdv_track_value(sidecar_track, &["COMMENT"]),
        pre_emphasis: false,
        extra,
    }
}

fn dvdv_sidecar_track_for_chapter<'a>(
    sidecar: &'a crate::tui::command::DvdVideoMetadataSidecar,
    title_number: u8,
    chapter_number: u16,
    output_track_number: u32,
) -> Option<&'a crate::tui::command::DvdVideoMetadataTrack> {
    sidecar
        .tracks
        .iter()
        .find(|track| track.source_title == Some(title_number) && track.source_chapter == Some(chapter_number))
        .or_else(|| sidecar.tracks.iter().find(|track| track.source_chapter == Some(chapter_number)))
        .or_else(|| sidecar.tracks.iter().find(|track| track.number == usize::from(chapter_number)))
        .or_else(|| sidecar.tracks.get(output_track_number.saturating_sub(1) as usize))
}

fn dvdv_album_value(
    sidecar: &crate::tui::command::DvdVideoMetadataSidecar,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| nonempty_cloned(sidecar.album.get(*key)))
}

fn dvdv_track_value(
    track: Option<&crate::tui::command::DvdVideoMetadataTrack>,
    keys: &[&str],
) -> Option<String> {
    let track = track?;
    keys.iter().find_map(|key| nonempty_cloned(track.tags.get(*key)))
}

fn nonempty_cloned(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn parse_positive_u32(value: &str) -> Option<u32> {
    let parsed = value.trim().parse::<u32>().ok()?;
    (parsed > 0).then_some(parsed)
}

fn metadata_extra_key(key: &str) -> String {
    musicbrainz_standard_tag_key(key)
        .unwrap_or_else(|| key.trim().to_ascii_lowercase().replace(' ', "_"))
}

fn musicbrainz_standard_tag_key(key: &str) -> Option<String> {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_uppercase())
        .collect();
    let tag = match normalized.as_str() {
        "MUSICBRAINZALBUMID" | "MUSICBRAINZRELEASEID" => "MUSICBRAINZ_ALBUMID",
        "MUSICBRAINZALBUMARTISTID" | "MUSICBRAINZRELEASEARTISTID" => {
            "MUSICBRAINZ_ALBUMARTISTID"
        }
        "MUSICBRAINZRELEASEGROUPID" => "MUSICBRAINZ_RELEASEGROUPID",
        "MUSICBRAINZTRACKID" | "MUSICBRAINZRECORDINGID" => "MUSICBRAINZ_TRACKID",
        "MUSICBRAINZRELEASETRACKID" => "MUSICBRAINZ_RELEASETRACKID",
        "MUSICBRAINZARTISTID" => "MUSICBRAINZ_ARTISTID",
        _ => return None,
    };
    Some(tag.to_string())
}

fn insert_nonempty(extra: &mut BTreeMap<String, String>, key: &str, value: String) {
    if !value.trim().is_empty() {
        extra.insert(key.to_string(), value);
    }
}

fn is_dvdv_standard_album_key(key: &str) -> bool {
    matches!(
        key,
        "ALBUM"
            | "ALBUMARTIST"
            | "ARTIST"
            | "GENRE"
            | "DATE"
            | "YEAR"
            | "DISCNUMBER"
    )
}

fn is_dvdv_standard_track_key(key: &str) -> bool {
    matches!(
        key,
        "TITLE"
            | "ARTIST"
            | "ALBUMARTIST"
            | "PERFORMER"
            | "COMPOSER"
            | "GENRE"
            | "DATE"
            | "YEAR"
            | "TRACKNUMBER"
            | "DISCNUMBER"
            | "ISRC"
            | "PUBLISHER"
            | "COPYRIGHT"
            | "COMMENT"
    )
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

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(ext))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {

    use super::dvdv_tool_versions;
    use super::super::errors::ToolRunnerError;
    use super::super::tool::{ToolBinary, ToolCommand, ToolRunner};
    use std::collections::{BTreeMap, HashMap};
    use std::path::PathBuf;
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

    #[test]
    fn dvdv_materializer_provenance_records_in_process_dvdvideo_and_detected_ffmpeg() {
        let runner = VersionOnlyRunner(HashMap::from([
            (ToolBinary::Ffmpeg, "7.1.3".to_string()),
        ]));
        let versions = dvdv_tool_versions(&runner);

        assert_eq!(versions.get("dvdvideo").map(String::as_str), Some("in-process"));
        assert_eq!(versions.get("ffmpeg").map(String::as_str), Some("7.1.3"));
    }

    #[test]
    fn dvdv_materializer_provenance_keeps_only_in_process_tool_when_ffmpeg_missing() {
        let runner = VersionOnlyRunner(HashMap::new());
        let versions = dvdv_tool_versions(&runner);
        let expected = BTreeMap::from([("dvdvideo".to_string(), "in-process".to_string())]);

        assert_eq!(versions, expected);
    }

    use super::{
        classify_cell_for_extraction, coalesce_authored_adjacent_ranges, dvdv_album_total_tracks,
        dvdv_sidecar_matches_selection, overlay_dvdv_album_metadata, selected_chapter_ordinals,
        track_metadata, CellExtractionKind, DvdvDefaultProgramScore, ReverseDvdvIdentity,
    };
    use super::TrackSelection;
    use dvdvideo::ifo::{CellPlaybackInfo, FrameRate, PgcTime};
    use std::collections::BTreeSet;



    fn default_score(
        track_count: usize,
        duration_frames: u64,
        stereo: bool,
        lossless: bool,
        vts_number: u8,
        title_number: u8,
        audio_stream_index: u8,
    ) -> DvdvDefaultProgramScore {
        default_score_with_duration_flag(
            true,
            track_count,
            duration_frames,
            stereo,
            lossless,
            vts_number,
            title_number,
            audio_stream_index,
        )
    }

    fn default_score_with_duration_flag(
        playback_path_has_duration: bool,
        track_count: usize,
        duration_frames: u64,
        stereo: bool,
        lossless: bool,
        vts_number: u8,
        title_number: u8,
        audio_stream_index: u8,
    ) -> DvdvDefaultProgramScore {
        DvdvDefaultProgramScore {
            playback_path_has_duration,
            track_count,
            duration_frames,
            stereo,
            lossless,
            coding_rank: if lossless { 4 } else { 2 },
            sample_rate: 48_000,
            bit_depth: if lossless { 24 } else { 0 },
            reverse_identity: ReverseDvdvIdentity {
                vts_number: u8::MAX.saturating_sub(vts_number),
                title_number: u8::MAX.saturating_sub(title_number),
                audio_stream_index: u8::MAX.saturating_sub(audio_stream_index),
            },
        }
    }


    fn ordinal_set(values: &[u32]) -> BTreeSet<u32> {
        values.iter().copied().collect()
    }

    #[test]
    fn selected_chapter_ordinals_limits_work_before_chapter_validation() {
        assert_eq!(
            selected_chapter_ordinals(8, &TrackSelection::Range { start: 1, end: 1 }).unwrap(),
            ordinal_set(&[1])
        );
        assert_eq!(
            selected_chapter_ordinals(8, &TrackSelection::Set(ordinal_set(&[2, 5]))).unwrap(),
            ordinal_set(&[2, 5])
        );
    }

    #[test]
    fn selected_chapter_ordinals_rejects_invalid_selection_before_preflight() {
        let err = selected_chapter_ordinals(3, &TrackSelection::Set(ordinal_set(&[4]))).unwrap_err();
        let message = format!("{err:?}");
        assert!(message.contains("outside valid range"));
    }


    #[test]
    fn dvdv_album_total_tracks_uses_materialized_selection_count() {
        assert_eq!(dvdv_album_total_tracks(0), 0);
        assert_eq!(dvdv_album_total_tracks(1), 1);
        assert_eq!(dvdv_album_total_tracks(3), 3);
    }

    #[test]
    fn dvdv_sidecar_overlay_maps_album_and_track_musicbrainz_ids_to_standard_extras() {
        let sidecar = crate::tui::command::DvdVideoMetadataSidecar {
            schema_version: 2,
            source: crate::tui::command::DvdVideoMetadataSource {
                path: PathBuf::from("concert.iso"),
                sidecar_kind: "dvd_video".to_string(),
                presentation: Some(crate::tui::command::DvdVideoPresentationIdentity {
                    vts_number: 1,
                    title_number: 1,
                    audio_stream_index: 0,
                    angle_number: Some(1),
                    track_count: Some(1),
                    duration_fingerprint: Some("dvdv-ms-v1:1:abc".to_string()),
                }),
                extra: BTreeMap::new(),
            },
            album: BTreeMap::from([
                ("ALBUM".to_string(), "Live at the Theater".to_string()),
                ("ALBUMARTIST".to_string(), "Example Band".to_string()),
                ("MUSICBRAINZ_ALBUMID".to_string(), "release-id".to_string()),
                ("MUSICBRAINZ_RELEASEGROUPID".to_string(), "group-id".to_string()),
                ("CATALOGNUMBER".to_string(), "DVD-001".to_string()),
            ]),
            tracks: vec![crate::tui::command::DvdVideoMetadataTrack {
                number: 1,
                label: "01".to_string(),
                source_title: None,
                source_chapter: None,
                tags: BTreeMap::from([
                    ("TITLE".to_string(), "Opening".to_string()),
                    ("ARTIST".to_string(), "Example Band".to_string()),
                    ("MUSICBRAINZ_TRACKID".to_string(), "recording-id".to_string()),
                    ("MUSICBRAINZ_RELEASETRACKID".to_string(), "release-track-id".to_string()),
                ]),
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        };

        let album = overlay_dvdv_album_metadata(
            super::AlbumMetadata {
                total_tracks: dvdv_album_total_tracks(1),
                ..super::AlbumMetadata::default()
            },
            Some(&sidecar),
        );
        let track = track_metadata(1, 1, 1, Some(&sidecar));

        assert_eq!(album.album.as_deref(), Some("Live at the Theater"));
        assert_eq!(album.album_artist.as_deref(), Some("Example Band"));
        assert_eq!(album.total_tracks, 1);
        assert_eq!(album.extra.get("MUSICBRAINZ_ALBUMID").map(String::as_str), Some("release-id"));
        assert_eq!(
            album.extra.get("MUSICBRAINZ_RELEASEGROUPID").map(String::as_str),
            Some("group-id")
        );
        assert_eq!(album.extra.get("catalognumber").map(String::as_str), Some("DVD-001"));
        assert_eq!(track.title.as_deref(), Some("Opening"));
        assert_eq!(track.artist.as_deref(), Some("Example Band"));
        assert_eq!(track.extra.get("MUSICBRAINZ_TRACKID").map(String::as_str), Some("recording-id"));
        assert_eq!(
            track.extra.get("MUSICBRAINZ_RELEASETRACKID").map(String::as_str),
            Some("release-track-id")
        );
    }

    #[test]
    fn default_program_score_prefers_computable_selected_angle_path() {
        let unsafe_interleaved_main = default_score_with_duration_flag(
            false,
            20,
            0,
            true,
            true,
            1,
            1,
            0,
        );
        let safe_extractable_program = default_score_with_duration_flag(
            true,
            1,
            60 * 75,
            false,
            false,
            1,
            2,
            0,
        );

        assert!(safe_extractable_program > unsafe_interleaved_main);
    }

    #[test]
    fn default_program_score_prefers_main_title_over_short_stereo_bonus() {
        let short_stereo_bonus = default_score(1, 60 * 75, true, true, 1, 1, 0);
        let chaptered_main_program = default_score(12, 55 * 60 * 75, false, false, 1, 2, 1);

        assert!(chaptered_main_program > short_stereo_bonus);
    }

    #[test]
    fn default_program_score_prefers_stereo_within_same_program() {
        let surround = default_score(10, 45 * 60 * 75, false, false, 1, 1, 0);
        let stereo = default_score(10, 45 * 60 * 75, true, false, 1, 1, 1);

        assert!(stereo > surround);
    }

    #[test]
    fn default_program_score_ties_break_to_lower_authored_identity() {
        let lower_identity = default_score(10, 45 * 60 * 75, true, false, 1, 1, 0);
        let higher_identity = default_score(10, 45 * 60 * 75, true, false, 2, 1, 0);

        assert!(lower_identity > higher_identity);
    }

    fn cell(category_byte0: u8) -> CellPlaybackInfo {
        CellPlaybackInfo {
            category_byte0,
            restricted: false,
            still_time: 0,
            cell_command: 0,
            playback_time: PgcTime {
                hours: 0,
                minutes: 0,
                seconds: 0,
                frames: 0,
                frame_rate: FrameRate::Ntsc30,
            },
            first_vobu_start_sector: 0,
            first_ilvu_end_sector: 0,
            last_vobu_start_sector: 0,
            last_vobu_end_sector: 0,
        }
    }

    fn dvdv_test_identity(
        vts_number: u8,
        title_number: u8,
        audio_stream_index: u8,
        angle_number: Option<u8>,
        track_count: usize,
        fingerprint: &str,
    ) -> crate::tui::command::DvdVideoPresentationIdentity {
        crate::tui::command::DvdVideoPresentationIdentity {
            vts_number,
            title_number,
            audio_stream_index,
            angle_number,
            track_count: Some(track_count),
            duration_fingerprint: Some(fingerprint.to_string()),
        }
    }

    #[test]
    fn dvdv_sidecar_matches_bound_presentation_only() {
        let sidecar = crate::tui::command::DvdVideoMetadataSidecar {
            schema_version: 3,
            source: crate::tui::command::DvdVideoMetadataSource {
                path: PathBuf::from("concert.iso"),
                sidecar_kind: "dvd_video".to_string(),
                presentation: Some(dvdv_test_identity(2, 4, 1, Some(1), 12, "fp-a")),
                extra: BTreeMap::new(),
            },
            album: BTreeMap::new(),
            tracks: Vec::new(),
            extra: BTreeMap::new(),
        };

        assert!(dvdv_sidecar_matches_selection(
            &sidecar,
            &dvdv_test_identity(2, 4, 1, Some(1), 12, "fp-a"),
            1,
            12,
            12,
            2,
        ));
        assert!(!dvdv_sidecar_matches_selection(
            &sidecar,
            &dvdv_test_identity(2, 5, 1, Some(1), 12, "fp-a"),
            1,
            12,
            12,
            2,
        ));
        assert!(!dvdv_sidecar_matches_selection(
            &sidecar,
            &dvdv_test_identity(2, 4, 2, Some(1), 12, "fp-a"),
            1,
            12,
            12,
            2,
        ));
        assert!(!dvdv_sidecar_matches_selection(
            &sidecar,
            &dvdv_test_identity(2, 4, 1, Some(1), 11, "fp-a"),
            1,
            12,
            12,
            2,
        ));
        assert!(!dvdv_sidecar_matches_selection(
            &sidecar,
            &dvdv_test_identity(2, 4, 1, Some(1), 12, "fp-b"),
            1,
            12,
            12,
            2,
        ));
    }

    #[test]
    fn legacy_dvdv_sidecar_applies_only_to_unambiguous_full_selection() {
        let sidecar = crate::tui::command::DvdVideoMetadataSidecar {
            schema_version: 1,
            source: crate::tui::command::DvdVideoMetadataSource {
                path: PathBuf::from("concert.iso"),
                sidecar_kind: "dvd_video".to_string(),
                presentation: None,
                extra: BTreeMap::new(),
            },
            album: BTreeMap::new(),
            tracks: vec![
                crate::tui::command::DvdVideoMetadataTrack {
                    number: 1,
                    label: "01".to_string(),
                    source_title: None,
                    source_chapter: None,
                    tags: BTreeMap::new(),
                    extra: BTreeMap::new(),
                },
                crate::tui::command::DvdVideoMetadataTrack {
                    number: 2,
                    label: "02".to_string(),
                    source_title: None,
                    source_chapter: None,
                    tags: BTreeMap::new(),
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        };
        let current = dvdv_test_identity(1, 1, 0, Some(1), 2, "fp");

        assert!(dvdv_sidecar_matches_selection(&sidecar, &current, 1, 2, 2, 1));
        assert!(!dvdv_sidecar_matches_selection(&sidecar, &current, 1, 2, 1, 1));
        assert!(!dvdv_sidecar_matches_selection(&sidecar, &current, 1, 2, 2, 2));
    }

    #[test]
    fn selected_subset_track_metadata_uses_output_order_but_overlays_source_chapter() {
        let sidecar = crate::tui::command::DvdVideoMetadataSidecar {
            schema_version: 3,
            source: crate::tui::command::DvdVideoMetadataSource {
                path: PathBuf::from("concert.iso"),
                sidecar_kind: "dvd_video".to_string(),
                presentation: Some(dvdv_test_identity(1, 1, 0, Some(1), 4, "fp")),
                extra: BTreeMap::new(),
            },
            album: BTreeMap::new(),
            tracks: vec![
                crate::tui::command::DvdVideoMetadataTrack { number: 1, label: "01".to_string(), source_title: None, source_chapter: None, tags: BTreeMap::from([("TITLE".to_string(), "One".to_string())]), extra: BTreeMap::new() },
                crate::tui::command::DvdVideoMetadataTrack { number: 2, label: "02".to_string(), source_title: None, source_chapter: None, tags: BTreeMap::from([("TITLE".to_string(), "Two".to_string()), ("TRACKNUMBER".to_string(), "2".to_string())]), extra: BTreeMap::new() },
                crate::tui::command::DvdVideoMetadataTrack { number: 3, label: "03".to_string(), source_title: None, source_chapter: None, tags: BTreeMap::from([("TITLE".to_string(), "Three".to_string()), ("TRACKNUMBER".to_string(), "3".to_string())]), extra: BTreeMap::new() },
            ],
            extra: BTreeMap::new(),
        };

        let track = track_metadata(1, 3, 1, Some(&sidecar));

        assert_eq!(track.title.as_deref(), Some("Three"));
        assert_eq!(track.track_number, Some(1));
        assert_eq!(
            track.extra.get("dvdv_sidecar_track_number").map(String::as_str),
            Some("3")
        );
    }

    #[test]
    fn coalesce_authored_adjacent_ranges_preserves_non_linear_playback_order() {
        let ranges = vec![(100, 109), (20, 29), (110, 119)];

        assert_eq!(
            coalesce_authored_adjacent_ranges(ranges),
            vec![(100, 109), (20, 29), (110, 119)]
        );
    }

    #[test]
    fn coalesce_authored_adjacent_ranges_merges_only_in_sequence_adjacency() {
        let ranges = vec![(10, 19), (20, 29), (50, 59), (60, 69)];

        assert_eq!(
            coalesce_authored_adjacent_ranges(ranges),
            vec![(10, 29), (50, 69)]
        );
    }

    #[test]
    fn coalesce_authored_adjacent_ranges_does_not_merge_overlap_or_repeats() {
        let ranges = vec![(10, 20), (15, 25), (26, 30), (10, 20)];

        assert_eq!(
            coalesce_authored_adjacent_ranges(ranges),
            vec![(10, 20), (15, 30), (10, 20)]
        );
    }

    #[test]
    fn classifies_angle_block_start_without_treating_all_cells_as_plain() {
        // bits 7..6 = first cell in block, bits 5..4 = angle block.
        assert_eq!(
            classify_cell_for_extraction(&cell(0b0101_0000)),
            CellExtractionKind::AngleBlockStart
        );
    }

    #[test]
    fn classifies_interleaved_cells_as_fail_safe() {
        assert_eq!(
            classify_cell_for_extraction(&cell(0b0000_0100)),
            CellExtractionKind::UnsafeInterleaved
        );
    }

    #[test]
    fn classifies_mid_block_cells_as_unexpected_without_reordering() {
        // bits 7..6 = in block, bits 5..4 = angle block.
        assert_eq!(
            classify_cell_for_extraction(&cell(0b1001_0000)),
            CellExtractionKind::UnexpectedBlockMember
        );
    }
}
