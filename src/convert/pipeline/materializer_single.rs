//! Single-file materializer for the unified path.
//!
//! A single audio file becomes a one-track `PreparedSource`. The same planner,
//! executor, metadata, ReplayGain, publish, and logging stages then handle it.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::errors::{MaterializeError, ToolRunnerError};
use super::reporter::PipelineReporter;
use super::tool::{ToolBinary, ToolCommand, ToolRunner};
use super::types::*;

pub struct SingleFileMaterializer;

#[async_trait]
impl super::stages::Materializer for SingleFileMaterializer {
    async fn materialize(
        &self,
        req: &PipelineRequest,
        _staging: &StagingDir,
        runner: &dyn ToolRunner,
        _reporter: Option<&dyn PipelineReporter>,
        _tool_paths: &HashMap<String, std::path::PathBuf>,
        cancel: &CancellationToken,
    ) -> Result<PreparedSource, MaterializeError> {
        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }
        if !req.container.is_file() {
            return Err(MaterializeError::Extraction(format!(
                "single-file source is not a regular file: {}",
                req.container.display()
            )));
        }

        let probe = probe_audio_file(&req.container, runner, cancel).await?;
        let metadata = read_track_metadata(&req.container);
        let track_number = metadata.track_number.unwrap_or(1).max(1);
        let track = PreparedTrack {
            id: TrackId {
                source_ordinal: 1,
                disc_number: metadata.disc_number,
                track_number,
            },
            source_ref: TrackSourceRef::StagedFile(req.container.clone()),
            metadata,
            expected_samples: probe.expected_samples,
            sample_rate: Some(probe.sample_rate),
            bit_depth: probe.bit_depth,
            source_audio: SourceAudioDescriptor::from_scalar(
                Some(probe.sample_rate),
                probe.bit_depth,
                Some(probe.coding),
            ),
        };

        let tracks = apply_track_selection(vec![track], &req.source.track_selection)?;
        let album_metadata = derive_album_metadata(&tracks);
        Ok(PreparedSource {
            container: req.container.clone(),
            kind: SourceKind::SingleFile,
            tracks,
            album_metadata,
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SingleFile,
                source_sha256: None,
                tool_versions: BTreeMap::new(),
                extracted_at: chrono::Utc::now(),
            },
        })
    }
}

struct ProbeResult {
    sample_rate: u32,
    expected_samples: Option<u64>,
    bit_depth: Option<u32>,
    coding: SourceAudioCoding,
}

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
            "stream=sample_rate,duration,bits_per_raw_sample,bits_per_sample,sample_fmt,codec_name".into(),
            "-show_entries".into(),
            "format=duration".into(),
            "-of".into(),
            "json".into(),
            path.to_string_lossy().into_owned(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(30),
    };

    let output = match runner.run(cmd, cancel).await {
        Ok(output) => output,
        Err(ToolRunnerError::Cancelled { .. }) => return Err(MaterializeError::Cancelled),
        Err(err) => return Err(err.into()),
    };
    parse_ffprobe_json(&output.stdout_tail)
}

fn parse_ffprobe_json(json: &str) -> Result<ProbeResult, MaterializeError> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|err| MaterializeError::Parse(format!("ffprobe JSON parse failed: {err}")))?;
    let sample_rate = value
        .pointer("/streams/0/sample_rate")
        .and_then(|value| value.as_str())
        .and_then(|text| text.parse::<u32>().ok())
        .unwrap_or(0);
    if sample_rate == 0 {
        return Err(MaterializeError::Parse(
            "ffprobe returned no valid sample_rate".to_string(),
        ));
    }

    let duration_secs = value
        .pointer("/streams/0/duration")
        .and_then(|value| value.as_str())
        .and_then(|text| text.parse::<f64>().ok())
        .or_else(|| {
            value
                .pointer("/format/duration")
                .and_then(|value| value.as_str())
                .and_then(|text| text.parse::<f64>().ok())
        });
    let expected_samples = duration_secs.map(|secs| (secs * f64::from(sample_rate)).round() as u64);
    let integer_bit_depth = value
        .pointer("/streams/0/bits_per_raw_sample")
        .and_then(json_u32)
        .filter(|bits| *bits > 0)
        .or_else(|| {
            value
                .pointer("/streams/0/bits_per_sample")
                .and_then(json_u32)
                .filter(|bits| *bits > 0)
        });
    let codec_name = value
        .pointer("/streams/0/codec_name")
        .and_then(|value| value.as_str());
    let sample_fmt = value
        .pointer("/streams/0/sample_fmt")
        .and_then(|value| value.as_str());
    let (coding, bit_depth) =
        classify_source_audio_probe(codec_name, sample_fmt, integer_bit_depth);

    Ok(ProbeResult {
        sample_rate,
        expected_samples,
        bit_depth,
        coding,
    })
}

