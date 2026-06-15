//! DVD-Video `dvdvideo` crate to unified disc-browser model mapping.

use std::path::Path;

use dvdvideo::{AudioCodingMode, AudioStreamAttr, DvdChapter, DvdDisc, DvdTitle, Pgc, VtsIfo};
use dvdvideo::ifo::{CellBlockMode, CellBlockType, CellPlaybackInfo, FrameRate, PgcTime};

use super::labels;
use super::model::*;

/// Build unified disc contents for a parsed DVD-Video source.
pub fn map_dvdv_disc(
    disc: &DvdDisc,
    vts_ifos: &[(u8, VtsIfo)],
    source_path: &Path,
) -> DiscContents {
    let file_stem = source_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let label = labels::disc_label(None, &disc.volume_id, file_stem, DiscFormat::DvdVideo);

    let mut presentations = Vec::new();
    let mut suppressed = Vec::new();

    for (vts_number, vts) in vts_ifos {
        let streams: Vec<_> = vts
            .audio_streams
            .iter()
            .filter(|attr| dvdv_coding(attr).is_some())
            .collect();
        if streams.is_empty() {
            suppressed.push(SuppressedPresentation {
                id: PresentationId::dvd_video(*vts_number, 1, 0),
                reason: "VTS declares no supported audio streams".to_string(),
                track_count: vts.titles.iter().map(|title| title.chapters.len()).sum(),
                duration_secs: 0.0,
                native_detail: Some(format!("VTS {}", vts_number)),
            });
            continue;
        }

        for title in &vts.titles {
            if title.chapters.is_empty() {
                suppressed.push(SuppressedPresentation {
                    id: PresentationId::dvd_video(*vts_number, title.number, streams[0].stream_index),
                    reason: "title contains no chapters".to_string(),
                    track_count: 0,
                    duration_secs: 0.0,
                    native_detail: Some(format!("VTS {} title {}", vts_number, title.number)),
                });
                continue;
            }

            for attr in &streams {
                let coding = dvdv_coding(attr).expect("filtered supported DVD-Video stream");
                let format = format_for_stream(*attr, coding);
                let angle_suffix = if title.angle_count > 1 {
                    format!(" · Angle 1/{}", title.angle_count)
                } else {
                    String::new()
                };
                let label = format!(
                    "VTS {:02} Title {:02} Stream {} · {}{}",
                    vts_number,
                    title.number,
                    dvd_video_audio_stream_display_number(attr.stream_index),
                    labels::presentation_label(&format),
                    angle_suffix
                );
                let tracks = title
                    .chapters
                    .iter()
                    .map(|chapter| DiscTrack {
                        number: u32::from(chapter.number),
                        title: Some(format!("Chapter {}", chapter.number)),
                        performer: None,
                        duration_secs: chapter_duration_secs(vts, title, chapter, 1),
                        format_note: if title.angle_count > 1 {
                            Some(format!("Default angle 1 of {}", title.angle_count))
                        } else {
                            None
                        },
                    })
                    .collect::<Vec<_>>();
                let total_duration_secs = tracks_total_duration_secs(&tracks).unwrap_or(0.0);

                presentations.push(DiscPresentation {
                    id: PresentationId::dvd_video(*vts_number, title.number, attr.stream_index),
                    label,
                    format,
                    tracks,
                    total_duration_secs,
                    album_title: None,
                    album_artist: None,
                    genre: None,
                    year: None,
                });
            }
        }
    }

    DiscContents {
        format: DiscFormat::DvdVideo,
        label,
        source_path: source_path.to_path_buf(),
        presentations,
        suppressed,
        copy_protection: CopyProtectionSummary {
            description: "CSS decryption is not implemented; materialization preflights selected DVD-Video sectors for MPEG PES scrambling flags and fails fast with a targeted message if likely CSS/encryption is detected".to_string(),
        },
        diagnostics: Vec::new(),
        album_title: None,
        album_artist: None,
        genre: None,
        year: None,
    }
}

fn format_for_stream(attr: &AudioStreamAttr, coding: crate::convert::pipeline::DvdVideoAudioCoding) -> AudioPresentationFormat {
    AudioPresentationFormat {
        codec: Some(coding.label().to_string()),
        sample_rate: attr.sample_frequency,
        bit_depth: attr.bit_depth,
        channels: Some(attr.channels),
        channel_layout: Some(channel_layout_label(attr.channels)),
        lossless: coding.is_lossless(),
        provenance: FormatProvenance::IfoAttributes,
    }
}

fn dvdv_coding(attr: &AudioStreamAttr) -> Option<crate::convert::pipeline::DvdVideoAudioCoding> {
    match attr.coding_mode {
        AudioCodingMode::Lpcm => Some(crate::convert::pipeline::DvdVideoAudioCoding::Lpcm),
        AudioCodingMode::Ac3 => Some(crate::convert::pipeline::DvdVideoAudioCoding::Ac3),
        AudioCodingMode::Dts => Some(crate::convert::pipeline::DvdVideoAudioCoding::Dts),
        AudioCodingMode::Mpeg1 | AudioCodingMode::Mpeg2Ext => {
            Some(crate::convert::pipeline::DvdVideoAudioCoding::Mpeg)
        }
        AudioCodingMode::Unknown(_) => None,
    }
}

