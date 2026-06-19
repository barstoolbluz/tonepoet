#![cfg(feature = "iso-isomage")]
#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use dvda_demuxer::tui::dvda::{parse_dvda_volume, DvdaDisc, IsoDvdaVolume};

#[derive(Clone, Debug)]
struct IsoExpectation {
    name: &'static str,
    ats_count: usize,
    cppm: bool,
    groups_min: usize,
    aott_entries_min: usize,
    titles_min: usize,
    tracks_min: usize,
    known_title_numbers: &'static [u8],
}

const ISO_FIXTURES: &[IsoExpectation] = &[
    IsoExpectation { name: "hdad2009", ats_count: 1, cppm: false, groups_min: 1, aott_entries_min: 1, titles_min: 2, tracks_min: 5, known_title_numbers: &[129, 130] },
    IsoExpectation { name: "ap_i_robot", ats_count: 1, cppm: false, groups_min: 1, aott_entries_min: 1, titles_min: 2, tracks_min: 10, known_title_numbers: &[] },
    IsoExpectation { name: "ap_friendly_card", ats_count: 1, cppm: false, groups_min: 1, aott_entries_min: 1, titles_min: 2, tracks_min: 10, known_title_numbers: &[] },
    IsoExpectation { name: "ap_eye_in_the_sky", ats_count: 1, cppm: false, groups_min: 1, aott_entries_min: 1, titles_min: 2, tracks_min: 10, known_title_numbers: &[] },
    IsoExpectation { name: "mgletsgetiton", ats_count: 1, cppm: true, groups_min: 4, aott_entries_min: 4, titles_min: 6, tracks_min: 29, known_title_numbers: &[129, 130, 131, 132, 133, 134] },
    IsoExpectation { name: "hawks_and_doves", ats_count: 2, cppm: true, groups_min: 1, aott_entries_min: 1, titles_min: 1, tracks_min: 9, known_title_numbers: &[] },
    IsoExpectation { name: "talking_heads_77", ats_count: 2, cppm: true, groups_min: 2, aott_entries_min: 2, titles_min: 3, tracks_min: 27, known_title_numbers: &[1, 129] },
];

#[test]
fn parses_phase0_iso_images_with_isomage_udf_backend() {
    let Some(root) = iso_root() else {
        eprintln!("skipping ISO/UDF validation; set DVDA_PHASE1_ISO_ROOT to a directory containing <fixture-name>.iso files");
        return;
    };

    for fixture in ISO_FIXTURES {
        let iso_path = find_iso(&root, fixture.name)
            .unwrap_or_else(|| panic!("missing ISO for fixture {} under {}", fixture.name, root.display()));
        let volume = IsoDvdaVolume::new(&iso_path)
            .unwrap_or_else(|err| panic!("failed to mount ISO {}: {err:#}", iso_path.display()));
        let disc = parse_dvda_volume(&volume)
            .unwrap_or_else(|err| panic!("failed to parse ISO {}: {err:#}", iso_path.display()));
        assert_iso_disc(fixture, &disc);
    }
}

fn iso_root() -> Option<PathBuf> {
    std::env::var_os("DVDA_PHASE1_ISO_ROOT").map(PathBuf::from)
}

fn find_iso(root: &Path, fixture_name: &str) -> Option<PathBuf> {
    let candidates = [
        root.join(format!("{fixture_name}.iso")),
        root.join(format!("{fixture_name}.ISO")),
        root.join(fixture_name).join(format!("{fixture_name}.iso")),
        root.join(fixture_name).join(format!("{fixture_name}.ISO")),
    ];
    candidates.into_iter().find(|path| path.is_file())
}

fn assert_iso_disc(fixture: &IsoExpectation, disc: &DvdaDisc) {
    assert_eq!(disc.title_sets.len(), fixture.ats_count, "{} ATS count", fixture.name);
    assert_eq!(disc.copy_protection.cppm_detected, fixture.cppm, "{} CPPM detection", fixture.name);
    assert!(disc.groups.len() >= fixture.groups_min, "{} group count", fixture.name);
    assert!(disc.amg.audio_title_table.len() >= fixture.aott_entries_min, "{} AOTT count", fixture.name);
    assert!(disc.title_count() >= fixture.titles_min, "{} title count", fixture.name);
    assert!(disc.track_count_from_atsi() >= fixture.tracks_min, "{} ATSI track count", fixture.name);

    let title_numbers: Vec<u8> = disc
        .title_sets
        .iter()
        .flat_map(|ts| ts.titles.iter().map(|title| title.title_nr))
        .collect();
    for wanted in fixture.known_title_numbers {
        assert!(title_numbers.contains(wanted), "{} missing title number {wanted}", fixture.name);
    }

    for entry in &disc.amg.audio_title_table {
        assert!(entry.playback_type.is_audio, "{} AOTT entry {} is not audio", fixture.name, entry.ordinal);
        assert!(entry.track_count > 0, "{} AOTT entry {} has no tracks", fixture.name, entry.ordinal);
        assert!(entry.len_in_pts > 0, "{} AOTT entry {} has no duration", fixture.name, entry.ordinal);
        assert!(entry.atsi_mat_sector > 0, "{} AOTT entry {} has no ATSI sector", fixture.name, entry.ordinal);
    }

    for title in disc.title_sets.iter().flat_map(|ts| ts.titles.iter()) {
        assert!(
            !title.audio_format_indices.is_empty(),
            "{} ATS {} title {} did not expose active audio-format index",
            fixture.name,
            title.title_set_nr,
            title.title_nr
        );
    }

    if fixture.name == "mgletsgetiton" {
        let ats1 = disc.title_sets.iter().find(|ts| ts.number == 1).unwrap();
        let active_indices: BTreeSet<u8> = ats1
            .titles
            .iter()
            .flat_map(|title| title.audio_format_indices.iter().copied())
            .collect();
        assert!(active_indices.contains(&0), "MGLETSGETITON ISO should expose ATS 01 format 0");
        assert!(active_indices.contains(&2), "MGLETSGETITON ISO should expose ATS 01 format 2");
    }
}
