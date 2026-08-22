//! Shared ReplayGain command and post-scan policy.
//!
//! The conversion pipeline and the metadata editor must invoke `loudgain`
//! with the same argument semantics. Track-only scans also share one cleanup
//! path for inherited album-level tags so the same file cannot acquire
//! different ReplayGain state depending on which UI initiated the scan.

use std::io;
use std::path::{Path, PathBuf};

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::ItemKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoudgainGrouping {
    Track,
    Album,
}

/// Build the canonical `loudgain` argv used by every caller.
#[must_use]
pub(crate) fn loudgain_args(
    grouping: LoudgainGrouping,
    prevent_clipping: bool,
    paths: &[PathBuf],
) -> Vec<String> {
    let mut args = Vec::with_capacity(paths.len().saturating_add(4));
    if grouping == LoudgainGrouping::Album {
        args.push("-a".to_string());
    }
    if prevent_clipping {
        args.push("-k".to_string());
    }
    args.push("-s".to_string());
    args.push("i".to_string());
    args.extend(paths.iter().map(|path| path.to_string_lossy().into_owned()));
    args
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplayGainTrackMeasurement {
    pub track_gain: String,
    pub track_peak: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplayGainSourceScan {
    pub tracks: Vec<ReplayGainTrackMeasurement>,
    pub album_gain: Option<String>,
    pub album_peak: Option<String>,
}

/// Build a scan-only loudgain command. `-O -q` gives a stable tab-delimited
/// result. Omitting `-s` is loudgain's documented analyze-only mode, which is
/// essential here because the inputs are read-only FIFO streams.
#[must_use]
pub(crate) fn loudgain_scan_args(
    grouping: LoudgainGrouping,
    prevent_clipping: bool,
    paths: &[PathBuf],
) -> Vec<String> {
    let mut args = Vec::with_capacity(paths.len().saturating_add(7));
    if grouping == LoudgainGrouping::Album {
        args.push("-a".to_string());
    }
    if prevent_clipping {
        args.push("-k".to_string());
    }
    args.extend(["-O".to_string(), "-q".to_string()]);
    args.extend(paths.iter().map(|path| path.to_string_lossy().into_owned()));
    args
}

fn parse_scan_row(line: &str) -> io::Result<(&str, &str, &str)> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() < 11 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("loudgain scan row has {} columns, expected at least 11", fields.len()),
        ));
    }
    // The first field is a path and could itself contain tabs. The ten result
    // columns are fixed at the right edge of the row. We need True_Peak and
    // Gain, which are respectively the 8th- and 3rd-from-last fields.
    let track_peak = fields[fields.len() - 8].trim();
    let gain = fields[fields.len() - 3].trim();
    let label = fields[..fields.len() - 10].join("\t");
    if track_peak.is_empty() || gain.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "loudgain scan row omitted peak or gain",
        ));
    }
    // Keep the path/Album label owned only long enough to classify this row.
    // Returning a leaked value would be wrong, so classify through the caller.
    let label_kind = if label == "Album" { "Album" } else { "Track" };
    Ok((label_kind, gain, track_peak))
}

/// Parse `loudgain -O -q` output. FIFO input names are intentionally short,
/// so the runner's bounded stdout capture comfortably contains the complete
/// CUE limit of 99 track rows plus the optional album row.
pub(crate) fn parse_loudgain_scan_output(
    stdout: &str,
    expected_tracks: usize,
    grouping: LoudgainGrouping,
) -> io::Result<ReplayGainSourceScan> {
    let mut tracks = Vec::with_capacity(expected_tracks);
    let mut album_gain = None;
    let mut album_peak = None;
    for line in stdout.lines().map(str::trim_end).filter(|line| !line.trim().is_empty()) {
        if line.starts_with("File\t") {
            continue;
        }
        let (kind, gain, peak) = parse_scan_row(line)?;
        if kind == "Album" {
            if album_gain.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "loudgain scan produced more than one album summary",
                ));
            }
            album_gain = Some(gain.to_string());
            album_peak = Some(peak.to_string());
        } else {
            tracks.push(ReplayGainTrackMeasurement {
                track_gain: gain.to_string(),
                track_peak: peak.to_string(),
            });
        }
    }
    if tracks.len() != expected_tracks {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "loudgain scan produced {} track rows, expected {expected_tracks}",
                tracks.len()
            ),
        ));
    }
    if grouping == LoudgainGrouping::Album && (album_gain.is_none() || album_peak.is_none()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "album loudgain scan omitted its album summary",
        ));
    }
    Ok(ReplayGainSourceScan {
        tracks,
        album_gain,
        album_peak,
    })
}

