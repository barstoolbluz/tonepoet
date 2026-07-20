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
use super::reporter::{PipelineEvent, PipelineReporter};
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
        reporter: Option<&dyn PipelineReporter>,
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

        let mut probe = probe_audio_file(&req.container, runner, cancel).await?;
        if probe.coding == SourceAudioCoding::Dsd {
            // ffprobe reports DSF/DFF byte rates and block-padded durations;
            // the container header carries the EXACT bit rate and per-channel
            // sample count. Prefer the header facts so post-encode sample
            // validation checks against reality, not an estimate.
            if let Ok(Some(dsd)) =
                crate::convert::pipeline::plan_bridge::dsd_source_metadata_from_path(
                    &req.container,
                )
            {
                let header_is_authoritative = !matches!(
                    dsd.validation,
                    crate::convert::pipeline::plan_bridge::DsdPlannerValidationStatus::Errors { .. }
                ) && dsd.sample_rate_hz > 0
                    && dsd.sample_count_per_channel.is_some_and(|count| count > 0);
                if header_is_authoritative {
                    probe.sample_rate = dsd.sample_rate_hz;
                    probe.expected_samples = dsd.sample_count_per_channel;
                }
            }
        }
        let (metadata, metadata_warnings) = read_track_metadata_with_warnings(&req.container)?;
        report_dsf_metadata_warnings(
            reporter,
            &req.item_id,
            &req.container,
            &metadata_warnings,
            0.5,
        )
        .await;
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
            warnings: metadata_warnings,
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
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
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
    let (sample_rate, expected_samples) =
        crate::convert::pipeline::normalize_dsd_probe_rate(coding, sample_rate, expected_samples);

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

#[cfg(test)]
pub(crate) fn read_track_metadata(path: &Path) -> Result<TrackMetadata, MaterializeError> {
    let (metadata, warnings) = read_track_metadata_with_warnings(path)?;
    for warning in &warnings {
        log::warn!(
            "DSF metadata degraded for '{}'; audio conversion will continue: {}",
            path.display(),
            warning
        );
    }
    Ok(metadata)
}

pub(crate) async fn report_dsf_metadata_warnings(
    reporter: Option<&dyn PipelineReporter>,
    item_id: &str,
    path: &Path,
    warnings: &[String],
    phase_progress: f32,
) {
    for warning in warnings {
        let message = format!(
            "DSF metadata warning for '{}': {}; audio conversion will continue",
            path.display(),
            warning
        );
        log::warn!("{message}");
        if let Some(reporter) = reporter {
            reporter
                .emit(PipelineEvent::Progress {
                    item_id: item_id.to_string(),
                    stage: PipelineStage::Materialize,
                    phase_progress: phase_progress.clamp(0.0, 1.0),
                    message: Some(message),
                })
                .await;
        }
    }
}

