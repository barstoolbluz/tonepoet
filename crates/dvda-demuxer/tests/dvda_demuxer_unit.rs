#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use dvda_demuxer::tui::dvda::ifo::amg::{parse_amg, parse_aott_srpt};
use dvda_demuxer::tui::dvda::ifo::atsi::parse_atsi;
use dvda_demuxer::tui::dvda::ifo::samg::parse_samg;
use dvda_demuxer::tui::dvda::sector::{build_aob_inventory, AobSectorReader};
use dvda_demuxer::tui::dvda::{
    bit_depth_from_code, channel_assignment, parse_channel_format, parse_dvda_volume,
    sample_rate_from_code, DirectoryDvdaVolume, DvdaError, DvdaVolume, SamgZone, DVD_BLOCK_SIZE,
};

const BLOCK: usize = DVD_BLOCK_SIZE as usize;

#[test]
fn parse_amg_and_aott_srpt_extracts_binary_fields_exactly() {
    let bytes = synthetic_amg_with_aott();

    let amg = parse_amg(&bytes, "AUDIO_TS.IFO").expect("AMG parses");
    assert_eq!(amg.audio_title_sets, 1);
    assert_eq!(amg.video_title_sets, 0);
    assert_eq!(amg.provider_identifier, "Tonepoet Test Disc");
    assert_eq!(amg.pointers.aott_srpt, 1);
    assert_eq!(amg.audio_title_table.len(), 2);

    let first = &amg.audio_title_table[0];
    assert_eq!(first.ordinal, 1);
    assert_eq!(first.playback_type.raw, 0x91);
    assert!(first.playback_type.is_audio);
    assert_eq!(first.playback_type.type_ext, 1);
    assert_eq!(first.playback_type.title_set_nr, 1);
    assert_eq!(first.track_count, 2);
    assert_eq!(first.len_in_pts, 180_000);
    assert_eq!(first.title_set_nr, 1);
    assert_eq!(first.title_nr, 129);
    assert_eq!(first.atsi_mat_sector, 32);

    let second = &amg.audio_title_table[1];
    assert_eq!(second.ordinal, 2);
    assert_eq!(second.playback_type.raw, 0x81);
    assert!(second.playback_type.is_audio);
    assert_eq!(second.playback_type.type_ext, 0);
    assert_eq!(second.playback_type.title_set_nr, 1);
    assert_eq!(second.track_count, 1);
    assert_eq!(second.len_in_pts, 90_000);
    assert_eq!(second.title_set_nr, 1);
    assert_eq!(second.title_nr, 130);
    assert_eq!(second.atsi_mat_sector, 64);

    let direct = parse_aott_srpt(&bytes, 1).expect("direct AOTT parse");
    assert_eq!(direct, amg.audio_title_table);
}

#[test]
fn parse_amg_rejects_bad_identifier_and_short_aott() {
    let mut bytes = synthetic_amg_with_aott();
    bytes[0] = b'X';
    let err = parse_amg(&bytes, "AUDIO_TS.IFO").unwrap_err();
    assert!(matches!(err, DvdaError::InvalidIdentifier { .. }));

    let short = vec![0u8; BLOCK + 3];
    let err = parse_aott_srpt(&short, 1).unwrap_err();
    assert!(matches!(err, DvdaError::OutOfBounds { .. }));
}