/// Apply a scan-only source measurement to the already-encoded outputs. This
/// reproduces the four standard `-s i` ReplayGain values; Album and Both use
/// the same `loudgain -a -s i` semantics in the established path and therefore
/// write both per-track and album values. Track mode also removes stale album
/// values exactly as the existing post-scan cleanup does.
///
/// Crucially, this does not save a generic Lofty `TaggedFile` after the
/// authoritative metadata stage. ReplayGain is an unrelated four-field edit,
/// so it must traverse Tonepoet's existing list-aware metadata mutation
/// boundary. That boundary owns native FLAC/APEv2 writers, ID3/MP4
/// preservation, mutation coordination, and post-write verification.
pub(crate) fn apply_source_scan(
    paths: &[PathBuf],
    mode: tonepoet_pipeline::ReplayGainMode,
    scan: &ReplayGainSourceScan,
) -> io::Result<()> {
    if paths.len() != scan.tracks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "ReplayGain source scan has {} tracks for {} outputs",
                scan.tracks.len(),
                paths.len()
            ),
        ));
    }
    let album_values = match mode {
        tonepoet_pipeline::ReplayGainMode::Track => None,
        tonepoet_pipeline::ReplayGainMode::Album | tonepoet_pipeline::ReplayGainMode::Both => {
            Some((
                scan.album_gain.as_deref().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "source scan has no album gain")
                })?,
                scan.album_peak.as_deref().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "source scan has no album peak")
                })?,
            ))
        }
    };

    for (path, measurement) in paths.iter().zip(&scan.tracks) {
        let mut changes = vec![
            (
                ItemKey::ReplayGainTrackGain,
                vec![measurement.track_gain.clone()],
            ),
            (
                ItemKey::ReplayGainTrackPeak,
                vec![measurement.track_peak.clone()],
            ),
        ];
        if let Some((album_gain, album_peak)) = album_values {
            changes.push((ItemKey::ReplayGainAlbumGain, vec![album_gain.to_string()]));
            changes.push((ItemKey::ReplayGainAlbumPeak, vec![album_peak.to_string()]));
        } else {
            // Empty lists are deletions at the format-aware writer boundary.
            changes.push((ItemKey::ReplayGainAlbumGain, Vec::new()));
            changes.push((ItemKey::ReplayGainAlbumPeak, Vec::new()));
        }

        let report = crate::tui::probe::write_all_tag_value_lists(path, &changes).map_err(
            |error| {
                io::Error::new(
                    io::ErrorKind::Other,
                    format!(
                        "write source-pass ReplayGain tags to '{}' through metadata preservation boundary: {error}",
                        path.display()
                    ),
                )
            },
        )?;
        for warning in report.durability_warnings {
            log::warn!(
                "source-pass ReplayGain metadata write warning for '{}': {warning}",
                path.display()
            );
        }
    }
    Ok(())
}

/// Remove stale album-level ReplayGain tags after a track-only scan.
///
/// Files without either album tag are not rewritten. Each changed file is read
/// once and rewritten once through Lofty; callers decide how to surface errors.
pub(crate) fn remove_stale_album_tags(paths: &[PathBuf]) -> io::Result<()> {
    for path in paths {
        remove_stale_album_tags_from_path(path)?;
    }
    Ok(())
}