pub(crate) fn read_track_metadata_with_warnings(
    path: &Path,
) -> Result<(TrackMetadata, Vec<String>), MaterializeError> {
    if crate::dsf_tags::is_dsf(path) {
        let outcome = crate::dsf_tags::read_with_warnings(path)
            .map_err(MaterializeError::Parse)?;
        return Ok((
            crate::dsf_tags::to_track_metadata(&outcome.snapshot),
            outcome.warnings,
        ));
    }
    use lofty::prelude::*;

    let tagged = match lofty::read_from_path(path) {
        Ok(tagged) => tagged,
        Err(_) => return Ok((TrackMetadata::default(), Vec::new())),
    };
    let tag = match tagged.primary_tag().or_else(|| tagged.first_tag()) {
        Some(tag) => tag,
        None => return Ok((TrackMetadata::default(), Vec::new())),
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

    // Preserve every source text item with explicit provenance. The plain
    // lowercased entry remains available to naming templates; the reserved
    // marker lets the metadata writer distinguish user tags from derived
    // pipeline extras and reproduce arbitrary custom keys without renaming.
    let tag_type = tag.tag_type();
    for item in tag.items() {
        if let lofty::tag::ItemValue::Text(text) = item.value() {
            let key = item_key_to_extra_key(item.key(), tag_type);
            insert_source_text_tag(&mut extra, &key, text);
        }
    }
    let pre_emphasis = source_text_tags_indicate_pre_emphasis(&extra);

    Ok((TrackMetadata {
        title: tag.title().map(|value| value.to_string()),
        artist: tag.artist().map(|value| value.to_string()),
        album_artist: tag
            .get_string(&lofty::tag::ItemKey::AlbumArtist)
            .map(|value| value.to_string()),
        composer: tag
            .get_string(&lofty::tag::ItemKey::Composer)
            .map(|value| value.to_string()),
        performer: tag
            .get_string(&lofty::tag::ItemKey::Performer)
            .map(|value| value.to_string()),
        genre: tag.genre().map(|value| value.to_string()),
        date: tag.year().map(|value| value.to_string()),
        track_number: tag.track().map(|value| value as u32),
        disc_number: tag.disk().map(|value| value as u32),
        isrc: tag
            .get_string(&lofty::tag::ItemKey::Isrc)
            .map(|value| value.to_string()),
        publisher: tag
            .get_string(&lofty::tag::ItemKey::Publisher)
            .map(|value| value.to_string()),
        copyright: tag
            .get_string(&lofty::tag::ItemKey::CopyrightMessage)
            .map(|value| value.to_string()),
        comment: tag.comment().map(|value| value.to_string()),
        pre_emphasis,
        extra,
    }, Vec::new()))
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

    #[tokio::test]
    async fn dsf_metadata_warning_is_visible_through_pipeline_progress() {
        let reporter = super::super::reporter::RecordingReporter::new();
        let path = PathBuf::from("quirky.dsf");
        let warning = "declared DSF file size does not match the readable file length".to_string();

        report_dsf_metadata_warnings(
            Some(&reporter),
            "queue-item-7",
            &path,
            std::slice::from_ref(&warning),
            0.25,
        )
        .await;

        let events = reporter.events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            PipelineEvent::Progress {
                item_id,
                stage,
                phase_progress,
                message,
            } => {
                assert_eq!(item_id, "queue-item-7");
                assert_eq!(*stage, PipelineStage::Materialize);
                assert_eq!(*phase_progress, 0.25);
                assert_eq!(
                    message.as_deref(),
                    Some(
                        "DSF metadata warning for 'quirky.dsf': declared DSF file size does not match the readable file length; audio conversion will continue"
                    )
                );
            }
            other => panic!("expected materialization progress warning, got {other:?}"),
        }
    }

    #[test]
    fn source_text_items_preserve_custom_tag_provenance_and_promote_pre_emphasis() {
        use lofty::config::WriteOptions;
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::tag::{ItemKey, ItemValue, TagItem};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("source.flac");
        std::fs::write(&path, include_bytes!("../../../tests/fixtures/silence.flac"))
            .expect("write FLAC fixture");
        let mut tagged = lofty::read_from_path(&path).expect("read FLAC fixture");
        if tagged.primary_tag().is_none() {
            let tag_type = tagged.primary_tag_type();
            tagged.insert_tag(lofty::tag::Tag::new(tag_type));
        }
        let tag = tagged.primary_tag_mut().expect("primary FLAC tag");
        for (key, value) in [("PRE_EMPHASIS", "1"), ("MY_NOTE", "keep me")] {
            let key = ItemKey::Unknown(key.to_string());
            tag.remove_key(&key);
            tag.insert_unchecked(TagItem::new(key, ItemValue::Text(value.to_string())));
        }
        tagged
            .save_to_path(&path, WriteOptions::default())
            .expect("save FLAC fixture tags");

        let metadata = read_track_metadata(&path).expect("read source metadata");
        assert!(metadata.pre_emphasis);
        assert_eq!(metadata.extra.get("my_note").map(String::as_str), Some("keep me"));
        assert_eq!(
            metadata
                .extra
                .get(&format!("{SOURCE_TEXT_TAG_EXTRA_PREFIX}my_note"))
                .map(String::as_str),
            Some("keep me")
        );
        assert_eq!(
            metadata
                .extra
                .get(&format!("{SOURCE_TEXT_TAG_EXTRA_PREFIX}pre_emphasis"))
                .map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn corrupt_dsf_metadata_degrades_to_empty_metadata_for_conversion() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("corrupt.dsf");
        crate::dsf_tags::write_test_dsf_fixture(&path, Some(b"NOT-AN-ID3-TAG"))
            .expect("write DSF with corrupt metadata area");

        let metadata = read_track_metadata(&path)
            .expect("unreadable DSF tag bytes must not block audio conversion");
        // Split across two asserts: 15-element tuples have no PartialEq/Debug.
        assert_eq!(
            (
                metadata.title.as_deref(),
                metadata.artist.as_deref(),
                metadata.album_artist.as_deref(),
                metadata.composer.as_deref(),
                metadata.performer.as_deref(),
                metadata.genre.as_deref(),
                metadata.date.as_deref(),
                metadata.track_number,
                metadata.disc_number,
                metadata.isrc.as_deref(),
            ),
            (None, None, None, None, None, None, None, None, None, None),
        );
        assert_eq!(
            (
                metadata.publisher.as_deref(),
                metadata.copyright.as_deref(),
                metadata.comment.as_deref(),
                metadata.pre_emphasis,
                metadata.extra.len(),
            ),
            (None, None, None, false, 0),
        );
    }

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
            warnings: Vec::new(),
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
