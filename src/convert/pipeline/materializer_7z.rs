//! PR 3 — `SevenZipMaterializer` implementation.
//!
//! Extracts 7z archives via `ToolRunner`, discovers audio files,
//! probes them with ffprobe, reads metadata with lofty, and returns
//! a `PreparedSource` with `TrackSourceRef::StagedFile` entries.
//!
//! Does not convert, tag, merge, run ReplayGain, generate feature
//! files, publish, write durable logs, or emit terminal events.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::errors::{MaterializeError, ToolRunnerError};
use super::progress::{heartbeat, OperationProgressTracker};
use super::reporter::PipelineReporter;
use super::tool::{ToolBinary, ToolCommand, ToolRunner};
use super::types::*;

// =========================================================================
// Audio file extensions accepted from extracted archives
// =========================================================================

const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "wav", "aiff", "aif", "wv", "mp3", "m4a", "aac", "opus", "ogg", "ape", "dsf", "dff",
    "w64", "rf64",
];

fn is_audio_extension(ext: &str) -> bool {
    AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str())
}

// =========================================================================
// SevenZipMaterializer
// =========================================================================

pub struct SevenZipMaterializer;

#[async_trait]
impl super::stages::Materializer for SevenZipMaterializer {
    async fn materialize(
        &self,
        req: &PipelineRequest,
        staging: &StagingDir,
        runner: &dyn ToolRunner,
        reporter: Option<&dyn PipelineReporter>,
        tool_paths: &HashMap<String, PathBuf>,
        cancel: &CancellationToken,
    ) -> Result<PreparedSource, MaterializeError> {
        // Ensure the staging directory exists.
        std::fs::create_dir_all(&staging.root)?;

        // 1. Extract the archive.
        extract_archive(req, staging, runner, reporter, tool_paths, cancel).await?;

        // Check cancellation between major steps.
        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }

        // 2. Discover audio files in the extraction tree.
        let audio_files = discover_audio_files(&staging.root)?;
        if audio_files.is_empty() {
            return Err(MaterializeError::Extraction(
                "no audio files found in archive".into(),
            ));
        }

        // 3. Probe each audio file and read metadata.
        let mut tracks = Vec::with_capacity(audio_files.len());
        for (idx, path) in audio_files.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(MaterializeError::Cancelled);
            }

            let probe = probe_audio_file(path, runner, cancel).await?;
            let metadata = read_track_metadata(path);

            let ordinal = (idx + 1) as u32;
            tracks.push(PreparedTrack {
                id: TrackId {
                    source_ordinal: ordinal,
                    disc_number: metadata.disc_number,
                    track_number: metadata.track_number.unwrap_or(ordinal),
                },
                source_ref: TrackSourceRef::StagedFile(path.clone()),
                metadata,
                expected_samples: probe.expected_samples,
                sample_rate: probe.sample_rate,
                bit_depth: probe.bit_depth,
            });
        }

        // 4. Apply track selection filter.
        let tracks = apply_track_selection(tracks, &req.source.track_selection)?;

        // 5. Derive album-level metadata from the tracks.
        let album_metadata = derive_album_metadata(&tracks);

        // 6. Build provenance.
        let provenance = ExtractionProvenance {
            source_kind: SourceKind::SevenZip,
            source_sha256: None,
            tool_versions: BTreeMap::new(),
            extracted_at: chrono::Utc::now(),
        };

        Ok(PreparedSource {
            container: req.container.clone(),
            kind: SourceKind::SevenZip,
            tracks,
            album_metadata,
            provenance,
        })
    }
}

// =========================================================================
// Archive extraction
// =========================================================================

