//! PR 1 — `Materializer` trait, every public stage-function
//! signature, the real `aggregate_album_outcome`, and the
//! `AlbumOutcome` → `ConversionStatus` mapping.
//!
//! Per the plan: PR 1 ships compiling, non-panicking stub bodies for
//! the free functions below. PRs 2–10 replace those bodies without
//! changing the signatures. `aggregate_album_outcome` and
//! `map_album_outcome` are real implementations in PR 1 — they are
//! pure logic and are exercised by PR 1's exit-condition tests.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::errors::{
    ConvertError, FeatureError, LogError, MaterializeError, MergeError, MetadataError,
    PlanError, PublishError, ReplayGainError, RequestValidationError, SourceDetectError,
    SourceDispatchError,
};
use super::reporter::PipelineReporter;
use super::tool::{ToolBinary, ToolCommand, ToolRunner};
use super::types::*;
use crate::convert::ConversionStatus;

// ===========================================================================
// Materializer trait
// ===========================================================================

#[async_trait]
pub trait Materializer: Send + Sync {
    /// Parse/unpack a container into a `PreparedSource`. Describes
    /// tracks; never cuts, decodes, transcodes, or encodes audio.
    async fn materialize(
        &self,
        req: &PipelineRequest,
        staging: &StagingDir,
        runner: &dyn ToolRunner,
        cancel: &CancellationToken,
    ) -> Result<PreparedSource, MaterializeError>;
}

// ===========================================================================
// Stage-function result aggregate
// ===========================================================================

/// Output of the `convert` stage: per-track records, the staged
/// artifacts for successful tracks, and the stage's own record.
#[derive(Debug, Clone)]
pub struct ConvertStageResult {
    pub tracks: Vec<TrackRecord>,
    pub artifacts: ArtifactSet,
    pub record: StageRecord,
}

// ===========================================================================
// Request validation / source detection / dispatch  (PR 4 bodies)
// ===========================================================================

/// PR 1 stub: accepts every request. PR 4 implements real validation.
pub fn validate_request(_req: &PipelineRequest) -> Result<(), RequestValidationError> {
    Ok(())
}

/// PR 1 stub: no detection rule wired. PR 4 adds the 7z rule, PR 8
/// the CUE rule, PR 9 the SACD rule.
pub fn detect_source_kind(_req: &PipelineRequest) -> Result<SourceKind, SourceDetectError> {
    Err(SourceDetectError::UnknownSource)
}

/// PR 1 stub: no materializer registered. PR 3/8/9 wire their arms.
pub fn materializer_for(
    kind: SourceKind,
) -> Result<Box<dyn Materializer>, SourceDispatchError> {
    Err(SourceDispatchError::Unsupported(kind))
}

// ===========================================================================
// Per-track realize  (PR 4 = StagedFile arm; PR 8 = ImageSegment;
// PR 9 = SacdTrack)
// ===========================================================================

/// PR 1 stub: no arm implemented. PR 4 fills the `StagedFile`
/// identity arm; PRs 8/9 fill the others.
pub async fn realize_track(
    _src: &TrackSourceRef,
    _req: &PipelineRequest,
    _staging: &StagingDir,
    _runner: &dyn ToolRunner,
    _cancel: &CancellationToken,
) -> Result<PathBuf, ConvertError> {
    Err(ConvertError::UnsupportedTrackSource)
}

// ===========================================================================
// Output planning  (PR 4 body)
// ===========================================================================

/// PR 1 stub: no planner. PR 4 implements path assignment.
pub fn plan_outputs(
    _source: &PreparedSource,
    _req: &PipelineRequest,
) -> Result<AlbumPlan, PlanError> {
    Err(PlanError::EmptyManifest)
}

// ===========================================================================
// Convert / merge / metadata / replaygain / features  (PR 4–6 bodies)
// ===========================================================================

