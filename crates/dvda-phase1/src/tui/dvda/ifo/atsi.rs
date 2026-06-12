#![forbid(unsafe_code)]

use crate::tui::dvda::endian::{be_u32, identifier, require_len, slice, u8_at};
use crate::tui::dvda::error::{DvdaError, Result};
use crate::tui::dvda::model::{
    channel_assignment, parse_channel_format, track_type_low_bits_candidate, AobFileEntry,
    AtsiHeader, AudioAttributes, AudioChapter, AudioCoding, AudioTitle, DiagnosticSeverity,
    DownmixChannelCoefficients, DownmixCoefficient, DownmixMatrix, DownmixPhase,
    DvdaDiagnostic, SectorRange, TitleSet, TitleSetKind, DVD_BLOCK_SIZE, DOWNMIX_SOURCE_CHANNELS,
};

const ATSI_IDENTIFIER: &str = "DVDAUDIO-ATS";

const AUDIO_FORMAT_BASE: usize = 0x100;
const AUDIO_FORMAT_SIZE: usize = 16;
const AUDIO_FORMAT_COUNT: usize = 8;
const DOWNMIX_BASE: usize = 0x180;
const DOWNMIX_SIZE: usize = 18;
const DOWNMIX_COUNT: usize = 14;
const ATSI_MAT_PARSED_SIZE: usize = DOWNMIX_BASE + DOWNMIX_COUNT * DOWNMIX_SIZE;
const DEFAULT_AUDIO_PGCIT_OFFSET: usize = 0x800;
const AUDIO_PGCIT_SIZE: usize = 8;
const ATS_TITLE_IDX_SIZE: usize = 8;
const ATS_TITLE_SIZE: usize = 16;
const ATS_TRACK_TIMESTAMP_SIZE: usize = 20;
const ATS_TRACK_SECTOR_SIZE: usize = 12;

pub fn parse_atsi(
    bytes: &[u8],
    source_file: &str,
    title_set_nr: u8,
    aobs: Vec<AobFileEntry>,
) -> Result<TitleSet> {
    require_len(bytes, ATSI_MAT_PARSED_SIZE, format!("{source_file} ATSI parsed MAT area"))?;
    let got = identifier(bytes, 0, 12, "ATSI identifier")?;
    if got != ATSI_IDENTIFIER {
        return Err(DvdaError::InvalidIdentifier {
            file: source_file.to_string(),
            expected: ATSI_IDENTIFIER,
            got,
        });
    }

    let header = AtsiHeader {
        ats_last_sector: be_u32(bytes, 0x0C, "ats_last_sector")?,
        atsi_last_sector: be_u32(bytes, 0x1C, "atsi_last_sector")?,
        specification_version: u8_at(bytes, 0x21, "specification_version")?,
        category: be_u32(bytes, 0x22, "ats_category")?,
        atsm_vobs: be_u32(bytes, 0xC0, "atsm_vobs")?,
        atstt_vobs: be_u32(bytes, 0xC4, "atstt_vobs")?,
        ats_ptt_srpt: be_u32(bytes, 0xC8, "ats_ptt_srpt")?,
        ats_pgcit: be_u32(bytes, 0xCC, "ats_pgcit")?,
        ats_c_adt: be_u32(bytes, 0xE0, "ats_c_adt")?,
        ats_vobu_admap: be_u32(bytes, 0xE4, "ats_vobu_admap")?,
    };

    let kind = if header.atsm_vobs == 0 { TitleSetKind::Audio } else { TitleSetKind::Video };
    let mut diagnostics = Vec::new();
    if kind != TitleSetKind::Audio {
        diagnostics.push(DvdaDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "dvda.atsi.video_titleset",
            message: format!("{source_file} has ATSM_VOBS != 0; retaining header but not treating it as primary audio"),
        });
    }

    let audio_formats = parse_audio_formats(bytes)?;
    let downmix_matrices = parse_downmix_matrices(bytes)?;
    let (audio_pgcit_offset, titles, format_diagnostics) = parse_audio_pgcit(bytes, &header, title_set_nr)?;
    diagnostics.extend(format_diagnostics);
    let aobs_last_sector = header
        .atsi_last_sector
        .checked_add(1)
        .and_then(|ifo_sectors| ifo_sectors.checked_mul(2))
        .and_then(|overhead| header.ats_last_sector.checked_sub(overhead));

    if aobs_last_sector.is_none() {
        diagnostics.push(DvdaDiagnostic::warn(
            "dvda.atsi.aob_sector_underflow",
            format!("{source_file}: ats_last_sector is too small to subtract IFO+BUP overhead"),
        ));
    }

    Ok(TitleSet {
        number: title_set_nr,
        source_file: source_file.to_string(),
        kind,
        header,
        audio_pgcit_offset,
        audio_formats,
        downmix_matrices,
        aobs,
        aobs_last_sector,
        titles,
        diagnostics,
    })
}