#[test]
fn parse_atsi_extracts_audio_formats_titles_tracks_and_sector_assignment() {
    let bytes = synthetic_atsi_two_track_title();
    let title_set = parse_atsi(&bytes, "ATS_01_0.IFO", 1, Vec::new()).expect("ATSI parses");

    assert_eq!(title_set.number, 1);
    assert_eq!(title_set.kind.to_string_for_test(), "audio");
    assert_eq!(title_set.header.atsm_vobs, 0);
    assert_eq!(title_set.header.ats_pgcit, 1);
    assert_eq!(title_set.audio_pgcit_offset, BLOCK);
    assert_eq!(title_set.aobs_last_sector, Some(996));
    assert_eq!(title_set.downmix_matrices.len(), 14);
    let matrix0 = &title_set.downmix_matrices[0];
    assert_eq!(matrix0.raw[0], 0x80);
    assert_eq!(matrix0.raw[1], 0x40);
    assert_eq!(matrix0.phase.left_mask, 0x80);
    assert_eq!(matrix0.phase.right_mask, 0x40);
    assert_eq!(matrix0.left_coefficient(0).unwrap().raw, 10);
    assert!(matrix0.left_coefficient(0).unwrap().inverse_phase);
    assert_eq!(matrix0.right_coefficient(0).unwrap().raw, 11);
    assert!(!matrix0.right_coefficient(0).unwrap().inverse_phase);
    assert_eq!(matrix0.left_coefficient(1).unwrap().raw, 200);
    assert!(!matrix0.left_coefficient(1).unwrap().inverse_phase);
    assert_eq!(matrix0.right_coefficient(1).unwrap().raw, 255);
    assert!(matrix0.right_coefficient(1).unwrap().inverse_phase);
    assert!(matrix0.left_coefficient(1).unwrap().attenuation_db().unwrap() < -40.0);
    assert!(matrix0.right_coefficient(1).unwrap().attenuation_db().is_none());

    let format0 = &title_set.audio_formats[0];
    assert!(format0.present);
    assert_eq!(format0.audio_type_raw, 0x0100);
    assert_eq!(format0.channel_format.group1_bits, Some(24));
    assert_eq!(format0.channel_format.group2_bits, Some(24));
    assert_eq!(format0.channel_format.group1_sample_rate, Some(192_000));
    assert_eq!(format0.channel_format.group2_sample_rate, Some(96_000));
    assert_eq!(format0.channel_format.assignment_code, 20);
    let assignment = format0.channel_assignment.as_ref().expect("assignment 20");
    assert_eq!(assignment.group1, &["L", "R", "Ls", "Rs"]);
    assert_eq!(assignment.group2, &["C", "LFE"]);
    assert_eq!(assignment.group1_channels, 4);
    assert_eq!(assignment.group2_channels, 2);

    assert_eq!(title_set.titles.len(), 1);
    let title = &title_set.titles[0];
    assert_eq!(title.title_set_nr, 1);
    assert_eq!(title.title_nr, 129);
    assert_eq!(title.uniform_track_type_low_bits_candidate, Some(0));
    assert_eq!(title.track_type_low_bits_candidates, vec![0]);
    assert_eq!(title.track_count_declared, 2);
    assert_eq!(title.index_count_declared, 3);
    assert_eq!(title.len_in_pts, 270_000);
    assert_eq!(title.chapters.len(), 2);

    let track1 = &title.chapters[0];
    assert_eq!(track1.track_nr, 1);
    assert_eq!(track1.track_type, 0);
    assert_eq!(track1.track_type_low_bits_candidate, 0);
    assert_eq!(track1.downmix_matrix, Some(1));
    assert_eq!(track1.index_start, 1);
    assert_eq!(track1.first_pts, 0);
    assert_eq!(track1.len_in_pts, 180_000);
    assert_eq!(track1.sector_ranges.len(), 2);
    assert_eq!(track1.first_sector(), Some(10));
    assert_eq!(track1.last_sector(), Some(29));

    let track2 = &title.chapters[1];
    assert_eq!(track2.track_nr, 2);
    assert_eq!(track2.track_type, 0);
    assert_eq!(track2.track_type_low_bits_candidate, 0);
    assert_eq!(track2.downmix_matrix, Some(2));
    assert_eq!(track2.index_start, 3);
    assert_eq!(track2.first_pts, 180_000);
    assert_eq!(track2.len_in_pts, 90_000);
    assert_eq!(track2.sector_ranges.len(), 1);
    assert_eq!(track2.first_sector(), Some(30));
    assert_eq!(track2.last_sector(), Some(39));
}

