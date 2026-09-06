#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use dvda_demuxer::tui::dvda::{parse_dvda_volume, DirectoryDvdaVolume};

#[derive(Clone, Debug)]
struct FixtureExpectation {
    name: &'static str,
    ats_count: usize,
    cppm: bool,
    groups_min: usize,
    aott_entries_min: usize,
    titles_min: usize,
    tracks_min: usize,
    known_title_numbers: &'static [u8],
}

const FIXTURES: &[FixtureExpectation] = &[
    FixtureExpectation {
        name: "hdad2009",
        ats_count: 1,
        cppm: false,
        groups_min: 1,
        aott_entries_min: 1,
        titles_min: 2,
        tracks_min: 5,
        known_title_numbers: &[129, 130],
    },
    FixtureExpectation {
        name: "ap_i_robot",
        ats_count: 1,
        cppm: false,
        groups_min: 1,
        aott_entries_min: 1,
        titles_min: 2,
        tracks_min: 10,
        known_title_numbers: &[],
    },
    FixtureExpectation {
        name: "ap_friendly_card",
        ats_count: 1,
        cppm: false,
        groups_min: 1,
        aott_entries_min: 1,
        titles_min: 2,
        tracks_min: 10,
        known_title_numbers: &[],
    },
    FixtureExpectation {
        name: "ap_eye_in_the_sky",
        ats_count: 1,
        cppm: false,
        groups_min: 1,
        aott_entries_min: 1,
        titles_min: 2,
        tracks_min: 10,
        known_title_numbers: &[],
    },
    FixtureExpectation {
        name: "mgletsgetiton",
        ats_count: 1,
        cppm: true,
        groups_min: 4,
        aott_entries_min: 4,
        titles_min: 6,
        tracks_min: 29,
        known_title_numbers: &[129, 130, 131, 132, 133, 134],
    },
    FixtureExpectation {
        name: "hawks_and_doves",
        ats_count: 2,
        cppm: true,
        groups_min: 1,
        aott_entries_min: 1,
        titles_min: 1,
        tracks_min: 9,
        known_title_numbers: &[],
    },
    FixtureExpectation {
        name: "talking_heads_77",
        ats_count: 2,
        cppm: true,
        groups_min: 2,
        aott_entries_min: 2,
        titles_min: 3,
        tracks_min: 27,
        known_title_numbers: &[1, 129],
    },
];

#[test]
fn parses_phase0_fixture_directories() {
    let root = fixture_root();
    if !root.is_dir() {
        eprintln!("skipping DVD-A fixture test; {} is absent", root.display());
        return;
    }

    for fixture in FIXTURES {
        let path = root.join(fixture.name);
        assert!(path.is_dir(), "fixture directory missing: {}", path.display());
        let volume = DirectoryDvdaVolume::new(&path);
        let disc = parse_dvda_volume(&volume).unwrap_or_else(|err| {
            panic!("failed to parse fixture {} at {}: {err:#}", fixture.name, path.display())
        });

        assert_eq!(disc.title_sets.len(), fixture.ats_count, "{} ATS count", fixture.name);
        assert_eq!(disc.copy_protection.cppm_detected, fixture.cppm, "{} CPPM detection", fixture.name);
        assert!(disc.groups.len() >= fixture.groups_min, "{} group count", fixture.name);
        assert!(
            disc.amg.audio_title_table.len() >= fixture.aott_entries_min,
            "{} AOTT entry count: got {}, expected at least {}",
            fixture.name,
            disc.amg.audio_title_table.len(),
            fixture.aott_entries_min
        );
        assert_aott_entries_resolve(fixture.name, &disc);
        assert_samg_repeated_copies_are_valid(fixture.name, &disc);
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

        for ts in &disc.title_sets {
            // Skip video title sets — they don't have audio format entries
            if ts.kind != dvda_demuxer::tui::dvda::model::TitleSetKind::Audio {
                continue;
            }
            assert_eq!(
                ts.audio_pgcit_offset,
                0x800,
                "{} ATS {} should parse audio_pgcit_t at the reference foobar offset",
                fixture.name,
                ts.number
            );
            assert!(
                ts.header.ats_pgcit == 0 || ts.header.ats_pgcit == 1,
                "{} ATS {} has unexpected ats_pgcit sector {}; parser used byte offset {}",
                fixture.name,
                ts.number,
                ts.header.ats_pgcit,
                ts.audio_pgcit_offset
            );
            assert_eq!(ts.aobs.len(), 9, "{} ATS {} should inventory 9 AOB parts", fixture.name, ts.number);
            for title in &ts.titles {
                assert!(!title.chapters.is_empty(), "{} ATS {} title {} has no chapters/tracks", fixture.name, ts.number, title.title_nr);
                assert!(
                    !title.track_type_low_bits_candidates.is_empty(),
                    "{} ATS {} title {} did not expose an active audio-format index",
                    fixture.name,
                    ts.number,
                    title.title_nr
                );
                for chapter in &title.chapters {
                    assert!(!chapter.sector_ranges.is_empty(), "{} ATS {} title {} track {} has no sector ranges", fixture.name, ts.number, title.title_nr, chapter.track_nr);
                }
            }
        }

    }
}

