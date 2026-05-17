//! PR 1 — `Materializer` trait, every public stage-function
//! signature, the real `aggregate_album_outcome`, and the
//! `AlbumOutcome` -> `ConversionStatus` mapping.
//!
//! Per the plan: PR 1 ships compiling, non-panicking stub bodies for
//! the free functions below. PRs 2–10 replace those bodies without
//! changing the signatures. `aggregate_album_outcome` and
//! `map_album_outcome` are real implementations in PR 1 — they are
//! pure logic and are exercised by PR 1's exit-condition tests.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fs, io};

use async_trait::async_trait;
use fs2::FileExt;
use tokio_util::sync::CancellationToken;

use super::errors::{
    ConvertError, FeatureError, LogError, MaterializeError, MergeError, MetadataError, PlanError,
    PublishError, ReplayGainError, RequestValidationError, SourceDetectError,
    SourceDispatchError, ToolRunnerError,
};
use super::materializer_7z::SevenZipMaterializer;
use super::materializer_cue::{is_cue_image_candidate, CueImageMaterializer};
use super::reporter::{PipelineEvent, PipelineReporter};
use super::tool::{CommandRecord, EnvVar, ToolBinary, ToolCommand, ToolRunner};
use super::types::*;
use crate::convert::{AudioFormat, ConversionStatus};
use tonepoet_backend::{
    Backend as BackendCrateKind, CommandBuilder, ConversionCommand,
    ConversionSettings as BackendConversionSettings,
};

const DEFAULT_CONVERT_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
const STAGING_PARENT_NAME: &str = ".tonepoet-staging";

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

/// Validate request fields the PR 4 orchestrator and stage bodies rely on.
pub fn validate_request(req: &PipelineRequest) -> Result<(), RequestValidationError> {
    if req.container.as_os_str().is_empty() {
        return Err(RequestValidationError::MissingContainer);
    }
    if !req.container.exists() {
        return Err(RequestValidationError::MissingContainer);
    }
    if !req.container.is_file() {
        return Err(RequestValidationError::InvalidOutputRoot(format!(
            "container is not a regular file: {}",
            req.container.display()
        )));
    }

    validate_root_dir(&req.output_root, "output_root")?;
    validate_root_dir(&req.log.root, "log.root")?;
    if req.naming.template.trim().is_empty() {
        return Err(RequestValidationError::InvalidTemplate(
            "template must not be empty".to_string(),
        ));
    }
    validate_template(&req.naming.template).map_err(RequestValidationError::InvalidTemplate)?;

    match &req.source.track_selection {
        TrackSelection::All => {}
        TrackSelection::Range { start, end } => {
            if *start == 0 || *end == 0 || start > end {
                return Err(RequestValidationError::InvalidStagePolicy(format!(
                    "invalid track selection range {start}..={end}"
                )));
            }
        }
        TrackSelection::Set(set) => {
            if set.is_empty() || set.iter().any(|n| *n == 0) {
                return Err(RequestValidationError::InvalidStagePolicy(
                    "track selection set must contain positive track numbers".to_string(),
                ));
            }
        }
    }

    if let Some(password) = &req.source.archive_password {
        if password.expose().is_empty() {
            return Err(RequestValidationError::InvalidSecretState(
                "archive password is present but empty".to_string(),
            ));
        }
    }

    Ok(())
}

/// Detect source kind for PR 4. Later PRs add CUE image and SACD ISO arms.
pub fn detect_source_kind(req: &PipelineRequest) -> Result<SourceKind, SourceDetectError> {
    let ext = req
        .container
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match ext.as_str() {
        "7z" => return Ok(SourceKind::SevenZip),
        _ => {}
    }

    if is_cue_image_candidate(req)? {
        return Ok(SourceKind::CueImage);
    }

    Err(SourceDetectError::UnknownSource)
}

/// Dispatch a supported source kind to its materializer.
pub fn materializer_for(
    kind: SourceKind,
) -> Result<Box<dyn Materializer>, SourceDispatchError> {
    match kind {
        SourceKind::SevenZip => Ok(Box::new(SevenZipMaterializer)),
        SourceKind::CueImage => Ok(Box::new(CueImageMaterializer)),
        _ => Err(SourceDispatchError::Unsupported(kind)),
    }
}

// ===========================================================================
// Per-track realize  (PR 4 = StagedFile arm; PR 8 = ImageSegment;
// PR 9 = SacdTrack)
// ===========================================================================

/// Realize a materialized track as a file the encoder can consume.
pub async fn realize_track(
    src: &TrackSourceRef,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<PathBuf, ConvertError> {
    if cancel.is_cancelled() {
        return Err(ConvertError::Realize("cancelled".to_string()));
    }

    match src {
        TrackSourceRef::StagedFile(path) => {
            if !path.exists() {
                return Err(ConvertError::TrackValidation(format!(
                    "staged track does not exist: {}",
                    path.display()
                )));
            }
            if !path.is_file() {
                return Err(ConvertError::TrackValidation(format!(
                    "staged track is not a regular file: {}",
                    path.display()
                )));
            }
            Ok(path.clone())
        }
        TrackSourceRef::ImageSegment {
            image,
            start_sample,
            samples,
        } => {
            realize_image_segment(
                image,
                *start_sample,
                *samples,
                req,
                staging,
                runner,
                cancel,
            )
            .await
        }
        TrackSourceRef::SacdTrack { .. } => Err(ConvertError::UnsupportedTrackSource),
    }
}

fn cue_segment_output_name(image: &Path, start_sample: u64, samples: u64) -> String {
    let stem = sanitize_segment_component(
        image
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("image"),
    );
    format!("{stem}_{start_sample:012}_{samples:012}.flac")
}

fn sanitize_segment_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "image".to_string()
    } else {
        sanitized
    }
}