#[test]
fn parse_atsi_exposes_per_title_and_per_chapter_audio_format_indices() {
    let bytes = synthetic_atsi_multi_format_titles();
    let title_set = parse_atsi(&bytes, "ATS_01_0.IFO", 1, Vec::new()).expect("ATSI parses");

    assert_eq!(title_set.audio_formats.len(), 8);
    assert!(title_set.audio_formats[0].present);
    assert_eq!(title_set.audio_formats[0].channel_format.group1_sample_rate, Some(96_000));
    assert_eq!(title_set.audio_formats[0].channel_assignment.as_ref().unwrap().group1_channels, 4);
    assert_eq!(title_set.audio_formats[0].channel_assignment.as_ref().unwrap().group2_channels, 2);
    assert!(title_set.audio_formats[2].present);
    assert_eq!(title_set.audio_formats[2].channel_format.group1_sample_rate, Some(192_000));
    assert_eq!(title_set.audio_formats[2].channel_assignment.as_ref().unwrap().group1_channels, 2);

    assert_eq!(title_set.titles.len(), 2);
    let multichannel = &title_set.titles[0];
    assert_eq!(multichannel.title_nr, 129);
    assert_eq!(multichannel.uniform_track_type_low_bits_candidate, Some(0));
    assert_eq!(multichannel.track_type_low_bits_candidates, vec![0]);
    assert_eq!(multichannel.chapters[0].track_type, 0);
    assert_eq!(multichannel.chapters[0].track_type_low_bits_candidate, 0);

    let stereo = &title_set.titles[1];
    assert_eq!(stereo.title_nr, 130);
    assert_eq!(stereo.uniform_track_type_low_bits_candidate, Some(2));
    assert_eq!(stereo.track_type_low_bits_candidates, vec![2]);
    assert_eq!(stereo.chapters[0].track_type, 2);
    assert_eq!(stereo.chapters[0].track_type_low_bits_candidate, 2);

    let active_indices: Vec<u8> = title_set
        .titles
        .iter()
        .flat_map(|title| title.track_type_low_bits_candidates.iter().copied())
        .collect();
    assert_eq!(active_indices, vec![0, 2]);
}

#[test]
fn parse_samg_extracts_absolute_sectors_zone_and_channel_format() {
    let bytes = synthetic_samg_with_tracks(2);
    let samg = parse_samg(&bytes, "AUDIO_PP.IFO").expect("SAMG parses");

    assert_eq!(samg.source_file, "AUDIO_PP.IFO");
    assert_eq!(samg.specification_version, 0x11);
    assert_eq!(samg.track_count_declared, 2);
    assert_eq!(samg.tracks.len(), 2);
    assert_eq!(samg.raw_len, bytes.len());
    assert_eq!(samg.expected_len, 128 * 1024);
    assert_eq!(samg.copy_size, 16 * 1024);
    assert_eq!(samg.copy_count, 8);
    assert!(!samg.repeated_copies_valid);
    assert!(samg.copy_validations.is_empty());
    assert!(samg.diagnostics.iter().any(|diag| diag.code == "dvda.samg.unexpected_size"));
    assert!(samg.diagnostics.iter().any(|diag| diag.code == "dvda.samg.copy_validation_skipped"));

    let first = &samg.tracks[0];
    assert_eq!(first.ordinal, 1);
    assert_eq!(first.group_nr, 1);
    assert_eq!(first.track_nr, 1);
    assert_eq!(first.zone, SamgZone::Aob);
    assert_eq!(first.channel_format.group1_sample_rate, Some(96_000));
    assert_eq!(first.channel_format.group2_sample_rate, Some(96_000));
    assert_eq!(first.channel_assignment.as_ref().unwrap().group1_channels, 2);
    assert_eq!(first.abs_first_sector, 1_000);
    assert_eq!(first.abs_first_sector_dup, 1_000);
    assert_eq!(first.abs_last_sector, 1_099);

    let second = &samg.tracks[1];
    assert_eq!(second.ordinal, 2);
    assert_eq!(second.group_nr, 1);
    assert_eq!(second.track_nr, 2);
    assert_eq!(second.zone, SamgZone::Vob);
    assert_eq!(second.flags & 0x20, 0x20);
    assert_eq!(second.abs_first_sector, 1_100);
    assert_eq!(second.abs_last_sector, 1_199);
}

