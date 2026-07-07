//! DVD-Audio Phase 2 fixture-corpus tests.
//!
//! These tests run only when the 7-disc fixture corpus is available at
//! `tests/fixtures/dvda` or `DVDA_FIXTURE_ROOT`. They deliberately exercise
//! structure materialization only; Phase 3 owns AOB demux/audio realization.

use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::convert::pipeline::tool::StubToolRunner;
use crate::tui::dvda::model::DVD_BLOCK_SIZE;
use crate::tui::dvda::sector::AobSectorReader;
use tokio_util::sync::CancellationToken;

fn fixture_root() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("DVDA_FIXTURE_ROOT") {
        let path = PathBuf::from(value);
        if path.exists() {
            return Some(path);
        }
    }

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dvda");
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

fn fixture_dirs_or_skip() -> Option<Vec<PathBuf>> {
    let Some(root) = fixture_root() else {
        eprintln!("skipping DVD-Audio fixture corpus tests: tests/fixtures/dvda is absent; set DVDA_FIXTURE_ROOT to run them against an external corpus");
        return None;
    };

    let mut fixtures = Vec::new();
    collect_dvda_fixture_dirs(&root, 4, &mut fixtures);
    fixtures.sort_by(|left, right| normalized_fixture_name(left).cmp(&normalized_fixture_name(right)));
    fixtures.dedup();

    assert!(
        !fixtures.is_empty(),
        "DVD-Audio fixture root exists but contains no directories with AUDIO_TS.IFO/DVD-Audio AMG magic: {}",
        root.display()
    );
    Some(fixtures)
}

fn collect_dvda_fixture_dirs(root: &Path, remaining_depth: usize, fixtures: &mut Vec<PathBuf>) {
    if directory_has_dvda_magic(root).unwrap_or(false) {
        fixtures.push(root.to_path_buf());
        return;
    }
    if remaining_depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_dvda_fixture_dirs(&path, remaining_depth - 1, fixtures);
        }
    }
}

fn assert_seven_disc_corpus(fixtures: &[PathBuf]) {
    assert_eq!(
        fixtures.len(),
        7,
        "expected the full 7-disc DVD-Audio fixture corpus, found {}: {:?}",
        fixtures.len(),
        fixtures
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
    );
}

fn parse_fixture_disc(fixture: &Path) -> DvdaDisc {
    let volume = DirectoryDvdaVolume::new(fixture.to_path_buf());
    parse_dvda_volume(&volume)
        .unwrap_or_else(|err| panic!("failed to parse DVD-Audio fixture {}: {err}", fixture.display()))
}

fn request_for_fixture(
    fixture: &Path,
    group: Option<u8>,
    track_selection: TrackSelection,
) -> PipelineRequest {
    let root = std::env::temp_dir().join("tonepoet-dvda-phase2-fixture-tests");
    let group_selection = group
        .map(DvdaGroupSelection::Group)
        .unwrap_or(DvdaGroupSelection::Default);
    PipelineRequest {
        job_id: "dvda-phase2-fixture-test".to_string(),
        item_id: normalized_fixture_name(fixture),
        container: fixture.to_path_buf(),
        source: SourceOptions {
            archive_password: None,
            sacd_area: None,
            dvda_group_selection: group_selection,
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
            track_selection,
        },
        // settings-sentinel-allow: fixture-only default for DVD-Audio materialization tests
        settings: tonepoet_pipeline::PipelineSettings::default(),
        worker_count: Some(1),
        scratch_staging: None,
        merge: false,
        output_root: root.join("out"),
        naming: NamingPolicy {
            template: "%NN% - %TITLE%".to_string(),
            folder_template: None,
            per_album_subdir: true,
            collision_policy: NamingCollisionPolicy::Fail,
        },
        publish: PublishPolicy {
            overwrite: OverwritePolicy::FailIfExists,
            same_filesystem_required: false,
            write_manifest: false,
        },
        log: LogPolicy {
            root: root.join("logs"),
            write_for_blocked: true,
            write_json_log: false,
            write_conversion_log: true,
        },
        stages: StagePolicy {
            metadata: StageRequirement::Disabled,
            replaygain: StageRequirement::Disabled,
            features: StageRequirement::Disabled,
            generate_cue: false,
        },
        failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
        album_batch: None,
        album_batch_track: None,
        suppress_incremental_conversion_log_append: false,
            companion: Default::default(),
            pre_extracted_staging: None,
            archive_metadata_overrides: Vec::new(),
        expected_album_track_count: None,
        container_extension: None,
        container_ffmpeg_flags: Vec::new(),
    }
}

fn materialize_fixture(
    fixture: &Path,
    group: Option<u8>,
    track_selection: TrackSelection,
) -> Result<PreparedSource, MaterializeError> {
    let cancel = CancellationToken::new();
    let req = request_for_fixture(fixture, group, track_selection);
    let volume = DirectoryDvdaVolume::new(fixture.to_path_buf());
    let volume_source = DvdaVolumeSourceRef::Directory {
        root: fixture.to_path_buf(),
    };
    let staging_root = std::env::temp_dir().join("tonepoet-dvda-fixture-test");
    let staging = StagingDir::new(staging_root, "fixture-test".to_string());
    let runner = StubToolRunner::new();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(materialize_prepared_source(&req, &volume_source, &volume, &staging, &runner, &cancel))
}

fn expected_track_count_for_group_model(
    disc: &DvdaDisc,
    group: &DvdaGroup,
) -> Result<usize, MaterializeError> {
    if group.title_refs.is_empty() {
        return Ok(group.samg_tracks.len());
    }

    let mut count = 0_usize;
    for title_ref in &group.title_refs {
        let title_set = find_title_set(disc, title_ref.title_set_nr)?;
        let title = find_title(title_set, title_ref)?;
        count += title.chapters.len();
    }
    Ok(count)
}

fn normalized_fixture_name(path: &Path) -> String {
    let label = path
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    label
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn known_cppm_fixture_label(path: &Path) -> Option<&'static str> {
    let name = normalized_fixture_name(path);
    if name.contains("mgletsgetiton") {
        return Some("MGLETSGETITON");
    }
    if name.contains("hawks") && name.contains("doves") {
        return Some("Hawks & Doves");
    }
    if name.contains("talkingheads77") || (name.contains("talkingheads") && name.contains("77")) {
        return Some("Talking Heads 77");
    }
    None
}

fn is_known_cppm_fixture(path: &Path) -> bool {
    known_cppm_fixture_label(path).is_some()
}


#[derive(Debug, Clone)]
struct GoldenProbeCorpus {
    by_fixture: BTreeMap<String, serde_json::Value>,
}

impl GoldenProbeCorpus {
    fn load_or_skip(root: &Path) -> Option<Self> {
        let path = root.join("corpus_probe_output.json");
        if !path.exists() {
            eprintln!(
                "skipping DVD-Audio golden-probe assertions: {} is absent",
                path.display()
            );
            return None;
        }

        let bytes = std::fs::read(&path)
            .unwrap_or_else(|err| panic!("failed to read golden DVD-Audio probe output {}: {err}", path.display()));
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|err| panic!("failed to parse golden DVD-Audio probe output {} as JSON: {err}", path.display()));
        let entries = value
            .as_array()
            .unwrap_or_else(|| panic!("golden DVD-Audio probe output {} must be a JSON array", path.display()));

        let mut by_fixture = BTreeMap::new();
        for entry in entries {
            let directory = entry
                .get("directory")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| panic!("golden DVD-Audio probe entry lacks string directory: {entry}"));
            let name = normalized_fixture_name(Path::new(directory));
            let old = by_fixture.insert(name.clone(), entry.clone());
            assert!(
                old.is_none(),
                "golden DVD-Audio probe output contains duplicate normalized fixture name {name}"
            );
        }

        Some(Self { by_fixture })
    }

    fn entry_for_fixture(&self, fixture: &Path) -> &serde_json::Value {
        let name = normalized_fixture_name(fixture);
        self.by_fixture.get(&name).unwrap_or_else(|| {
            panic!(
                "golden DVD-Audio probe output has no entry for fixture {} (normalized name {name})",
                fixture.display()
            )
        })
    }

    fn expected_fixture_count(&self) -> usize {
        self.by_fixture.len()
    }
}