async fn realize_image_segment(
    image: &Path,
    start_sample: u64,
    samples: u64,
    _req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<PathBuf, ConvertError> {
    if samples == 0 {
        return Err(ConvertError::TrackValidation(
            "image segment has zero samples".to_string(),
        ));
    }
    if !image.is_file() {
        return Err(ConvertError::TrackValidation(format!(
            "image file does not exist: {}",
            image.display()
        )));
    }

    let realized_dir = staging.root.join("realized-image-segments");
    fs::create_dir_all(&realized_dir)?;
    let out_path = realized_dir.join(cue_segment_output_name(image, start_sample, samples));
    let _ = fs::remove_file(&out_path);

    let direct = cut_segment_with_ffmpeg(image, start_sample, samples, &out_path, runner, cancel).await;
    if let Err(err) = direct {
        let _ = fs::remove_file(&out_path);
        if !has_path_extension(image, "wv") {
            return Err(err);
        }

        let fallback = async {
            let wav_path = ensure_decoded_wavpack_image(image, &realized_dir, runner, cancel).await?;
            cut_segment_with_ffmpeg(&wav_path, start_sample, samples, &out_path, runner, cancel).await
        }
        .await;

        if let Err(err) = fallback {
            let _ = fs::remove_file(&out_path);
            return Err(err);
        }
    }

    if let Err(err) = validate_realized_segment(&out_path, samples, runner, cancel).await {
        let _ = fs::remove_file(&out_path);
        return Err(err);
    }

    Ok(out_path)
}

async fn cut_segment_with_ffmpeg(
    input: &Path,
    start_sample: u64,
    samples: u64,
    out_path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(), ConvertError> {
    let end_sample = start_sample.checked_add(samples).ok_or_else(|| {
        ConvertError::TrackValidation("image segment sample range overflows".to_string())
    })?;

    // Deliberately avoid input-side `-ss` here. On compressed images, ffmpeg may
    // seek to the closest preceding seek point and then make filters count from
    // that decoded stream, which can yield the expected length but the wrong
    // source-aligned first sample. Absolute sample trimming is slower for late
    // tracks, but it preserves correctness until a separately proven exact
    // seek/copy strategy is introduced.
    let filter = format!(
        "atrim=start_sample={start_sample}:end_sample={end_sample},asetpts=PTS-STARTPTS"
    );
    let cmd = ToolCommand {
        binary: ToolBinary::Ffmpeg,
        args: vec![
            "-y".into(),
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-i".into(),
            input.to_string_lossy().into_owned(),
            "-map".into(),
            "0:a:0".into(),
            "-vn".into(),
            "-af".into(),
            filter,
            "-c:a".into(),
            "flac".into(),
            "-compression_level".into(),
            "0".into(),
            out_path.to_string_lossy().into_owned(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: DEFAULT_CONVERT_TIMEOUT,
    };

    match runner.run(cmd, cancel).await {
        Ok(_) => Ok(()),
        Err(ToolRunnerError::Cancelled { .. }) => Err(ConvertError::Realize("cancelled".to_string())),
        Err(err) => Err(ConvertError::Tool(err)),
    }
}

async fn ensure_decoded_wavpack_image(
    input: &Path,
    realized_dir: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<PathBuf, ConvertError> {
    let cache_dir = realized_dir.join("decoded-image-cache");
    fs::create_dir_all(&cache_dir)?;

    let wav_path = cache_dir.join(decoded_wavpack_cache_name(input));
    if cached_audio_is_ready(&wav_path) {
        return Ok(wav_path);
    }

    let lock_path = wav_path.with_extension("wav.lock");
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)?;
    lock_file.lock_exclusive()?;

    let result = async {
        if cached_audio_is_ready(&wav_path) {
            return Ok(wav_path.clone());
        }

        let tmp_path = wav_path.with_file_name(format!(
            ".{}.{}.tmp",
            wav_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("decoded.wav"),
            std::process::id()
        ));
        let _ = fs::remove_file(&tmp_path);

        if let Err(err) = decode_wavpack_image(input, &tmp_path, runner, cancel).await {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }

        fs::rename(&tmp_path, &wav_path)?;
        Ok(wav_path.clone())
    }
    .await;

    let _ = lock_file.unlock();
    result
}

fn cached_audio_is_ready(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false)
}

fn decoded_wavpack_cache_name(input: &Path) -> String {
    let stem = sanitize_segment_component(
        input
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("image"),
    );
    let hash = stable_path_hash(input);
    format!("{stem}_{hash:016x}.wav")
}

fn stable_path_hash(path: &Path) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

async fn decode_wavpack_image(
    input: &Path,
    out_path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(), ConvertError> {
    let cmd = ToolCommand {
        binary: ToolBinary::Wvunpack,
        args: vec![
            "-q".into(),
            "-o".into(),
            out_path.to_string_lossy().into_owned(),
            input.to_string_lossy().into_owned(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: DEFAULT_CONVERT_TIMEOUT,
    };

    match runner.run(cmd, cancel).await {
        Ok(_) => Ok(()),
        Err(ToolRunnerError::Cancelled { .. }) => Err(ConvertError::Realize("cancelled".to_string())),
        Err(err) => Err(ConvertError::Tool(err)),
    }
}

async fn validate_realized_segment(
    out_path: &Path,
    expected_samples: u64,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(), ConvertError> {
    let metadata = fs::metadata(out_path).map_err(|err| {
        ConvertError::TrackValidation(format!(
            "realized image segment missing: {} ({err})",
            out_path.display()
        ))
    })?;
    if metadata.len() == 0 {
        return Err(ConvertError::TrackValidation(format!(
            "realized image segment is empty: {}",
            out_path.display()
        )));
    }

    let probe = probe_realized_segment(out_path, runner, cancel).await?;
    let actual_samples = probe.samples.ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "could not measure realized segment samples: {}",
            out_path.display()
        ))
    })?;
    let delta = actual_samples.abs_diff(expected_samples);
    let allowed = if probe.exact { 0 } else { (probe.sample_rate / 75).max(1) as u64 };
    if delta > allowed {
        return Err(ConvertError::TrackValidation(format!(
            "realized segment sample drift: expected {expected_samples}, got {actual_samples}, allowed {allowed}"
        )));
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct RealizedProbe {
    sample_rate: u32,
    samples: Option<u64>,
    exact: bool,
}

async fn probe_realized_segment(
    path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<RealizedProbe, ConvertError> {
    let cmd = ToolCommand {
        binary: ToolBinary::Ffprobe,
        args: vec![
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            "a:0".into(),
            "-show_entries".into(),
            "stream=sample_rate,duration_ts,time_base,duration".into(),
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
        Err(ToolRunnerError::Cancelled { .. }) => return Err(ConvertError::Realize("cancelled".to_string())),
        Err(err) => return Err(ConvertError::Tool(err)),
    };
    parse_realized_probe_json(&output.stdout_tail)
}

fn parse_realized_probe_json(json: &str) -> Result<RealizedProbe, ConvertError> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|err| ConvertError::TrackValidation(format!("ffprobe JSON parse failed: {err}")))?;
    let stream = value
        .pointer("/streams/0")
        .ok_or_else(|| ConvertError::TrackValidation("ffprobe returned no audio stream".to_string()))?;
    let sample_rate = stream
        .get("sample_rate")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    if sample_rate == 0 {
        return Err(ConvertError::TrackValidation(
            "ffprobe returned no valid sample_rate".to_string(),
        ));
    }

    if let Some(samples) = samples_from_stream_duration_ts(stream, sample_rate) {
        return Ok(RealizedProbe { sample_rate, samples: Some(samples), exact: true });
    }

    let duration_secs = stream
        .get("duration")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<f64>().ok())
        .or_else(|| {
            value
                .pointer("/format/duration")
                .and_then(|value| value.as_str())
                .and_then(|value| value.parse::<f64>().ok())
        });
    let samples = duration_secs.map(|duration| (duration * f64::from(sample_rate)).round() as u64);
    Ok(RealizedProbe { sample_rate, samples, exact: false })
}

fn samples_from_stream_duration_ts(stream: &serde_json::Value, sample_rate: u32) -> Option<u64> {
    let duration_ts = stream.get("duration_ts").and_then(json_u64)?;
    let time_base = stream.get("time_base")?.as_str()?;
    let (num, den) = time_base.split_once('/')?;
    let num = num.parse::<u64>().ok()?;
    let den = den.parse::<u64>().ok()?;
    if den == 0 {
        return None;
    }
    let samples = (duration_ts as u128)
        .checked_mul(num as u128)?
        .checked_mul(sample_rate as u128)?
        .checked_div(den as u128)?;
    u64::try_from(samples).ok()
}

fn json_u64(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn has_path_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(ext))
        .unwrap_or(false)
}


// ===========================================================================
// Output planning  (PR 4 body)
// ===========================================================================

/// Assign stable final output paths for every prepared track.
pub fn plan_outputs(
    source: &PreparedSource,
    req: &PipelineRequest,
) -> Result<AlbumPlan, PlanError> {
    if source.tracks.is_empty() {
        return Err(PlanError::EmptyManifest);
    }
    validate_template(&req.naming.template).map_err(PlanError::InvalidTemplate)?;

    let output_root = normalize_path(&req.output_root);
    let album_component = sanitize_component(
        source
            .album_metadata
            .album
            .as_deref()
            .or_else(|| source.container.file_stem().and_then(|s| s.to_str()))
            .unwrap_or("Album"),
    );
    let album_dir = if req.naming.per_album_subdir {
        output_root.join(album_component)
    } else {
        output_root.clone()
    };

    let mut entries = Vec::with_capacity(source.tracks.len());
    let mut seen = BTreeSet::new();
    let mut seen_track_ids = BTreeSet::new();
    let reserved_staging_root = output_root.join(STAGING_PARENT_NAME);

    for track in &source.tracks {
        if !seen_track_ids.insert(track.id.clone()) {
            return Err(PlanError::InvalidTrackSelection(format!(
                "duplicate track id in prepared source: source ordinal {}",
                track.id.source_ordinal
            )));
        }
        let rel = render_track_template(&req.naming.template, source, track, req.target_format)?;
        reject_escaping_path(&rel).map_err(PlanError::InvalidTemplate)?;

        let mut final_path = normalize_path(&album_dir.join(rel));
        append_default_extension(&mut final_path, req.target_format);
        if !path_is_under_root(&final_path, &output_root) {
            return Err(PlanError::PathOutsideOutputRoot(
                final_path.display().to_string(),
            ));
        }
        if path_is_under_root(&final_path, &reserved_staging_root) {
            return Err(PlanError::PathOutsideOutputRoot(format!(
                "planned path uses reserved staging directory: {}",
                final_path.display()
            )));
        }

        let mut collision_key = normalized_collision_key(&final_path);
        if seen.contains(&collision_key) {
            match req.naming.collision_policy {
                NamingCollisionPolicy::Fail => {
                    return Err(PlanError::NamingCollision(final_path.display().to_string()));
                }
                NamingCollisionPolicy::AppendStableSuffix => {
                    let original = final_path.clone();
                    let mut attempt = 1_u32;
                    loop {
                        final_path = append_collision_suffix(
                            &original,
                            &format!("{:03}-{attempt}", track.id.source_ordinal),
                        );
                        collision_key = normalized_collision_key(&final_path);
                        if !seen.contains(&collision_key) {
                            break;
                        }
                        attempt += 1;
                    }
                }
            }
        }
        seen.insert(collision_key);

        entries.push(PlannedTrackOutput { track_id: track.id.clone(), final_path });
    }

    Ok(AlbumPlan { album_dir, entries })
}

// ===========================================================================
// Convert / merge / metadata / replaygain / features  (PR 4–6 bodies)
// ===========================================================================

/// Convert every planned track. Per-track failures are represented in
/// `TrackRecord`s; successful artifacts only contain tracks that encoded.
pub async fn convert_tracks(
    source: &PreparedSource,
    plan: &AlbumPlan,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> ConvertStageResult {
    let mut records = Vec::with_capacity(source.tracks.len());
    let mut artifacts = Vec::new();
    let planned: BTreeMap<TrackId, PathBuf> = plan
        .entries
        .iter()
        .map(|entry| (entry.track_id.clone(), entry.final_path.clone()))
        .collect();

    let convert_root = staging.root.join("converted");
    if let Err(err) = fs::create_dir_all(&convert_root) {
        let error = format!("could not create conversion staging directory: {err}");
        for track in &source.tracks {
            records.push(failed_track_record(
                track,
                None,
                None,
                Vec::new(),
                error.clone(),
            ));
        }
        return ConvertStageResult {
            tracks: records,
            artifacts: ArtifactSet {
                audio: AudioArtifacts::Tracks(artifacts),
                sidecars: Vec::new(),
            },
            record: stage_record(PipelineStage::Convert, StageOutcome::Failed(error)),
        };
    }

    for track in &source.tracks {
        if cancel.is_cancelled() {
            records.push(failed_track_record(
                track,
                None,
                None,
                Vec::new(),
                "cancelled".to_string(),
            ));
            continue;
        }

        let Some(final_path) = planned.get(&track.id).cloned() else {
            records.push(failed_track_record(
                track,
                None,
                None,
                Vec::new(),
                format!("missing planned output for track {}", track.id.source_ordinal),
            ));
            continue;
        };

        let staged_path = staged_audio_path(&convert_root, &final_path, &track.id, req.target_format);
        let realized_input = match realize_track(&track.source_ref, req, staging, runner, cancel).await {
            Ok(path) => path,
            Err(err) => {
                records.push(failed_track_record(
                    track,
                    None,
                    Some(staged_path.clone()),
                    Vec::new(),
                    err.to_string(),
                ));
                continue;
            }
        };

        if let Some(parent) = staged_path.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                records.push(failed_track_record(
                    track,
                    Some(realized_input.clone()),
                    Some(staged_path.clone()),
                    Vec::new(),
                    format!("could not create output directory: {err}"),
                ));
                continue;
            }
        }

        let bytes_in = file_len(&realized_input);
        let cmd = match encode_command(&realized_input, &staged_path, req) {
            Ok(cmd) => cmd,
            Err(err) => {
                records.push(failed_track_record(
                    track,
                    Some(realized_input),
                    Some(staged_path),
                    Vec::new(),
                    err.to_string(),
                ));
                continue;
            }
        };
        let output = runner.run(cmd, cancel).await;
        match output {
            Ok(tool_output) => {
                let command = tool_output.command.clone();
                let bytes_out = file_len(&staged_path);
                if bytes_out.unwrap_or(0) == 0 {
                    records.push(failed_track_record(
                        track,
                        Some(realized_input),
                        Some(staged_path.clone()),
                        vec![command],
                        format!("encoder did not produce output: {}", staged_path.display()),
                    ));
                    continue;
                }

                let record = TrackRecord {
                    track_id: track.id.clone(),
                    outcome: TrackOutcome::Ok,
                    source_ref: track.source_ref.clone(),
                    realized_input: Some(realized_input),
                    output_file: Some(staged_path.clone()),
                    commands: vec![command],
                    bytes_in,
                    bytes_out,
                    duration: Some(tool_output.elapsed),
                };
                artifacts.push(TrackArtifact {
                    track_id: track.id.clone(),
                    staged_path,
                    final_path,
                    samples: track.expected_samples,
                });
                records.push(record);
            }
            Err(err) => {
                let commands = command_from_tool_error(&err).into_iter().collect();
                records.push(failed_track_record(
                    track,
                    Some(realized_input),
                    Some(staged_path),
                    commands,
                    err.to_string(),
                ));
            }
        }
    }

    ConvertStageResult {
        tracks: records,
        artifacts: ArtifactSet {
            audio: AudioArtifacts::Tracks(artifacts),
            sidecars: Vec::new(),
        },
        record: stage_record(PipelineStage::Convert, StageOutcome::Ok),
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
    if !req.merge {
        return Ok((
            artifacts,
            StageRecord { stage: PipelineStage::Merge, outcome: StageOutcome::Skipped },
        ));
    }

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

    let ext = track_artifacts[0]
        .staged_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("flac");
    let merged_staged = staging.root.join(format!("merged.{}", ext));
    let concat_list = staging.root.join("_merge_concat.txt");

    let mut list_content = String::new();
    for t in &track_artifacts {
        let escaped = t.staged_path.display().to_string().replace('\'', "'\\''");
        list_content.push_str(&format!("file '{}'\n", escaped));
    }
    fs::write(&concat_list, &list_content)?;

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
    let _ = fs::remove_file(&concat_list);

    if let Err(e) = merge_result {
        let _ = fs::remove_file(&merged_staged);
        return Err(e.into());
    }

    // Validate merged output via ffprobe.
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
            let _ = fs::remove_file(&merged_staged);
            return Err(e.into());
        }
    };

    let (actual_sample_rate, actual_duration) =
        parse_merge_probe(&probe_output.stdout_tail)?;
    let actual_samples = (actual_duration * actual_sample_rate as f64).round() as u64;

    let expected_sum: Option<u64> = track_artifacts
        .iter()
        .map(|t| t.samples)
        .collect::<Option<Vec<u64>>>()
        .map(|v| v.iter().sum());

    if let Some(expected) = expected_sum {
        let tolerance = actual_sample_rate as u64;
        let diff = if actual_samples > expected {
            actual_samples - expected
        } else {
            expected - actual_samples
        };
        if diff > tolerance {
            let _ = fs::remove_file(&merged_staged);
            return Err(MergeError::DurationMismatch(format!(
                "expected ~{} samples, got ~{} (diff {} exceeds tolerance {})",
                expected, actual_samples, diff, tolerance,
            )));
        }
    }

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

/// Apply metadata tags to staged audio artifacts.
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
                    return Err(MetadataError::Tool(ToolRunnerError::Cancelled {
                        command: CommandRecord {
                            binary: ToolBinary::Metaflac,
                            sanitized_args: vec![], cwd: None, env_keys: vec![],
                            exit: None, stdout_tail: String::new(),
                            stderr_tail: String::new(), elapsed: Duration::ZERO,
                        },
                    }));
                }
                let meta = source.tracks.iter()
                    .find(|t| t.id == artifact.track_id)
                    .map(|t| &t.metadata);
                if let Some(meta) = meta {
                    tag_audio_file(&artifact.staged_path, meta, &source.album_metadata, runner, cancel).await?;
                }
            }
        }
        AudioArtifacts::Merged(merged) => {
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

fn push_tag_value(tags: &mut Vec<(String, String)>, key: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    if tags.iter().any(|(existing, _)| existing == key) {
        return;
    }
    tags.push((key.to_string(), value.to_string()));
}

fn cue_extra_tag_key(scope: &str, key: &str) -> String {
    let mut suffix = String::new();
    for ch in key.chars() {
        if ch.is_ascii_alphanumeric() {
            suffix.push(ch.to_ascii_uppercase());
        } else if !suffix.ends_with('_') {
            suffix.push('_');
        }
    }
    let suffix = suffix.trim_matches('_');
    if suffix.is_empty() {
        format!("TONEPOET_{scope}_EXTRA")
    } else {
        format!("TONEPOET_{scope}_{suffix}")
    }
}

async fn tag_audio_file(
    path: &Path,
    meta: &TrackMetadata,
    album: &AlbumMetadata,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(), MetadataError> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();

    let mut tags: Vec<(String, String)> = Vec::new();
    if let Some(ref v) = meta.title { push_tag_value(&mut tags, "TITLE", v); }
    if let Some(ref v) = meta.artist { push_tag_value(&mut tags, "ARTIST", v); }
    if let Some(ref v) = meta.album_artist { push_tag_value(&mut tags, "ALBUMARTIST", v); }
    if let Some(ref v) = album.album { push_tag_value(&mut tags, "ALBUM", v); }
    if let Some(ref v) = meta.genre { push_tag_value(&mut tags, "GENRE", v); }
    if let Some(ref v) = meta.date { push_tag_value(&mut tags, "DATE", v); }
    if let Some(n) = meta.track_number { push_tag_value(&mut tags, "TRACKNUMBER", &n.to_string()); }
    if let Some(n) = meta.disc_number { push_tag_value(&mut tags, "DISCNUMBER", &n.to_string()); }
    if let Some(ref v) = meta.comment { push_tag_value(&mut tags, "COMMENT", v); }

    // PR 8 CUE-specific metadata. The materializer preserves these fields in
    // TrackMetadata/AlbumMetadata, and the metadata stage writes them through
    // so published split files remain self-describing.
    if let Some(ref v) = meta.composer { push_tag_value(&mut tags, "COMPOSER", v); }
    if let Some(ref v) = meta.performer { push_tag_value(&mut tags, "PERFORMER", v); }
    if let Some(ref v) = meta.isrc { push_tag_value(&mut tags, "ISRC", v); }
    if let Some(ref v) = meta.publisher { push_tag_value(&mut tags, "PUBLISHER", v); }
    if let Some(ref v) = meta.copyright { push_tag_value(&mut tags, "COPYRIGHT", v); }
    if meta.pre_emphasis {
        push_tag_value(&mut tags, "PRE_EMPHASIS", "1");
        push_tag_value(&mut tags, "CUE_FLAGS", "PRE");
    }
    if album.total_tracks > 0 { push_tag_value(&mut tags, "TOTALTRACKS", &album.total_tracks.to_string()); }
    if let Some(n) = album.total_discs { push_tag_value(&mut tags, "TOTALDISCS", &n.to_string()); }
    if let Some(n) = album.disc_number { push_tag_value(&mut tags, "DISCNUMBER", &n.to_string()); }
    if let Some(v) = album.extra.get("catalog") { push_tag_value(&mut tags, "CATALOG", v); }
    for (key, value) in &album.extra {
        let tag_key = cue_extra_tag_key("ALBUM", key);
        push_tag_value(&mut tags, &tag_key, value);
    }
    for (key, value) in &meta.extra {
        let tag_key = cue_extra_tag_key("TRACK", key);
        push_tag_value(&mut tags, &tag_key, value);
    }

    if tags.is_empty() { return Ok(()); }

    let cmd = match ext.as_str() {
        "flac" => {
            let mut args = Vec::new();
            for (k, v) in &tags {
                args.push(format!("--remove-tag={}", k));
                args.push(format!("--set-tag={}={}", k, v));
            }
            args.push(path.display().to_string());
            ToolCommand { binary: ToolBinary::Metaflac, args, secret_args: vec![], cwd: None, env: vec![], timeout: Duration::from_secs(30) }
        }
        "opus" | "ogg" => {
            let mut args = Vec::new();
            for (k, _) in &tags { args.push("--delete".into()); args.push(k.clone()); }
            for (k, v) in &tags { args.push("-s".into()); args.push(format!("{}={}", k, v)); }
            args.push("--in-place".into());
            args.push(path.display().to_string());
            ToolCommand { binary: ToolBinary::Opustags, args, secret_args: vec![], cwd: None, env: vec![], timeout: Duration::from_secs(30) }
        }
        "wv" => {
            let mut args = vec!["-q".to_string()];
            for (k, v) in &tags { args.push("-w".into()); args.push(format!("{}={}", k, v)); }
            args.push(path.display().to_string());
            ToolCommand { binary: ToolBinary::Wvtag, args, secret_args: vec![], cwd: None, env: vec![], timeout: Duration::from_secs(30) }
        }
        "mp3" | "m4a" | "aac" | "wav" | "aiff" | "aif" => {
            let tmp = path.with_extension(format!("tmp.{}", path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")));
            let mut args = vec!["-y".into(), "-i".into(), path.display().to_string()];
            for (k, v) in &tags { args.push("-metadata".into()); args.push(format!("{}={}", k.to_lowercase(), v)); }
            args.push("-c".into()); args.push("copy".into());
            args.push(tmp.display().to_string());
            ToolCommand { binary: ToolBinary::Ffmpeg, args, secret_args: vec![], cwd: None, env: vec![], timeout: Duration::from_secs(60) }
        }
        _ => { return Err(MetadataError::UnsupportedTagFormat(ext)); }
    };

    runner.run(cmd, cancel).await.map_err(MetadataError::Tool)?;

    if matches!(ext.as_str(), "mp3" | "m4a" | "aac" | "wav" | "aiff" | "aif") {
        let tmp = path.with_extension(format!("tmp.{}", path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")));
        if tmp.exists() { fs::rename(&tmp, path)?; }
    }

    Ok(())
}

/// Apply ReplayGain tags via loudgain.
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
            args.push("-a".to_string());
            args.push("-k".to_string());
            args.push("-s".to_string());
            args.push("i".to_string());
            for t in tracks { args.push(t.staged_path.display().to_string()); }
        }
        AudioArtifacts::Merged(merged) => {
            args.push("-k".to_string());
            args.push("-s".to_string());
            args.push("i".to_string());
            args.push(merged.staged_path.display().to_string());
        }
    }

    let cmd = ToolCommand {
        binary: ToolBinary::Loudgain, args, secret_args: vec![],
        cwd: None, env: vec![], timeout: Duration::from_secs(600),
    };
    runner.run(cmd, cancel).await.map_err(ReplayGainError::Tool)?;
    Ok(StageRecord { stage: PipelineStage::ReplayGain, outcome: StageOutcome::Ok })
}

/// Generate feature sidecars: conversion log and CUE sheet.
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

    let log_staged = staging.root.join("conversion_log.txt");
    let log_content = build_conversion_log(outcome, source, req);
    fs::write(&log_staged, &log_content)?;
    artifacts.sidecars.push(SidecarArtifact {
        kind: SidecarKind::ConversionLog,
        staged_path: log_staged,
        final_path: album_dir.join("conversion_log.txt"),
    });

    if source.tracks.len() > 1 {
        let cue_staged = staging.root.join("album.cue");
        let cue_content = build_cue_sheet(source, &artifacts);
        fs::write(&cue_staged, &cue_content)?;
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

fn build_conversion_log(outcome: &AlbumOutcome, source: &PreparedSource, req: &PipelineRequest) -> String {
    let mut log = String::new();
    log.push_str(&format!("Conversion Log — {}\n", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")));
    log.push_str(&format!("Source: {}\n", source.container.display()));
    log.push_str(&format!("Target format: {:?}\n", req.target_format));
    log.push_str(&format!("Tracks in source: {}\n\n", source.tracks.len()));
    match outcome {
        AlbumOutcome::Complete { tracks, .. } => { log.push_str(&format!("Result: Complete ({} tracks)\n", tracks.len())); }
        AlbumOutcome::Partial { successful, failed, .. } => { log.push_str(&format!("Result: Partial ({} ok, {} failed)\n", successful.len(), failed.len())); }
        AlbumOutcome::Blocked { reason, .. } => { log.push_str(&format!("Result: Blocked ({:?})\n", reason)); }
    }
    log
}

fn build_cue_sheet(source: &PreparedSource, artifacts: &ArtifactSet) -> String {
    let mut cue = String::new();
    if let Some(ref album) = source.album_metadata.album { cue.push_str(&format!("TITLE \"{}\"\n", album)); }
    if let Some(ref artist) = source.album_metadata.album_artist { cue.push_str(&format!("PERFORMER \"{}\"\n", artist)); }
    match &artifacts.audio {
        AudioArtifacts::Tracks(tracks) => {
            for (i, t) in tracks.iter().enumerate() {
                let filename = t.staged_path.file_name().and_then(|f| f.to_str()).unwrap_or("unknown");
                cue.push_str(&format!("FILE \"{}\" WAVE\n", filename));
                cue.push_str(&format!("  TRACK {:02} AUDIO\n", i + 1));
                if let Some(st) = source.tracks.iter().find(|s| s.id == t.track_id) {
                    if let Some(ref title) = st.metadata.title { cue.push_str(&format!("    TITLE \"{}\"\n", title)); }
                    if let Some(ref artist) = st.metadata.artist { cue.push_str(&format!("    PERFORMER \"{}\"\n", artist)); }
                }
                cue.push_str("    INDEX 01 00:00:00\n");
            }
        }
        AudioArtifacts::Merged(merged) => {
            let filename = merged.staged_path.file_name().and_then(|f| f.to_str()).unwrap_or("merged");
            cue.push_str(&format!("FILE \"{}\" WAVE\n", filename));
            for (i, st) in source.tracks.iter().enumerate() {
                cue.push_str(&format!("  TRACK {:02} AUDIO\n", i + 1));
                if let Some(ref title) = st.metadata.title { cue.push_str(&format!("    TITLE \"{}\"\n", title)); }
                if let Some(ref artist) = st.metadata.artist { cue.push_str(&format!("    PERFORMER \"{}\"\n", artist)); }
                cue.push_str("    INDEX 01 00:00:00\n");
            }
        }
    }
    cue
}

// ===========================================================================
// Publish  (PR 4 bodies)
// ===========================================================================

/// Build the staged-to-final publish plan from staged artifacts.
pub fn build_publish_plan(
    artifacts: &ArtifactSet,
    req: &PipelineRequest,
) -> Result<PublishPlan, PublishError> {
    let output_root = normalize_path(&req.output_root);
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();

    match &artifacts.audio {
        AudioArtifacts::Tracks(tracks) => {
            for artifact in tracks {
                push_publish_entry(
                    &mut entries,
                    &mut seen,
                    &output_root,
                    artifact.staged_path.clone(),
                    artifact.final_path.clone(),
                    PublishRole::Audio,
                )?;
            }
        }
        AudioArtifacts::Merged(merged) => {
            push_publish_entry(
                &mut entries,
                &mut seen,
                &output_root,
                merged.staged_path.clone(),
                merged.final_path.clone(),
                PublishRole::Audio,
            )?;
        }
    }

    for sidecar in &artifacts.sidecars {
        push_publish_entry(
            &mut entries,
            &mut seen,
            &output_root,
            sidecar.staged_path.clone(),
            sidecar.final_path.clone(),
            PublishRole::Sidecar(sidecar.kind.clone()),
        )?;
    }

    if entries.is_empty() {
        return Err(PublishError::StagingMissing);
    }
    let album_dir = infer_publish_album_dir(&entries, &output_root, req.naming.per_album_subdir)?;

    Ok(PublishPlan { album_dir, entries })
}

/// Publish a whole album atomically by filling a temp directory beside the
/// final album directory, then renaming it into place.
pub fn publish_album_output(
    staging: StagingDir,
    plan: &PublishPlan,
    policy: PublishPolicy,
) -> Result<PublishedAlbum, PublishError> {
    if plan.entries.is_empty() {
        return Err(PublishError::StagingMissing);
    }
    if !staging.root.exists() {
        return Err(PublishError::StagingMissing);
    }

    let final_parent = parent_dir_or_current(&plan.album_dir);
    let _publish_lock = acquire_publish_lock(&plan.album_dir)?;

    let album_name = plan
        .album_dir
        .file_name()
        .and_then(|s| s.to_str())
        .map(sanitize_component)
        .unwrap_or_else(|| "album".to_string());
    let marker_path = final_parent.join(format!(".{album_name}.publish-in-progress"));
    repair_interrupted_publish(&plan.album_dir, &marker_path)?;
    cleanup_orphan_publish_temps(final_parent, &album_name)?;

    let temp_dir = unique_path(final_parent, &format!(".{album_name}.tmp"));
    let backup_dir = unique_path(final_parent, &format!(".{album_name}.backup"));

    if let Err(err) = fs::create_dir_all(&temp_dir) {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(PublishError::Io(err));
    }

    let mut published_entries = Vec::with_capacity(plan.entries.len());
    for entry in &plan.entries {
        if !entry.staged_path.exists() {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(PublishError::StagingMissing);
        }
        let rel = match entry.final_path.strip_prefix(&plan.album_dir) {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => {
                return cleanup_publish_temp(
                    &temp_dir,
                    PublishError::PathOutsideOutputRoot(entry.final_path.display().to_string()),
                );
            }
        };
        if let Err(err) = reject_escaping_path(&rel) {
            return cleanup_publish_temp(&temp_dir, PublishError::PathOutsideOutputRoot(err));
        }
        let staged_final = temp_dir.join(rel);
        if let Some(parent) = staged_final.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                return cleanup_publish_temp(&temp_dir, PublishError::Io(err));
            }
        }
        if let Err(err) = copy_or_rename_into_publish_temp(
            &entry.staged_path,
            &staged_final,
            policy.same_filesystem_required,
        ) {
            return cleanup_publish_temp(&temp_dir, err);
        }
        let bytes = match fs::metadata(&staged_final) {
            Ok(metadata) => metadata.len(),
            Err(err) => return cleanup_publish_temp(&temp_dir, PublishError::Io(err)),
        };
        published_entries.push(PublishedEntry {
            final_path: entry.final_path.clone(),
            role: entry.role.clone(),
            bytes,
        });
    }

    let mut backup_made = false;
    if plan.album_dir.exists() {
        match policy.overwrite {
            OverwritePolicy::FailIfExists => {
                let _ = fs::remove_dir_all(&temp_dir);
                return Err(PublishError::DestinationExists(plan.album_dir.display().to_string()));
            }
            OverwritePolicy::ReplaceWithBackup => {
                if let Err(err) = write_publish_marker(&marker_path, &backup_dir) {
                    let _ = fs::remove_dir_all(&temp_dir);
                    return Err(err);
                }
                fs::rename(&plan.album_dir, &backup_dir).map_err(|err| {
                    let _ = fs::remove_dir_all(&temp_dir);
                    let _ = fs::remove_file(&marker_path);
                    PublishError::BackupFailed(format!(
                        "{} -> {}: {err}",
                        plan.album_dir.display(),
                        backup_dir.display()
                    ))
                })?;
                backup_made = true;
            }
        }
    }

    if let Err(err) = fs::rename(&temp_dir, &plan.album_dir) {
        let _ = fs::remove_dir_all(&temp_dir);
        if backup_made {
            if let Err(rollback_err) = fs::rename(&backup_dir, &plan.album_dir) {
                return Err(PublishError::RollbackFailed(format!(
                    "publish failed with {err}; rollback failed with {rollback_err}; recovery marker left at {}",
                    marker_path.display()
                )));
            }
            let _ = fs::remove_file(&marker_path);
            sync_parent_dir_best_effort(&marker_path);
        }
        return Err(PublishError::AtomicRename(format!(
            "{} -> {}: {err}",
            temp_dir.display(),
            plan.album_dir.display()
        )));
    }
    if backup_made {
        let _ = fs::remove_file(&marker_path);
    }
    sync_parent_dir_best_effort(&plan.album_dir);

    drop(staging);

    Ok(PublishedAlbum {
        album_dir: plan.album_dir.clone(),
        entries: published_entries,
    })
}

// ===========================================================================
// Durable log  (PR 6 body; PR 4 ships a minimal interim body)
// ===========================================================================

/// Write the interim durable JSON report used by PR 4.
pub fn write_durable_log(
    report: &PipelineReport,
    log: &LogPolicy,
) -> Result<PathBuf, LogError> {
    fs::create_dir_all(&log.root).map_err(LogError::Io)?;
    let job = sanitize_component(&report.request.job_id);
    let item = sanitize_component(&report.request.item_id);
    let path = log.root.join(format!("{job}-{item}.json"));
    let mut report_to_write = report.clone();
    report_to_write.durable_log = Some(path.clone());
    let bytes = serde_json::to_vec_pretty(&report_to_write)
        .map_err(|err| LogError::Serialization(err.to_string()))?;
    write_bytes_atomically(&path, &bytes).map_err(LogError::Io)?;
    Ok(path)
}

// ===========================================================================
// Orchestrator  (PR 4 body, final shape)
// ===========================================================================

/// Run the staged pipeline for one queue item. `process_item` does not call
/// this yet; PR 4 freezes this orchestration shape for later PRs.
pub async fn run_pipeline_item(
    req: PipelineRequest,
    runner: &dyn ToolRunner,
    reporter: &dyn PipelineReporter,
    cancel: &CancellationToken,
) -> PipelineReport {
    let item_id = req.item_id.clone();
    let mut source = None;
    let mut plan = None;
    let mut artifacts = None;
    let mut published = None;
    let mut tracks = Vec::new();
    let mut stages = Vec::new();

    if cancel.is_cancelled() {
        let outcome = AlbumOutcome::Blocked {
            successful: Vec::new(),
            failed: Vec::new(),
            stages,
            reason: BlockReason::Cancelled,
        };
        return finalize_report(&req, reporter, source, plan, artifacts, published, outcome).await;
    }

    if let Err(err) = validate_request(&req) {
        let record = stage_record(PipelineStage::Materialize, StageOutcome::Failed(err.to_string()));
        emit_stage_finished(reporter, &item_id, record.clone()).await;
        stages.push(record);
        let outcome = AlbumOutcome::Blocked {
            successful: Vec::new(),
            failed: Vec::new(),
            stages,
            reason: BlockReason::MaterializeFailed,
        };
        return finalize_report(&req, reporter, source, plan, artifacts, published, outcome).await;
    }

    let staging_parent = staging_parent_for(&req);
    if let Err(err) = fs::create_dir_all(&staging_parent) {
        let record = stage_record(
            PipelineStage::Materialize,
            StageOutcome::Failed(format!("could not create staging parent directory: {err}")),
        );
        emit_stage_finished(reporter, &item_id, record.clone()).await;
        stages.push(record);
        let outcome = AlbumOutcome::Blocked {
            successful: Vec::new(),
            failed: Vec::new(),
            stages,
            reason: BlockReason::MaterializeFailed,
        };
        return finalize_report(&req, reporter, source, plan, artifacts, published, outcome).await;
    }
    let _run_lock = match acquire_run_lock(&staging_parent, &req.job_id, &req.item_id) {
        Ok(lock) => lock,
        Err(err) => {
            let record = stage_record(PipelineStage::Materialize, StageOutcome::Failed(err.to_string()));
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            let outcome = AlbumOutcome::Blocked {
                successful: Vec::new(),
                failed: Vec::new(),
                stages,
                reason: BlockReason::MaterializeFailed,
            };
            return finalize_report(&req, reporter, source, plan, artifacts, published, outcome).await;
        }
    };
    let staging_root = staging_parent.join(format!(
        "{}-{}",
        sanitize_component(&req.job_id),
        sanitize_component(&req.item_id)
    ));
    let _ = delete_stale_staging_dir(&staging_root);
    let staging = StagingDir::new(staging_root, req.job_id.clone());

    if let Err(err) = fs::create_dir_all(&staging.root) {
        let record = stage_record(
            PipelineStage::Materialize,
            StageOutcome::Failed(format!("could not create staging directory: {err}")),
        );
        emit_stage_finished(reporter, &item_id, record.clone()).await;
        stages.push(record);
        let outcome = AlbumOutcome::Blocked {
            successful: Vec::new(),
            failed: Vec::new(),
            stages,
            reason: BlockReason::MaterializeFailed,
        };
        return finalize_report(&req, reporter, source, plan, artifacts, published, outcome).await;
    }

    emit_stage_started(reporter, &item_id, PipelineStage::Materialize).await;
    let materialized = match detect_source_kind(&req) {
        Ok(kind) => match materializer_for(kind) {
            Ok(materializer) => materializer.materialize(&req, &staging, runner, cancel).await,
            Err(err) => Err(MaterializeError::Parse(err.to_string())),
        },
        Err(err) => Err(MaterializeError::Parse(err.to_string())),
    };
    match materialized {
        Ok(prepared) => {
            let record = stage_record(PipelineStage::Materialize, StageOutcome::Ok);
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            source = Some(prepared);
        }
        Err(err) => {
            let reason = if matches!(err, MaterializeError::Cancelled) {
                BlockReason::Cancelled
            } else {
                BlockReason::MaterializeFailed
            };
            let record = stage_record(PipelineStage::Materialize, StageOutcome::Failed(err.to_string()));
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            let outcome = AlbumOutcome::Blocked {
                successful: Vec::new(),
                failed: Vec::new(),
                stages,
                reason,
            };
            return finalize_report(&req, reporter, source, plan, artifacts, published, outcome).await;
        }
    }

    if cancel.is_cancelled() {
        let outcome = AlbumOutcome::Blocked {
            successful: Vec::new(),
            failed: Vec::new(),
            stages,
            reason: BlockReason::Cancelled,
        };
        return finalize_report(&req, reporter, source, plan, artifacts, published, outcome).await;
    }

    emit_stage_started(reporter, &item_id, PipelineStage::PlanOutputs).await;
    match plan_outputs(source.as_ref().expect("materialized source present"), &req) {
        Ok(album_plan) => {
            let record = stage_record(PipelineStage::PlanOutputs, StageOutcome::Ok);
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            plan = Some(album_plan);
        }
        Err(err) => {
            let record = stage_record(PipelineStage::PlanOutputs, StageOutcome::Failed(err.to_string()));
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            let outcome = AlbumOutcome::Blocked {
                successful: Vec::new(),
                failed: Vec::new(),
                stages,
                reason: BlockReason::PlanFailed,
            };
            return finalize_report(&req, reporter, source, plan, artifacts, published, outcome).await;
        }
    }

    emit_stage_started(reporter, &item_id, PipelineStage::Convert).await;
    let converted = convert_tracks(
        source.as_ref().expect("source present"),
        plan.as_ref().expect("plan present"),
        &req,
        &staging,
        runner,
        cancel,
    )
    .await;
    emit_stage_finished(reporter, &item_id, converted.record.clone()).await;
    stages.push(converted.record.clone());
    tracks = converted.tracks.clone();
    artifacts = Some(converted.artifacts);

    let mut current_outcome = aggregate_album_outcome(tracks.clone(), stages.clone(), req.failure_policy);
    if cancel.is_cancelled() {
        current_outcome = cancelled_outcome_from(current_outcome);
        return finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await;
    }
    if matches!(current_outcome, AlbumOutcome::Partial { .. })
        && artifacts.as_ref().map(audio_artifact_count).unwrap_or(0) == 0
    {
        current_outcome = AlbumOutcome::Blocked {
            successful: successful_tracks_from(&current_outcome),
            failed: failed_tracks_from(&current_outcome),
            stages: stages_from(&current_outcome),
            reason: BlockReason::TrackFailures,
        };
    }
    if matches!(current_outcome, AlbumOutcome::Blocked { .. }) {
        return finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await;
    }

    if req.merge {
        emit_stage_started(reporter, &item_id, PipelineStage::Merge).await;
        match merge_tracks(
            artifacts.take().expect("artifacts present"),
            &req,
            &staging,
            runner,
            cancel,
        )
        .await
        {
            Ok((merged_artifacts, record)) => {
                emit_stage_finished(reporter, &item_id, record.clone()).await;
                stages.push(record);
                artifacts = Some(merged_artifacts);
            }
            Err(err) => {
                let record = stage_record(PipelineStage::Merge, StageOutcome::Failed(err.to_string()));
                emit_stage_finished(reporter, &item_id, record.clone()).await;
                stages.push(record);
                current_outcome = aggregate_album_outcome(tracks.clone(), stages.clone(), req.failure_policy);
                return finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await;
            }
        }
    } else {
        let record = stage_record(PipelineStage::Merge, StageOutcome::Skipped);
        emit_stage_finished(reporter, &item_id, record.clone()).await;
        stages.push(record);
    }

    if req.stages.metadata == StageRequirement::Enabled {
        emit_stage_started(reporter, &item_id, PipelineStage::Metadata).await;
        match apply_metadata(
            artifacts.as_ref().expect("artifacts present"),
            source.as_ref().expect("source present"),
            &req,
            runner,
            cancel,
        )
        .await
        {
            Ok(record) => {
                emit_stage_finished(reporter, &item_id, record.clone()).await;
                stages.push(record);
            }
            Err(err) => {
                let record = stage_record(PipelineStage::Metadata, StageOutcome::Failed(err.to_string()));
                emit_stage_finished(reporter, &item_id, record.clone()).await;
                stages.push(record);
                current_outcome = aggregate_album_outcome(tracks.clone(), stages.clone(), req.failure_policy);
                return finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await;
            }
        }
    } else {
        let record = stage_record(PipelineStage::Metadata, StageOutcome::Skipped);
        emit_stage_finished(reporter, &item_id, record.clone()).await;
        stages.push(record);
    }

    if req.stages.replaygain == StageRequirement::Enabled {
        emit_stage_started(reporter, &item_id, PipelineStage::ReplayGain).await;
        match apply_replaygain(
            artifacts.as_ref().expect("artifacts present"),
            &req,
            runner,
            cancel,
        )
        .await
        {
            Ok(record) => {
                emit_stage_finished(reporter, &item_id, record.clone()).await;
                stages.push(record);
            }
            Err(err) => {
                let record = stage_record(PipelineStage::ReplayGain, StageOutcome::Failed(err.to_string()));
                emit_stage_finished(reporter, &item_id, record.clone()).await;
                stages.push(record);
                current_outcome = aggregate_album_outcome(tracks.clone(), stages.clone(), req.failure_policy);
                return finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await;
            }
        }
    } else {
        let record = stage_record(PipelineStage::ReplayGain, StageOutcome::Skipped);
        emit_stage_finished(reporter, &item_id, record.clone()).await;
        stages.push(record);
    }

    current_outcome = aggregate_album_outcome(tracks.clone(), stages.clone(), req.failure_policy);
    if matches!(current_outcome, AlbumOutcome::Blocked { .. }) {
        return finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await;
    }

    if req.stages.features == StageRequirement::Enabled {
        emit_stage_started(reporter, &item_id, PipelineStage::Features).await;
        match run_features(
            artifacts.take().expect("artifacts present"),
            &current_outcome,
            source.as_ref().expect("source present"),
            &req,
            &staging,
            runner,
            cancel,
        )
        .await
        {
            Ok((feature_artifacts, record)) => {
                emit_stage_finished(reporter, &item_id, record.clone()).await;
                stages.push(record);
                artifacts = Some(feature_artifacts);
            }
            Err(err) => {
                let record = stage_record(PipelineStage::Features, StageOutcome::Failed(err.to_string()));
                emit_stage_finished(reporter, &item_id, record.clone()).await;
                stages.push(record);
                current_outcome = aggregate_album_outcome(tracks.clone(), stages.clone(), req.failure_policy);
                return finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await;
            }
        }
    } else {
        let record = stage_record(PipelineStage::Features, StageOutcome::Skipped);
        emit_stage_finished(reporter, &item_id, record.clone()).await;
        stages.push(record);
    }

    current_outcome = aggregate_album_outcome(tracks.clone(), stages.clone(), req.failure_policy);
    if cancel.is_cancelled() {
        current_outcome = cancelled_outcome_from(current_outcome);
        return finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await;
    }
    if matches!(current_outcome, AlbumOutcome::Blocked { .. }) {
        return finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await;
    }

    emit_stage_started(reporter, &item_id, PipelineStage::Publish).await;
    match artifacts
        .as_ref()
        .ok_or(PublishError::StagingMissing)
        .and_then(|artifact_set| build_publish_plan(artifact_set, &req))
        .and_then(|publish_plan| publish_album_output(staging, &publish_plan, req.publish.clone()))
    {
        Ok(album) => {
            let record = stage_record(PipelineStage::Publish, StageOutcome::Ok);
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            published = Some(album);
        }
        Err(err) => {
            let record = stage_record(PipelineStage::Publish, StageOutcome::Failed(err.to_string()));
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            current_outcome = AlbumOutcome::Blocked {
                successful: successful_tracks_from(&current_outcome),
                failed: failed_tracks_from(&current_outcome),
                stages,
                reason: BlockReason::PublishFailed,
            };
            return finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await;
        }
    }

    current_outcome = aggregate_album_outcome(tracks, stages, req.failure_policy);
    finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await
}

async fn finalize_report(
    req: &PipelineRequest,
    reporter: &dyn PipelineReporter,
    source: Option<PreparedSource>,
    plan: Option<AlbumPlan>,
    artifacts: Option<ArtifactSet>,
    published: Option<PublishedAlbum>,
    mut outcome: AlbumOutcome,
) -> PipelineReport {
    let item_id = req.item_id.clone();
    let mut durable_log = None;
    let mut terminal_error_override: Option<String> = None;
    let should_write = match &outcome {
        AlbumOutcome::Complete { .. } | AlbumOutcome::Partial { .. } => true,
        AlbumOutcome::Blocked { .. } => req.log.write_for_blocked,
    };

    if should_write {
        emit_stage_started(reporter, &item_id, PipelineStage::DurableLog).await;
        let ok_record = stage_record(PipelineStage::DurableLog, StageOutcome::Ok);
        let mut logged_outcome = outcome.clone();
        push_stage(&mut logged_outcome, ok_record.clone());
        let report_to_write = PipelineReport {
            request: RedactedPipelineRequest::from(req),
            source: source.clone(),
            plan: plan.clone(),
            artifacts: artifacts.clone(),
            published: published.clone(),
            outcome: logged_outcome.clone(),
            durable_log: None,
        };
        match write_durable_log(&report_to_write, &req.log) {
            Ok(path) => {
                durable_log = Some(path);
                outcome = logged_outcome;
                emit_stage_finished(reporter, &item_id, ok_record).await;
            }
            Err(err) => {
                let error_text = err.to_string();
                let terminal_error = durable_log_failure_terminal_error(
                    &outcome,
                    published.as_ref(),
                    &error_text,
                );
                let record = stage_record(PipelineStage::DurableLog, StageOutcome::Failed(error_text));
                push_stage(&mut outcome, record.clone());
                emit_stage_finished(reporter, &item_id, record).await;
                terminal_error_override = Some(terminal_error);
                if !matches!(outcome, AlbumOutcome::Blocked { .. }) {
                    outcome = AlbumOutcome::Blocked {
                        successful: successful_tracks_from(&outcome),
                        failed: failed_tracks_from(&outcome),
                        stages: stages_from(&outcome),
                        reason: BlockReason::DurableLogFailed,
                    };
                }
            }
        }
    } else {
        let record = stage_record(PipelineStage::DurableLog, StageOutcome::Skipped);
        push_stage(&mut outcome, record.clone());
        emit_stage_finished(reporter, &item_id, record).await;
    }

    let status = terminal_status(
        &outcome,
        published.as_ref(),
        durable_log.as_deref(),
        terminal_error_override,
    );
    reporter
        .emit(PipelineEvent::Terminal {
            item_id,
            status,
        })
        .await;

    PipelineReport {
        request: RedactedPipelineRequest::from(req),
        source,
        plan,
        artifacts,
        published,
        outcome,
        durable_log,
    }
}

// ===========================================================================
// Real PR 1 logic — outcome aggregation + queue-status mapping
// ===========================================================================

/// Aggregate per-track records + stage records into an `AlbumOutcome`.
///
/// Rules:
/// - A `StageRecord` with `StageOutcome::Failed` always blocks ->
///   `Blocked` with `RequiredStageFailure`. Disabled stages reach
///   aggregation as `StageOutcome::Skipped` and never block.
/// - Otherwise, with no failed tracks -> `Complete`.
/// - With failed tracks: `FailAlbumOnAnyTrackFailure` -> `Blocked`
///   (`TrackFailures`); `AllowPartialAlbum` -> `Partial`.
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
/// `Complete` -> `Completed`, `Partial` -> `Partial`, `Blocked` ->
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

// ===========================================================================
// Private helpers
// ===========================================================================

fn durable_log_failure_terminal_error(
    outcome: &AlbumOutcome,
    published: Option<&PublishedAlbum>,
    error_text: &str,
) -> String {
    if let Some(album) = published {
        return format!(
            "durable log failed after publish; final artifacts may already exist at {}: {error_text}",
            album.album_dir.display()
        );
    }

    if let AlbumOutcome::Blocked { reason, .. } = outcome {
        return format!(
            "album blocked: {:?}; durable log failed: {error_text}",
            reason
        );
    }

    format!("durable log failed: {error_text}")
}

fn terminal_status(
    outcome: &AlbumOutcome,
    published: Option<&PublishedAlbum>,
    durable_log: Option<&Path>,
    error_override: Option<String>,
) -> ConversionStatus {
    if let Some(error) = error_override {
        return ConversionStatus::Failed {
            error,
            log_path: durable_log.map(Path::to_path_buf),
        };
    }
    map_album_outcome(outcome, published, durable_log)
}

fn validate_root_dir(path: &Path, label: &str) -> Result<(), RequestValidationError> {
    if path.as_os_str().is_empty() {
        return Err(RequestValidationError::InvalidOutputRoot(format!(
            "{label} must not be empty"
        )));
    }
    if path.exists() && !path.is_dir() {
        return Err(RequestValidationError::InvalidOutputRoot(format!(
            "{label} is not a directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn stage_record(stage: PipelineStage, outcome: StageOutcome) -> StageRecord {
    StageRecord { stage, outcome }
}

async fn emit_stage_started(
    reporter: &dyn PipelineReporter,
    item_id: &str,
    stage: PipelineStage,
) {
    reporter
        .emit(PipelineEvent::StageStarted {
            item_id: item_id.to_string(),
            stage,
        })
        .await;
}

async fn emit_stage_finished(
    reporter: &dyn PipelineReporter,
    item_id: &str,
    record: StageRecord,
) {
    reporter
        .emit(PipelineEvent::StageFinished {
            item_id: item_id.to_string(),
            record,
        })
        .await;
}

fn validate_template(template: &str) -> Result<(), String> {
    let known = [
        "NN", "N", "TRACK", "TITLE", "ARTIST", "ALBUM_ARTIST", "ALBUM", "DISC", "FORMAT",
    ];
    let mut rest = template;
    while let Some(start) = rest.find('%') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('%') else {
            return Err("unclosed % token".to_string());
        };
        let token = &rest[..end];
        if !known.contains(&token) {
            return Err(format!("unknown token %{token}%"));
        }
        rest = &rest[end + 1..];
    }
    Ok(())
}

fn render_track_template(
    template: &str,
    source: &PreparedSource,
    track: &PreparedTrack,
    format: AudioFormat,
) -> Result<PathBuf, PlanError> {
    let title = track
        .metadata
        .title
        .as_deref()
        .map(sanitize_component)
        .unwrap_or_else(|| format!("Track {:02}", track.id.track_number));
    let artist = track
        .metadata
        .artist
        .as_deref()
        .or(source.album_metadata.album_artist.as_deref())
        .map(sanitize_component)
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let album_artist = source
        .album_metadata
        .album_artist
        .as_deref()
        .map(sanitize_component)
        .unwrap_or_else(|| artist.clone());
    let album = source
        .album_metadata
        .album
        .as_deref()
        .map(sanitize_component)
        .unwrap_or_else(|| "Album".to_string());
    let disc = track.id.disc_number.or(track.metadata.disc_number).unwrap_or(1);
    let n = track.metadata.track_number.unwrap_or(track.id.track_number);

    let mut rendered = template.to_string();
    rendered = rendered.replace("%NN%", &format!("{n:02}"));
    rendered = rendered.replace("%N%", &n.to_string());
    rendered = rendered.replace("%TRACK%", &n.to_string());
    rendered = rendered.replace("%TITLE%", &title);
    rendered = rendered.replace("%ARTIST%", &artist);
    rendered = rendered.replace("%ALBUM_ARTIST%", &album_artist);
    rendered = rendered.replace("%ALBUM%", &album);
    rendered = rendered.replace("%DISC%", &disc.to_string());
    rendered = rendered.replace("%FORMAT%", format.extension());

    let rel = PathBuf::from(rendered);
    if rel.as_os_str().is_empty() {
        return Err(PlanError::InvalidTemplate("template rendered empty".to_string()));
    }
    Ok(rel)
}

fn sanitize_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();
    let trimmed = sanitized.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        "untitled".to_string()
    } else {
        trimmed
    }
}

fn reject_escaping_path(path: &Path) -> Result<(), String> {
    if path.is_absolute() {
        return Err(format!("absolute path not allowed: {}", path.display()));
    }
    for component in path.components() {
        match component {
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(format!("path escapes destination: {}", path.display()));
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(())
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn path_is_under_root(path: &Path, root: &Path) -> bool {
    let path = normalize_path(path);
    let root = normalize_path(root);
    path == root || path.starts_with(root)
}

fn parent_dir_or_current(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn normalized_collision_key(path: &Path) -> String {
    path.to_string_lossy().to_ascii_lowercase()
}

fn append_default_extension(path: &mut PathBuf, format: AudioFormat) {
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case(format.extension()))
        .unwrap_or(false)
    {
        return;
    }
    path.set_extension(format.extension());
}

fn append_collision_suffix(path: &Path, suffix: &str) -> PathBuf {
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("track");
    let ext = path.extension().and_then(|s| s.to_str());
    let mut file_name = format!("{stem}-{suffix}");
    if let Some(ext) = ext {
        file_name.push('.');
        file_name.push_str(ext);
    }
    parent.join(file_name)
}

fn staged_audio_path(
    convert_root: &Path,
    final_path: &Path,
    id: &TrackId,
    format: AudioFormat,
) -> PathBuf {
    let file_stem = final_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(sanitize_component)
        .unwrap_or_else(|| format!("track-{:03}", id.source_ordinal));
    convert_root.join(format!(
        "{:03}-{}.{}",
        id.source_ordinal,
        file_stem,
        format.extension()
    ))
}

fn encode_command(input: &Path, output: &Path, req: &PipelineRequest) -> Result<ToolCommand, ConvertError> {
    let backend = match req.encode.backend {
        EncodeBackend::Sox => BackendCrateKind::Sox,
        EncodeBackend::Auto | EncodeBackend::Ffmpeg | EncodeBackend::BackendCrate => {
            BackendCrateKind::FFmpeg
        }
    };
    let settings = backend_settings(req);
    let command = CommandBuilder::new(backend)
        .build(input, output, &settings)
        .map_err(|err| ConvertError::Backend(err.to_string()))?;
    tool_command_from_backend(command)
}

fn backend_settings(req: &PipelineRequest) -> BackendConversionSettings {
    let mut settings = BackendConversionSettings::default();
    settings.format = backend_audio_format(req.target_format);
    settings.compression_level = req.encode.compression_level;
    settings.mp3_bitrate = req.encode.bitrate;
    if req.encode.bitrate.is_some() {
        settings.mp3_mode = Some(tonepoet_backend::Mp3Mode::Cbr);
    }
    settings.dither_type = match req.encode.dither {
        DitherPolicy::Off => Some(tonepoet_backend::DitherType::None),
        DitherPolicy::On | DitherPolicy::Auto => None,
    };
    settings.overwrite = true;
    settings
}

fn backend_audio_format(format: AudioFormat) -> tonepoet_backend::AudioFormat {
    match format {
        AudioFormat::Flac => tonepoet_backend::AudioFormat::Flac,
        AudioFormat::Wav => tonepoet_backend::AudioFormat::Wav,
        AudioFormat::Aiff => tonepoet_backend::AudioFormat::Aiff,
        AudioFormat::WavPack => tonepoet_backend::AudioFormat::WavPack,
        AudioFormat::Mp3 => tonepoet_backend::AudioFormat::Mp3,
        AudioFormat::Aac => tonepoet_backend::AudioFormat::Aac,
        AudioFormat::Opus => tonepoet_backend::AudioFormat::Opus,
        AudioFormat::Alac => tonepoet_backend::AudioFormat::Alac,
    }
}

fn tool_command_from_backend(command: ConversionCommand) -> Result<ToolCommand, ConvertError> {
    let binary = match command.program.as_str() {
        "ffmpeg" => ToolBinary::Ffmpeg,
        "sox" => ToolBinary::Sox,
        other => {
            return Err(ConvertError::Backend(format!(
                "unsupported backend program: {other}"
            )));
        }
    };
    let env = command
        .environment
        .into_iter()
        .map(|(key, value)| EnvVar {
            key,
            value: SecretString::new(value),
            secret: false,
        })
        .collect();
    Ok(ToolCommand {
        binary,
        args: command.arguments,
        secret_args: Vec::new(),
        cwd: None,
        env,
        timeout: command.expected_duration.unwrap_or(DEFAULT_CONVERT_TIMEOUT),
    })
}

fn file_len(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|m| m.len())
}

fn failed_track_record(
    track: &PreparedTrack,
    realized_input: Option<PathBuf>,
    output_file: Option<PathBuf>,
    commands: Vec<CommandRecord>,
    error: String,
) -> TrackRecord {
    TrackRecord {
        track_id: track.id.clone(),
        outcome: TrackOutcome::Err(non_empty_error(error)),
        source_ref: track.source_ref.clone(),
        realized_input,
        output_file,
        commands,
        bytes_in: None,
        bytes_out: None,
        duration: None,
    }
}

fn non_empty_error(error: String) -> String {
    if error.trim().is_empty() {
        "unknown error".to_string()
    } else {
        error
    }
}

fn command_from_tool_error(err: &super::errors::ToolRunnerError) -> Option<CommandRecord> {
    match err {
        super::errors::ToolRunnerError::Spawn { command }
        | super::errors::ToolRunnerError::Timeout { command, .. }
        | super::errors::ToolRunnerError::Cancelled { command }
        | super::errors::ToolRunnerError::NonZeroExit { command, .. } => Some(command.clone()),
        super::errors::ToolRunnerError::Io(_) => None,
    }
}

fn push_publish_entry(
    entries: &mut Vec<PublishEntry>,
    seen: &mut BTreeSet<String>,
    output_root: &Path,
    staged_path: PathBuf,
    final_path: PathBuf,
    role: PublishRole,
) -> Result<(), PublishError> {
    let final_path = normalize_path(&final_path);
    if !path_is_under_root(&final_path, output_root) {
        return Err(PublishError::PathOutsideOutputRoot(final_path.display().to_string()));
    }
    let key = normalized_collision_key(&final_path);
    if !seen.insert(key) {
        return Err(PublishError::DestinationExists(final_path.display().to_string()));
    }
    entries.push(PublishEntry { staged_path, final_path, role });
    Ok(())
}

fn infer_publish_album_dir(
    entries: &[PublishEntry],
    output_root: &Path,
    per_album_subdir: bool,
) -> Result<PathBuf, PublishError> {
    if !per_album_subdir {
        return Ok(output_root.to_path_buf());
    }

    let mut album_component: Option<PathBuf> = None;
    for entry in entries {
        let final_path = normalize_path(&entry.final_path);
        let rel = final_path.strip_prefix(output_root).map_err(|_| {
            PublishError::PathOutsideOutputRoot(final_path.display().to_string())
        })?;
        let first_component = rel.components().find_map(|component| match component {
            Component::Normal(name) => Some(PathBuf::from(name.to_os_string())),
            _ => None,
        });
        let Some(first_component) = first_component else {
            return Err(PublishError::PathOutsideOutputRoot(format!(
                "publish artifact has no album directory below output_root: {}",
                final_path.display()
            )));
        };

        match &album_component {
            Some(existing) if existing == &first_component => {}
            Some(existing) => {
                return Err(PublishError::PathOutsideOutputRoot(format!(
                    "publish entries cross album boundary: {} and {}",
                    output_root.join(existing).display(),
                    output_root.join(first_component).display()
                )));
            }
            None => album_component = Some(first_component),
        }
    }

    let Some(component) = album_component else {
        return Err(PublishError::StagingMissing);
    };
    let album_dir = output_root.join(component);
    for entry in entries {
        if !path_is_under_root(&entry.final_path, &album_dir) {
            return Err(PublishError::PathOutsideOutputRoot(
                entry.final_path.display().to_string(),
            ));
        }
    }
    Ok(album_dir)
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct PublishRecoveryMarker {
    version: u8,
    album_dir_name: String,
    backup_dir_name: String,
}

fn write_publish_marker(marker_path: &Path, backup_dir: &Path) -> Result<(), PublishError> {
    let album_dir = marker_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix('.'))
        .and_then(|name| name.strip_suffix(".publish-in-progress"))
        .ok_or_else(|| {
            PublishError::BackupFailed(format!(
                "could not derive album name from publish marker {}",
                marker_path.display()
            ))
        })?;
    let backup_dir_name = backup_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            PublishError::BackupFailed(format!(
                "backup directory has no valid file name: {}",
                backup_dir.display()
            ))
        })?
        .to_string();
    let marker = PublishRecoveryMarker {
        version: 1,
        album_dir_name: album_dir.to_string(),
        backup_dir_name,
    };
    let bytes = serde_json::to_vec_pretty(&marker).map_err(|err| {
        PublishError::BackupFailed(format!(
            "could not serialize publish recovery marker {}: {err}",
            marker_path.display()
        ))
    })?;
    write_bytes_atomically(marker_path, &bytes).map_err(|err| {
        PublishError::BackupFailed(format!(
            "could not install publish recovery marker {}: {err}",
            marker_path.display()
        ))
    })?;
    Ok(())
}

fn repair_interrupted_publish(album_dir: &Path, marker_path: &Path) -> Result<(), PublishError> {
    if !marker_path.exists() {
        return Ok(());
    }

    if album_dir.exists() {
        // A final album already exists. Either the previous publish succeeded before
        // marker cleanup, or it failed before the backup move. In both cases, keep
        // any backup directory in place and clear the stale marker.
        let _ = fs::remove_file(marker_path);
        sync_parent_dir_best_effort(marker_path);
        return Ok(());
    }

    let marker_text = fs::read_to_string(marker_path).map_err(PublishError::Io)?;
    let backup_dir = backup_dir_from_marker(album_dir, marker_path, &marker_text)?;

    if backup_dir.exists() {
        fs::rename(&backup_dir, album_dir).map_err(|err| {
            PublishError::RollbackFailed(format!(
                "could not repair interrupted publish by restoring {} -> {}: {err}",
                backup_dir.display(),
                album_dir.display()
            ))
        })?;
        let _ = fs::remove_file(marker_path);
        sync_parent_dir_best_effort(album_dir);
        return Ok(());
    }

    Err(PublishError::RollbackFailed(format!(
        "publish recovery marker {} was present but validated backup {} was missing",
        marker_path.display(),
        backup_dir.display()
    )))
}

fn backup_dir_from_marker(
    album_dir: &Path,
    marker_path: &Path,
    marker_text: &str,
) -> Result<PathBuf, PublishError> {
    if let Ok(marker) = serde_json::from_str::<PublishRecoveryMarker>(marker_text) {
        return validate_structured_marker(album_dir, marker_path, marker);
    }

    // v9 wrote the backup path as a newline-terminated string. Accept that legacy
    // marker only if it validates as a backup directory for this exact album.
    let legacy = PathBuf::from(marker_text.trim());
    validate_backup_dir(album_dir, marker_path, legacy)
}

fn validate_structured_marker(
    album_dir: &Path,
    marker_path: &Path,
    marker: PublishRecoveryMarker,
) -> Result<PathBuf, PublishError> {
    if marker.version != 1 {
        return Err(PublishError::RollbackFailed(format!(
            "unsupported publish recovery marker version {} in {}",
            marker.version,
            marker_path.display()
        )));
    }

    let expected_album = album_dir_marker_name(album_dir)?;
    if marker.album_dir_name != expected_album {
        return Err(PublishError::RollbackFailed(format!(
            "publish recovery marker {} targets album {}, not {}",
            marker_path.display(),
            marker.album_dir_name,
            expected_album
        )));
    }

    let backup_name = PathBuf::from(&marker.backup_dir_name);
    reject_escaping_path(&backup_name).map_err(PublishError::PathOutsideOutputRoot)?;
    if backup_name.components().count() != 1 {
        return Err(PublishError::RollbackFailed(format!(
            "publish recovery marker {} contains non-leaf backup name {}",
            marker_path.display(),
            marker.backup_dir_name
        )));
    }

    let parent = parent_dir_or_current(album_dir);
    validate_backup_dir(album_dir, marker_path, parent.join(backup_name))
}

fn validate_backup_dir(
    album_dir: &Path,
    marker_path: &Path,
    backup_dir: PathBuf,
) -> Result<PathBuf, PublishError> {
    let parent = normalize_path(parent_dir_or_current(album_dir));
    let backup_parent = normalize_path(parent_dir_or_current(&backup_dir));
    if backup_parent != parent {
        return Err(PublishError::RollbackFailed(format!(
            "publish recovery marker {} points outside album parent: {}",
            marker_path.display(),
            backup_dir.display()
        )));
    }

    let backup_name = backup_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            PublishError::RollbackFailed(format!(
                "publish recovery marker {} contains invalid backup path {}",
                marker_path.display(),
                backup_dir.display()
            ))
        })?;
    let expected_prefix = backup_dir_prefix(album_dir)?;
    if !backup_name.starts_with(&expected_prefix) {
        return Err(PublishError::RollbackFailed(format!(
            "publish recovery marker {} points at non-matching backup {}",
            marker_path.display(),
            backup_dir.display()
        )));
    }

    Ok(backup_dir)
}

fn album_dir_marker_name(album_dir: &Path) -> Result<String, PublishError> {
    album_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_component)
        .ok_or_else(|| {
            PublishError::BackupFailed(format!(
                "album directory has no valid file name: {}",
                album_dir.display()
            ))
        })
}

fn backup_dir_prefix(album_dir: &Path) -> Result<String, PublishError> {
    Ok(format!(".{}.backup-", album_dir_marker_name(album_dir)?))
}

fn cleanup_orphan_publish_temps(parent: &Path, album_name: &str) -> Result<(), PublishError> {
    let prefix = format!(".{album_name}.tmp-");
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(PublishError::Io(err)),
    };

    for entry in entries {
        let entry = entry.map_err(PublishError::Io)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(&prefix) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path).map_err(PublishError::Io)?;
        } else {
            fs::remove_file(&path).map_err(PublishError::Io)?;
        }
    }
    Ok(())
}