#[test]
fn parse_samg_validates_full_repeated_copy_structure_without_rejecting_short_fixtures() {
    let bytes = synthetic_full_samg_repeated_copies(2, None);
    let samg = parse_samg(&bytes, "AUDIO_PP.IFO").expect("full repeated SAMG parses");

    assert_eq!(samg.raw_len, 128 * 1024);
    assert!(samg.repeated_copies_valid);
    assert_eq!(samg.copy_validations.len(), 7);
    assert!(samg.copy_validations.iter().all(|copy| copy.matches_first_copy));
    assert!(!samg.diagnostics.iter().any(|diag| diag.code == "dvda.samg.copy_mismatch"));

    let mismatched = synthetic_full_samg_repeated_copies(2, Some(3));
    let samg = parse_samg(&mismatched, "AUDIO_PP.IFO").expect("mismatched repeated SAMG still parses");
    assert!(!samg.repeated_copies_valid);
    assert_eq!(samg.copy_validations[2].copy_index, 3);
    assert!(!samg.copy_validations[2].matches_first_copy);
    assert!(samg.diagnostics.iter().any(|diag| diag.code == "dvda.samg.copy_mismatch"));
}

#[test]
fn parse_atsi_uses_header_pgcit_sector_or_reference_0x800_fallback() {
    let explicit = synthetic_atsi_two_track_title();
    let parsed = parse_atsi(&explicit, "ATS_01_0.IFO", 1, Vec::new()).expect("explicit PGCIT pointer parses");
    assert_eq!(parsed.header.ats_pgcit, 1);
    assert_eq!(parsed.audio_pgcit_offset, 0x800);

    let mut fallback = explicit;
    put_be32(&mut fallback, 0xcc, 0);
    let parsed = parse_atsi(&fallback, "ATS_01_0.IFO", 1, Vec::new()).expect("0 PGCIT pointer falls back");
    assert_eq!(parsed.header.ats_pgcit, 0);
    assert_eq!(parsed.audio_pgcit_offset, 0x800);
    assert_eq!(parsed.titles.len(), 1);
}

#[test]
fn channel_assignment_and_rate_depth_tables_match_reference_values() {
    assert_eq!(bit_depth_from_code(0), Some(16));
    assert_eq!(bit_depth_from_code(1), Some(20));
    assert_eq!(bit_depth_from_code(2), Some(24));
    assert_eq!(bit_depth_from_code(3), None);

    assert_eq!(sample_rate_from_code(0x00), Some(48_000));
    assert_eq!(sample_rate_from_code(0x01), Some(96_000));
    assert_eq!(sample_rate_from_code(0x02), Some(192_000));
    assert_eq!(sample_rate_from_code(0x08), Some(44_100));
    assert_eq!(sample_rate_from_code(0x09), Some(88_200));
    assert_eq!(sample_rate_from_code(0x0a), Some(176_400));
    assert_eq!(sample_rate_from_code(0x03), None);

    let assignment0 = channel_assignment(0).unwrap();
    assert_eq!(assignment0.group1, &["C"]);
    assert!(assignment0.group2.is_empty());

    let assignment12 = channel_assignment(12).unwrap();
    assert_eq!(assignment12.group1, &["L", "R"]);
    assert_eq!(assignment12.group2, &["C", "LFE", "Ls", "Rs"]);
    assert_eq!(assignment12.group1_channels + assignment12.group2_channels, 6);

    let assignment20 = channel_assignment(20).unwrap();
    assert_eq!(assignment20.group1, &["L", "R", "Ls", "Rs"]);
    assert_eq!(assignment20.group2, &["C", "LFE"]);
    assert_eq!(assignment20.group1_channels + assignment20.group2_channels, 6);
    assert!(channel_assignment(21).is_none());

    let fmt = parse_channel_format([0x21, 0x2a, 12]);
    assert_eq!(fmt.group1_bits, Some(24));
    assert_eq!(fmt.group2_bits, Some(20));
    assert_eq!(fmt.group1_sample_rate, Some(192_000));
    assert_eq!(fmt.group2_sample_rate, Some(176_400));
    assert_eq!(fmt.assignment_code, 12);
}