fn json_u32(value: &serde_json::Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .or_else(|| value.as_str().and_then(|text| text.parse::<u32>().ok()))
}

pub(crate) fn read_track_metadata(path: &Path) -> TrackMetadata {
    use lofty::prelude::*;

    let tagged = match lofty::read_from_path(path) {
        Ok(tagged) => tagged,
        Err(_) => return TrackMetadata::default(),
    };
    let tag = match tagged.primary_tag().or_else(|| tagged.first_tag()) {
        Some(tag) => tag,
        None => return TrackMetadata::default(),
    };

    let mut extra = BTreeMap::new();
    if let Some(album) = tag.album() {
        extra.insert("album".to_string(), album.to_string());
    }
    // Disc totals prove a multi-disc layout even when this materializer sees a
    // single track (folder album batches dispatch one request per file). The
    // template disc-folder machinery reads this through the "disctotal" hint.
    if let Some(total) = tag.disk_total().filter(|value| *value > 0) {
        extra.insert("disctotal".to_string(), total.to_string());
    }

    TrackMetadata {
        title: tag.title().map(|value| value.to_string()),
        artist: tag.artist().map(|value| value.to_string()),
        album_artist: tag
            .get_string(&lofty::tag::ItemKey::AlbumArtist)
            .map(|value| value.to_string()),
        genre: tag.genre().map(|value| value.to_string()),
        date: tag.year().map(|value| value.to_string()),
        track_number: tag.track().map(|value| value as u32),
        disc_number: tag.disk().map(|value| value as u32),
        comment: tag.comment().map(|value| value.to_string()),
        extra,
        ..TrackMetadata::default()
    }
}

fn apply_track_selection(
    tracks: Vec<PreparedTrack>,
    selection: &TrackSelection,
) -> Result<Vec<PreparedTrack>, MaterializeError> {
    match selection {
        TrackSelection::All => Ok(tracks),
        TrackSelection::Range { start, end } if *start <= 1 && *end >= 1 => Ok(tracks),
        TrackSelection::Set(set) if set.contains(&1) => Ok(tracks),
        TrackSelection::Range { .. } | TrackSelection::Set(_) => Ok(Vec::new()),
    }
}

fn derive_album_metadata(tracks: &[PreparedTrack]) -> AlbumMetadata {
    if tracks.is_empty() {
        return AlbumMetadata::default();
    }
    let metadata = &tracks[0].metadata;
    AlbumMetadata {
        album: metadata.extra.get("album").cloned(),
        album_artist: metadata.album_artist.clone().or_else(|| metadata.artist.clone()),
        genre: metadata.genre.clone(),
        date: metadata.date.clone(),
        total_tracks: tracks.len() as u32,
        total_discs: metadata
            .extra
            .get("disctotal")
            .and_then(|value| value.trim().parse::<u32>().ok())
            .filter(|value| *value > 0)
            .or_else(|| metadata.disc_number.map(|_| 1)),
        disc_number: metadata.disc_number,
        extra: metadata.extra.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn track_with_metadata(metadata: TrackMetadata) -> PreparedTrack {
        PreparedTrack {
            id: TrackId {
                source_ordinal: 1,
                disc_number: metadata.disc_number,
                track_number: metadata.track_number.unwrap_or(1),
            },
            source_ref: TrackSourceRef::StagedFile(PathBuf::from(
                "/library/album/disc 1/01 - Track.flac",
            )),
            metadata,
            expected_samples: None,
            sample_rate: Some(44_100),
            bit_depth: Some(16),
            source_audio: SourceAudioDescriptor::default(),
        }
    }

    #[test]
    fn derive_album_metadata_reads_disc_total_from_tag_extras() {
        let mut metadata = TrackMetadata::default();
        metadata.disc_number = Some(1);
        metadata.track_number = Some(1);
        metadata
            .extra
            .insert("disctotal".to_string(), "2".to_string());

        let album = derive_album_metadata(&[track_with_metadata(metadata)]);
        assert_eq!(album.total_discs, Some(2));
        assert_eq!(album.disc_number, Some(1));
    }

    #[test]
    fn derive_album_metadata_without_disc_total_defaults_to_single_disc() {
        let mut metadata = TrackMetadata::default();
        metadata.disc_number = Some(1);
        metadata.track_number = Some(1);

        let album = derive_album_metadata(&[track_with_metadata(metadata)]);
        assert_eq!(album.total_discs, Some(1));
    }
}