fn cleanup_publish_temp<T>(temp_dir: &Path, err: PublishError) -> Result<T, PublishError> {
    let _ = fs::remove_dir_all(temp_dir);
    Err(err)
}

fn copy_or_rename_into_publish_temp(
    src: &Path,
    dst: &Path,
    same_filesystem_required: bool,
) -> Result<(), PublishError> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(err) if is_cross_device_error(&err) => {
            if same_filesystem_required {
                return Err(PublishError::CrossDeviceCopy(format!(
                    "{} -> {}: {err}",
                    src.display(),
                    dst.display()
                )));
            }
            copy_across_devices(src, dst).map_err(|copy_err| {
                PublishError::CrossDeviceCopy(format!(
                    "{} -> {}: {copy_err}",
                    src.display(),
                    dst.display()
                ))
            })
        }
        Err(err) => Err(PublishError::Io(err)),
    }
}

fn copy_across_devices(src: &Path, dst: &Path) -> Result<(), io::Error> {
    fs::copy(src, dst)?;
    let file = fs::OpenOptions::new().read(true).write(true).open(dst)?;
    file.sync_all()?;
    sync_parent_dir_best_effort(dst);
    Ok(())
}

fn write_bytes_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;

    let parent = parent_dir_or_current(path);
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|err| err.error)?;
    sync_parent_dir_best_effort(path);
    Ok(())
}