#[test]
fn directory_volume_aob_inventory_and_sector_reader_handle_boundary_crossing() {
    let temp = TempDir::new("dvda-aob-test");
    let audio_ts = temp.path().join("AUDIO_TS");
    fs::create_dir_all(&audio_ts).expect("create AUDIO_TS");

    let mut part1 = vec![0x11; BLOCK];
    part1.extend(std::iter::repeat(0x22).take(BLOCK));
    let part2 = vec![0x33; BLOCK];
    write_file(&audio_ts.join("ats_01_1.aob"), &part1);
    write_file(&audio_ts.join("ATS_01_2.AOB"), &part2);

    let volume = DirectoryDvdaVolume::new(temp.path());
    assert!(volume.exists_audio_ts_file("ATS_01_1.AOB"));
    assert_eq!(volume.file_len("ATS_01_1.AOB").unwrap(), Some((2 * BLOCK) as u64));
    assert_eq!(volume.file_len("ATS_01_9.AOB").unwrap(), None);

    let aobs = build_aob_inventory(&volume, 1).expect("AOB inventory");
    assert_eq!(aobs.len(), 9);
    assert!(aobs[0].exists);
    assert_eq!(aobs[0].block_first, 0);
    assert_eq!(aobs[0].block_last, 1);
    assert!(aobs[1].exists);
    assert_eq!(aobs[1].block_first, 2);
    assert_eq!(aobs[1].block_last, 2);
    assert!(!aobs[2].exists);
    assert_eq!(aobs[2].block_first, 3);

    let reader = AobSectorReader::new(&volume, &aobs);
    let crossed = reader.read_blocks(1, 2).expect("read across AOB boundary");
    assert_eq!(crossed.len(), 2 * BLOCK);
    assert!(crossed[..BLOCK].iter().all(|b| *b == 0x22));
    assert!(crossed[BLOCK..].iter().all(|b| *b == 0x33));

    let err = reader.read_blocks(3, 1).unwrap_err();
    assert!(matches!(err, DvdaError::Parse { .. }));
}

#[test]
fn directory_volume_falls_back_to_bup_and_rejects_path_escape_names() {
    let temp = TempDir::new("dvda-dir-volume-test");
    fs::create_dir_all(temp.path()).expect("create temp root");
    write_file(&temp.path().join("AUDIO_TS.BUP"), b"backup-ifo");

    let volume = DirectoryDvdaVolume::new(temp.path());
    let (source, bytes) = volume
        .read_with_backup("AUDIO_TS.IFO", "AUDIO_TS.BUP")
        .expect("BUP fallback");
    assert_eq!(source, "AUDIO_TS.BUP");
    assert_eq!(bytes, b"backup-ifo");

    let err = volume.read_audio_ts_file("../AUDIO_TS.IFO").unwrap_err();
    assert!(matches!(err, DvdaError::Parse { .. }));
}

