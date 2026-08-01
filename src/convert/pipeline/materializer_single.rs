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
        let (metadata, mut metadata_warnings, metadata_recovered_by_fallback) =
            read_track_metadata_with_warnings(&req.container)?;
        if req
            .album_batch
            .as_ref()
            .is_some_and(|batch| batch.uses_completion_order())
        {
            metadata_warnings.push(completion_order_metadata_warning(
                metadata.track_number,
                req.album_batch_track.as_ref(),
            ));
        }
        report_metadata_warnings(
            reporter,
            &req.item_id,
            &req.container,
            &metadata_warnings,
            0.5,
        )
        .await;
        let track_number = single_file_filename_track_number(
            metadata.track_number,
            req.album_batch_track.as_ref(),
        );
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
        let album_metadata = derive_single_file_album_metadata(
            &tracks,
            metadata_recovered_by_fallback,
        );
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


fn derive_single_file_album_metadata(
    tracks: &[PreparedTrack],
    metadata_recovered_by_fallback: bool,
) -> AlbumMetadata {
    let mut album_metadata = derive_album_metadata(tracks);
    if metadata_recovered_by_fallback {
        // Recovered numeric totals live in the immutable fallback snapshot under
        // the NUL-prefixed canonical namespace, not the ordinary `extra` map, so
        // the planning-side album counts must be sourced from there. Later label,
        // batch-identity, or path enrichment cannot invalidate these source-proven
        // values. `album`, `genre`, `date`, `album_artist`, and `disc_number` are
        // already carried by the ordinary/typed fields the fallback reader
        // populates, so only the totals need snapshot sourcing here.
        let snapshot_total = |canonical_keys: &[&str]| {
            tracks.first().and_then(|track| {
                canonical_keys.iter().find_map(|key| {
                    fallback_source_tag_value(&track.metadata.extra, key)
                        .and_then(|value| value.trim().parse::<u32>().ok())
                        .filter(|value| *value > 0)
                })
            })
        };
        if let Some(source_total) = snapshot_total(&["TRACKTOTAL", "TOTALTRACKS"]) {
            album_metadata.total_tracks = source_total;
        }
        if let Some(source_disc_total) = snapshot_total(&["DISCTOTAL", "TOTALDISCS"]) {
            album_metadata.total_discs = Some(source_disc_total);
        }
        album_metadata.extra.insert(
            FALLBACK_RECOVERED_METADATA_EXTRA_KEY.to_string(),
            "native-apev2".to_string(),
        );
    }
    album_metadata
}

fn completion_order_metadata_warning(
    metadata_track_number: Option<u32>,
    batch_track: Option<&AlbumBatchTrackContext>,
) -> String {
    let mut warning =
        "Track ordering unavailable; album publication is shared and the conversion log records tracks in completion order"
            .to_string();
    if metadata_track_number.is_none() && batch_track.is_some() {
        warning.push_str(
            "; filenames numbered by dispatch order; no TRACKNUMBER tags written",
        );
    }
    warning
}

pub(crate) fn single_file_filename_track_number(
    metadata_track_number: Option<u32>,
    batch_track: Option<&AlbumBatchTrackContext>,
) -> u32 {
    metadata_track_number
        .or_else(|| batch_track.map(|track| track.track_number))
        .unwrap_or(1)
        .max(1)
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
    let (metadata, warnings, _) = read_track_metadata_with_warnings(path)?;
    for warning in &warnings {
        log::warn!(
            "metadata degraded for '{}'; audio conversion will continue: {}",
            path.display(),
            warning
        );
    }
    Ok(metadata)
}