fn acquire_publish_lock(album_dir: &Path) -> Result<FileLock, PublishError> {
    let lock_path = album_lock_path(album_dir);
    acquire_file_lock(&lock_path, "album directory is locked by another process")
        .map_err(|err| match err {
            LockAcquireError::Busy(message) => PublishError::DestinationExists(message),
            LockAcquireError::Io(err) => PublishError::Io(err),
        })
}

fn acquire_run_lock(
    staging_parent: &Path,
    job_id: &str,
    item_id: &str,
) -> Result<FileLock, MaterializeError> {
    let lock_path = staging_parent.join(format!(
        ".{}-{}.run.lock",
        sanitize_component(job_id),
        sanitize_component(item_id)
    ));
    acquire_file_lock(&lock_path, "pipeline item is already running")
        .map_err(|err| match err {
            LockAcquireError::Busy(message) => MaterializeError::Parse(message),
            LockAcquireError::Io(err) => MaterializeError::Io(err),
        })
}

fn acquire_file_lock(lock_path: &Path, busy_message: &str) -> Result<FileLock, LockAcquireError> {
    let parent = parent_dir_or_current(lock_path);
    fs::create_dir_all(parent).map_err(LockAcquireError::Io)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(lock_path)
        .map_err(LockAcquireError::Io)?;

    match file.try_lock_exclusive() {
        Ok(()) => Ok(FileLock { file }),
        Err(err) if is_lock_contention(&err) => {
            Err(LockAcquireError::Busy(busy_message.to_string()))
        }
        Err(err) => Err(LockAcquireError::Io(err)),
    }
}