#[test]
fn volume_parse_reports_samg_incomplete_relative_to_atsi() {
    let temp = TempDir::new("dvda-samg-incomplete-test");
    fs::create_dir_all(temp.path()).expect("create temp root");
    write_file(&temp.path().join("AUDIO_TS.IFO"), &synthetic_amg_with_aott());
    write_file(&temp.path().join("ATS_01_0.IFO"), &synthetic_atsi_two_track_title());
    write_file(&temp.path().join("AUDIO_PP.IFO"), &synthetic_samg_with_tracks(1));

    let disc = parse_dvda_volume(&DirectoryDvdaVolume::new(temp.path())).expect("disc parse");
    assert_eq!(disc.amg.audio_title_table.len(), 2);
    assert_eq!(disc.title_sets.len(), 1);
    assert_eq!(disc.track_count_from_atsi(), 2);
    assert_eq!(disc.samg.as_ref().unwrap().tracks.len(), 1);
    assert!(disc
        .diagnostics
        .iter()
        .any(|diag| diag.code == "dvda.samg.incomplete_relative_to_atsi"));
}

fn synthetic_amg_with_aott() -> Vec<u8> {
    let mut bytes = vec![0u8; 2 * BLOCK];
    bytes[0x00..0x0c].copy_from_slice(b"DVDAUDIO-AMG");
    put_be32(&mut bytes, 0x0c, 2_048);
    put_be32(&mut bytes, 0x1c, 1);
    bytes[0x21] = 0x11;
    put_be32(&mut bytes, 0x22, 0x0000_0001);
    put_be16(&mut bytes, 0x26, 1);
    put_be16(&mut bytes, 0x28, 1);
    bytes[0x2a] = 1;
    bytes[0x3e] = 0;
    bytes[0x3f] = 1;
    write_ascii(&mut bytes, 0x40, 32, "Tonepoet Test Disc");
    put_be64(&mut bytes, 0x60, 0x0102_0304_0506_0708);
    put_be32(&mut bytes, 0x80, 0x900);
    put_be32(&mut bytes, 0x84, 0);
    put_be32(&mut bytes, 0xc8, 1);

    let base = BLOCK;
    put_be16(&mut bytes, base, 2);
    put_be16(&mut bytes, base + 2, (4 + 2 * 14 - 1) as u16);
    write_aott_entry(&mut bytes, base + 4, 0x91, 2, 180_000, 1, 129, 32);
    write_aott_entry(&mut bytes, base + 18, 0x81, 1, 90_000, 1, 130, 64);
    bytes
}

fn synthetic_atsi_two_track_title() -> Vec<u8> {
    let mut bytes = vec![0u8; 0x1000];
    bytes[0x00..0x0c].copy_from_slice(b"DVDAUDIO-ATS");
    put_be32(&mut bytes, 0x0c, 1_000);
    put_be32(&mut bytes, 0x1c, 1);
    bytes[0x21] = 0x11;
    put_be32(&mut bytes, 0x22, 0x0000_0001);
    put_be32(&mut bytes, 0xc0, 0);
    put_be32(&mut bytes, 0xc4, 2);
    put_be32(&mut bytes, 0xc8, 0);
    put_be32(&mut bytes, 0xcc, 1);
    put_be32(&mut bytes, 0xe0, 0);
    put_be32(&mut bytes, 0xe4, 0);

    put_be16(&mut bytes, 0x100, 0x0100);
    bytes[0x102] = 0x22; // group1/group2 24-bit
    bytes[0x103] = 0x21; // group1 192k, group2 96k
    bytes[0x104] = 20;   // 4+2 channel assignment

    // Downmix matrix 0: typed phase and coefficient decoding fixture.
    bytes[0x180] = 0x80; // source channel 0 inverted for L
    bytes[0x181] = 0x40; // source channel 1 inverted for R
    bytes[0x182] = 10;
    bytes[0x183] = 11;
    bytes[0x184] = 200;
    bytes[0x185] = 255;

    let pgcit = BLOCK;
    put_be16(&mut bytes, pgcit, 1);
    put_be32(&mut bytes, pgcit + 4, 107); // through third 12-byte sector entry

    let idx = pgcit + 8;
    bytes[idx] = 129;
    put_be32(&mut bytes, idx + 4, 16);

    let title = pgcit + 16;
    bytes[title + 2] = 2; // tracks
    bytes[title + 3] = 3; // indexes
    put_be32(&mut bytes, title + 4, 270_000);
    put_be16(&mut bytes, title + 0x0c, 56);

    let ts1 = title + 16;
    bytes[ts1] = 0;
    bytes[ts1 + 1] = 1;
    bytes[ts1 + 4] = 1;
    put_be32(&mut bytes, ts1 + 6, 0);
    put_be32(&mut bytes, ts1 + 10, 180_000);

    let ts2 = ts1 + 20;
    bytes[ts2] = 0;
    bytes[ts2 + 1] = 2;
    bytes[ts2 + 4] = 3;
    put_be32(&mut bytes, ts2 + 6, 180_000);
    put_be32(&mut bytes, ts2 + 10, 90_000);

    let sectors = title + 56;
    write_sector_entry(&mut bytes, sectors, 10, 19);
    write_sector_entry(&mut bytes, sectors + 12, 20, 29);
    write_sector_entry(&mut bytes, sectors + 24, 30, 39);
    bytes
}