fn parse_audio_formats(bytes: &[u8]) -> Result<Vec<AudioAttributes>> {
    let mut out = Vec::with_capacity(AUDIO_FORMAT_COUNT);
    for i in 0..AUDIO_FORMAT_COUNT {
        let off = AUDIO_FORMAT_BASE + i * AUDIO_FORMAT_SIZE;
        let entry = slice(bytes, off, AUDIO_FORMAT_SIZE, format!("audio_format[{i}]"))?;
        let audio_type_raw = u16::from_be_bytes([entry[0], entry[1]]);
        let raw = [entry[2], entry[3], entry[4]];
        let channel_format = parse_channel_format(raw);
        let channel_assignment = channel_assignment(channel_format.assignment_code);
        out.push(AudioAttributes {
            format_index: i as u8,
            present: audio_type_raw == 0x0100,
            audio_type_raw,
            channel_format,
            channel_assignment,
            coding: AudioCoding::Unknown,
        });
    }
    Ok(out)
}

fn parse_downmix_matrices(bytes: &[u8]) -> Result<Vec<DownmixMatrix>> {
    let mut out = Vec::with_capacity(DOWNMIX_COUNT);
    for i in 0..DOWNMIX_COUNT {
        let off = DOWNMIX_BASE + i * DOWNMIX_SIZE;
        let raw_slice = slice(bytes, off, DOWNMIX_SIZE, format!("downmix_matrix[{i}]"))?;
        let mut raw = [0u8; DOWNMIX_SIZE];
        raw.copy_from_slice(raw_slice);
        let phase = DownmixPhase { left_mask: raw[0], right_mask: raw[1] };
        let mut channels = Vec::with_capacity(DOWNMIX_SOURCE_CHANNELS);
        for ch in 0..DOWNMIX_SOURCE_CHANNELS {
            let bit = 1u8 << (DOWNMIX_SOURCE_CHANNELS - ch - 1);
            channels.push(DownmixChannelCoefficients {
                source_channel: ch as u8,
                left: DownmixCoefficient {
                    raw: raw[2 + ch * 2],
                    inverse_phase: (phase.left_mask & bit) != 0,
                },
                right: DownmixCoefficient {
                    raw: raw[3 + ch * 2],
                    inverse_phase: (phase.right_mask & bit) != 0,
                },
            });
        }
        out.push(DownmixMatrix { index: i as u8, raw, phase, channels });
    }
    Ok(out)
}