#[derive(Debug, Clone)]
struct GoldenTrackFacts {
    group: u8,
    track_nr: u8,
    first_pts: u32,
    len_in_pts: u32,
    first_sector: u32,
    last_sector: u32,
    sample_rate: Option<u32>,
    bit_depth: Option<u32>,
}

#[derive(Debug, Clone)]
struct GoldenAtsiTrackFacts {
    title_set_nr: u8,
    title_nr: u8,
    track_nr: u8,
    first_pts: u32,
    len_in_pts: u32,
    track_type: u8,
    index_start: u8,
    first_sector: u32,
    last_sector: u32,
}

#[derive(Debug, Clone)]
struct GoldenAtsiAudioFacts {
    sample_rate: Option<u32>,
    bit_depth: Option<u32>,
    format_resolution: &'static str,
}

fn golden_fixture_root_or_skip() -> Option<(PathBuf, Vec<PathBuf>, GoldenProbeCorpus)> {
    let Some(root) = fixture_root() else {
        eprintln!("skipping DVD-Audio golden-probe tests: tests/fixtures/dvda is absent; set DVDA_FIXTURE_ROOT to run them");
        return None;
    };
    let mut fixtures = fixture_dirs_or_skip()?;
    fixtures.sort_by(|left, right| normalized_fixture_name(left).cmp(&normalized_fixture_name(right)));
    let corpus = GoldenProbeCorpus::load_or_skip(&root)?;

    let fixture_names = fixtures
        .iter()
        .map(|fixture| normalized_fixture_name(fixture))
        .collect::<BTreeSet<_>>();
    let golden_names = corpus.by_fixture.keys().cloned().collect::<BTreeSet<_>>();

    if fixture_names != golden_names {
        let missing_from_golden = fixture_names
            .difference(&golden_names)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let stale_golden_entries = golden_names
            .difference(&fixture_names)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let message = format!(
            "corpus_probe_output.json is not in sync with the DVD-Audio fixture corpus \
             (missing_from_golden=[{missing_from_golden}], stale_golden_entries=[{stale_golden_entries}])"
        );
        if bool_env("DVDA_REQUIRE_GOLDEN_PROBE") {
            panic!("DVDA_REQUIRE_GOLDEN_PROBE=1 but {message}");
        }
        eprintln!("skipping DVD-Audio golden-probe tests: {message}");
        return None;
    }

    Some((root, fixtures, corpus))
}

fn json_u64_at<'a>(value: &'a serde_json::Value, key: &str, context: &str) -> u64 {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| panic!("golden DVD-Audio probe {context} lacks numeric {key}"))
}

#[allow(dead_code)]
fn json_bool_at(value: &serde_json::Value, key: &str, context: &str) -> bool {
    value
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or_else(|| panic!("golden DVD-Audio probe {context} lacks boolean {key}"))
}

#[allow(dead_code)]
fn json_array_at<'a>(value: &'a serde_json::Value, key: &str, context: &str) -> &'a Vec<serde_json::Value> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("golden DVD-Audio probe {context} lacks array {key}"))
}

#[allow(dead_code)]
fn json_object_at<'a>(value: &'a serde_json::Value, key: &str, context: &str) -> &'a serde_json::Map<String, serde_json::Value> {
    value
        .get(key)
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("golden DVD-Audio probe {context} lacks object {key}"))
}

fn u8_from_json(value: u64, context: &str) -> u8 {
    u8::try_from(value).unwrap_or_else(|_| panic!("golden DVD-Audio probe {context} value {value} does not fit in u8"))
}

fn u32_from_json(value: u64, context: &str) -> u32 {
    u32::try_from(value).unwrap_or_else(|_| panic!("golden DVD-Audio probe {context} value {value} does not fit in u32"))
}