/// PR 1 stub: produces no tracks, no artifacts, a `Skipped` record.
pub async fn convert_tracks(
    _source: &PreparedSource,
    _plan: &AlbumPlan,
    _req: &PipelineRequest,
    _staging: &StagingDir,
    _runner: &dyn ToolRunner,
    _cancel: &CancellationToken,
) -> ConvertStageResult {
    ConvertStageResult {
        tracks: Vec::new(),
        artifacts: ArtifactSet {
            audio: AudioArtifacts::Tracks(Vec::new()),
            sidecars: Vec::new(),
        },
        record: StageRecord {
            stage: PipelineStage::Convert,
            outcome: StageOutcome::Skipped,
        },
    }
}

/// Merge per-track audio artifacts into a single file using ffmpeg's
/// concat demuxer (`-c copy`, no re-encoding). Skipped when
/// `req.merge` is false.
pub async fn merge_tracks(
    artifacts: ArtifactSet,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(ArtifactSet, StageRecord), MergeError> {
    // ---- skip when merge is off ----------------------------------------
    if !req.merge {
        return Ok((
            artifacts,
            StageRecord { stage: PipelineStage::Merge, outcome: StageOutcome::Skipped },
        ));
    }

    // ---- extract per-track artifacts -----------------------------------
    let (track_artifacts, sidecars) = match artifacts.audio {
        AudioArtifacts::Tracks(tracks) => (tracks, artifacts.sidecars),
        AudioArtifacts::Merged(_) => {
            return Err(MergeError::UnsupportedFormat(
                "artifacts are already merged".into(),
            ));
        }
    };

    if track_artifacts.is_empty() {
        return Err(MergeError::UnsupportedFormat(
            "no track artifacts to merge".into(),
        ));
    }

    // ---- single track: wrap as MergedArtifact, no concat ---------------
    if track_artifacts.len() == 1 {
        let t = &track_artifacts[0];
        let merged = MergedArtifact {
            staged_path: t.staged_path.clone(),
            final_path: t.final_path.clone(),
            total_samples: t.samples.unwrap_or(0),
            source_tracks: vec![t.track_id.clone()],
        };
        return Ok((
            ArtifactSet { audio: AudioArtifacts::Merged(merged), sidecars },
            StageRecord { stage: PipelineStage::Merge, outcome: StageOutcome::Ok },
        ));
    }

    // ---- multi-track: ffmpeg concat ------------------------------------
    let ext = track_artifacts[0]
        .staged_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("flac");
    let merged_staged = staging.root.join(format!("merged.{}", ext));
    let concat_list = staging.root.join("_merge_concat.txt");

    // Build the concat list file.
    let mut list_content = String::new();
    for t in &track_artifacts {
        let escaped = t.staged_path.display().to_string().replace('\'', "'\\''");
        list_content.push_str(&format!("file '{}'\n", escaped));
    }
    std::fs::write(&concat_list, &list_content)?;

    // Build and run the ffmpeg concat command.
    let cmd = ToolCommand {
        binary: ToolBinary::Ffmpeg,
        args: vec![
            "-y".into(),
            "-f".into(), "concat".into(),
            "-safe".into(), "0".into(),
            "-i".into(), concat_list.display().to_string(),
            "-c".into(), "copy".into(),
            merged_staged.display().to_string(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(3600),
    };

    let merge_result = runner.run(cmd, cancel).await;

    // Clean up the concat list regardless of outcome.
    let _ = std::fs::remove_file(&concat_list);

    // Handle merge failure — delete partial output.
    if let Err(e) = merge_result {
        let _ = std::fs::remove_file(&merged_staged);
        return Err(e.into()); // ToolRunnerError → MergeError::Tool
    }

    // ---- validate merged output via ffprobe ----------------------------
    // (If cancellation fired after the merge but before the probe,
    // the next runner.run() call catches it via the CancellationToken.)
    let probe_cmd = ToolCommand {
        binary: ToolBinary::Ffprobe,
        args: vec![
            "-v".into(), "error".into(),
            "-select_streams".into(), "a:0".into(),
            "-show_entries".into(), "stream=sample_rate,duration".into(),
            "-show_entries".into(), "format=duration".into(),
            "-of".into(), "json".into(),
            merged_staged.display().to_string(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(30),
    };

    let probe_output = match runner.run(probe_cmd, cancel).await {
        Ok(o) => o,
        Err(e) => {
            let _ = std::fs::remove_file(&merged_staged);
            return Err(e.into());
        }
    };

    let (actual_sample_rate, actual_duration) =
        parse_merge_probe(&probe_output.stdout_tail)?;
    let actual_samples = (actual_duration * actual_sample_rate as f64).round() as u64;

    // Validate against expected sum when all input tracks have sample counts.
    let expected_sum: Option<u64> = track_artifacts
        .iter()
        .map(|t| t.samples)
        .collect::<Option<Vec<u64>>>()
        .map(|v| v.iter().sum());

    if let Some(expected) = expected_sum {
        // Tolerance: 1 second of samples at the merged file's sample rate.
        let tolerance = actual_sample_rate as u64;
        let diff = if actual_samples > expected {
            actual_samples - expected
        } else {
            expected - actual_samples
        };
        if diff > tolerance {
            let _ = std::fs::remove_file(&merged_staged);
            return Err(MergeError::DurationMismatch(format!(
                "expected ~{} samples, got ~{} (diff {} exceeds tolerance {})",
                expected, actual_samples, diff, tolerance,
            )));
        }
    }

    // ---- build MergedArtifact ------------------------------------------
    let final_dir = track_artifacts[0]
        .final_path
        .parent()
        .unwrap_or(Path::new("."));
    let merged_final = final_dir.join(format!("merged.{}", ext));

    let source_tracks: Vec<TrackId> = track_artifacts
        .iter()
        .map(|t| t.track_id.clone())
        .collect();

    let merged = MergedArtifact {
        staged_path: merged_staged,
        final_path: merged_final,
        total_samples: actual_samples,
        source_tracks,
    };

    Ok((
        ArtifactSet { audio: AudioArtifacts::Merged(merged), sidecars },
        StageRecord { stage: PipelineStage::Merge, outcome: StageOutcome::Ok },
    ))
}

/// Parse ffprobe JSON for the merged-file validation probe.
/// Returns `(sample_rate, duration_secs)`.
fn parse_merge_probe(json_str: &str) -> Result<(u32, f64), MergeError> {
    let val: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| MergeError::DurationMismatch(format!("probe parse failed: {e}")))?;

    let sample_rate = val
        .pointer("/streams/0/sample_rate")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    if sample_rate == 0 {
        return Err(MergeError::DurationMismatch(
            "merged file has no valid sample_rate".into(),
        ));
    }

    let duration = val
        .pointer("/streams/0/duration")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| {
            val.pointer("/format/duration")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
        })
        .unwrap_or(0.0);

    Ok((sample_rate, duration))
}

/// Minimal `CommandRecord` for between-step cancellation errors where
/// no real command was issued.
fn dummy_command_record(binary: ToolBinary) -> super::tool::CommandRecord {
    super::tool::CommandRecord {
        binary,
        sanitized_args: vec![],
        cwd: None,
        env_keys: vec![],
        exit: None,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        elapsed: Duration::ZERO,
    }
}

/// Apply metadata tags to staged audio artifacts. Dispatches to the
/// correct tagging tool per format (metaflac, opustags, wvtag, ffmpeg).
pub async fn apply_metadata(
    artifacts: &ArtifactSet,
    source: &PreparedSource,
    req: &PipelineRequest,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<StageRecord, MetadataError> {
    if req.stages.metadata == StageRequirement::Disabled {
        return Ok(StageRecord { stage: PipelineStage::Metadata, outcome: StageOutcome::Skipped });
    }

    match &artifacts.audio {
        AudioArtifacts::Tracks(tracks) => {
            for artifact in tracks {
                if cancel.is_cancelled() {
                    return Err(MetadataError::Tool(
                        super::errors::ToolRunnerError::Cancelled {
                            command: dummy_command_record(ToolBinary::Metaflac),
                        },
                    ));
                }
                let meta = source
                    .tracks
                    .iter()
                    .find(|t| t.id == artifact.track_id)
                    .map(|t| &t.metadata);
                if let Some(meta) = meta {
                    tag_audio_file(&artifact.staged_path, meta, &source.album_metadata, runner, cancel).await?;
                }
            }
        }
        AudioArtifacts::Merged(merged) => {
            // For merged files, apply album-level metadata.
            let album_as_track = TrackMetadata {
                title: source.album_metadata.album.clone(),
                artist: source.album_metadata.album_artist.clone(),
                album_artist: source.album_metadata.album_artist.clone(),
                genre: source.album_metadata.genre.clone(),
                date: source.album_metadata.date.clone(),
                ..TrackMetadata::default()
            };
            tag_audio_file(&merged.staged_path, &album_as_track, &source.album_metadata, runner, cancel).await?;
        }
    }

    Ok(StageRecord { stage: PipelineStage::Metadata, outcome: StageOutcome::Ok })
}

/// Build and run tagging commands for a single audio file.
async fn tag_audio_file(
    path: &Path,
    meta: &TrackMetadata,
    album: &AlbumMetadata,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(), MetadataError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let mut tags: Vec<(String, String)> = Vec::new();
    if let Some(ref v) = meta.title { tags.push(("TITLE".into(), v.clone())); }
    if let Some(ref v) = meta.artist { tags.push(("ARTIST".into(), v.clone())); }
    if let Some(ref v) = meta.album_artist { tags.push(("ALBUMARTIST".into(), v.clone())); }
    if let Some(ref v) = album.album { tags.push(("ALBUM".into(), v.clone())); }
    if let Some(ref v) = meta.genre { tags.push(("GENRE".into(), v.clone())); }
    if let Some(ref v) = meta.date { tags.push(("DATE".into(), v.clone())); }
    if let Some(n) = meta.track_number { tags.push(("TRACKNUMBER".into(), n.to_string())); }
    if let Some(n) = meta.disc_number { tags.push(("DISCNUMBER".into(), n.to_string())); }
    if let Some(ref v) = meta.comment { tags.push(("COMMENT".into(), v.clone())); }

    if tags.is_empty() {
        return Ok(());
    }

    let cmd = match ext.as_str() {
        "flac" => {
            let mut args = Vec::new();
            for (k, v) in &tags {
                args.push(format!("--remove-tag={}", k));
                args.push(format!("--set-tag={}={}", k, v));
            }
            args.push(path.display().to_string());
            ToolCommand {
                binary: ToolBinary::Metaflac,
                args,
                secret_args: vec![],
                cwd: None,
                env: vec![],
                timeout: Duration::from_secs(30),
            }
        }
        "opus" | "ogg" => {
            let mut args = Vec::new();
            for (k, _) in &tags {
                args.push("--delete".into());
                args.push(k.clone());
            }
            for (k, v) in &tags {
                args.push("-s".into());
                args.push(format!("{}={}", k, v));
            }
            args.push("--in-place".into());
            args.push(path.display().to_string());
            ToolCommand {
                binary: ToolBinary::Opustags,
                args,
                secret_args: vec![],
                cwd: None,
                env: vec![],
                timeout: Duration::from_secs(30),
            }
        }
        "wv" => {
            let mut args = vec!["-q".to_string()];
            for (k, v) in &tags {
                args.push("-w".into());
                args.push(format!("{}={}", k, v));
            }
            args.push(path.display().to_string());
            ToolCommand {
                binary: ToolBinary::Wvtag,
                args,
                secret_args: vec![],
                cwd: None,
                env: vec![],
                timeout: Duration::from_secs(30),
            }
        }
        "mp3" | "m4a" | "aac" | "wav" | "aiff" | "aif" => {
            let mut args = vec![
                "-y".into(),
                "-i".into(),
                path.display().to_string(),
            ];
            for (k, v) in &tags {
                args.push("-metadata".into());
                args.push(format!("{}={}", k.to_lowercase(), v));
            }
            args.push("-c".into());
            args.push("copy".into());
            let tmp = path.with_extension(format!(
                "tmp.{}",
                path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
            ));
            args.push(tmp.display().to_string());
            ToolCommand {
                binary: ToolBinary::Ffmpeg,
                args,
                secret_args: vec![],
                cwd: None,
                env: vec![],
                timeout: Duration::from_secs(60),
            }
        }
        _ => {
            return Err(MetadataError::UnsupportedTagFormat(ext));
        }
    };

    runner.run(cmd, cancel).await.map_err(MetadataError::Tool)?;

    // For ffmpeg-based formats, rename temp file over original.
    if matches!(ext.as_str(), "mp3" | "m4a" | "aac" | "wav" | "aiff" | "aif") {
        let tmp = path.with_extension(format!(
            "tmp.{}",
            path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
        ));
        if tmp.exists() {
            std::fs::rename(&tmp, path)?;
        }
    }

    Ok(())
}

/// Apply ReplayGain tags via loudgain. Album mode for per-track
/// artifacts, single-file mode for merged.
pub async fn apply_replaygain(
    artifacts: &ArtifactSet,
    req: &PipelineRequest,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<StageRecord, ReplayGainError> {
    if req.stages.replaygain == StageRequirement::Disabled {
        return Ok(StageRecord { stage: PipelineStage::ReplayGain, outcome: StageOutcome::Skipped });
    }

    let mut args = Vec::new();

    match &artifacts.audio {
        AudioArtifacts::Tracks(tracks) => {
            if tracks.is_empty() {
                return Ok(StageRecord { stage: PipelineStage::ReplayGain, outcome: StageOutcome::Skipped });
            }
            args.push("-a".to_string()); // album mode
            args.push("-k".to_string()); // prevent clipping
            args.push("-s".to_string()); // tag mode
            args.push("i".to_string());  // ReplayGain 2.0
            for t in tracks {
                args.push(t.staged_path.display().to_string());
            }
        }
        AudioArtifacts::Merged(merged) => {
            // Single-file mode — no -a flag.
            args.push("-k".to_string());
            args.push("-s".to_string());
            args.push("i".to_string());
            args.push(merged.staged_path.display().to_string());
        }
    }

    let cmd = ToolCommand {
        binary: ToolBinary::Loudgain,
        args,
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(600), // RG scan can be slow for long albums
    };

    runner.run(cmd, cancel).await.map_err(ReplayGainError::Tool)?;

    Ok(StageRecord { stage: PipelineStage::ReplayGain, outcome: StageOutcome::Ok })
}

/// Generate feature sidecars: conversion log and CUE sheet (where
/// applicable). Returns an updated `ArtifactSet` with the sidecars
/// added so publish includes them.
pub async fn run_features(
    mut artifacts: ArtifactSet,
    outcome: &AlbumOutcome,
    source: &PreparedSource,
    req: &PipelineRequest,
    staging: &StagingDir,
    _runner: &dyn ToolRunner,
    _cancel: &CancellationToken,
) -> Result<(ArtifactSet, StageRecord), FeatureError> {
    if req.stages.features == StageRequirement::Disabled {
        return Ok((
            artifacts,
            StageRecord { stage: PipelineStage::Features, outcome: StageOutcome::Skipped },
        ));
    }

    let album_dir = req.output_root.clone();

    // ---- conversion log sidecar ----------------------------------------
    let log_staged = staging.root.join("conversion_log.txt");
    let log_content = build_conversion_log(outcome, source, req);
    std::fs::write(&log_staged, &log_content)?;

    artifacts.sidecars.push(SidecarArtifact {
        kind: SidecarKind::ConversionLog,
        staged_path: log_staged,
        final_path: album_dir.join("conversion_log.txt"),
    });

    // ---- CUE sheet sidecar (multi-track only) --------------------------
    if source.tracks.len() > 1 {
        let cue_staged = staging.root.join("album.cue");
        let cue_content = build_cue_sheet(source, &artifacts);
        std::fs::write(&cue_staged, &cue_content)?;

        artifacts.sidecars.push(SidecarArtifact {
            kind: SidecarKind::CueSheet,
            staged_path: cue_staged,
            final_path: album_dir.join("album.cue"),
        });
    }

    Ok((
        artifacts,
        StageRecord { stage: PipelineStage::Features, outcome: StageOutcome::Ok },
    ))
}

/// Build a human-readable conversion log.
fn build_conversion_log(
    outcome: &AlbumOutcome,
    source: &PreparedSource,
    req: &PipelineRequest,
) -> String {
    let mut log = String::new();
    log.push_str(&format!("Conversion Log — {}\n", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")));
    log.push_str(&format!("Source: {}\n", source.container.display()));
    log.push_str(&format!("Target format: {:?}\n", req.target_format));
    log.push_str(&format!("Tracks in source: {}\n\n", source.tracks.len()));

    match outcome {
        AlbumOutcome::Complete { tracks, .. } => {
            log.push_str(&format!("Result: Complete ({} tracks)\n", tracks.len()));
        }
        AlbumOutcome::Partial { successful, failed, .. } => {
            log.push_str(&format!(
                "Result: Partial ({} ok, {} failed)\n",
                successful.len(),
                failed.len()
            ));
        }
        AlbumOutcome::Blocked { reason, .. } => {
            log.push_str(&format!("Result: Blocked ({:?})\n", reason));
        }
    }

    log
}

/// Build a minimal CUE sheet for the album.
fn build_cue_sheet(source: &PreparedSource, artifacts: &ArtifactSet) -> String {
    let mut cue = String::new();

    if let Some(ref album) = source.album_metadata.album {
        cue.push_str(&format!("TITLE \"{}\"\n", album));
    }
    if let Some(ref artist) = source.album_metadata.album_artist {
        cue.push_str(&format!("PERFORMER \"{}\"\n", artist));
    }

    match &artifacts.audio {
        AudioArtifacts::Tracks(tracks) => {
            for (i, t) in tracks.iter().enumerate() {
                let filename = t.staged_path
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or("unknown");
                cue.push_str(&format!("FILE \"{}\" WAVE\n", filename));
                cue.push_str(&format!("  TRACK {:02} AUDIO\n", i + 1));
                // Look up title from source metadata.
                if let Some(st) = source.tracks.iter().find(|s| s.id == t.track_id) {
                    if let Some(ref title) = st.metadata.title {
                        cue.push_str(&format!("    TITLE \"{}\"\n", title));
                    }
                    if let Some(ref artist) = st.metadata.artist {
                        cue.push_str(&format!("    PERFORMER \"{}\"\n", artist));
                    }
                }
                cue.push_str("    INDEX 01 00:00:00\n");
            }
        }
        AudioArtifacts::Merged(merged) => {
            let filename = merged.staged_path
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("merged");
            cue.push_str(&format!("FILE \"{}\" WAVE\n", filename));
            for (i, st) in source.tracks.iter().enumerate() {
                cue.push_str(&format!("  TRACK {:02} AUDIO\n", i + 1));
                if let Some(ref title) = st.metadata.title {
                    cue.push_str(&format!("    TITLE \"{}\"\n", title));
                }
                if let Some(ref artist) = st.metadata.artist {
                    cue.push_str(&format!("    PERFORMER \"{}\"\n", artist));
                }
                cue.push_str("    INDEX 01 00:00:00\n");
            }
        }
    }

    cue
}

// ===========================================================================
// Publish  (PR 4 bodies)
// ===========================================================================

/// PR 1 stub: empty plan rooted at the request's output root. PR 4
/// implements the real staged-to-final mapping.
pub fn build_publish_plan(
    _artifacts: &ArtifactSet,
    req: &PipelineRequest,
) -> Result<PublishPlan, PublishError> {
    Ok(PublishPlan { album_dir: req.output_root.clone(), entries: Vec::new() })
}

/// PR 1 stub: performs no move. Consumes the `StagingDir` (its
/// `Drop` cleans the tree). PR 4 implements atomic publish.
pub fn publish_album_output(
    _staging: StagingDir,
    _plan: &PublishPlan,
    _policy: PublishPolicy,
) -> Result<PublishedAlbum, PublishError> {
    Err(PublishError::StagingMissing)
}

// ===========================================================================
// Durable log  (PR 6 body; PR 4 ships a minimal interim body)
// ===========================================================================

/// Serialize the full `PipelineReport` as JSON and write it to
/// `LogPolicy.root`. The report uses `RedactedPipelineRequest` so
/// secrets never enter the durable log.
pub fn write_durable_log(
    report: &PipelineReport,
    log: &LogPolicy,
) -> Result<PathBuf, LogError> {
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| LogError::Serialization(e.to_string()))?;

    std::fs::create_dir_all(&log.root)?;

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let filename = format!("{}_{}.json", report.request.job_id, timestamp);
    let path = log.root.join(filename);

    std::fs::write(&path, json)?;
    Ok(path)
}

// ===========================================================================
// Orchestrator  (PR 4 body, final shape)
// ===========================================================================

/// PR 1 stub: returns a `Blocked` report without running stages.
/// PR 4 implements the final orchestrator shape; PRs 5–6 fill stage
/// bodies; the orchestrator itself does not change after PR 4.
pub async fn run_pipeline_item(
    req: PipelineRequest,
    _runner: &dyn ToolRunner,
    _reporter: &dyn PipelineReporter,
    _cancel: &CancellationToken,
) -> PipelineReport {
    PipelineReport {
        request: RedactedPipelineRequest::from(&req),
        source: None,
        plan: None,
        artifacts: None,
        published: None,
        outcome: AlbumOutcome::Blocked {
            successful: Vec::new(),
            failed: Vec::new(),
            stages: Vec::new(),
            reason: BlockReason::MaterializeFailed,
        },
        durable_log: None,
    }
}

// ===========================================================================
// Real PR 1 logic — outcome aggregation + queue-status mapping
// ===========================================================================

/// Aggregate per-track records + stage records into an `AlbumOutcome`.
///
/// Rules:
/// - A `StageRecord` with `StageOutcome::Failed` always blocks →
///   `Blocked` with `RequiredStageFailure`. Disabled stages reach
///   aggregation as `StageOutcome::Skipped` and never block.
/// - Otherwise, with no failed tracks → `Complete`.
/// - With failed tracks: `FailAlbumOnAnyTrackFailure` → `Blocked`
///   (`TrackFailures`); `AllowPartialAlbum` → `Partial`.
pub fn aggregate_album_outcome(
    tracks: Vec<TrackRecord>,
    stages: Vec<StageRecord>,
    policy: FailurePolicy,
) -> AlbumOutcome {
    let (successful, failed): (Vec<TrackRecord>, Vec<TrackRecord>) = tracks
        .into_iter()
        .partition(|t| matches!(t.outcome, TrackOutcome::Ok));

    if let Some(failed_stage) = stages
        .iter()
        .find(|s| matches!(s.outcome, StageOutcome::Failed(_)))
        .map(|s| s.stage)
    {
        return AlbumOutcome::Blocked {
            successful,
            failed,
            stages,
            reason: BlockReason::RequiredStageFailure(failed_stage),
        };
    }

    if failed.is_empty() {
        return AlbumOutcome::Complete { tracks: successful, stages };
    }

    match policy {
        FailurePolicy::FailAlbumOnAnyTrackFailure => AlbumOutcome::Blocked {
            successful,
            failed,
            stages,
            reason: BlockReason::TrackFailures,
        },
        FailurePolicy::AllowPartialAlbum => AlbumOutcome::Partial { successful, failed, stages },
    }
}

/// Map a finished `AlbumOutcome` to a terminal `ConversionStatus`.
/// `Complete` → `Completed`, `Partial` → `Partial`, `Blocked` →
/// `Failed`.
pub fn map_album_outcome(
    outcome: &AlbumOutcome,
    published: Option<&PublishedAlbum>,
    durable_log: Option<&Path>,
) -> ConversionStatus {
    let output_path = published
        .map(|p| p.album_dir.clone())
        .unwrap_or_default();
    let log_path = durable_log.map(|p| p.to_path_buf());

    match outcome {
        AlbumOutcome::Complete { .. } => ConversionStatus::Completed { output_path, log_path },
        AlbumOutcome::Partial { successful, failed, .. } => ConversionStatus::Partial {
            output_path,
            successful: successful.len() as u32,
            failed: failed.len() as u32,
            log_path: log_path.unwrap_or_default(),
        },
        AlbumOutcome::Blocked { reason, .. } => ConversionStatus::Failed {
            error: format!("album blocked: {:?}", reason),
            log_path,
        },
    }
}