/// Build and run the 7z extraction command through `ToolRunner`.
async fn extract_archive(
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    reporter: Option<&dyn PipelineReporter>,
    tool_paths: &HashMap<String, PathBuf>,
    cancel: &CancellationToken,
) -> Result<(), MaterializeError> {
    let mut args = vec![
        "x".to_string(),
        req.container.display().to_string(),
        "-mmt=on".to_string(),
    ];
    let mut secret_args = Vec::new();

    // Password — expose only at the arg boundary; mark for redaction.
    if let Some(ref pw) = req.source.archive_password {
        let pw_arg = format!("-p{}", pw.expose());
        secret_args.push(args.len());
        args.push(pw_arg);
    }

    args.push(format!("-o{}", staging.root.display()));
    args.push("-y".to_string());

    let cmd = ToolCommand {
        binary: ToolBinary::SevenZip,
        args,
        secret_args,
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(3600), // 1 hour max for large archives
    };

    let result = match reporter {
        Some(rpt) => {
            let mut tracker = OperationProgressTracker::new(
                req.item_id.clone(),
                PipelineStage::Materialize,
                Some(rpt),
            );
            heartbeat::run_with_heartbeat(
                runner.run(cmd, cancel),
                &mut tracker,
                "archive-extraction",
                "Extracting archive\u{2026}",
                Duration::from_secs(5),
            )
            .await
        }
        None => runner.run(cmd, cancel).await,
    };

    match result {
        Ok(_output) => Ok(()),
        Err(ToolRunnerError::Cancelled { .. }) => Err(MaterializeError::Cancelled),
        Err(ToolRunnerError::NonZeroExit { stderr_tail, .. }) => {
            // Detect encrypted-archive failures.
            let lower = stderr_tail.to_lowercase();
            if lower.contains("wrong password")
                || lower.contains("encrypted")
                || lower.contains("can not open encrypted")
            {
                Err(MaterializeError::Encrypted)
            } else {
                Err(MaterializeError::Extraction(stderr_tail))
            }
        }
        Err(e) => Err(e.into()), // MaterializeError::Tool
    }
}

// =========================================================================
// Audio file discovery
// =========================================================================

/// Recursively walk `dir`, collect audio files, and return them sorted
/// by path for deterministic ordering.
fn discover_audio_files(dir: &Path) -> Result<Vec<PathBuf>, MaterializeError> {
    let mut files = Vec::new();
    walk_audio_files(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn walk_audio_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), MaterializeError> {
    let entries = std::fs::read_dir(dir).map_err(MaterializeError::Io)?;
    // Collect and sort directory entries for deterministic traversal.
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            walk_audio_files(&path, out)?;
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if is_audio_extension(ext) {
                    out.push(path);
                }
            }
        }
    }
    Ok(())
}

// =========================================================================
// ffprobe probing through ToolRunner
// =========================================================================

/// Probed audio properties for one file.
pub(crate) struct ProbeResult {
    pub sample_rate: u32,
    pub expected_samples: Option<u64>,
    pub bit_depth: Option<u32>,
}