fn golden_cppm_mkb_present(entry: &serde_json::Value) -> bool {
    entry
        .get("cppm")
        .and_then(|cppm| cppm.get("mkb_present"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn golden_samg_tracks(entry: &serde_json::Value) -> Vec<GoldenTrackFacts> {
    let Some(samg) = entry.get("samg") else {
        return Vec::new();
    };
    let Some(tracks) = samg.get("tracks").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };

    tracks
        .iter()
        .map(|track| {
            let context = format!("SAMG track {track}");
            let sample_rate = nonzero_u32_field(track, "group1_sample_rate", &context);
            let bit_depth = nonzero_u32_field(track, "group1_bit_depth", &context);
            GoldenTrackFacts {
                group: u8_from_json(json_u64_at(track, "group", &context), &context),
                track_nr: u8_from_json(json_u64_at(track, "track", &context), &context),
                first_pts: u32_from_json(json_u64_at(track, "first_pts", &context), &context),
                len_in_pts: u32_from_json(json_u64_at(track, "len_in_pts", &context), &context),
                first_sector: u32_from_json(json_u64_at(track, "abs_first_sector", &context), &context),
                last_sector: u32_from_json(json_u64_at(track, "abs_last_sector", &context), &context),
                sample_rate,
                bit_depth,
            }
        })
        .collect()
}

fn nonzero_u32_field(value: &serde_json::Value, key: &str, context: &str) -> Option<u32> {
    let raw = value.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0);
    if raw == 0 {
        None
    } else {
        Some(u32_from_json(raw, context))
    }
}

fn golden_group_counts(entry: &serde_json::Value) -> BTreeMap<u8, usize> {
    let mut counts = BTreeMap::<u8, usize>::new();
    for track in golden_samg_tracks(entry) {
        *counts.entry(track.group).or_default() += 1;
    }
    counts
}

fn golden_track_for_group_and_number(
    entry: &serde_json::Value,
    group: u8,
    track_nr: u8,
) -> Option<GoldenTrackFacts> {
    golden_samg_tracks(entry)
        .into_iter()
        .find(|track| track.group == group && track.track_nr == track_nr)
}

fn golden_atsi_track(
    entry: &serde_json::Value,
    title_set_nr: u8,
    title_nr: u8,
    track_nr: u8,
) -> Option<GoldenAtsiTrackFacts> {
    let atsi = entry.get("atsi")?.as_object()?;
    let ats_key = format!("ATS_{title_set_nr:02}");
    let ats = atsi.get(&ats_key)?;
    let titles = ats.get("titles")?.as_array()?;
    for title in titles {
        let current_title_nr = title.get("title_number")?.as_u64()? as u8;
        if current_title_nr != title_nr {
            continue;
        }
        for track in title.get("tracks")?.as_array()? {
            let current_track_nr = track.get("track_number")?.as_u64()? as u8;
            if current_track_nr != track_nr {
                continue;
            }
            let context = format!("{ats_key} title {title_nr} track {track_nr}");
            return Some(GoldenAtsiTrackFacts {
                title_set_nr,
                title_nr,
                track_nr,
                first_pts: u32_from_json(json_u64_at(track, "first_pts", &context), &context),
                len_in_pts: u32_from_json(json_u64_at(track, "len_in_pts", &context), &context),
                track_type: u8_from_json(json_u64_at(track, "track_type", &context), &context),
                index_start: u8_from_json(json_u64_at(track, "index_start", &context), &context),
                first_sector: u32_from_json(json_u64_at(track, "first_sector", &context), &context),
                last_sector: u32_from_json(json_u64_at(track, "last_sector", &context), &context),
            });
        }
    }
    None
}

fn golden_atsi_audio_facts(
    entry: &serde_json::Value,
    title_set_nr: u8,
) -> Option<GoldenAtsiAudioFacts> {
    let atsi = entry.get("atsi")?.as_object()?;
    let ats_key = format!("ATS_{title_set_nr:02}");
    let ats = atsi.get(&ats_key)?;
    let formats = ats.get("audio_formats")?.as_array()?;
    if formats.is_empty() {
        return Some(GoldenAtsiAudioFacts {
            sample_rate: None,
            bit_depth: None,
            format_resolution: "no_present_format",
        });
    }
    if formats.len() != 1 {
        return Some(GoldenAtsiAudioFacts {
            sample_rate: None,
            bit_depth: None,
            format_resolution: "multiple_present_formats_unknown_until_aob_demux",
        });
    }

    let format = &formats[0];
    let context = format!("{ats_key} audio format");
    let g1_rate = nonzero_u32_field(format, "group1_sample_rate", &context);
    let g2_rate = nonzero_u32_field(format, "group2_sample_rate", &context);
    let g1_bits = nonzero_u32_field(format, "group1_bit_depth", &context);
    let g2_bits = nonzero_u32_field(format, "group2_bit_depth", &context);

    Some(GoldenAtsiAudioFacts {
        sample_rate: collapse_equal_or_single(g1_rate, g2_rate),
        bit_depth: collapse_equal_or_single(g1_bits, g2_bits),
        format_resolution: "single_present_format",
    })
}

fn collapse_equal_or_single(left: Option<u32>, right: Option<u32>) -> Option<u32> {
    match (left, right) {
        (Some(left), Some(right)) if left != right => None,
        (Some(value), _) | (_, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn prepared_source_snapshot_for_assertion(source: &PreparedSource) -> serde_json::Value {
    let tracks = source
        .tracks
        .iter()
        .map(|track| match &track.source_ref {
            TrackSourceRef::DvdaTrack {
                group_nr,
                title_set_nr,
                title_nr,
                title_ordinal,
                group_track_ordinal,
                ats_track_nr,
                samg_track_nr,
                samg_ordinal,
                sector_address_space,
                first_pts,
                len_in_pts,
                track_type,
                index_start,
                downmix_matrix,
                audio_format_index,
                sector_ranges,
                ..
            } => serde_json::json!({
                "ordinal": track.id.source_ordinal,
                "track_number": track.id.track_number,
                "group": group_nr,
                "title_set": title_set_nr,
                "title_nr": title_nr,
                "title_ordinal": title_ordinal,
                "group_track_ordinal": group_track_ordinal,
                "ats_track_nr": ats_track_nr,
                "samg_track_nr": samg_track_nr,
                "samg_ordinal": samg_ordinal,
                "sector_address_space": format!("{:?}", sector_address_space),
                "first_pts": first_pts,
                "len_in_pts": len_in_pts,
                "track_type": track_type,
                "index_start": index_start,
                "downmix_matrix": downmix_matrix,
                "audio_format_index": audio_format_index,
                "sample_rate": track.scalar_sample_rate(),
                "bit_depth": track.bit_depth,
                "expected_samples": track.expected_samples,
                "sector_ranges": sector_ranges.iter().map(|range| serde_json::json!({
                    "index": range.index_nr,
                    "first": range.first,
                    "last": range.last,
                })).collect::<Vec<_>>(),
            }),
            other => panic!("expected DVD-Audio track in snapshot assertion, got {other:?}"),
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "kind": "DvdAudio",
        "album_total_tracks": source.album_metadata.total_tracks,
        "album_extra": source.album_metadata.extra,
        "tracks": tracks,
    })
}

fn assert_snapshot_matches_probe(source: &PreparedSource, golden_entry: &serde_json::Value, group: u8) {
    let snapshot = prepared_source_snapshot_for_assertion(source);
    let tracks = snapshot
        .get("tracks")
        .and_then(serde_json::Value::as_array)
        .expect("snapshot tracks");
    let expected_count = golden_group_counts(golden_entry)
        .get(&group)
        .copied()
        .unwrap_or(0);
    assert_eq!(tracks.len(), expected_count, "normalized PreparedSource snapshot track count changed");

    for track in tracks {
        if let Some(title_set_nr) = track.get("title_set").and_then(serde_json::Value::as_u64) {
            let title_nr = track.get("title_nr").and_then(serde_json::Value::as_u64).unwrap_or(0) as u8;
            let ats_track_nr = track.get("ats_track_nr").and_then(serde_json::Value::as_u64).unwrap_or(0) as u8;
            let expected = golden_atsi_track(golden_entry, title_set_nr as u8, title_nr, ats_track_nr)
                .unwrap_or_else(|| panic!("golden probe lacks ATSI track for snapshot group {group}, ATS {title_set_nr}, title {title_nr}, ATS track {ats_track_nr}"));
            assert_eq!(track.get("first_pts").and_then(serde_json::Value::as_u64), Some(u64::from(expected.first_pts)));
            assert_eq!(track.get("len_in_pts").and_then(serde_json::Value::as_u64), Some(u64::from(expected.len_in_pts)));
        } else if let Some(samg_track_nr) = track.get("samg_track_nr").and_then(serde_json::Value::as_u64).map(|value| value as u8) {
            if let Some(expected) = golden_track_for_group_and_number(golden_entry, group, samg_track_nr) {
                assert_eq!(track.get("first_pts").and_then(serde_json::Value::as_u64), Some(u64::from(expected.first_pts)));
                assert_eq!(track.get("len_in_pts").and_then(serde_json::Value::as_u64), Some(u64::from(expected.len_in_pts)));
            }
        }
    }
}

fn assert_tracks_are_structure_only_dvda_tracks(source: &PreparedSource, expected_group: u8) {
    assert_eq!(source.kind, SourceKind::DvdAudio);
    for track in &source.tracks {
        match &track.source_ref {
            TrackSourceRef::DvdaTrack {
                volume_source,
                group_nr,
                sector_address_space,
                first_pts,
                len_in_pts,
                track_type,
                index_start,
                title_table_offset,
                title_len_in_pts,
                title_track_count_declared,
                title_index_count_declared,
                sector_ranges,
                ..
            } => {
                assert!(matches!(volume_source, DvdaVolumeSourceRef::Directory { .. }));
                assert_eq!(*group_nr, expected_group);
                assert!(
                    *len_in_pts > 0,
                    "DVD-Audio PreparedTrack {} has zero PTS length",
                    track.id.source_ordinal
                );
                let expected_first_pts = first_pts.to_string();
                let expected_len_pts = len_in_pts.to_string();
                assert_eq!(
                    track.metadata.extra.get("dvda_first_pts").map(String::as_str),
                    Some(expected_first_pts.as_str())
                );
                assert_eq!(
                    track.metadata.extra.get("dvda_len_pts").map(String::as_str),
                    Some(expected_len_pts.as_str())
                );
                match sector_address_space {
                    DvdaSectorAddressSpace::AtsAobRelative { .. } => {
                        assert!(
                            track_type.is_some(),
                            "DVD-Audio ATS PreparedTrack {} lacks typed track_type",
                            track.id.source_ordinal
                        );
                        assert!(
                            index_start.is_some(),
                            "DVD-Audio ATS PreparedTrack {} lacks typed index_start",
                            track.id.source_ordinal
                        );
                        assert!(title_table_offset.is_some());
                        assert!(title_len_in_pts.is_some());
                        assert!(title_track_count_declared.is_some());
                        assert!(title_index_count_declared.is_some());
                    }
                    DvdaSectorAddressSpace::DiscAbsolute { .. } => {
                        assert!(
                            track_type.is_some(),
                            "DVD-Audio DiscAbsolute PreparedTrack {} lacks typed track_type",
                            track.id.source_ordinal
                        );
                    }
                    DvdaSectorAddressSpace::SamgAbsolute => {
                        assert_eq!(*track_type, None);
                        assert_eq!(*index_start, None);
                        assert_eq!(*title_table_offset, None);
                        assert_eq!(*title_len_in_pts, None);
                        assert_eq!(*title_track_count_declared, None);
                        assert_eq!(*title_index_count_declared, None);
                    }
                }
                assert!(
                    !sector_ranges.is_empty(),
                    "DVD-Audio PreparedTrack {} has no sector ranges",
                    track.id.source_ordinal
                );
                for range in sector_ranges {
                    assert!(
                        range.last >= range.first,
                        "DVD-Audio PreparedTrack {} has inverted sector range {}-{}",
                        track.id.source_ordinal,
                        range.first,
                        range.last
                    );
                }
            }
            other => panic!("expected DVD-Audio source ref, got {other:?}"),
        }
    }
}

#[test]
fn seven_disc_fixture_corpus_materializes_structure_with_expected_track_counts() {
    let Some(fixtures) = fixture_dirs_or_skip() else {
        return;
    };
    assert_seven_disc_corpus(&fixtures);

    let mut materialized = 0_usize;
    for fixture in fixtures {
        let disc = parse_fixture_disc(&fixture);
        let default_group = select_group(&disc, None)
            .unwrap_or_else(|err| panic!("default group selection failed for {}: {err}", fixture.display()));
        let expected_count = expected_track_count_for_group_model(&disc, default_group)
            .unwrap_or_else(|err| panic!("expected-count derivation failed for {}: {err}", fixture.display()));

        match materialize_fixture(&fixture, None, TrackSelection::All) {
            Ok(source) => {
                assert!(
                    !is_known_cppm_fixture(&fixture),
                    "known CPPM fixture materialized successfully: {}",
                    fixture.display()
                );
                assert_eq!(
                    source.tracks.len(),
                    expected_count,
                    "PreparedTrack count mismatch for default group {} in {}",
                    default_group.group_nr,
                    fixture.display()
                );
                assert_eq!(source.album_metadata.total_tracks, expected_count as u32);
                assert_tracks_are_structure_only_dvda_tracks(&source, default_group.group_nr);
                materialized += 1;
            }
            Err(MaterializeError::BlockedSource { blocked, .. }) => {
                assert!(
                    is_known_cppm_fixture(&fixture)
                        || disc.copy_protection.mkb_present
                        || disc.copy_protection.cppm_detected,
                    "unexpected encrypted result for {}",
                    fixture.display()
                );
                assert!(!blocked.source.tracks.is_empty(), "blocked DVD-Audio source retained no track structure");
            }
            Err(err) => panic!("DVD-Audio fixture {} failed materialization: {err}", fixture.display()),
        }
    }

    assert!(
        materialized > 0,
        "the seven-disc DVD-Audio fixture corpus contains no unencrypted discs to structure-materialize"
    );
}

#[test]
fn seven_disc_fixture_corpus_rejects_the_three_known_cppm_discs() {
    let Some(fixtures) = fixture_dirs_or_skip() else {
        return;
    };
    assert_seven_disc_corpus(&fixtures);

    let mut found = BTreeSet::new();
    for fixture in fixtures.iter().filter(|path| is_known_cppm_fixture(path)) {
        let disc = parse_fixture_disc(fixture);
        let label = known_cppm_fixture_label(fixture).expect("known CPPM fixture label");
        found.insert(label);
        assert!(
            disc.copy_protection.mkb_present || disc.copy_protection.cppm_detected,
            "known CPPM fixture did not parse as copy-protected: {}",
            fixture.display()
        );
        let err = materialize_fixture(fixture, None, TrackSelection::All)
            .expect_err("known CPPM fixture should be blocked after structure materialization");
        match err {
            MaterializeError::BlockedSource { blocked, .. } => {
                assert!(!blocked.source.tracks.is_empty(), "blocked CPPM source lost parsed track structure");
                assert!(matches!(blocked.reason, SourceBlockReason::DvdaCppm(_)));
            }
            other => panic!(
                "known CPPM fixture produced wrong materialization error for {}: {other}",
                fixture.display()
            ),
        }
    }

    let mut expected = BTreeSet::new();
    expected.insert("MGLETSGETITON");
    expected.insert("Hawks & Doves");
    expected.insert("Talking Heads 77");
    assert_eq!(
        found, expected,
        "the fixture corpus should contain all three named CPPM discs"
    );
}

#[test]
fn seven_disc_fixture_corpus_group_selection_matches_the_parser_model() {
    let Some(fixtures) = fixture_dirs_or_skip() else {
        return;
    };
    assert_seven_disc_corpus(&fixtures);

    let mut exercised_groups = 0_usize;
    for fixture in fixtures.iter().filter(|path| !is_known_cppm_fixture(path)) {
        let disc = parse_fixture_disc(fixture);
        if disc.copy_protection.mkb_present || disc.copy_protection.cppm_detected {
            continue;
        }

        for group in &disc.groups {
            let expected_count = expected_track_count_for_group_model(&disc, group)
                .unwrap_or_else(|err| panic!("expected-count derivation failed for {} group {}: {err}", fixture.display(), group.group_nr));
            if expected_count == 0 {
                continue;
            }

            let source = materialize_fixture(fixture, Some(group.group_nr), TrackSelection::All)
                .unwrap_or_else(|err| panic!("group {} materialization failed for {}: {err}", group.group_nr, fixture.display()));
            assert_eq!(
                source.tracks.len(),
                expected_count,
                "PreparedTrack count mismatch for {} group {}",
                fixture.display(),
                group.group_nr
            );
            let expected_group_value = group.group_nr.to_string();
            assert_eq!(
                source.album_metadata.extra.get("dvda_group"),
                Some(&expected_group_value)
            );
            assert_tracks_are_structure_only_dvda_tracks(&source, group.group_nr);
            exercised_groups += 1;
        }
    }

    assert!(
        exercised_groups > 0,
        "group-selection corpus test did not exercise any unencrypted DVD-Audio groups"
    );
}

#[test]
fn seven_disc_fixture_corpus_track_selection_filters_after_materialization() {
    let Some(fixtures) = fixture_dirs_or_skip() else {
        return;
    };
    assert_seven_disc_corpus(&fixtures);

    for fixture in fixtures.iter().filter(|path| !is_known_cppm_fixture(path)) {
        let disc = parse_fixture_disc(fixture);
        if disc.copy_protection.mkb_present || disc.copy_protection.cppm_detected {
            continue;
        }
        let default_group = select_group(&disc, None)
            .unwrap_or_else(|err| panic!("default group selection failed for {}: {err}", fixture.display()));
        let expected_count = expected_track_count_for_group_model(&disc, default_group)
            .unwrap_or_else(|err| panic!("expected-count derivation failed for {}: {err}", fixture.display()));
        if expected_count < 2 {
            continue;
        }

        let range_selected = materialize_fixture(
            fixture,
            None,
            TrackSelection::Range { start: 2, end: 2 },
        )
        .unwrap_or_else(|err| panic!("range track selection failed for {}: {err}", fixture.display()));
        assert_eq!(range_selected.tracks.len(), 1);
        assert_eq!(range_selected.tracks[0].id.source_ordinal, 2);
        assert_tracks_are_structure_only_dvda_tracks(&range_selected, default_group.group_nr);

        let mut set = BTreeSet::new();
        set.insert(1);
        set.insert(expected_count as u32);
        let set_selected = materialize_fixture(fixture, None, TrackSelection::Set(set))
            .unwrap_or_else(|err| panic!("set track selection failed for {}: {err}", fixture.display()));
        let expected_set_len = if expected_count == 1 { 1 } else { 2 };
        assert_eq!(set_selected.tracks.len(), expected_set_len);
        assert_eq!(set_selected.tracks.first().map(|track| track.id.source_ordinal), Some(1));
        assert_eq!(
            set_selected.tracks.last().map(|track| track.id.source_ordinal),
            Some(expected_count as u32)
        );
        assert_tracks_are_structure_only_dvda_tracks(&set_selected, default_group.group_nr);
        return;
    }

    panic!("no unencrypted DVD-Audio fixture with at least two tracks was available for track-selection coverage");
}

#[test]
fn seven_disc_fixture_corpus_has_parser_independent_golden_probe_data() {
    let Some((_root, fixtures, golden)) = golden_fixture_root_or_skip() else {
        return;
    };
    assert_seven_disc_corpus(&fixtures);
    assert_eq!(
        golden.expected_fixture_count(),
        7,
        "corpus_probe_output.json must contain exactly the seven DVD-Audio fixture entries"
    );

    let fixture_names = fixtures
        .iter()
        .map(|fixture| normalized_fixture_name(fixture))
        .collect::<BTreeSet<_>>();
    let golden_names = golden.by_fixture.keys().cloned().collect::<BTreeSet<_>>();
    assert_eq!(fixture_names, golden_names, "fixture directories and golden probe entries differ");
}

#[test]
fn seven_disc_fixture_corpus_cppm_matches_golden_probe_outcomes() {
    let Some((_root, fixtures, golden)) = golden_fixture_root_or_skip() else {
        return;
    };
    assert_seven_disc_corpus(&fixtures);

    let mut encrypted_names = BTreeSet::new();
    for fixture in &fixtures {
        let entry = golden.entry_for_fixture(fixture);
        let expected_encrypted = golden_cppm_mkb_present(entry);
        let result = materialize_fixture(fixture, None, TrackSelection::All);
        match (expected_encrypted, result) {
            (true, Err(MaterializeError::BlockedSource { blocked, .. })) => {
                assert!(!blocked.source.tracks.is_empty(), "blocked golden CPPM source lost parsed structure");
                encrypted_names.insert(normalized_fixture_name(fixture));
            }
            (true, Ok(_)) => panic!(
                "golden probe marks {} as CPPM/MKB-protected, but materialization succeeded",
                fixture.display()
            ),
            (true, Err(err)) => panic!(
                "golden probe marks {} as CPPM/MKB-protected, but materialization failed with {err}",
                fixture.display()
            ),
            (false, Ok(_)) => {}
            (false, Err(err)) => panic!(
                "golden probe marks {} as unencrypted, but materialization failed with {err}",
                fixture.display()
            ),
        }
    }

    let expected = [
        "hawksanddoves".to_string(),
        "mgletsgetiton".to_string(),
        "talkingheads77".to_string(),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    assert_eq!(
        encrypted_names, expected,
        "golden CPPM outcomes should match the three known protected fixtures"
    );
}

#[test]
fn seven_disc_fixture_corpus_group_counts_match_golden_probe_not_parser_model() {
    let Some((_root, fixtures, golden)) = golden_fixture_root_or_skip() else {
        return;
    };
    assert_seven_disc_corpus(&fixtures);

    let mut exercised_groups = 0usize;
    for fixture in fixtures.iter() {
        let entry = golden.entry_for_fixture(fixture);
        if golden_cppm_mkb_present(entry) {
            continue;
        }
        let golden_group_counts = golden_group_counts(entry);
        assert!(
            !golden_group_counts.is_empty(),
            "golden probe entry for {} has no SAMG group/track count oracle",
            fixture.display()
        );

        for (group, expected_count) in golden_group_counts {
            let source = materialize_fixture(fixture, Some(group), TrackSelection::All)
                .unwrap_or_else(|err| panic!("materialization failed for {} group {group}: {err}", fixture.display()));
            assert_eq!(
                source.tracks.len(),
                expected_count,
                "PreparedTrack count for {} group {group} must match corpus_probe_output.json, not a parser-derived model",
                fixture.display()
            );
            assert_eq!(source.album_metadata.total_tracks, expected_count as u32);
            assert_snapshot_matches_probe(&source, entry, group);
            exercised_groups += 1;
        }
    }

    assert!(exercised_groups > 0, "no unencrypted golden groups were exercised");
}

#[test]
fn seven_disc_fixture_corpus_track_boundaries_match_golden_probe() {
    let Some((_root, fixtures, golden)) = golden_fixture_root_or_skip() else {
        return;
    };
    assert_seven_disc_corpus(&fixtures);

    let mut checked_tracks = 0usize;
    for fixture in fixtures.iter() {
        let entry = golden.entry_for_fixture(fixture);
        if golden_cppm_mkb_present(entry) {
            continue;
        }
        for (group, _count) in golden_group_counts(entry) {
            let source = materialize_fixture(fixture, Some(group), TrackSelection::All)
                .unwrap_or_else(|err| panic!("materialization failed for {} group {group}: {err}", fixture.display()));
            for track in &source.tracks {
                let TrackSourceRef::DvdaTrack {
                    title_set_nr,
                    title_nr,
                    group_track_ordinal,
                    ats_track_nr,
                    samg_track_nr,
                    sector_address_space,
                    first_pts,
                    len_in_pts,
                    track_type,
                    index_start,
                    sector_ranges,
                    ..
                } = &track.source_ref else {
                    panic!("expected DVD-Audio source ref for {}", fixture.display());
                };

                match (title_set_nr, title_nr, sector_address_space) {
                    (Some(title_set_nr), Some(title_nr), DvdaSectorAddressSpace::AtsAobRelative { .. }) => {
                        let ats_track_nr = ats_track_nr.expect("ATS track should carry an ATS-local track number");
                        let expected = golden_atsi_track(entry, *title_set_nr, *title_nr, ats_track_nr)
                            .unwrap_or_else(|| panic!("golden probe lacks ATSI track for {} ATS {} title {} ATS track {}", fixture.display(), title_set_nr, title_nr, ats_track_nr));
                        assert_eq!(expected.title_set_nr, *title_set_nr);
                        assert_eq!(expected.title_nr, *title_nr);
                        assert_eq!(expected.track_nr, ats_track_nr);
                        assert!(*group_track_ordinal >= 1);
                        assert_eq!(*first_pts, expected.first_pts);
                        assert_eq!(*len_in_pts, expected.len_in_pts);
                        assert_eq!(*track_type, Some(expected.track_type));
                        assert_eq!(*index_start, Some(expected.index_start));
                        assert_eq!(sector_ranges.first().map(|range| range.first), Some(expected.first_sector));
                        assert_eq!(sector_ranges.last().map(|range| range.last), Some(expected.last_sector));
                        checked_tracks += 1;
                    }
                    (None, None, DvdaSectorAddressSpace::SamgAbsolute) => {
                        let samg_track_nr = samg_track_nr.expect("SAMG track should carry a SAMG group track number");
                        assert_eq!(*group_track_ordinal, u32::from(samg_track_nr));
                        let expected = golden_track_for_group_and_number(entry, group, samg_track_nr)
                            .unwrap_or_else(|| panic!("golden probe lacks SAMG track for {} group {group} SAMG track {}", fixture.display(), samg_track_nr));
                        assert_eq!(*first_pts, expected.first_pts);
                        assert_eq!(*len_in_pts, expected.len_in_pts);
                        assert_eq!(sector_ranges.first().map(|range| range.first), Some(expected.first_sector));
                        assert_eq!(sector_ranges.last().map(|range| range.last), Some(expected.last_sector));
                        checked_tracks += 1;
                    }
                    other => panic!("unexpected DVD-Audio source-address combination for {}: {other:?}", fixture.display()),
                }
            }
        }
    }

    assert!(checked_tracks > 0, "no fixture track boundaries were checked against the golden probe");
}


#[test]
fn seven_disc_fixture_corpus_audio_facts_match_golden_probe_where_ifo_proves_them() {
    let Some((_root, fixtures, golden)) = golden_fixture_root_or_skip() else {
        return;
    };
    assert_seven_disc_corpus(&fixtures);

    let mut checked_audio_facts = 0usize;
    for fixture in fixtures.iter() {
        let entry = golden.entry_for_fixture(fixture);
        if golden_cppm_mkb_present(entry) {
            continue;
        }
        for (group, _count) in golden_group_counts(entry) {
            let source = materialize_fixture(fixture, Some(group), TrackSelection::All)
                .unwrap_or_else(|err| panic!("materialization failed for {} group {group}: {err}", fixture.display()));
            for track in &source.tracks {
                let TrackSourceRef::DvdaTrack {
                    title_set_nr,
                    group_track_ordinal,
                    ats_track_nr,
                    samg_track_nr,
                    sector_address_space,
                    ..
                } = &track.source_ref else {
                    panic!("expected DVD-Audio source ref for {}", fixture.display());
                };

                match (title_set_nr, sector_address_space) {
                    (Some(title_set_nr), DvdaSectorAddressSpace::AtsAobRelative { .. }) => {
                        let expected = golden_atsi_audio_facts(entry, *title_set_nr)
                            .unwrap_or_else(|| panic!("golden probe lacks ATS audio-format facts for {} ATS {}", fixture.display(), title_set_nr));
                        assert_eq!(
                            track.scalar_sample_rate(),
                            expected.sample_rate,
                            "sample rate mismatch for {} ATS {} track {}",
                            fixture.display(),
                            title_set_nr,
                            ats_track_nr.unwrap_or(0)
                        );
                        assert_eq!(
                            track.bit_depth,
                            expected.bit_depth,
                            "bit depth mismatch for {} ATS {} track {}",
                            fixture.display(),
                            title_set_nr,
                            ats_track_nr.unwrap_or(0)
                        );
                        assert_eq!(
                            track.metadata.extra.get("dvda_audio_format_resolution").map(String::as_str),
                            Some(expected.format_resolution)
                        );
                        checked_audio_facts += 1;
                    }
                    (None, DvdaSectorAddressSpace::SamgAbsolute) => {
                        let samg_track_nr = samg_track_nr.expect("SAMG track should carry a SAMG group track number");
                        assert_eq!(*group_track_ordinal, u32::from(samg_track_nr));
                        let expected = golden_track_for_group_and_number(entry, group, samg_track_nr)
                            .unwrap_or_else(|| panic!("golden probe lacks SAMG track for {} group {group} SAMG track {}", fixture.display(), samg_track_nr));
                        assert_eq!(track.scalar_sample_rate(), expected.sample_rate);
                        assert_eq!(track.bit_depth, expected.bit_depth);
                        checked_audio_facts += 1;
                    }
                    _ => {}
                }
            }
        }
    }

    assert!(checked_audio_facts > 0, "no fixture audio facts were checked against the golden probe");
}

#[derive(Debug, Clone)]
struct UdfIsoFixturePair {
    fixture: PathBuf,
    iso: PathBuf,
}

fn bool_env(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"))
        .unwrap_or(false)
}

fn udf_iso_fixture_pairs_or_skip() -> Option<Vec<UdfIsoFixturePair>> {
    let Some(fixture_root) = fixture_root() else {
        if bool_env("DVDA_REQUIRE_UDF_ISO_FIXTURES") {
            panic!("DVDA_REQUIRE_UDF_ISO_FIXTURES=1 but no DVD-Audio directory fixtures were found; set DVDA_FIXTURE_ROOT");
        }
        eprintln!("skipping DVD-Audio UDF ISO tests: directory fixture root is absent; set DVDA_FIXTURE_ROOT and DVDA_ISO_FIXTURE_ROOT");
        return None;
    };
    let Some(fixtures) = fixture_dirs_or_skip() else {
        return None;
    };
    let iso_files = discover_udf_iso_files(&fixture_root);
    if iso_files.is_empty() {
        if bool_env("DVDA_REQUIRE_UDF_ISO_FIXTURES") {
            panic!("DVDA_REQUIRE_UDF_ISO_FIXTURES=1 but no .iso/.img files were found; set DVDA_ISO_FIXTURE_ROOT to the seven real DVD-Audio UDF ISOs");
        }
        eprintln!("skipping DVD-Audio UDF ISO tests: no .iso/.img files were found; set DVDA_ISO_FIXTURE_ROOT to run real-disc UDF coverage");
        return None;
    }

    let mut pairs = Vec::new();
    for fixture in fixtures {
        if let Some(iso) = matching_iso_for_fixture(&fixture, &iso_files) {
            pairs.push(UdfIsoFixturePair { fixture, iso });
        }
    }

    if pairs.is_empty() {
        let message = format!(
            "found {} ISO image(s), but none matched the seven DVD-Audio directory fixture names; use matching stems such as hdad2009.iso or set DVDA_UDF_ISO_MANIFEST",
            iso_files.len()
        );
        if bool_env("DVDA_REQUIRE_UDF_ISO_FIXTURES") {
            panic!("{message}");
        }
        eprintln!("skipping DVD-Audio UDF ISO tests: {message}");
        return None;
    }

    pairs.sort_by(|left, right| normalized_fixture_name(&left.fixture).cmp(&normalized_fixture_name(&right.fixture)));
    if bool_env("DVDA_REQUIRE_UDF_ISO_FIXTURES") {
        assert_eq!(
            pairs.len(),
            7,
            "real UDF ISO coverage is required: expected matched ISO images for all seven DVD-Audio fixtures, found {}: {:?}",
            pairs.len(),
            pairs
                .iter()
                .map(|pair| format!("{} => {}", pair.fixture.display(), pair.iso.display()))
                .collect::<Vec<_>>()
        );
    }
    Some(pairs)
}

fn discover_udf_iso_files(fixture_root: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(root) = std::env::var("DVDA_ISO_FIXTURE_ROOT") {
        roots.push(PathBuf::from(root));
    }
    if let Ok(manifest) = std::env::var("DVDA_UDF_ISO_MANIFEST") {
        roots.extend(iso_files_from_manifest(Path::new(&manifest)));
    }
    roots.push(fixture_root.to_path_buf());
    if let Some(parent) = fixture_root.parent() {
        roots.push(parent.join("dvda-isos"));
        roots.push(parent.join("dvda_iso"));
        roots.push(parent.join("dvda_isos"));
    }

    let mut out = Vec::new();
    for root in roots.into_iter().filter(|root| root.exists()) {
        if root.is_file() {
            if is_iso_path(&root) {
                out.push(root);
            }
        } else {
            collect_iso_files(&root, 5, &mut out);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn iso_files_from_manifest(path: &Path) -> Vec<PathBuf> {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|err| panic!("failed to read DVD-Audio UDF ISO manifest {}: {err}", path.display()));
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|err| panic!("failed to parse DVD-Audio UDF ISO manifest {} as JSON: {err}", path.display()));
    let Some(entries) = value.as_array() else {
        panic!("DVD-Audio UDF ISO manifest {} must be a JSON array of paths or objects with an iso field", path.display());
    };
    entries
        .iter()
        .filter_map(|entry| {
            entry
                .as_str()
                .or_else(|| entry.get("iso").and_then(serde_json::Value::as_str))
                .map(PathBuf::from)
        })
        .collect()
}

fn collect_iso_files(root: &Path, remaining_depth: usize, out: &mut Vec<PathBuf>) {
    if remaining_depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_iso_files(&path, remaining_depth - 1, out);
        } else if is_iso_path(&path) {
            out.push(path);
        }
    }
}

fn is_iso_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "iso" | "img"))
        .unwrap_or(false)
}

fn matching_iso_for_fixture(fixture: &Path, iso_files: &[PathBuf]) -> Option<PathBuf> {
    let fixture_name = normalized_fixture_name(fixture);
    iso_files
        .iter()
        .find(|iso| {
            let stem = normalized_fixture_name(Path::new(
                iso.file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default(),
            ));
            !stem.is_empty() && (stem == fixture_name || stem.contains(&fixture_name) || fixture_name.contains(&stem))
        })
        .cloned()
}

fn disc_navigation_summary(disc: &DvdaDisc) -> serde_json::Value {
    serde_json::json!({
        "copy_protection": {
            "mkb_present": disc.copy_protection.mkb_present,
            "cppm_detected": disc.copy_protection.cppm_detected,
        },
        "groups": disc.groups.iter().map(|group| serde_json::json!({
            "group_nr": group.group_nr,
            "title_refs": group.title_refs.iter().map(|title_ref| serde_json::json!({
                "title_set_nr": title_ref.title_set_nr,
                "title_nr": title_ref.title_nr,
                "kind": format!("{:?}", title_ref.kind),
            })).collect::<Vec<_>>(),
            "samg_tracks": group.samg_tracks.iter().map(|track_ref| serde_json::json!({
                "samg_ordinal": track_ref.samg_ordinal,
                "group": track_ref.group_nr,
                "track": track_ref.track_nr,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "title_sets": disc.title_sets.iter().map(|title_set| serde_json::json!({
            "number": title_set.number,
            "kind": format!("{:?}", title_set.kind),
            "audio_pgcit_offset": title_set.audio_pgcit_offset,
            "present_audio_formats": title_set.audio_formats.iter().filter(|format| format.present).map(|format| serde_json::json!({
                "format_index": format.format_index,
                "audio_type_raw": format.audio_type_raw,
                "group1_sample_rate": format.channel_format.group1_sample_rate,
                "group2_sample_rate": format.channel_format.group2_sample_rate,
                "group1_bits": format.channel_format.group1_bits,
                "group2_bits": format.channel_format.group2_bits,
                "assignment_code": format.channel_format.assignment_code,
            })).collect::<Vec<_>>(),
            "aobs": title_set.aobs.iter().map(|aob| serde_json::json!({
                "file_name": aob.file_name,
                "exists": aob.exists,
                "byte_len": aob.byte_len,
                "block_first": aob.block_first,
                "block_last": aob.block_last,
            })).collect::<Vec<_>>(),
            "titles": title_set.titles.iter().map(|title| serde_json::json!({
                "title_set_nr": title.title_set_nr,
                "title_nr": title.title_nr,
                "title_ordinal": title.title_ordinal,
                "title_table_offset": title.title_table_offset,
                "track_type_low_bits_candidates": title.track_type_low_bits_candidates,
                "track_count_declared": title.track_count_declared,
                "index_count_declared": title.index_count_declared,
                "len_in_pts": title.len_in_pts,
                "chapters": title.chapters.iter().map(|chapter| serde_json::json!({
                    "track_nr": chapter.track_nr,
                    "track_type": chapter.track_type,
                    "track_type_low_bits_candidate": chapter.track_type_low_bits_candidate,
                    "downmix_matrix": chapter.downmix_matrix,
                    "index_start": chapter.index_start,
                    "first_pts": chapter.first_pts,
                    "len_in_pts": chapter.len_in_pts,
                    "sector_ranges": chapter.sector_ranges.iter().map(|range| serde_json::json!({
                        "index_nr": range.index_nr,
                        "first": range.first,
                        "last": range.last,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn golden_aob_lengths(entry: &serde_json::Value) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    collect_golden_aob_lengths(entry, &mut out);
    out
}

fn collect_golden_aob_lengths(value: &serde_json::Value, out: &mut BTreeMap<String, u64>) {
    match value {
        serde_json::Value::Object(map) => {
            let maybe_name = map
                .get("file_name")
                .or_else(|| map.get("filename"))
                .or_else(|| map.get("name"))
                .and_then(serde_json::Value::as_str);
            if let Some(name) = maybe_name {
                if audio_ts_name_key(name).ends_with(".AOB") {
                    if let Some(len) = map
                        .get("byte_len")
                        .or_else(|| map.get("bytes"))
                        .or_else(|| map.get("size_bytes"))
                        .or_else(|| map.get("size"))
                        .or_else(|| map.get("len"))
                        .and_then(serde_json::Value::as_u64)
                    {
                        let key = audio_ts_name_key(name);
                        if let Some(old) = out.insert(key.clone(), len) {
                            assert_eq!(old, len, "golden probe reports inconsistent byte lengths for {key}");
                        }
                    }
                }
            }
            for child in map.values() {
                collect_golden_aob_lengths(child, out);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                collect_golden_aob_lengths(child, out);
            }
        }
        _ => {}
    }
}

fn audio_ts_name_key(name: &str) -> String {
    let basename = name
        .rsplit(|ch| ch == '/' || ch == '\\')
        .next()
        .unwrap_or(name)
        .trim();
    basename.to_ascii_uppercase()
}

fn existing_aob_pairs_for_payload_compare(iso_disc: &DvdaDisc, fixture: &Path) -> Vec<(AobFileEntry, PathBuf)> {
    let mut out = Vec::new();
    for title_set in &iso_disc.title_sets {
        for aob in &title_set.aobs {
            if !aob.exists || aob.byte_len < DVD_BLOCK_SIZE {
                continue;
            }
            let path = fixture.join("AUDIO_TS").join(&aob.file_name);
            if path.exists() {
                if let Ok(metadata) = path.metadata() {
                    if metadata.len() == aob.byte_len {
                        out.push((aob.clone(), path));
                    }
                }
            }
        }
    }
    out
}

fn read_directory_bytes(path: &Path, offset: u64, len: usize) -> Vec<u8> {
    let mut file = std::fs::File::open(path)
        .unwrap_or_else(|err| panic!("failed to open directory fixture payload {}: {err}", path.display()));
    file.seek(SeekFrom::Start(offset))
        .unwrap_or_else(|err| panic!("failed to seek directory fixture payload {} to {offset}: {err}", path.display()));
    let mut out = vec![0u8; len];
    file.read_exact(&mut out)
        .unwrap_or_else(|err| panic!("failed to read directory fixture payload {}: {err}", path.display()));
    out
}

#[test]
fn real_udf_iso_fixture_contract_is_configurable_and_strict_when_requested() {
    let Some(pairs) = udf_iso_fixture_pairs_or_skip() else {
        return;
    };
    if bool_env("DVDA_REQUIRE_UDF_ISO_FIXTURES") {
        assert_eq!(pairs.len(), 7, "strict UDF ISO fixture mode requires all seven paired real-disc ISO images");
    }
    for pair in pairs {
        assert!(pair.iso.exists(), "matched UDF ISO fixture does not exist: {}", pair.iso.display());
        assert!(pair.fixture.exists(), "matched directory fixture does not exist: {}", pair.fixture.display());
    }
}

#[test]
fn real_udf_iso_path_lookup_and_ifo_parse_match_directory_fixture() {
    let Some(pairs) = udf_iso_fixture_pairs_or_skip() else {
        return;
    };

    let mut checked = 0usize;
    for pair in pairs {
        let iso_volume = IsoUdfDvdaVolume::open(pair.iso.clone())
            .unwrap_or_else(|err| panic!("failed to open real UDF DVD-Audio ISO {}: {err}", pair.iso.display()));
        let amg_info = iso_volume
            .audio_ts_file_info("AUDIO_TS.IFO")
            .unwrap_or_else(|err| panic!("failed to inspect AUDIO_TS.IFO in {}: {err}", pair.iso.display()))
            .unwrap_or_else(|| panic!("UDF ISO {} does not expose AUDIO_TS/AUDIO_TS.IFO", pair.iso.display()));
        assert_eq!(amg_info.name, "AUDIO_TS.IFO");
        assert!(amg_info.len >= 12, "AUDIO_TS.IFO in {} is too short to contain the AMG identifier", pair.iso.display());
        assert!(!amg_info.extents.is_empty(), "AUDIO_TS.IFO in {} has no indexed UDF extent metadata", pair.iso.display());

        let mut amg = iso_volume.open_audio_ts_file("AUDIO_TS.IFO")
            .unwrap_or_else(|err| panic!("failed to read AUDIO_TS.IFO from {}: {err}", pair.iso.display()));
        let mut magic = [0u8; 12];
        amg.read_exact(&mut magic)
            .unwrap_or_else(|err| panic!("failed to read AMG identifier from {}: {err}", pair.iso.display()));
        assert_eq!(&magic, b"DVDAUDIO-AMG", "UDF ISO {} did not expose AMG magic at AUDIO_TS.IFO byte offset 0", pair.iso.display());

        let iso_disc = parse_dvda_volume(&iso_volume)
            .unwrap_or_else(|err| panic!("failed to parse real UDF DVD-Audio ISO {}: {err}", pair.iso.display()));
        let dir_disc = parse_fixture_disc(&pair.fixture);
        assert_eq!(
            disc_navigation_summary(&iso_disc),
            disc_navigation_summary(&dir_disc),
            "UDF ISO parse result differs from paired directory fixture for {}",
            pair.fixture.display()
        );
        checked += 1;
    }

    assert!(checked > 0, "no real UDF ISO fixtures were parsed");
}

#[test]
fn real_udf_iso_aob_lengths_match_golden_probe_or_directory_payloads() {
    let Some((root, _fixtures, golden)) = golden_fixture_root_or_skip() else {
        if bool_env("DVDA_REQUIRE_UDF_ISO_FIXTURES") {
            panic!("strict UDF ISO fixture mode requires tests/fixtures/dvda/corpus_probe_output.json or DVDA_FIXTURE_ROOT/corpus_probe_output.json for independent AOB byte-length checks");
        }
        return;
    };
    let Some(pairs) = udf_iso_fixture_pairs_or_skip() else {
        return;
    };

    let mut checked_aobs = 0usize;
    for pair in pairs {
        let entry = golden.entry_for_fixture(&pair.fixture);
        let golden_lengths = golden_aob_lengths(entry);
        let iso_volume = IsoUdfDvdaVolume::open(pair.iso.clone())
            .unwrap_or_else(|err| panic!("failed to open real UDF DVD-Audio ISO {}: {err}", pair.iso.display()));
        let iso_infos = iso_volume
            .audio_ts_files_info()
            .into_iter()
            .map(|info| (audio_ts_name_key(&info.name), info))
            .collect::<BTreeMap<_, _>>();

        if golden_lengths.is_empty() {
            eprintln!(
                "golden probe {} has no AOB byte-length facts for {}; falling back to directory payload length comparison",
                root.join("corpus_probe_output.json").display(),
                pair.fixture.display()
            );
            let iso_disc = parse_dvda_volume(&iso_volume)
                .unwrap_or_else(|err| panic!("failed to parse real UDF DVD-Audio ISO {}: {err}", pair.iso.display()));
            for (aob, dir_path) in existing_aob_pairs_for_payload_compare(&iso_disc, &pair.fixture) {
                let info = iso_infos
                    .get(&audio_ts_name_key(&aob.file_name))
                    .unwrap_or_else(|| panic!("UDF ISO {} did not index expected AOB {}", pair.iso.display(), aob.file_name));
                let dir_len = dir_path.metadata().expect("directory AOB metadata").len();
                assert_eq!(info.len, dir_len, "UDF AOB size mismatch for {} in {}", aob.file_name, pair.iso.display());
                checked_aobs += 1;
            }
            continue;
        }

        for (name, expected_len) in golden_lengths {
            let info = iso_infos
                .get(&name)
                .unwrap_or_else(|| panic!("UDF ISO {} did not index golden AOB {name}", pair.iso.display()));
            assert_eq!(
                info.len,
                expected_len,
                "UDF AOB byte length for {name} in {} differs from corpus_probe_output.json",
                pair.iso.display()
            );
            assert!(
                !info.extents.is_empty(),
                "UDF AOB {name} in {} has no extent metadata",
                pair.iso.display()
            );
            let extent_sum = info.extents.iter().map(|extent| extent.len).sum::<u64>();
            assert!(
                extent_sum >= info.len,
                "UDF AOB extents for {name} in {} cover only {extent_sum} bytes for a {} byte file",
                pair.iso.display(),
                info.len
            );
            checked_aobs += 1;
        }
    }

    assert!(checked_aobs > 0, "real UDF ISO AOB length test did not check any AOB files");
}

#[test]
fn real_udf_iso_aob_sector_reads_match_directory_payloads_when_payloads_are_available() {
    let Some(pairs) = udf_iso_fixture_pairs_or_skip() else {
        return;
    };

    let mut checked_reads = 0usize;
    let mut checked_cross_aob_boundary = false;
    for pair in pairs {
        let iso_volume = IsoUdfDvdaVolume::open(pair.iso.clone())
            .unwrap_or_else(|err| panic!("failed to open real UDF DVD-Audio ISO {}: {err}", pair.iso.display()));
        let iso_disc = parse_dvda_volume(&iso_volume)
            .unwrap_or_else(|err| panic!("failed to parse real UDF DVD-Audio ISO {}: {err}", pair.iso.display()));

        for title_set in &iso_disc.title_sets {
            let reader = AobSectorReader::new(&iso_volume, &title_set.aobs);
            for (aob, dir_path) in existing_aob_pairs_for_payload_compare(&iso_disc, &pair.fixture)
                .into_iter()
                .filter(|(aob, _)| aob.title_set_nr == title_set.number)
            {
                let block_count = 1_u32.min((aob.byte_len / DVD_BLOCK_SIZE) as u32);
                if block_count == 0 {
                    continue;
                }
                let iso_bytes = reader
                    .read_blocks(aob.block_first, block_count)
                    .unwrap_or_else(|err| panic!("failed to read AOB sectors from {} {}: {err}", pair.iso.display(), aob.file_name));
                let dir_bytes = read_directory_bytes(&dir_path, 0, iso_bytes.len());
                assert_eq!(iso_bytes, dir_bytes, "AOB sector reader bytes differ for {} in {}", aob.file_name, pair.iso.display());
                checked_reads += 1;
            }

            for window in title_set.aobs.windows(2) {
                let left = &window[0];
                let right = &window[1];
                if !(left.exists && right.exists && left.byte_len >= DVD_BLOCK_SIZE && right.byte_len >= DVD_BLOCK_SIZE) {
                    continue;
                }
                if left.byte_len % DVD_BLOCK_SIZE != 0 {
                    continue;
                }
                let left_path = pair.fixture.join("AUDIO_TS").join(&left.file_name);
                let right_path = pair.fixture.join("AUDIO_TS").join(&right.file_name);
                if !(left_path.exists() && right_path.exists()) {
                    continue;
                }
                if left_path.metadata().map(|m| m.len()).ok() != Some(left.byte_len)
                    || right_path.metadata().map(|m| m.len()).ok() != Some(right.byte_len)
                {
                    continue;
                }
                let iso_bytes = reader
                    .read_blocks(left.block_last, 2)
                    .unwrap_or_else(|err| panic!("failed to read across AOB boundary {} -> {} from {}: {err}", left.file_name, right.file_name, pair.iso.display()));
                let mut expected = read_directory_bytes(
                    &left_path,
                    left.byte_len - DVD_BLOCK_SIZE,
                    DVD_BLOCK_SIZE as usize,
                );
                expected.extend(read_directory_bytes(&right_path, 0, DVD_BLOCK_SIZE as usize));
                assert_eq!(iso_bytes, expected, "AOB sector reader bytes differ across {} -> {} in {}", left.file_name, right.file_name, pair.iso.display());
                checked_cross_aob_boundary = true;
                checked_reads += 1;
                break;
            }
        }
    }

    if checked_reads == 0 {
        if bool_env("DVDA_REQUIRE_UDF_ISO_FIXTURES") {
            panic!("strict UDF ISO fixture mode requires directory AOB payloads with matching lengths so ISO sector reads can be byte-compared");
        }
        eprintln!("skipping DVD-Audio UDF AOB byte-compare assertions: paired directory fixtures do not include full AOB payload files");
        return;
    }
    assert!(checked_reads > 0, "real UDF ISO AOB sector reader test did not check any sector reads");
    if bool_env("DVDA_REQUIRE_UDF_ISO_FIXTURES") {
        assert!(checked_cross_aob_boundary, "strict UDF ISO fixture mode requires at least one split AOB pair so cross-file AOB sector reads are covered");
    }
}

#[test]
fn real_udf_iso_reads_across_multi_extent_aob_boundaries_when_present() {
    let Some(pairs) = udf_iso_fixture_pairs_or_skip() else {
        return;
    };

    let mut checked_multi_extent = 0usize;
    for pair in pairs {
        let iso_volume = IsoUdfDvdaVolume::open(pair.iso.clone())
            .unwrap_or_else(|err| panic!("failed to open real UDF DVD-Audio ISO {}: {err}", pair.iso.display()));
        let infos = iso_volume.audio_ts_files_info();
        for info in infos.into_iter().filter(|info| audio_ts_name_key(&info.name).ends_with(".AOB") && info.extents.len() >= 2) {
            let dir_path = pair.fixture.join("AUDIO_TS").join(&info.name);
            if !(dir_path.exists() && dir_path.metadata().map(|m| m.len()).ok() == Some(info.len)) {
                continue;
            }
            let first_extent_len = info.extents[0].len;
            if first_extent_len == 0 || first_extent_len >= info.len {
                continue;
            }
            let read_start = first_extent_len.saturating_sub(1024);
            let read_len = ((info.len - read_start).min(4096)) as usize;
            if read_len == 0 {
                continue;
            }

            let mut udf_file = iso_volume.open_audio_ts_file(&info.name)
                .unwrap_or_else(|err| panic!("failed to open UDF AOB {} from {}: {err}", info.name, pair.iso.display()));
            udf_file
                .seek(SeekFrom::Start(read_start))
                .unwrap_or_else(|err| panic!("failed to seek UDF AOB {} in {}: {err}", info.name, pair.iso.display()));
            let mut iso_bytes = vec![0u8; read_len];
            udf_file
                .read_exact(&mut iso_bytes)
                .unwrap_or_else(|err| panic!("failed to read UDF AOB {} across extent boundary in {}: {err}", info.name, pair.iso.display()));
            let dir_bytes = read_directory_bytes(&dir_path, read_start, read_len);
            assert_eq!(iso_bytes, dir_bytes, "UDF multi-extent AOB read differs for {} in {}", info.name, pair.iso.display());
            checked_multi_extent += 1;
            break;
        }
    }

    if checked_multi_extent == 0 {
        if bool_env("DVDA_REQUIRE_UDF_ISO_FIXTURES") {
            eprintln!("strict UDF ISO fixture mode found no multi-extent AOB with paired directory payload; cross-AOB-file reads are covered separately, but add a fragmented UDF fixture for extent-boundary proof");
        } else {
            eprintln!("skipping DVD-Audio multi-extent UDF AOB byte comparison: no paired multi-extent AOB payload was available");
        }
    }
}