fn album_lock_path(album_dir: &Path) -> PathBuf {
    match album_dir.file_name().and_then(|name| name.to_str()) {
        Some(name) => album_dir.with_file_name(format!("{name}.lock")),
        None => PathBuf::from(format!("{}.lock", album_dir.display())),
    }
}

fn is_lock_contention(err: &io::Error) -> bool {
    if err.kind() == io::ErrorKind::WouldBlock {
        return true;
    }

    #[cfg(unix)]
    {
        return matches!(err.raw_os_error(), Some(11) | Some(35));
    }

    #[cfg(windows)]
    {
        return err.raw_os_error() == Some(33);
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = err;
        false
    }
}

enum LockAcquireError {
    Busy(String),
    Io(io::Error),
}

struct FileLock {
    file: fs::File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn sync_parent_dir_best_effort(path: &Path) {
    let parent = parent_dir_or_current(path);
    let Ok(dir) = fs::File::open(parent) else {
        return;
    };
    let _ = dir.sync_all();
}

fn is_cross_device_error(err: &io::Error) -> bool {
    #[cfg(unix)]
    {
        err.raw_os_error() == Some(18)
    }
    #[cfg(windows)]
    {
        err.raw_os_error() == Some(17)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = err;
        false
    }
}

fn unique_path(parent: &Path, prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0_u32..1000 {
        let candidate = parent.join(format!("{prefix}-{nanos}-{attempt}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{prefix}-{nanos}-fallback"))
}

fn staging_parent_for(req: &PipelineRequest) -> PathBuf {
    if req.naming.per_album_subdir {
        return req.output_root.join(STAGING_PARENT_NAME);
    }

    let parent = req
        .output_root
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    parent.join(STAGING_PARENT_NAME)
}

fn audio_artifact_count(artifacts: &ArtifactSet) -> usize {
    match &artifacts.audio {
        AudioArtifacts::Tracks(tracks) => tracks.len(),
        AudioArtifacts::Merged(_) => 1,
    }
}

fn delete_stale_staging_dir(path: &Path) -> io::Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn cancelled_outcome_from(outcome: AlbumOutcome) -> AlbumOutcome {
    AlbumOutcome::Blocked {
        successful: successful_tracks_from(&outcome),
        failed: failed_tracks_from(&outcome),
        stages: stages_from(&outcome),
        reason: BlockReason::Cancelled,
    }
}

fn push_stage(outcome: &mut AlbumOutcome, record: StageRecord) {
    match outcome {
        AlbumOutcome::Complete { stages, .. }
        | AlbumOutcome::Partial { stages, .. }
        | AlbumOutcome::Blocked { stages, .. } => stages.push(record),
    }
}

fn stages_from(outcome: &AlbumOutcome) -> Vec<StageRecord> {
    match outcome {
        AlbumOutcome::Complete { stages, .. }
        | AlbumOutcome::Partial { stages, .. }
        | AlbumOutcome::Blocked { stages, .. } => stages.clone(),
    }
}

fn successful_tracks_from(outcome: &AlbumOutcome) -> Vec<TrackRecord> {
    match outcome {
        AlbumOutcome::Complete { tracks, .. } => tracks.clone(),
        AlbumOutcome::Partial { successful, .. } | AlbumOutcome::Blocked { successful, .. } => {
            successful.clone()
        }
    }
}

fn failed_tracks_from(outcome: &AlbumOutcome) -> Vec<TrackRecord> {
    match outcome {
        AlbumOutcome::Complete { .. } => Vec::new(),
        AlbumOutcome::Partial { failed, .. } | AlbumOutcome::Blocked { failed, .. } => {
            failed.clone()
        }
    }
}
