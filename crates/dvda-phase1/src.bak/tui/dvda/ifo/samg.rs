#![forbid(unsafe_code)]

use crate::tui::dvda::endian::{be_u16, be_u32, identifier, require_len, slice, u8_at};
use crate::tui::dvda::error::{DvdaError, Result};
use crate::tui::dvda::model::{
    channel_assignment, parse_channel_format, DvdaDiagnostic, SamgCopyValidation, SamgInfo,
    SamgTrack, SamgZone,
};

const SAMG_IDENTIFIER: &str = "DVDAUDIOSAPP";
const SAMG_MIN_SIZE: usize = 0x10;
const SAMG_TRACK_BASE: usize = 0x10;
const SAMG_TRACK_SIZE: usize = 52;
const SAMG_MAX_TRACKS: usize = 314;
const SAMG_COPY_SIZE: usize = 16 * 1024;
const SAMG_COPY_COUNT: usize = 8;
const SAMG_EXPECTED_SIZE: usize = SAMG_COPY_SIZE * SAMG_COPY_COUNT;

pub fn parse_samg(bytes: &[u8], source_file: &str) -> Result<SamgInfo> {
    require_len(bytes, SAMG_MIN_SIZE, format!("{source_file} SAMG header"))?;
    let got = identifier(bytes, 0, 12, "SAMG identifier")?;
    if got != SAMG_IDENTIFIER {
        return Err(DvdaError::InvalidIdentifier {
            file: source_file.to_string(),
            expected: SAMG_IDENTIFIER,
            got,
        });
    }

    let mut diagnostics = validate_samg_container(bytes, source_file);
    let declared = be_u16(bytes, 0x0C, "samg_nr_of_tracks")?;
    let specification_version = u8_at(bytes, 0x0F, "specification_version")?;
    let count = (declared as usize).min(SAMG_MAX_TRACKS);
    if declared as usize > SAMG_MAX_TRACKS {
        diagnostics.push(DvdaDiagnostic::warn(
            "dvda.samg.too_many_tracks",
            format!(
                "{source_file}: SAMG declares {declared} tracks; parsing first {SAMG_MAX_TRACKS}"
            ),
        ));
    }

    let required_for_declared = SAMG_TRACK_BASE + count * SAMG_TRACK_SIZE;
    require_len(
        bytes,
        required_for_declared,
        format!("{source_file} declared SAMG track records"),
    )?;

    let mut tracks = Vec::with_capacity(count);
    for i in 0..count {
        let off = SAMG_TRACK_BASE + i * SAMG_TRACK_SIZE;
        let entry = slice(bytes, off, SAMG_TRACK_SIZE, format!("samg_track[{i}]"))?;
        let raw_channel = [entry[0x11], entry[0x12], entry[0x13]];
        let channel_format = parse_channel_format(raw_channel);
        let channel_assignment = channel_assignment(channel_format.assignment_code);
        let flags = entry[0x10];
        tracks.push(SamgTrack {
            ordinal: (i + 1).min(u16::MAX as usize) as u16,
            group_nr: entry[0x02],
            track_nr: entry[0x03],
            first_pts: be_u32(entry, 0x04, "samg first_pts")?,
            len_in_pts: be_u32(entry, 0x08, "samg len_in_pts")?,
            zone: if (flags & 0x20) == 0 { SamgZone::Aob } else { SamgZone::Vob },
            flags,
            channel_format,
            channel_assignment,
            abs_first_sector: be_u32(entry, 0x28, "samg abs_first_sect")?,
            abs_first_sector_dup: be_u32(entry, 0x2C, "samg abs_first_sect_dup")?,
            abs_last_sector: be_u32(entry, 0x30, "samg abs_last_sect")?,
        });
    }

    let copy_validations = validate_repeated_copies(bytes, source_file, &mut diagnostics);
    let repeated_copies_valid = copy_validations.len() == SAMG_COPY_COUNT - 1
        && copy_validations.iter().all(|copy| copy.matches_first_copy);

    Ok(SamgInfo {
        source_file: source_file.to_string(),
        specification_version,
        track_count_declared: declared,
        tracks,
        raw_len: bytes.len(),
        expected_len: SAMG_EXPECTED_SIZE,
        copy_size: SAMG_COPY_SIZE,
        copy_count: SAMG_COPY_COUNT as u8,
        repeated_copies_valid,
        copy_validations,
        diagnostics,
    })
}

fn validate_samg_container(bytes: &[u8], source_file: &str) -> Vec<DvdaDiagnostic> {
    let mut diagnostics = Vec::new();
    if bytes.len() != SAMG_EXPECTED_SIZE {
        diagnostics.push(DvdaDiagnostic::warn(
            "dvda.samg.unexpected_size",
            format!(
                "{source_file}: SAMG is {} bytes; expected {SAMG_EXPECTED_SIZE} bytes (8 copies of 16 KiB)",
                bytes.len()
            ),
        ));
    }
    diagnostics
}

fn validate_repeated_copies(
    bytes: &[u8],
    source_file: &str,
    diagnostics: &mut Vec<DvdaDiagnostic>,
) -> Vec<SamgCopyValidation> {
    if bytes.len() < SAMG_EXPECTED_SIZE {
        diagnostics.push(DvdaDiagnostic::info(
            "dvda.samg.copy_validation_skipped",
            format!(
                "{source_file}: SAMG repeated-copy validation skipped because fewer than {SAMG_EXPECTED_SIZE} bytes were supplied"
            ),
        ));
        return Vec::new();
    }

    let first_copy = &bytes[..SAMG_COPY_SIZE];
    let mut validations = Vec::with_capacity(SAMG_COPY_COUNT - 1);
    for copy_index in 1..SAMG_COPY_COUNT {
        let byte_start = copy_index * SAMG_COPY_SIZE;
        let copy = &bytes[byte_start..byte_start + SAMG_COPY_SIZE];
        let matches_first_copy = copy == first_copy;
        if !matches_first_copy {
            diagnostics.push(DvdaDiagnostic::warn(
                "dvda.samg.copy_mismatch",
                format!(
                    "{source_file}: SAMG copy {} at byte {byte_start} differs from copy 0",
                    copy_index
                ),
            ));
        }
        validations.push(SamgCopyValidation {
            copy_index: copy_index as u8,
            byte_start,
            matches_first_copy,
        });
    }
    validations
}