fn channel_layout_label(channels: u8) -> String {
    match channels {
        1 => "Mono".to_string(),
        2 => "Stereo".to_string(),
        6 => "5.1".to_string(),
        8 => "7.1".to_string(),
        n => format!("{}ch", n),
    }
}

fn chapter_duration_secs(
    vts: &VtsIfo,
    title: &DvdTitle,
    chapter: &DvdChapter,
    angle_number: u8,
) -> Option<f64> {
    let pgc = vts.pgcs.get(usize::from(chapter.pgcn.checked_sub(1)?))?;
    if chapter.start_cell == 0 || chapter.end_cell < chapter.start_cell {
        return None;
    }

    if !pgc.cells.is_empty() {
        return chapter_duration_from_selected_angle_cells(title, chapter, pgc, angle_number);
    }

    // Defensive fallback for unusual IFOs that expose a chapter spanning an
    // entire PGC but omit the C_PBI entries. Avoid using the whole-PGC duration
    // for a partial chapter because that would overstate multi-chapter PGCs.
    if chapter.start_cell == 1 && chapter.end_cell == pgc.number_of_cells {
        Some(pgc_time_secs(pgc.playback_time))
    } else {
        None
    }
}

fn chapter_duration_from_selected_angle_cells(
    title: &DvdTitle,
    chapter: &DvdChapter,
    pgc: &Pgc,
    angle_number: u8,
) -> Option<f64> {
    let mut total = 0.0;
    let mut cell_nr = chapter.start_cell;
    while cell_nr <= chapter.end_cell {
        let cell = pgc_cell(pgc, cell_nr)?;
        match classify_cell_for_duration(cell) {
            Some(CellDurationKind::Plain) => {
                total += pgc_time_secs(cell.playback_time);
                if cell_nr == chapter.end_cell {
                    break;
                }
                cell_nr += 1;
            }
            Some(CellDurationKind::AngleBlockStart) => {
                let block = collect_non_interleaved_angle_block(title, chapter, pgc, cell_nr)?;
                let selected_offset = usize::from(angle_number.max(1).saturating_sub(1));
                let selected_cell_nr = *block.cell_numbers.get(selected_offset)?;
                let selected_cell = pgc_cell(pgc, selected_cell_nr)?;
                total += pgc_time_secs(selected_cell.playback_time);
                if block.end_cell >= chapter.end_cell {
                    break;
                }
                cell_nr = block.end_cell + 1;
            }
            _ => return None,
        }
    }
    Some(total)
}

fn pgc_cell(pgc: &Pgc, cell_nr: u8) -> Option<&CellPlaybackInfo> {
    pgc.cells.get(usize::from(cell_nr.checked_sub(1)?))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellDurationKind {
    Plain,
    AngleBlockStart,
}

fn classify_cell_for_duration(cell: &CellPlaybackInfo) -> Option<CellDurationKind> {
    if cell.interleaved() {
        return None;
    }
    match (cell.block_type(), cell.block_mode()) {
        (CellBlockType::Normal, CellBlockMode::NotInBlock) => Some(CellDurationKind::Plain),
        (CellBlockType::Angle, CellBlockMode::FirstCellInBlock) => Some(CellDurationKind::AngleBlockStart),
        (CellBlockType::Angle, CellBlockMode::CellInBlock | CellBlockMode::LastCellInBlock)
        | (CellBlockType::Angle, CellBlockMode::NotInBlock)
        | (CellBlockType::Normal, _)
        | (CellBlockType::Reserved2 | CellBlockType::Reserved3, _) => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AngleBlock {
    cell_numbers: Vec<u8>,
    end_cell: u8,
}

fn collect_non_interleaved_angle_block(
    title: &DvdTitle,
    chapter: &DvdChapter,
    pgc: &Pgc,
    start_cell: u8,
) -> Option<AngleBlock> {
    let mut cell_numbers = Vec::new();
    let mut cell_nr = start_cell;

    loop {
        if cell_nr > chapter.end_cell {
            return None;
        }
        let cell = pgc_cell(pgc, cell_nr)?;
        if cell.interleaved() || cell.block_type() != CellBlockType::Angle {
            return None;
        }
        cell_numbers.push(cell_nr);
        match cell.block_mode() {
            CellBlockMode::FirstCellInBlock if cell_nr == start_cell => {}
            CellBlockMode::CellInBlock => {}
            CellBlockMode::LastCellInBlock => {
                let declared_angles = usize::from(title.angle_count.max(1));
                if cell_numbers.len() < declared_angles {
                    return None;
                }
                return Some(AngleBlock {
                    cell_numbers,
                    end_cell: cell_nr,
                });
            }
            _ => return None,
        }
        if cell_nr == chapter.end_cell {
            return None;
        }
        cell_nr += 1;
    }
}

fn tracks_total_duration_secs(tracks: &[DiscTrack]) -> Option<f64> {
    let mut total = 0.0;
    for track in tracks {
        total += track.duration_secs?;
    }
    Some(total)
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