fn assert_samg_repeated_copies_are_valid(fixture_name: &str, disc: &dvda_demuxer::tui::dvda::DvdaDisc) {
    let samg = disc
        .samg
        .as_ref()
        .unwrap_or_else(|| panic!("{fixture_name} should include AUDIO_PP.IFO/SAMG"));
    assert_eq!(
        samg.raw_len, samg.expected_len,
        "{fixture_name} SAMG should be a full 128 KiB repeated-copy structure"
    );
    assert_eq!(samg.copy_size, 16 * 1024, "{fixture_name} SAMG copy size");
    assert_eq!(samg.copy_count, 8, "{fixture_name} SAMG copy count");
    assert_eq!(samg.copy_validations.len(), 7, "{fixture_name} SAMG copy validations");
    assert!(
        samg.repeated_copies_valid,
        "{fixture_name} SAMG repeated copies should match copy 0"
    );
}

fn assert_aott_entries_resolve(fixture_name: &str, disc: &dvda_demuxer::tui::dvda::DvdaDisc) {
    let mut refs = BTreeSet::new();
    for entry in &disc.amg.audio_title_table {
        if !entry.playback_type.is_audio {
            // Null/terminator or video entries can appear in the AOTT. Skip them.
            continue;
        }
        // NOTE: The low nibble of playback_type.raw is the group/presentation
        // number, NOT the title set number. MGLETSGETITON has groups 1-4 all in
        // ATS 01. Do not assert nibble == title_set_nr.
        assert!(entry.track_count > 0, "{fixture_name} AOTT entry {} has zero track count", entry.ordinal);
        assert!(entry.len_in_pts > 0, "{fixture_name} AOTT entry {} has zero duration", entry.ordinal);
        assert!(entry.atsi_mat_sector > 0, "{fixture_name} AOTT entry {} has zero ATSI sector", entry.ordinal);
        assert!(
            refs.insert((entry.title_set_nr, entry.title_nr)),
            "{fixture_name} duplicate AOTT title reference ATS {} title {}",
            entry.title_set_nr,
            entry.title_nr
        );
        let resolved = disc.title_sets.iter().any(|ts| {
            ts.number == entry.title_set_nr && ts.titles.iter().any(|title| title.title_ordinal == entry.title_nr)
        });
        assert!(
            resolved,
            "{fixture_name} AOTT entry {} references missing ATS {} title {}",
            entry.ordinal,
            entry.title_set_nr,
            entry.title_nr
        );
    }
}

fn fixture_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest.join("tests/fixtures/dvda"),
        manifest.parent().unwrap_or(manifest).join("tests/fixtures/dvda"),
    ];
    candidates
        .into_iter()
        .find(|path| path.is_dir())
        .unwrap_or_else(|| manifest.join("tests/fixtures/dvda"))
}