fn parse_audio_pgcit(
    bytes: &[u8],
    header: &AtsiHeader,
    title_set_nr: u8,
) -> Result<(usize, Vec<AudioTitle>, Vec<DvdaDiagnostic>)> {
    let pgcit_offset = if header.ats_pgcit != 0 {
        (header.ats_pgcit as usize)
            .checked_mul(DVD_BLOCK_SIZE as usize)
            .ok_or_else(|| DvdaError::parse("audio_pgcit", "ATSI PGCIT sector offset overflows usize"))?
    } else {
        DEFAULT_AUDIO_PGCIT_OFFSET
    };

    let pgcit = slice(bytes, pgcit_offset, AUDIO_PGCIT_SIZE, "audio_pgcit")?;
    let nr_of_titles = u16::from_be_bytes([pgcit[0], pgcit[1]]);
    let last_byte = u32::from_be_bytes([pgcit[4], pgcit[5], pgcit[6], pgcit[7]]) as usize;
    let pgcit_available = bytes.len().saturating_sub(pgcit_offset);
    let bounded_len = pgcit_available.min(last_byte.saturating_add(1));
    let pgcit_data = slice(bytes, pgcit_offset, bounded_len, "audio_pgcit bounded data")?;

    let mut titles = Vec::with_capacity(nr_of_titles as usize);
    let diagnostics = Vec::new();
    for i in 0..nr_of_titles as usize {
        let idx_off = AUDIO_PGCIT_SIZE + i * ATS_TITLE_IDX_SIZE;
        let idx = slice(pgcit_data, idx_off, ATS_TITLE_IDX_SIZE, format!("ats_title_idx[{i}]"))?;
        let title_nr = idx[0];
        let title_table_offset = u32::from_be_bytes([idx[4], idx[5], idx[6], idx[7]]);
        let title_off = title_table_offset as usize;
        let title = slice(pgcit_data, title_off, ATS_TITLE_SIZE, format!("ats_title[{i}]"))?;
        let tracks = title[2];
        let indexes = title[3];
        let len_in_pts = u32::from_be_bytes([title[4], title[5], title[6], title[7]]);
        let track_sector_table_offset = u16::from_be_bytes([title[0x0C], title[0x0D]]) as usize;

        let timestamps_off = title_off + ATS_TITLE_SIZE;
        let sectors_off = title_off + track_sector_table_offset;
        let mut chapters = Vec::with_capacity(tracks as usize);

        for j in 0..tracks as usize {
            let ts_off = timestamps_off + j * ATS_TRACK_TIMESTAMP_SIZE;
            let ts = slice(pgcit_data, ts_off, ATS_TRACK_TIMESTAMP_SIZE, format!("ats_track_timestamp[{i}][{j}]"))?;
            let downmix_matrix_raw = ts[1];
            let downmix_matrix = (downmix_matrix_raw < DOWNMIX_COUNT as u8).then_some(downmix_matrix_raw);
            let track_type = ts[0];
            let track_type_low_bits_candidate = track_type_low_bits_candidate(track_type);
            chapters.push(AudioChapter {
                track_nr: (j + 1).min(u8::MAX as usize) as u8,
                track_type,
                track_type_low_bits_candidate,
                downmix_matrix,
                index_start: ts[4],
                first_pts: u32::from_be_bytes([ts[6], ts[7], ts[8], ts[9]]),
                len_in_pts: u32::from_be_bytes([ts[10], ts[11], ts[12], ts[13]]),
                sector_ranges: Vec::new(),
            });
        }

        for j in 0..indexes as usize {
            let sector_off = sectors_off + j * ATS_TRACK_SECTOR_SIZE;
            let sector = slice(pgcit_data, sector_off, ATS_TRACK_SECTOR_SIZE, format!("ats_track_sector[{i}][{j}]"))?;
            let range = SectorRange {
                index_nr: (j + 1).min(u8::MAX as usize) as u8,
                first: u32::from_be_bytes([sector[4], sector[5], sector[6], sector[7]]),
                last: u32::from_be_bytes([sector[8], sector[9], sector[10], sector[11]]),
            };
            assign_sector_range(&mut chapters, range);
        }

        let track_type_low_bits_candidates = distinct_track_type_low_bits_candidates(&chapters);
        let uniform_track_type_low_bits_candidate = if track_type_low_bits_candidates.len() == 1 {
            Some(track_type_low_bits_candidates[0])
        } else {
            None
        };

        titles.push(AudioTitle {
            title_set_nr,
            title_nr,
            title_ordinal: (i + 1).min(u8::MAX as usize) as u8,
            title_table_offset,
            uniform_track_type_low_bits_candidate,
            track_type_low_bits_candidates,
            track_count_declared: tracks,
            index_count_declared: indexes,
            len_in_pts,
            chapters,
        });
    }

    Ok((pgcit_offset, titles, diagnostics))
}


fn distinct_track_type_low_bits_candidates(chapters: &[AudioChapter]) -> Vec<u8> {
    let mut out = Vec::new();
    for chapter in chapters {
        let candidate = chapter.track_type_low_bits_candidate;
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

fn assign_sector_range(chapters: &mut [AudioChapter], range: SectorRange) {
    for k in 0..chapters.len() {
        let curr = chapters[k].index_start;
        let next = chapters.get(k + 1).map(|ch| ch.index_start).unwrap_or(0);
        if range.index_nr >= curr && (range.index_nr < next || next == 0) {
            chapters[k].sector_ranges.push(range.clone());
        }
    }
}