/// Probe a single audio file via ffprobe through `ToolRunner`.
async fn probe_audio_file(
    path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<ProbeResult, MaterializeError> {
    let cmd = ToolCommand {
        binary: ToolBinary::Ffprobe,
        args: vec![
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            "a:0".into(),
            "-show_entries".into(),
            "stream=sample_rate,duration,bits_per_raw_sample,bits_per_sample".into(),
            "-show_entries".into(),
            "format=duration".into(),
            "-of".into(),
            "json".into(),
            path.display().to_string(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(30),
    };

    let output = match runner.run(cmd, cancel).await {
        Ok(o) => o,
        Err(ToolRunnerError::Cancelled { .. }) => return Err(MaterializeError::Cancelled),
        Err(e) => return Err(e.into()),
    };

    parse_ffprobe_json(&output.stdout_tail)
}

/// Parse the JSON output of ffprobe to extract sample_rate and duration,
/// then compute expected_samples.
fn parse_ffprobe_json(json_str: &str) -> Result<ProbeResult, MaterializeError> {
    let val: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| MaterializeError::Parse(format!("ffprobe JSON parse failed: {e}")))?;

    // Sample rate: streams[0].sample_rate (string in ffprobe JSON).
    let sample_rate = val
        .pointer("/streams/0/sample_rate")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    if sample_rate == 0 {
        return Err(MaterializeError::Parse(
            "ffprobe returned no valid sample_rate".into(),
        ));
    }

    // Duration: prefer stream duration, fall back to format duration.
    let duration_secs: Option<f64> = val
        .pointer("/streams/0/duration")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| {
            val.pointer("/format/duration")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
        });

    let expected_samples = duration_secs.map(|d| (d * sample_rate as f64).round() as u64);
    let bit_depth = val
        .pointer("/streams/0/bits_per_raw_sample")
        .or_else(|| val.pointer("/streams/0/bits_per_sample"))
        .and_then(json_u32);

    Ok(ProbeResult {
        sample_rate,
        expected_samples,
        bit_depth,
    })
}

fn json_u32(value: &serde_json::Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .or_else(|| value.as_str().and_then(|text| text.parse::<u32>().ok()))
}

// =========================================================================
// Metadata reading via lofty (in-process, no ToolRunner)
// =========================================================================

/// Read tags from an audio file. Returns default metadata on failure
/// (e.g. empty or unrecognised files).
fn read_track_metadata(path: &Path) -> TrackMetadata {
    use lofty::prelude::*;

    let tagged = match lofty::read_from_path(path) {
        Ok(t) => t,
        Err(_) => return TrackMetadata::default(),
    };

    let tag = match tagged.primary_tag().or_else(|| tagged.first_tag()) {
        Some(t) => t,
        None => return TrackMetadata::default(),
    };

    // Store album name in `extra` — TrackMetadata has no dedicated
    // album field, but we need it for AlbumMetadata derivation.
    let mut extra = BTreeMap::new();
    if let Some(album) = tag.album() {
        extra.insert("album".to_string(), album.to_string());
    }

    // Enumerate all text tag items into `extra` so naming templates can use
    // arbitrary format-specific fields such as CATALOGNUMBER, BARCODE,
    // MUSICBRAINZ_ALBUMID, and RELEASECOUNTRY. Keys are lowercased to match
    // the template engine's fallthrough lookup.
    let tag_type = tag.tag_type();
    for item in tag.items() {
        if let lofty::tag::ItemValue::Text(text) = item.value() {
            let key = item_key_to_extra_key(item.key(), tag_type);
            if !key.is_empty() {
                extra.entry(key).or_insert_with(|| text.clone());
            }
        }
    }

    TrackMetadata {
        title: tag.title().map(|s| s.to_string()),
        artist: tag.artist().map(|s| s.to_string()),
        album_artist: tag
            .get_string(&lofty::tag::ItemKey::AlbumArtist)
            .map(|s| s.to_string()),
        composer: tag
            .get_string(&lofty::tag::ItemKey::Composer)
            .map(|s| s.to_string()),
        performer: tag
            .get_string(&lofty::tag::ItemKey::Performer)
            .map(|s| s.to_string()),
        genre: tag.genre().map(|s| s.to_string()),
        date: tag.year().map(|y| y.to_string()),
        track_number: tag.track().map(|t| t as u32),
        disc_number: tag.disk().map(|d| d as u32),
        isrc: tag
            .get_string(&lofty::tag::ItemKey::Isrc)
            .map(|s| s.to_string()),
        publisher: tag
            .get_string(&lofty::tag::ItemKey::Publisher)
            .map(|s| s.to_string()),
        copyright: tag
            .get_string(&lofty::tag::ItemKey::CopyrightMessage)
            .map(|s| s.to_string()),
        comment: tag.comment().map(|s| s.to_string()),
        pre_emphasis: false,
        extra,
    }
}

fn item_key_to_extra_key(key: &lofty::tag::ItemKey, tag_type: lofty::tag::TagType) -> String {
    if let Some(mapped) = key.map_key(tag_type, true) {
        return mapped.to_lowercase();
    }

    match key {
        lofty::tag::ItemKey::Unknown(value) => value.to_lowercase(),
        _ => format!("{key:?}").to_lowercase(),
    }
}

// =========================================================================
// Track selection
// =========================================================================

/// Filter `tracks` according to `selection`. Operates on
/// `source_ordinal` (1-based position), not `track_number`.
fn apply_track_selection(
    tracks: Vec<PreparedTrack>,
    selection: &TrackSelection,
) -> Result<Vec<PreparedTrack>, MaterializeError> {
    match selection {
        TrackSelection::All => Ok(tracks),
        TrackSelection::Range { start, end } => {
            if *start == 0 || *end == 0 || start > end {
                return Err(MaterializeError::InvalidTrackSelection(format!(
                    "invalid range {start}-{end}"
                )));
            }
            let max_ordinal = tracks.len() as u32;
            if *start > max_ordinal {
                return Err(MaterializeError::InvalidTrackSelection(format!(
                    "range start {start} exceeds track count {max_ordinal}"
                )));
            }
            Ok(tracks
                .into_iter()
                .filter(|t| t.id.source_ordinal >= *start && t.id.source_ordinal <= *end)
                .collect())
        }
        TrackSelection::Set(indices) => {
            if indices.is_empty() {
                return Err(MaterializeError::InvalidTrackSelection(
                    "empty track set".into(),
                ));
            }
            let max_ordinal = tracks.len() as u32;
            for &idx in indices {
                if idx == 0 || idx > max_ordinal {
                    return Err(MaterializeError::InvalidTrackSelection(format!(
                        "track {idx} outside valid range 1-{max_ordinal}"
                    )));
                }
            }
            Ok(tracks
                .into_iter()
                .filter(|t| indices.contains(&t.id.source_ordinal))
                .collect())
        }
    }
}

// =========================================================================
// Album metadata derivation
// =========================================================================

/// Derive `AlbumMetadata` from common tag values across all tracks.
fn derive_album_metadata(tracks: &[PreparedTrack]) -> AlbumMetadata {
    if tracks.is_empty() {
        return AlbumMetadata::default();
    }

    // Helper: if all tracks agree on a field, return it.
    fn common<F>(tracks: &[PreparedTrack], f: F) -> Option<String>
    where
        F: Fn(&TrackMetadata) -> &Option<String>,
    {
        let first = f(&tracks[0].metadata).as_ref()?;
        if tracks
            .iter()
            .all(|t| f(&t.metadata).as_deref() == Some(first))
        {
            Some(first.clone())
        } else {
            None
        }
    }

    let total_tracks = tracks.len() as u32;
    let total_discs = tracks.iter().filter_map(|t| t.id.disc_number).max();

    // Album name lives in extra["album"] (TrackMetadata has no
    // dedicated album field). Extract if all tracks agree.
    let album = {
        let first = tracks[0].metadata.extra.get("album");
        if let Some(a) = first {
            if tracks
                .iter()
                .all(|t| t.metadata.extra.get("album") == Some(a))
            {
                Some(a.clone())
            } else {
                None
            }
        } else {
            None
        }
    };

    // Promote album-wide extra tags so folder templates can use custom
    // variables from 7z-contained audio files. A tag is album-wide only when
    // every prepared track that carries the key agrees on the same value.
    // This keeps per-track-only values out of folder paths while enabling
    // common release fields such as CATALOGNUMBER, BARCODE,
    // MUSICBRAINZ_ALBUMID, and RELEASECOUNTRY.
    let mut extra = BTreeMap::new();
    for key in tracks
        .iter()
        .flat_map(|track| track.metadata.extra.keys())
    {
        if extra.contains_key(key) {
            continue;
        }
        let Some(first) = tracks[0].metadata.extra.get(key) else {
            continue;
        };
        if tracks
            .iter()
            .all(|track| track.metadata.extra.get(key) == Some(first))
        {
            extra.insert(key.clone(), first.clone());
        }
    }

    AlbumMetadata {
        album,
        album_artist: common(tracks, |m| &m.album_artist).or_else(|| common(tracks, |m| &m.artist)),
        genre: common(tracks, |m| &m.genre),
        date: common(tracks, |m| &m.date),
        total_tracks,
        total_discs,
        disc_number: if total_discs.is_some() {
            tracks[0].id.disc_number
        } else {
            None
        },
        extra,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ffprobe_json_extracts_bit_depth_from_raw_sample_field() {
        let json = r#"{
            "streams": [{
                "sample_rate": "96000",
                "duration": "1.5",
                "bits_per_raw_sample": "24"
            }]
        }"#;
        let probe = parse_ffprobe_json(json).unwrap();
        assert_eq!(probe.sample_rate, 96_000);
        assert_eq!(probe.expected_samples, Some(144_000));
        assert_eq!(probe.bit_depth, Some(24));
    }

    #[test]
    fn parse_ffprobe_json_falls_back_to_bits_per_sample() {
        let json = r#"{
            "streams": [{
                "sample_rate": "44100",
                "duration": "2.0",
                "bits_per_sample": 16
            }]
        }"#;
        let probe = parse_ffprobe_json(json).unwrap();
        assert_eq!(probe.sample_rate, 44_100);
        assert_eq!(probe.expected_samples, Some(88_200));
        assert_eq!(probe.bit_depth, Some(16));
    }
    #[test]
    fn derive_album_metadata_promotes_common_extra_tags_for_folder_templates() {
        let make_track = |ordinal: u32, catalog: &str, barcode: &str| PreparedTrack {
            id: TrackId {
                source_ordinal: ordinal,
                disc_number: Some(1),
                track_number: ordinal,
            },
            source_ref: TrackSourceRef::StagedFile(PathBuf::from(format!("/stage/{ordinal:02}.flac"))),
            metadata: TrackMetadata {
                title: Some(format!("Track {ordinal}")),
                artist: Some("Miles Davis".to_string()),
                album_artist: Some("Miles Davis".to_string()),
                genre: Some("Jazz".to_string()),
                date: Some("1971".to_string()),
                track_number: Some(ordinal),
                disc_number: Some(1),
                extra: BTreeMap::from([
                    ("album".to_string(), "A Tribute to Jack Johnson".to_string()),
                    ("catalognumber".to_string(), catalog.to_string()),
                    ("barcode".to_string(), barcode.to_string()),
                ]),
                ..TrackMetadata::default()
            },
            expected_samples: None,
            sample_rate: 44_100,
            bit_depth: Some(24),
        };

        let tracks = vec![
            make_track(1, "CK-1234", "074646123426"),
            make_track(2, "CK-1234", "074646123426"),
        ];
        let album = derive_album_metadata(&tracks);

        assert_eq!(album.extra.get("catalognumber").map(String::as_str), Some("CK-1234"));
        assert_eq!(album.extra.get("barcode").map(String::as_str), Some("074646123426"));
        assert_eq!(album.album.as_deref(), Some("A Tribute to Jack Johnson"));
    }

    #[test]
    fn derive_album_metadata_does_not_promote_track_specific_extra_tags() {
        let make_track = |ordinal: u32, isrc: &str| PreparedTrack {
            id: TrackId {
                source_ordinal: ordinal,
                disc_number: Some(1),
                track_number: ordinal,
            },
            source_ref: TrackSourceRef::StagedFile(PathBuf::from(format!("/stage/{ordinal:02}.flac"))),
            metadata: TrackMetadata {
                title: Some(format!("Track {ordinal}")),
                artist: Some("Miles Davis".to_string()),
                album_artist: Some("Miles Davis".to_string()),
                extra: BTreeMap::from([
                    ("album".to_string(), "A Tribute to Jack Johnson".to_string()),
                    ("isrc".to_string(), isrc.to_string()),
                ]),
                ..TrackMetadata::default()
            },
            expected_samples: None,
            sample_rate: 44_100,
            bit_depth: Some(24),
        };

        let tracks = vec![make_track(1, "USSM17100001"), make_track(2, "USSM17100002")];
        let album = derive_album_metadata(&tracks);

        assert!(!album.extra.contains_key("isrc"));
        assert_eq!(album.extra.get("album").map(String::as_str), Some("A Tribute to Jack Johnson"));
    }

}