fn remove_stale_album_tags_from_path(path: &Path) -> io::Result<()> {
    let mut tagged = lofty::read_from_path(path).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("read ReplayGain output '{}': {error}", path.display()),
        )
    })?;
    let Some(tag) = tagged.primary_tag_mut() else {
        return Ok(());
    };
    let has_album_gain = tag
        .get_string(&ItemKey::ReplayGainAlbumGain)
        .is_some();
    let has_album_peak = tag
        .get_string(&ItemKey::ReplayGainAlbumPeak)
        .is_some();
    if !has_album_gain && !has_album_peak {
        return Ok(());
    }
    tag.remove_key(&ItemKey::ReplayGainAlbumGain);
    tag.remove_key(&ItemKey::ReplayGainAlbumPeak);
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "remove stale album ReplayGain tags from '{}': {error}",
                    path.display()
                ),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loudgain_args_honor_grouping_and_clipping_policy() {
        let paths = vec![
            PathBuf::from("/music/01.flac"),
            PathBuf::from("/music/02.flac"),
        ];

        assert_eq!(
            loudgain_args(LoudgainGrouping::Album, true, &paths),
            vec![
                "-a".to_string(),
                "-k".to_string(),
                "-s".to_string(),
                "i".to_string(),
                "/music/01.flac".to_string(),
                "/music/02.flac".to_string(),
            ]
        );
        assert_eq!(
            loudgain_args(LoudgainGrouping::Track, false, &paths),
            vec![
                "-s".to_string(),
                "i".to_string(),
                "/music/01.flac".to_string(),
                "/music/02.flac".to_string(),
            ]
        );
    }

    #[test]
    fn loudgain_scan_args_are_read_only_and_keep_clipping_policy() {
        let paths = vec![PathBuf::from("track-001.wav"), PathBuf::from("track-002.wav")];
        assert_eq!(
            loudgain_scan_args(LoudgainGrouping::Album, true, &paths),
            vec![
                "-a".to_string(),
                "-k".to_string(),
                "-O".to_string(),
                "-q".to_string(),
                "track-001.wav".to_string(),
                "track-002.wav".to_string(),
            ]
        );
        assert!(!loudgain_scan_args(LoudgainGrouping::Track, false, &paths)
            .iter()
            .any(|arg| arg == "-s"));
    }

    #[test]
    fn loudgain_scan_parser_uses_gain_and_true_peak_and_album_summary() {
        let output = concat!(
            "File\tLoudness\tRange\tTrue_Peak\tTrue_Peak_dBTP\tReference\tWill_clip\tClip_prevent\tGain\tNew_Peak\tNew_Peak_dBTP\n",
            "track-001.wav\t-5.16 LUFS\t5.65 dB\t1.057608\t0.49 dBTP\t-18.00 LUFS\tN\tN\t-12.84 dB\t0.241255\t-12.35 dBTP\n",
            "track-002.wav\t-6.00 LUFS\t4.00 dB\t0.950000\t-0.45 dBTP\t-18.00 LUFS\tN\tN\t-12.00 dB\t0.238000\t-12.45 dBTP\n",
            "Album\t-5.60 LUFS\t6.00 dB\t1.057608\t0.49 dBTP\t-18.00 LUFS\tN\tN\t-12.40 dB\t0.253700\t-11.91 dBTP\n",
        );
        let scan = parse_loudgain_scan_output(output, 2, LoudgainGrouping::Album)
            .expect("valid loudgain scan");
        assert_eq!(scan.tracks[0].track_gain, "-12.84 dB");
        assert_eq!(scan.tracks[0].track_peak, "1.057608");
        assert_eq!(scan.tracks[1].track_gain, "-12.00 dB");
        assert_eq!(scan.album_gain.as_deref(), Some("-12.40 dB"));
        assert_eq!(scan.album_peak.as_deref(), Some("1.057608"));
    }

    #[test]
    fn loudgain_scan_parser_tolerates_tabs_in_fifo_label() {
        let output = "odd\tname.wav\t-5.16 LUFS\t5.65 dB\t1.057608\t0.49 dBTP\t-18.00 LUFS\tN\tN\t-12.84 dB\t0.241255\t-12.35 dBTP\n";
        let scan = parse_loudgain_scan_output(output, 1, LoudgainGrouping::Track)
            .expect("tab in input label must not shift result columns");
        assert_eq!(scan.tracks[0].track_gain, "-12.84 dB");
        assert_eq!(scan.tracks[0].track_peak, "1.057608");
        assert!(scan.album_gain.is_none());
    }

    #[test]
    fn source_scan_applies_standard_track_and_album_values() {
        use lofty::file::TaggedFileExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("track.flac");
        std::fs::write(&path, include_bytes!("../../tests/fixtures/silence.flac"))
            .expect("copy FLAC fixture");
        let scan = ReplayGainSourceScan {
            tracks: vec![ReplayGainTrackMeasurement {
                track_gain: "-7.25 dB".to_string(),
                track_peak: "0.923100".to_string(),
            }],
            album_gain: Some("-6.80 dB".to_string()),
            album_peak: Some("0.977200".to_string()),
        };

        apply_source_scan(
            std::slice::from_ref(&path),
            tonepoet_pipeline::ReplayGainMode::Both,
            &scan,
        )
        .expect("apply source ReplayGain values");

        let tagged = lofty::read_from_path(&path).expect("read tagged fixture");
        let tag = tagged.primary_tag().expect("primary tag");
        assert_eq!(tag.get_string(&ItemKey::ReplayGainTrackGain), Some("-7.25 dB"));
        assert_eq!(tag.get_string(&ItemKey::ReplayGainTrackPeak), Some("0.923100"));
        assert_eq!(tag.get_string(&ItemKey::ReplayGainAlbumGain), Some("-6.80 dB"));
        assert_eq!(tag.get_string(&ItemKey::ReplayGainAlbumPeak), Some("0.977200"));
    }

    fn metadata_entry_snapshot(
        path: &Path,
        key: &ItemKey,
    ) -> (String, Vec<usize>) {
        let entries = crate::tui::probe::read_all_tags(path)
            .expect("read metadata through preservation reader");
        let entry = entries
            .iter()
            .find(|entry| &entry.item_key == key)
            .unwrap_or_else(|| panic!("missing metadata entry for {key:?}"));
        (
            entry.value.clone(),
            entry.per_file_stored_value_counts.clone(),
        )
    }

    fn metadata_display_snapshot(path: &Path, display_key: &str) -> (String, Vec<usize>) {
        let entries = crate::tui::probe::read_all_tags(path)
            .expect("read metadata through preservation reader");
        let entry = entries
            .iter()
            .find(|entry| entry.display_key == display_key)
            .unwrap_or_else(|| panic!("missing metadata entry for {display_key}"));
        (
            entry.value.clone(),
            entry.per_file_stored_value_counts.clone(),
        )
    }

    fn write_minimal_pcm_aiff(path: &Path) {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FORM");
        bytes.extend_from_slice(&48u32.to_be_bytes());
        bytes.extend_from_slice(b"AIFF");
        bytes.extend_from_slice(b"COMM");
        bytes.extend_from_slice(&18u32.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&16u16.to_be_bytes());
        // 44100.0 as an IEEE 754 80-bit extended precision value.
        bytes.extend_from_slice(&[0x40, 0x0e, 0xac, 0x44, 0, 0, 0, 0, 0, 0]);
        bytes.extend_from_slice(b"SSND");
        bytes.extend_from_slice(&10u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0i16.to_be_bytes());
        std::fs::write(path, bytes).expect("write minimal PCM AIFF fixture");
    }

    fn seed_id3v24_carrier(path: &Path) {
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::tag::{ItemValue, Tag, TagItem, TagType};

        let mut tagged = lofty::read_from_path(path).expect("read AIFF before ID3 seed");
        let mut tag = Tag::new(TagType::Id3v2);
        tag.insert_unchecked(TagItem::new(
            ItemKey::TrackTitle,
            ItemValue::Text("ID3 seed".to_string()),
        ));
        tagged.insert_tag(tag);
        tagged
            .save_to_path(path, WriteOptions::default())
            .expect("seed AIFF ID3v2.4 carrier through Lofty");
    }

    #[test]
    fn source_scan_preserves_unrelated_repeated_flac_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("track.flac");
        std::fs::write(&path, include_bytes!("../../tests/fixtures/silence.flac"))
            .expect("copy FLAC fixture");
        crate::tui::probe::write_all_tag_value_lists(
            &path,
            &[
                (
                    ItemKey::TrackArtist,
                    vec!["Artist A".to_string(), "Artist A".to_string(), "Artist B".to_string()],
                ),
                (
                    ItemKey::Composer,
                    vec!["Composer A".to_string(), "Composer B".to_string()],
                ),
            ],
        )
        .expect("seed repeated FLAC metadata through authoritative writer");
        let artist_before = metadata_entry_snapshot(&path, &ItemKey::TrackArtist);
        let composer_before = metadata_entry_snapshot(&path, &ItemKey::Composer);

        let scan = ReplayGainSourceScan {
            tracks: vec![ReplayGainTrackMeasurement {
                track_gain: "-7.25 dB".to_string(),
                track_peak: "0.923100".to_string(),
            }],
            album_gain: Some("-6.80 dB".to_string()),
            album_peak: Some("0.977200".to_string()),
        };
        apply_source_scan(
            std::slice::from_ref(&path),
            tonepoet_pipeline::ReplayGainMode::Both,
            &scan,
        )
        .expect("apply source ReplayGain through preservation writer");

        assert_eq!(metadata_entry_snapshot(&path, &ItemKey::TrackArtist), artist_before);
        assert_eq!(metadata_entry_snapshot(&path, &ItemKey::Composer), composer_before);
    }


    #[test]
    fn source_scan_preserves_unrelated_aiff_id3_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("track.aiff");
        write_minimal_pcm_aiff(&path);
        seed_id3v24_carrier(&path);
        crate::tui::probe::write_all_tag_value_lists(
            &path,
            &[
                (
                    ItemKey::TrackArtist,
                    vec!["Artist A".to_string(), "Artist A".to_string(), "Artist B".to_string()],
                ),
                (ItemKey::Producer, vec!["Producer A".to_string()]),
                (
                    ItemKey::Arranger,
                    vec!["Arranger A".to_string(), "Arranger B".to_string()],
                ),
            ],
        )
        .expect("seed AIFF ID3 metadata through authoritative writer");
        let artist_before = metadata_display_snapshot(&path, "ARTIST");
        let producer_before = metadata_display_snapshot(&path, "PRODUCER");
        let arranger_before = metadata_display_snapshot(&path, "ARRANGER");

        let scan = ReplayGainSourceScan {
            tracks: vec![ReplayGainTrackMeasurement {
                track_gain: "-7.25 dB".to_string(),
                track_peak: "0.923100".to_string(),
            }],
            album_gain: Some("-6.80 dB".to_string()),
            album_peak: Some("0.977200".to_string()),
        };
        apply_source_scan(
            std::slice::from_ref(&path),
            tonepoet_pipeline::ReplayGainMode::Both,
            &scan,
        )
        .expect("apply source ReplayGain to AIFF through preservation writer");

        assert_eq!(metadata_display_snapshot(&path, "ARTIST"), artist_before);
        assert_eq!(metadata_display_snapshot(&path, "PRODUCER"), producer_before);
        assert_eq!(metadata_display_snapshot(&path, "ARRANGER"), arranger_before);
    }

    #[test]
    fn source_scan_preserves_unrelated_mp4_multivalue_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("track.m4a");
        std::fs::write(
            &path,
            include_bytes!("../../tests/fixtures/metadata_persistence/mp4.m4a"),
        )
        .expect("copy MP4 fixture");
        crate::tui::probe::write_all_tag_value_lists(
            &path,
            &[
                (
                    ItemKey::Performer,
                    vec!["Performer A".to_string(), "Performer B".to_string()],
                ),
                (
                    ItemKey::Arranger,
                    vec!["Arranger A".to_string(), "Arranger B".to_string()],
                ),
            ],
        )
        .expect("seed MP4 multivalue metadata through authoritative writer");
        let performer_before = metadata_entry_snapshot(&path, &ItemKey::Performer);
        let arranger_before = metadata_entry_snapshot(&path, &ItemKey::Arranger);

        let scan = ReplayGainSourceScan {
            tracks: vec![ReplayGainTrackMeasurement {
                track_gain: "-7.25 dB".to_string(),
                track_peak: "0.923100".to_string(),
            }],
            album_gain: Some("-6.80 dB".to_string()),
            album_peak: Some("0.977200".to_string()),
        };
        apply_source_scan(
            std::slice::from_ref(&path),
            tonepoet_pipeline::ReplayGainMode::Both,
            &scan,
        )
        .expect("apply source ReplayGain to MP4 through preservation writer");

        assert_eq!(metadata_entry_snapshot(&path, &ItemKey::Performer), performer_before);
        assert_eq!(metadata_entry_snapshot(&path, &ItemKey::Arranger), arranger_before);
    }

    #[test]
    fn track_cleanup_removes_only_album_level_tags() {
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::tag::{ItemValue, TagItem};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("track.flac");
        std::fs::write(&path, include_bytes!("../../tests/fixtures/silence.flac"))
            .expect("copy FLAC fixture");

        let mut tagged = lofty::read_from_path(&path).expect("read fixture");
        if tagged.primary_tag().is_none() {
            let tag_type = tagged.primary_tag_type();
            tagged.insert_tag(lofty::tag::Tag::new(tag_type));
        }
        let tag = tagged.primary_tag_mut().expect("fixture primary tag");
        for (key, value) in [
            (ItemKey::ReplayGainTrackGain, "-7.25 dB"),
            (ItemKey::ReplayGainTrackPeak, "0.9231"),
            (ItemKey::ReplayGainAlbumGain, "-6.80 dB"),
            (ItemKey::ReplayGainAlbumPeak, "0.9772"),
        ] {
            tag.insert_unchecked(TagItem::new(key, ItemValue::Text(value.to_string())));
        }
        tagged
            .save_to_path(&path, WriteOptions::default())
            .expect("seed ReplayGain tags");

        remove_stale_album_tags(std::slice::from_ref(&path)).expect("remove album tags");

        let tagged = lofty::read_from_path(&path).expect("read cleaned fixture");
        let tag = tagged.primary_tag().expect("cleaned primary tag");
        assert_eq!(
            tag.get_string(&ItemKey::ReplayGainTrackGain),
            Some("-7.25 dB")
        );
        assert_eq!(
            tag.get_string(&ItemKey::ReplayGainTrackPeak),
            Some("0.9231")
        );
        assert!(tag.get_string(&ItemKey::ReplayGainAlbumGain).is_none());
        assert!(tag.get_string(&ItemKey::ReplayGainAlbumPeak).is_none());
    }

}
