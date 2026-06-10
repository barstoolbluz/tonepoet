#![forbid(unsafe_code)]

use crate::tui::dvda::endian::{ascii_trim_nul, be_u16, be_u32, be_u64, identifier, require_len, sector_to_offset, slice, u8_at};
use crate::tui::dvda::error::{DvdaError, Result};
use crate::tui::dvda::model::{AmgInfo, AmgPointers, AudioPlaybackType, AudioTitleTableEntry};

const AMGI_MAT_MIN_SIZE: usize = 0xE0;
const AMGI_IDENTIFIER: &str = "DVDAUDIO-AMG";
const AOTT_ENTRY_SIZE: usize = 14;

pub fn parse_amg(bytes: &[u8], source_file: &str) -> Result<AmgInfo> {
    require_len(bytes, AMGI_MAT_MIN_SIZE, format!("{source_file} AMG header"))?;
    let got = identifier(bytes, 0, 12, "AMG identifier")?;
    if got != AMGI_IDENTIFIER {
        return Err(DvdaError::InvalidIdentifier {
            file: source_file.to_string(),
            expected: AMGI_IDENTIFIER,
            got,
        });
    }

    let pointers = AmgPointers {
        amg_asvs: be_u32(bytes, 0x30, "amg_asvs")?,
        amgm_vobs: be_u32(bytes, 0xC0, "amgm_vobs")?,
        att_srpt: be_u32(bytes, 0xC4, "att_srpt")?,
        aott_srpt: be_u32(bytes, 0xC8, "aott_srpt")?,
        amgm_pgci_ut: be_u32(bytes, 0xCC, "amgm_pgci_ut")?,
        ats_atrt: be_u32(bytes, 0xD0, "ats_atrt")?,
        txtdt_mgi: be_u32(bytes, 0xD4, "txtdt_mgi")?,
        amgm_c_adt: be_u32(bytes, 0xD8, "amgm_c_adt")?,
        amgm_vobu_admap: be_u32(bytes, 0xDC, "amgm_vobu_admap")?,
    };

    let provider_identifier = ascii_trim_nul(slice(bytes, 0x40, 32, "provider_identifier")?);
    let audio_title_table = parse_aott_srpt(bytes, pointers.aott_srpt)?;

    Ok(AmgInfo {
        source_file: source_file.to_string(),
        last_sector: be_u32(bytes, 0x0C, "amg_last_sector")?,
        ifo_last_sector: be_u32(bytes, 0x1C, "amgi_last_sector")?,
        specification_version: u8_at(bytes, 0x21, "specification_version")?,
        category: be_u32(bytes, 0x22, "amg_category")?,
        nr_of_volumes: be_u16(bytes, 0x26, "amg_nr_of_volumes")?,
        this_volume_nr: be_u16(bytes, 0x28, "amg_this_volume_nr")?,
        disc_side: u8_at(bytes, 0x2A, "disc_side")?,
        audio_title_sets: u8_at(bytes, 0x3F, "amg_nr_of_audio_title_sets")?,
        video_title_sets: u8_at(bytes, 0x3E, "amg_nr_of_video_title_sets")?,
        provider_identifier,
        position_code: be_u64(bytes, 0x60, "amg_pos_code")?,
        ifo_last_byte: be_u32(bytes, 0x80, "amgi_last_byte")?,
        first_play_pgc: be_u32(bytes, 0x84, "first_play_pgc")?,
        pointers,
        audio_title_table,
    })
}

pub fn parse_aott_srpt(bytes: &[u8], sector: u32) -> Result<Vec<AudioTitleTableEntry>> {
    if sector == 0 {
        return Ok(Vec::new());
    }
    let base = sector_to_offset(sector)?;
    let header = slice(bytes, base, 4, "AOTT_SRPT header")?;
    let nr_of_srpts = u16::from_be_bytes([header[0], header[1]]);
    let last_byte = u16::from_be_bytes([header[2], header[3]]) as usize;
    let table_len = last_byte.saturating_add(1);
    let table = slice(bytes, base, table_len, "AOTT_SRPT table")?;
    let mut entries = Vec::with_capacity(nr_of_srpts as usize);
    let mut off = 4usize;
    for ordinal in 1..=nr_of_srpts {
        if off + AOTT_ENTRY_SIZE > table.len() {
            return Err(DvdaError::bounds("AOTT_SRPT entry", base + off, AOTT_ENTRY_SIZE, bytes.len()));
        }
        let entry = &table[off..off + AOTT_ENTRY_SIZE];
        let pb_raw = entry[0];
        let playback_type = AudioPlaybackType {
            title_set_nr: pb_raw & 0x0f,
            type_ext: (pb_raw >> 4) & 0x07,
            is_audio: (pb_raw & 0x80) != 0,
            raw: pb_raw,
        };
        entries.push(AudioTitleTableEntry {
            ordinal,
            playback_type,
            track_count: entry[1],
            len_in_pts: u32::from_be_bytes([entry[4], entry[5], entry[6], entry[7]]),
            title_set_nr: entry[8],
            title_nr: entry[9],
            atsi_mat_sector: u32::from_be_bytes([entry[10], entry[11], entry[12], entry[13]]),
        });
        off += AOTT_ENTRY_SIZE;
    }
    Ok(entries)
}