pub(crate) async fn report_metadata_warnings(
    reporter: Option<&dyn PipelineReporter>,
    item_id: &str,
    path: &Path,
    warnings: &[String],
    phase_progress: f32,
) {
    for warning in warnings {
        let message = format!(
            "Metadata warning for '{}': {}; audio conversion will continue",
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
) -> Result<(TrackMetadata, Vec<String>, bool), MaterializeError> {
    if crate::dsf_tags::is_dsf(path) {
        let outcome = crate::dsf_tags::read_with_warnings(path)
            .map_err(MaterializeError::Parse)?;
        return Ok((
            crate::dsf_tags::to_track_metadata(&outcome.snapshot),
            outcome.warnings,
            false,
        ));
    }
    use lofty::prelude::*;

    match lofty::read_from_path(path) {
        Ok(tagged) => {
            let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
                return Ok((
                    TrackMetadata::default(),
                    vec![format!(
                        "Tag read: FAILED (no readable metadata tag found in '{}') - converted without metadata",
                        path.display()
                    )],
                    false,
                ));
            };
            Ok((track_metadata_from_lofty_tag(tag), Vec::new(), false))
        }
        Err(lofty_error)
            if crate::metadata_persistence::native_ape_error_is_eligible(&lofty_error) =>
        {
            match crate::metadata_persistence::read_native_ape_fallback(path) {
                Ok(outcome) => {
                    let metadata = track_metadata_from_neutral_ape_rows(&outcome.rows);
                    let warning = outcome.warning.map(|warning| warning.message()).unwrap_or_else(|| {
                        format!(
                            "Lofty rejected the APEv2 tag in '{}': {lofty_error}; recovered all readable fields with the bounded native reader",
                            path.display()
                        )
                    });
                    Ok((metadata, vec![warning], true))
                }
                Err(native_error) => Ok((
                    TrackMetadata::default(),
                    vec![format!(
                        "Tag read: FAILED ({lofty_error}; native APEv2 fallback refused: {native_error}) - converted without metadata"
                    )],
                    false,
                )),
            }
        }
        Err(error) => Ok((
            TrackMetadata::default(),
            vec![format!(
                "Tag read: FAILED ({error}) - converted without metadata"
            )],
            false,
        )),
    }
}

fn track_metadata_from_lofty_tag(tag: &lofty::tag::Tag) -> TrackMetadata {
    use lofty::prelude::*;

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

    TrackMetadata {
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
    }
}

fn track_metadata_from_neutral_ape_rows(
    rows: &[crate::metadata_persistence::NeutralApeRow],
) -> TrackMetadata {
    let text = |canonical_key: &str| {
        rows.iter()
            .find(|row| row.canonical_key == canonical_key && !row.is_binary)
            .map(|row| row.value.clone())
    };
    let number = |canonical_key: &str| {
        text(canonical_key).and_then(|value| value.trim().parse::<u32>().ok())
    };
    let year = || {
        let value = text("DATE")?;
        let leading = value.trim().chars().take(4).collect::<String>();
        (leading.len() == 4 && leading.chars().all(|character| character.is_ascii_digit()))
            .then_some(leading)
    };

    let mut extra = BTreeMap::new();
    for row in rows.iter().filter(|row| !row.is_binary) {
        let key = match &row.item_key {
            lofty::tag::ItemKey::Unknown(_) => row.raw_key.to_ascii_lowercase(),
            key => item_key_to_extra_key(key, lofty::tag::TagType::Ape),
        };
        insert_source_text_tag(&mut extra, &key, &row.value);
        // Capture the fallback reader's immutable canonical source value before
        // any album-label, batch-identity, or path-derived enrichment can alter
        // the ordinary metadata model used for naming and organization.
        insert_fallback_source_tag(&mut extra, &row.canonical_key, &row.value);
    }
    if let Some(album) = text("ALBUM") {
        extra.insert("album".to_string(), album);
    }
    if let Some(total) = text("DISCTOTAL") {
        extra.insert("disctotal".to_string(), total);
    }
    let pre_emphasis = source_text_tags_indicate_pre_emphasis(&extra);

    TrackMetadata {
        title: text("TITLE"),
        artist: text("ARTIST"),
        album_artist: text("ALBUMARTIST"),
        composer: text("COMPOSER"),
        performer: text("PERFORMER"),
        genre: text("GENRE"),
        date: year(),
        track_number: number("TRACKNUMBER"),
        disc_number: number("DISCNUMBER"),
        isrc: text("ISRC"),
        publisher: text("PUBLISHER"),
        copyright: text("COPYRIGHT"),
        comment: text("COMMENT"),
        pre_emphasis,
        extra,
    }
}

pub(super) fn item_key_to_extra_key(
    key: &lofty::tag::ItemKey,
    tag_type: lofty::tag::TagType,
) -> String {
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

        report_metadata_warnings(
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
                        "Metadata warning for 'quirky.dsf': declared DSF file size does not match the readable file length; audio conversion will continue"
                    )
                );
            }
            other => panic!("expected materialization progress warning, got {other:?}"),
        }
    }

    #[test]
    fn invalid_ape_fallback_marks_recovery_and_preserves_full_valid_tag_set() {
        use lofty::tag::ItemKey;

        let temp = tempfile::tempdir().expect("invalid APE materializer tempdir");
        let path = temp.path().join("supertramp-invalid-key.wv");
        std::fs::write(
            &path,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/metadata_persistence/ape.wv"
            )),
        )
        .expect("write APE fixture");
        crate::tui::probe::write_all_tags(
            &path,
            &[
                (ItemKey::TrackTitle, Some("Give a Little Bit".to_string())),
                (ItemKey::TrackArtist, Some("Supertramp".to_string())),
                (ItemKey::AlbumTitle, Some("Even in the Quietest Moments...".to_string())),
                (ItemKey::Genre, Some("Rock".to_string())),
                (ItemKey::Year, Some("1977".to_string())),
                (ItemKey::Comment, Some("US A&M SP-4634".to_string())),
                (ItemKey::TrackNumber, Some("1".to_string())),
                (ItemKey::TrackTotal, Some("7".to_string())),
                (
                    ItemKey::Unknown("ALBUM ARTIST".to_string()),
                    Some("Supertramp".to_string()),
                ),
            ],
        )
        .expect("seed valid APE fields");
        crate::tui::probe::inject_invalid_ape_key_item_for_test(
            &path,
            "&год".as_bytes(),
            b"invalid",
        )
        .expect("inject invalid APE key");

        let (metadata, warnings, recovered) =
            read_track_metadata_with_warnings(&path).expect("tolerant fallback read");

        assert!(recovered, "the native fallback must set the recovery authority bit");
        assert!(warnings.iter().any(|warning| {
            warning.contains("invalid APE key skipped") && warning.contains("&год")
        }));
        assert_eq!(metadata.title.as_deref(), Some("Give a Little Bit"));
        assert_eq!(metadata.artist.as_deref(), Some("Supertramp"));
        assert_eq!(metadata.album_artist.as_deref(), Some("Supertramp"));
        assert_eq!(metadata.genre.as_deref(), Some("Rock"));
        assert_eq!(metadata.date.as_deref(), Some("1977"));
        assert_eq!(metadata.comment.as_deref(), Some("US A&M SP-4634"));
        assert_eq!(metadata.track_number, Some(1));
        assert_eq!(
            metadata.extra.get("album").map(String::as_str),
            Some("Even in the Quietest Moments...")
        );
        for (key, expected) in [
            ("TITLE", "Give a Little Bit"),
            ("ARTIST", "Supertramp"),
            ("ALBUM", "Even in the Quietest Moments..."),
            ("ALBUMARTIST", "Supertramp"),
            ("GENRE", "Rock"),
            ("DATE", "1977"),
            ("COMMENT", "US A&M SP-4634"),
            ("TRACKNUMBER", "1"),
            ("TRACKTOTAL", "7"),
        ] {
            assert_eq!(
                fallback_source_tag_value(&metadata.extra, key),
                Some(expected),
                "fallback source-authority snapshot missing {key}"
            );
        }

        let track = PreparedTrack {
            id: TrackId {
                source_ordinal: 1,
                disc_number: metadata.disc_number,
                track_number: metadata.track_number.unwrap_or(1),
            },
            source_ref: TrackSourceRef::StagedFile(path.clone()),
            metadata,
            expected_samples: None,
            sample_rate: Some(44_100),
            bit_depth: Some(24),
            source_audio: SourceAudioDescriptor::from_scalar(
                Some(44_100),
                Some(24),
                Some(SourceAudioCoding::Pcm),
            ),
            warnings,
        };
        let tracks = vec![track];
        let album_metadata = derive_single_file_album_metadata(&tracks, recovered);
        assert_eq!(album_metadata.total_tracks, 7);
        assert_eq!(album_metadata.album_artist.as_deref(), Some("Supertramp"));
        let prepared = PreparedSource {
            container: path,
            kind: SourceKind::SingleFile,
            album_metadata,
            tracks,
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SingleFile,
                source_sha256: None,
                tool_versions: BTreeMap::new(),
                extracted_at: chrono::Utc::now(),
            },
        };
        assert!(
            crate::convert::pipeline::plan_bridge::source_needs_authoritative_metadata(&prepared),
            "the actual invalid-APEv2 recovery result must require the authoritative metadata stage"
        );
        let tags = crate::convert::pipeline::stages::authoritative_metadata_tags(
            &prepared.tracks[0].metadata,
            &prepared.album_metadata,
        );
        for expected in [
            ("TITLE", "Give a Little Bit"),
            ("ARTIST", "Supertramp"),
            ("ALBUM", "Even in the Quietest Moments..."),
            ("ALBUMARTIST", "Supertramp"),
            ("GENRE", "Rock"),
            ("DATE", "1977"),
            ("COMMENT", "US A&M SP-4634"),
            ("TRACKNUMBER", "1"),
            ("TRACKTOTAL", "7"),
        ] {
            assert!(
                tags.iter().any(|(key, value)| key == expected.0 && value == expected.1),
                "the authoritative writer did not receive recovered {expected:?}: {tags:?}"
            );
        }
        assert!(!tags.iter().any(|(key, _)| {
            key == "ALBUM ARTIST" || key == "TONEPOET_FALLBACK_RECOVERED_METADATA"
        }));
    }

    #[test]
    fn fallback_authority_does_not_promote_single_file_planning_defaults_to_tags() {
        let track = PreparedTrack {
            id: TrackId {
                source_ordinal: 1,
                disc_number: Some(1),
                track_number: 1,
            },
            source_ref: TrackSourceRef::StagedFile(PathBuf::from("untagged.wv")),
            metadata: TrackMetadata {
                artist: Some("Solo Artist".to_string()),
                disc_number: Some(1),
                extra: BTreeMap::from([("album".to_string(), "Source Album".to_string())]),
                ..TrackMetadata::default()
            },
            expected_samples: None,
            sample_rate: Some(44_100),
            bit_depth: Some(24),
            source_audio: SourceAudioDescriptor::from_scalar(
                Some(44_100),
                Some(24),
                Some(SourceAudioCoding::Pcm),
            ),
            warnings: Vec::new(),
        };

        let album = derive_single_file_album_metadata(&[track], true);

        // Planning metadata retains the established single-file defaults. The
        // authoritative writer is responsible for distinguishing these
        // organizational values from source-evidenced tags.
        assert_eq!(album.album.as_deref(), Some("Source Album"));
        assert_eq!(album.album_artist.as_deref(), Some("Solo Artist"));
        assert_eq!(album.total_tracks, 1);
        assert_eq!(album.total_discs, Some(1));
        assert!(album.extra.contains_key(FALLBACK_RECOVERED_METADATA_EXTRA_KEY));
    }

    #[test]
    fn dispatch_ordinal_is_filename_only_fallback_for_untagged_tracks() {
        let batch_track = AlbumBatchTrackContext::new(7, None, 7);
        assert_eq!(
            single_file_filename_track_number(None, Some(&batch_track)),
            7
        );
        assert_eq!(
            single_file_filename_track_number(Some(3), Some(&batch_track)),
            3,
            "source TRACKNUMBER remains authoritative"
        );
        assert!(completion_order_metadata_warning(None, Some(&batch_track))
            .contains("filenames numbered by dispatch order; no TRACKNUMBER tags written"));
        assert!(!completion_order_metadata_warning(Some(3), Some(&batch_track))
            .contains("filenames numbered by dispatch order"));
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
    fn tolerant_ape_rows_populate_named_fields_and_full_provenance() {
        use crate::metadata_persistence::NeutralApeRow;
        use lofty::tag::ItemKey;

        let rows = [
            ("Title", "TITLE", ItemKey::TrackTitle, "Give a Little Bit"),
            ("Artist", "ARTIST", ItemKey::TrackArtist, "Supertramp"),
            (
                "Album",
                "ALBUM",
                ItemKey::AlbumTitle,
                "Even in the Quietest Moments...",
            ),
            ("Genre", "GENRE", ItemKey::Genre, "Rock"),
            ("Year", "DATE", ItemKey::Year, "1977"),
            (
                "Comment",
                "COMMENT",
                ItemKey::Comment,
                "US A&M SP-4634 LP",
            ),
            (
                "MY_NOTE",
                "MY_NOTE",
                ItemKey::Unknown("MY_NOTE".to_string()),
                "keep me",
            ),
        ]
        .into_iter()
        .map(|(raw_key, canonical_key, item_key, value)| NeutralApeRow {
            raw_key: raw_key.to_string(),
            canonical_key: canonical_key.to_string(),
            item_key,
            value: value.to_string(),
            is_binary: false,
        })
        .collect::<Vec<_>>();

        let metadata = track_metadata_from_neutral_ape_rows(&rows);

        assert_eq!(metadata.title.as_deref(), Some("Give a Little Bit"));
        assert_eq!(metadata.artist.as_deref(), Some("Supertramp"));
        assert_eq!(
            metadata.extra.get("album").map(String::as_str),
            Some("Even in the Quietest Moments...")
        );
        assert_eq!(metadata.genre.as_deref(), Some("Rock"));
        assert_eq!(metadata.date.as_deref(), Some("1977"));
        assert_eq!(metadata.comment.as_deref(), Some("US A&M SP-4634 LP"));
        assert_eq!(metadata.extra.get("my_note").map(String::as_str), Some("keep me"));
        assert_eq!(
            metadata
                .extra
                .get(&format!("{SOURCE_TEXT_TAG_EXTRA_PREFIX}my_note"))
                .map(String::as_str),
            Some("keep me")
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