fn synthetic_atsi_multi_format_titles() -> Vec<u8> {
    let mut bytes = vec![0u8; 0x1000];
    bytes[0x00..0x0c].copy_from_slice(b"DVDAUDIO-ATS");
    put_be32(&mut bytes, 0x0c, 1_000);
    put_be32(&mut bytes, 0x1c, 1);
    bytes[0x21] = 0x11;
    put_be32(&mut bytes, 0x22, 0x0000_0001);
    put_be32(&mut bytes, 0xc0, 0);
    put_be32(&mut bytes, 0xc4, 2);
    put_be32(&mut bytes, 0xcc, 1);

    // Format 0: 96/24 4+2 multichannel.
    put_be16(&mut bytes, 0x100, 0x0100);
    bytes[0x102] = 0x22;
    bytes[0x103] = 0x11;
    bytes[0x104] = 20;

    // Format 2: 192/24 stereo. The second channel group is unused by assignment 1.
    let fmt2 = 0x100 + 2 * 16;
    put_be16(&mut bytes, fmt2, 0x0100);
    bytes[fmt2 + 2] = 0x20;
    bytes[fmt2 + 3] = 0x20;
    bytes[fmt2 + 4] = 1;

    let pgcit = BLOCK;
    put_be16(&mut bytes, pgcit, 2);
    put_be32(&mut bytes, pgcit + 4, 119);

    let idx1 = pgcit + 8;
    bytes[idx1] = 129;
    put_be32(&mut bytes, idx1 + 4, 24);

    let idx2 = pgcit + 16;
    bytes[idx2] = 130;
    put_be32(&mut bytes, idx2 + 4, 72);

    write_single_track_title(&mut bytes, pgcit + 24, 0, 90_000, 100, 199);
    write_single_track_title(&mut bytes, pgcit + 72, 2, 45_000, 200, 249);
    bytes
}

fn write_single_track_title(bytes: &mut [u8], title: usize, track_type: u8, len_pts: u32, first_sector: u32, last_sector: u32) {
    bytes[title + 2] = 1;
    bytes[title + 3] = 1;
    put_be32(bytes, title + 4, len_pts);
    put_be16(bytes, title + 0x0c, 36);

    let ts = title + 16;
    bytes[ts] = track_type;
    bytes[ts + 1] = 0;
    bytes[ts + 4] = 1;
    put_be32(bytes, ts + 6, 0);
    put_be32(bytes, ts + 10, len_pts);

    write_sector_entry(bytes, title + 36, first_sector, last_sector);
}

fn synthetic_samg_with_tracks(track_count: u16) -> Vec<u8> {
    let mut bytes = vec![0u8; 0x10 + (track_count as usize) * 52];
    write_samg_first_copy(&mut bytes, track_count);
    bytes
}

fn synthetic_full_samg_repeated_copies(track_count: u16, corrupt_copy: Option<usize>) -> Vec<u8> {
    let mut first_copy = vec![0u8; 16 * 1024];
    write_samg_first_copy(&mut first_copy, track_count);
    let mut bytes = Vec::with_capacity(128 * 1024);
    for copy in 0..8 {
        let mut next = first_copy.clone();
        if corrupt_copy == Some(copy) {
            next[0x40] ^= 0xff;
        }
        bytes.extend_from_slice(&next);
    }
    bytes
}

fn write_samg_first_copy(bytes: &mut [u8], track_count: u16) {
    bytes[0x00..0x0c].copy_from_slice(b"DVDAUDIOSAPP");
    put_be16(bytes, 0x0c, track_count);
    bytes[0x0f] = 0x11;
    for i in 0..track_count as usize {
        let off = 0x10 + i * 52;
        bytes[off + 0x02] = 1;
        bytes[off + 0x03] = (i + 1) as u8;
        put_be32(bytes, off + 0x04, (i as u32) * 90_000);
        put_be32(bytes, off + 0x08, 90_000);
        bytes[off + 0x10] = if i == 1 { 0x20 } else { 0x00 };
        bytes[off + 0x11] = 0x22;
        bytes[off + 0x12] = 0x11;
        bytes[off + 0x13] = 1;
        put_be32(bytes, off + 0x28, 1_000 + (i as u32) * 100);
        put_be32(bytes, off + 0x2c, 1_000 + (i as u32) * 100);
        put_be32(bytes, off + 0x30, 1_099 + (i as u32) * 100);
    }
}

fn write_aott_entry(bytes: &mut [u8], off: usize, pb_raw: u8, tracks: u8, len_pts: u32, ats: u8, title: u8, atsi_sector: u32) {
    bytes[off] = pb_raw;
    bytes[off + 1] = tracks;
    put_be32(bytes, off + 4, len_pts);
    bytes[off + 8] = ats;
    bytes[off + 9] = title;
    put_be32(bytes, off + 10, atsi_sector);
}

fn write_sector_entry(bytes: &mut [u8], off: usize, first: u32, last: u32) {
    put_be32(bytes, off + 4, first);
    put_be32(bytes, off + 8, last);
}

fn write_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap_or_else(|err| panic!("failed to write {}: {err}", path.display()));
}

fn write_ascii(bytes: &mut [u8], off: usize, len: usize, value: &str) {
    let dst = &mut bytes[off..off + len];
    dst.fill(0);
    let src = value.as_bytes();
    let n = src.len().min(len);
    dst[..n].copy_from_slice(&src[..n]);
}

fn put_be16(bytes: &mut [u8], off: usize, value: u16) {
    bytes[off..off + 2].copy_from_slice(&value.to_be_bytes());
}

fn put_be32(bytes: &mut [u8], off: usize, value: u32) {
    bytes[off..off + 4].copy_from_slice(&value.to_be_bytes());
}

fn put_be64(bytes: &mut [u8], off: usize, value: u64) {
    bytes[off..off + 8].copy_from_slice(&value.to_be_bytes());
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).unwrap_or_else(|err| panic!("failed to create {}: {err}", path.display()));
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

trait TitleSetKindForTest {
    fn to_string_for_test(&self) -> &'static str;
}

impl TitleSetKindForTest for dvda_demuxer::tui::dvda::TitleSetKind {
    fn to_string_for_test(&self) -> &'static str {
        match self {
            dvda_demuxer::tui::dvda::TitleSetKind::Audio => "audio",
            dvda_demuxer::tui::dvda::TitleSetKind::Video => "video",
            dvda_demuxer::tui::dvda::TitleSetKind::Unknown => "unknown",
        }
    }
}
