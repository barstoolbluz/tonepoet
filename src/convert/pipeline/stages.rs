//! PR 1 — `Materializer` trait, every public stage-function
//! signature, the real `aggregate_album_outcome`, and the
//! `AlbumOutcome` -> `ConversionStatus` mapping.
//!
//! Per the plan: PR 1 ships compiling, non-panicking stub bodies for
//! the free functions below. PRs 2–10 replace those bodies without
//! changing the signatures. `aggregate_album_outcome` and
//! `map_album_outcome` are real implementations in PR 1 — they are
//! pure logic and are exercised by PR 1's exit-condition tests.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fs, io};

use async_trait::async_trait;
use fs2::FileExt;
use tokio_util::sync::CancellationToken;

use super::errors::{
    ConvertError, FeatureError, LogError, MaterializeError, MergeError, MetadataError, PlanError,
    PublishError, ReplayGainError, RequestValidationError, SourceDetectError, SourceDispatchError,
    ToolRunnerError,
};
use super::materializer_7z::SevenZipMaterializer;
use super::materializer_cue::{is_cue_image_candidate, CueImageMaterializer};
use super::materializer_sacd::{is_sacd_iso_candidate, SacdIsoMaterializer};
use super::materializer_single::SingleFileMaterializer;
use super::track_executor::{
    execute_planned_track_conversion, run_tool_command_with_concurrency,
};
pub use super::track_executor::ToolConcurrencyLimits;
use super::plan_bridge::{metadata_obligations_for_request, orchestrator_metadata_stage_required};
use super::progress::{
    heartbeat, OperationProgressTracker,
};
use super::reporter::{PipelineEvent, PipelineReporter};
use super::tool::{CommandRecord, RealToolRunner, ToolBinary, ToolCommand, ToolRunner};
use super::types::*;
use crate::convert::ConversionStatus;
use tonepoet_pipeline::AudioFormat as PlannerAudioFormat;
use crate::tui::sacd::{
    parse_sacd_iso, AreaInfo, PlayTime, SacdError, SacdMetadata, TrackEntry, SACD_FRAME_RATE,
    SACD_SAMPLE_RATE_HZ,
};
use sacd_rs::extract::{extract_track, ExtractOptions, ExtractStats, OutputFormat};
use sacd_rs::iso_reader::IsoReader;

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
        reporter: Option<&dyn PipelineReporter>,
        tool_paths: &HashMap<String, PathBuf>,
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
    if let Some(folder_template) = &req.naming.folder_template {
        validate_template(folder_template).map_err(RequestValidationError::InvalidTemplate)?;
    }

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
    let name = req
        .container
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if is_sacd_iso_candidate(req)? {
        return Ok(SourceKind::SacdIso);
    }
    if !matches!(req.source.cue_sidecar, CueSidecarPolicy::IgnoreCue) && is_cue_image_candidate(req)? {
        return Ok(SourceKind::CueImage);
    }
    if matches!(ext.as_str(), "7z" | "zip" | "rar" | "tar" | "iso" | "cab" | "dmg" | "tgz" | "tbz2" | "txz")
        || name.ends_with(".tar.gz")
        || name.ends_with(".tar.bz2")
        || name.ends_with(".tar.xz")
        || name.ends_with(".tar.zst")
        || name.ends_with(".tar.lz")
        || name.ends_with(".tar.lzma")
    {
        return Ok(SourceKind::SevenZip);
    }
    if is_single_audio_extension(&ext) {
        return Ok(SourceKind::SingleFile);
    }
    Err(SourceDetectError::UnknownSource)
}

fn is_single_audio_extension(ext: &str) -> bool {
    matches!(
        ext,
        "flac" | "wav" | "wave" | "aiff" | "aif" | "aifc" | "wv" | "mp3" | "m4a" | "mp4"
            | "aac" | "opus" | "ogg" | "ape" | "w64" | "rf64" | "dsf" | "dff"
    )
}

/// Dispatch a supported source kind to its materializer.
pub fn materializer_for(kind: SourceKind) -> Result<Box<dyn Materializer>, SourceDispatchError> {
    match kind {
        SourceKind::SingleFile => Ok(Box::new(SingleFileMaterializer)),
        SourceKind::SevenZip => Ok(Box::new(SevenZipMaterializer)),
        SourceKind::CueImage => Ok(Box::new(CueImageMaterializer)),
        SourceKind::SacdIso => Ok(Box::new(SacdIsoMaterializer)),
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
    progress_tracker: Option<&mut OperationProgressTracker<'_>>,
) -> Result<PathBuf, ConvertError> {
    realize_track_with_tool_limits(
        src,
        req,
        staging,
        runner,
        cancel,
        None,
        progress_tracker,
    )
    .await
}

pub async fn realize_track_with_tool_limits(
    src: &TrackSourceRef,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
    progress_tracker: Option<&mut OperationProgressTracker<'_>>,
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
                tool_concurrency_limits.as_ref(),
            )
            .await
        }
        TrackSourceRef::SacdTrack {
            iso,
            track_index,
            area,
        } => realize_sacd_track(iso, *track_index, *area, staging, cancel, progress_tracker).await,
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
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
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

    let direct = cut_segment_with_ffmpeg(
        image,
        start_sample,
        samples,
        &out_path,
        runner,
        cancel,
        tool_concurrency_limits,
    )
    .await;
    if let Err(err) = direct {
        let _ = fs::remove_file(&out_path);
        if !has_path_extension(image, "wv") {
            return Err(err);
        }

        let fallback = async {
            let wav_path = ensure_decoded_wavpack_image(
                image,
                &realized_dir,
                runner,
                cancel,
                tool_concurrency_limits,
            )
            .await?;
            cut_segment_with_ffmpeg(
                &wav_path,
                start_sample,
                samples,
                &out_path,
                runner,
                cancel,
                tool_concurrency_limits,
            )
            .await?;

            if req.settings.metadata.transfer_tags || req.settings.metadata.preserve_artwork {
                reattach_image_metadata_with_ffmpeg(
                    image,
                    &out_path,
                    runner,
                    cancel,
                    tool_concurrency_limits,
                )
                .await?;
            }

            Ok(())
        }
        .await;

        if let Err(err) = fallback {
            let _ = fs::remove_file(&out_path);
            return Err(err);
        }
    }

    if let Err(err) = validate_realized_segment(
        &out_path,
        samples,
        runner,
        cancel,
        tool_concurrency_limits,
    )
    .await
    {
        let _ = fs::remove_file(&out_path);
        return Err(err);
    }

    Ok(out_path)
}

fn cut_segment_ffmpeg_args(input: &Path, filter: &str, out_path: &Path) -> Vec<String> {
    vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-i".into(),
        input.to_string_lossy().into_owned(),
        "-map".into(),
        "0:a:0".into(),
        "-map".into(),
        "0:v?".into(),
        "-map_metadata".into(),
        "0".into(),
        "-af".into(),
        filter.to_string(),
        "-c:a".into(),
        "flac".into(),
        "-c:v".into(),
        "copy".into(),
        "-compression_level".into(),
        "0".into(),
        out_path.to_string_lossy().into_owned(),
    ]
}

fn reattach_image_metadata_ffmpeg_args(segment: &Path, image: &Path, out_path: &Path) -> Vec<String> {
    vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-i".into(),
        segment.to_string_lossy().into_owned(),
        "-i".into(),
        image.to_string_lossy().into_owned(),
        "-map".into(),
        "0:a:0".into(),
        "-map".into(),
        "1:v?".into(),
        "-map_metadata".into(),
        "1".into(),
        "-c:a".into(),
        "copy".into(),
        "-c:v".into(),
        "copy".into(),
        out_path.to_string_lossy().into_owned(),
    ]
}

#[cfg(test)]
mod cue_image_segment_command_tests {
    use super::*;

    #[test]
    fn cut_segment_command_copies_image_metadata_and_artwork() {
        let args = cut_segment_ffmpeg_args(
            Path::new("album.flac"),
            "atrim=start_sample=0:end_sample=44100,asetpts=PTS-STARTPTS",
            Path::new("segment.flac"),
        );

        assert!(has_adjacent_arg(&args, "-map", "0:a:0"));
        assert!(has_adjacent_arg(&args, "-map", "0:v?"));
        assert!(has_adjacent_arg(&args, "-map_metadata", "0"));
        assert!(has_adjacent_arg(&args, "-c:a", "flac"));
        assert!(has_adjacent_arg(&args, "-c:v", "copy"));
        assert!(!has_adjacent_arg(&args, "-map_metadata", "-1"));
        assert!(!args.iter().any(|arg| arg == "-vn"));
    }

    #[test]
    fn wavpack_fallback_reattach_command_maps_original_image_artwork_and_tags() {
        let args = reattach_image_metadata_ffmpeg_args(
            Path::new("realized.flac"),
            Path::new("album.wv"),
            Path::new("reattached.flac"),
        );

        assert_eq!(args.iter().filter(|arg| *arg == "-i").count(), 2);
        assert!(has_adjacent_arg(&args, "-map", "0:a:0"));
        assert!(has_adjacent_arg(&args, "-map", "1:v?"));
        assert!(has_adjacent_arg(&args, "-map_metadata", "1"));
        assert!(has_adjacent_arg(&args, "-c:a", "copy"));
        assert!(has_adjacent_arg(&args, "-c:v", "copy"));
        assert!(!args.iter().any(|arg| arg == "-vn"));
    }

    fn has_adjacent_arg(args: &[String], left: &str, right: &str) -> bool {
        args.windows(2).any(|window| window[0] == left && window[1] == right)
    }
}

async fn cut_segment_with_ffmpeg(
    input: &Path,
    start_sample: u64,
    samples: u64,
    out_path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
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
    let filter =
        format!("atrim=start_sample={start_sample}:end_sample={end_sample},asetpts=PTS-STARTPTS");
    let cmd = ToolCommand {
        binary: ToolBinary::Ffmpeg,
        args: cut_segment_ffmpeg_args(input, &filter, out_path),
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: DEFAULT_CONVERT_TIMEOUT,
    };

    match run_tool_command_with_concurrency(cmd, runner, cancel, tool_concurrency_limits).await {
        Ok(_) => Ok(()),
        Err(ToolRunnerError::Cancelled { .. }) => {
            Err(ConvertError::Realize("cancelled".to_string()))
        }
        Err(err) => Err(ConvertError::Tool(err)),
    }
}

async fn reattach_image_metadata_with_ffmpeg(
    image: &Path,
    out_path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<(), ConvertError> {
    let tmp_path = out_path.with_file_name(format!(
        ".{}.reattach.{}.flac",
        out_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("segment.flac"),
        std::process::id()
    ));
    let _ = fs::remove_file(&tmp_path);

    let cmd = ToolCommand {
        binary: ToolBinary::Ffmpeg,
        args: reattach_image_metadata_ffmpeg_args(out_path, image, &tmp_path),
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: DEFAULT_CONVERT_TIMEOUT,
    };

    let result = match run_tool_command_with_concurrency(cmd, runner, cancel, tool_concurrency_limits).await {
        Ok(_) => {
            fs::rename(&tmp_path, out_path)?;
            Ok(())
        }
        Err(ToolRunnerError::Cancelled { .. }) => {
            Err(ConvertError::Realize("cancelled".to_string()))
        }
        Err(err) => Err(ConvertError::Tool(err)),
    };

    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }

    result
}

async fn ensure_decoded_wavpack_image(
    input: &Path,
    realized_dir: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
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

        if let Err(err) = decode_wavpack_image(
            input,
            &tmp_path,
            runner,
            cancel,
            tool_concurrency_limits,
        )
        .await
        {
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
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
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

    match run_tool_command_with_concurrency(cmd, runner, cancel, tool_concurrency_limits).await {
        Ok(_) => Ok(()),
        Err(ToolRunnerError::Cancelled { .. }) => {
            Err(ConvertError::Realize("cancelled".to_string()))
        }
        Err(err) => Err(ConvertError::Tool(err)),
    }
}

async fn validate_realized_segment(
    out_path: &Path,
    expected_samples: u64,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
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

    let probe = probe_realized_segment_with_tool_limits(
        out_path,
        runner,
        cancel,
        tool_concurrency_limits,
    )
    .await?;
    let actual_samples = probe.samples.ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "could not measure realized segment samples: {}",
            out_path.display()
        ))
    })?;
    let delta = actual_samples.abs_diff(expected_samples);
    let allowed = if probe.exact {
        0
    } else {
        (probe.sample_rate / 75).max(1) as u64
    };
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

#[allow(dead_code)]
async fn probe_realized_segment(
    path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<RealizedProbe, ConvertError> {
    probe_realized_segment_with_tool_limits(path, runner, cancel, None).await
}

async fn probe_realized_segment_with_tool_limits(
    path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
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

    let output = match run_tool_command_with_concurrency(cmd, runner, cancel, tool_concurrency_limits).await {
        Ok(output) => output,
        Err(ToolRunnerError::Cancelled { .. }) => {
            return Err(ConvertError::Realize("cancelled".to_string()))
        }
        Err(err) => return Err(ConvertError::Tool(err)),
    };
    parse_realized_probe_json(&output.stdout_tail)
}

fn parse_realized_probe_json(json: &str) -> Result<RealizedProbe, ConvertError> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|err| {
        ConvertError::TrackValidation(format!("ffprobe JSON parse failed: {err}"))
    })?;
    let stream = value.pointer("/streams/0").ok_or_else(|| {
        ConvertError::TrackValidation("ffprobe returned no audio stream".to_string())
    })?;
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
        return Ok(RealizedProbe {
            sample_rate,
            samples: Some(samples),
            exact: true,
        });
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
    Ok(RealizedProbe {
        sample_rate,
        samples,
        exact: false,
    })
}

/// Validate the encoded output's sample count against expected samples.
///
/// Only checks lossless PCM formats (FLAC, WAV, AIFF, WavPack, ALAC) where
/// sample count must be preserved. Lossy (MP3, AAC, Opus) and DSD formats
/// are skipped because codec padding or a different sample model makes strict
/// comparison invalid.
///
/// Returns the actual probed sample count on success, or the original
/// expected_samples if validation was skipped or the probe couldn't
/// determine the sample count. Mismatches are logged as warnings, not
/// fatal errors — the extraction-stage validation already caught source
/// problems, so a post-encode drift may indicate an encoder quirk rather
/// than data loss.
#[allow(dead_code)]
async fn validate_encoded_output(
    out_path: &Path,
    expected_samples: Option<u64>,
    target_format: &tonepoet_pipeline::AudioFormat,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Option<u64> {
    validate_encoded_output_with_tool_limits(
        out_path,
        expected_samples,
        target_format,
        runner,
        cancel,
        None,
    )
    .await
}

async fn validate_encoded_output_with_tool_limits(
    out_path: &Path,
    expected_samples: Option<u64>,
    target_format: &tonepoet_pipeline::AudioFormat,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Option<u64> {
    let Some(expected) = expected_samples else {
        return None;
    };

    if !target_format.is_pcm_lossless() {
        return Some(expected);
    }

    let probe = match probe_realized_segment_with_tool_limits(
        out_path,
        runner,
        cancel,
        tool_concurrency_limits,
    )
    .await
    {
        Ok(probe) => probe,
        Err(err) => {
            log::warn!(
                "post-encode sample validation skipped (probe failed): {} — {err}",
                out_path.display()
            );
            return Some(expected);
        }
    };

    let Some(actual) = probe.samples else {
        log::warn!(
            "post-encode sample validation skipped (no sample count): {}",
            out_path.display()
        );
        return Some(expected);
    };

    let delta = actual.abs_diff(expected);
    let allowed = if probe.exact {
        0
    } else {
        (probe.sample_rate / 75).max(1) as u64
    };

    if delta > allowed {
        log::warn!(
            "post-encode sample drift: expected {expected}, got {actual}, allowed {allowed} — {}",
            out_path.display()
        );
    }

    Some(actual)
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
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn has_path_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(ext))
        .unwrap_or(false)
}

// ===========================================================================
// SACD track realization  (PR 9)
// ===========================================================================

async fn realize_sacd_track(
    iso: &Path,
    track_index: u32,
    area: SacdArea,
    staging: &StagingDir,
    cancel: &CancellationToken,
    progress_tracker: Option<&mut OperationProgressTracker<'_>>,
) -> Result<PathBuf, ConvertError> {
    if cancel.is_cancelled() {
        return Err(ConvertError::Realize("cancelled".to_string()));
    }
    if !iso.is_file() {
        return Err(ConvertError::TrackValidation(format!(
            "SACD ISO does not exist: {}",
            iso.display()
        )));
    }

    let iso = iso.to_path_buf();
    let staging_root = staging.root.clone();

    let output = match progress_tracker {
        Some(tracker) => {
            heartbeat::run_with_heartbeat(
                async {
                    let iso = iso.clone();
                    let staging_root = staging_root.clone();
                    tokio::task::spawn_blocking(move || {
                        realize_sacd_track_blocking(&iso, track_index, area, &staging_root)
                    })
                    .await
                    .map_err(|err| {
                        ConvertError::Realize(format!("SACD extraction task failed: {err}"))
                    })?
                },
                tracker,
                "sacd-extraction",
                "Extracting DSD audio\u{2026}",
                Duration::from_secs(5),
            )
            .await?
        }
        None => tokio::task::spawn_blocking(move || {
            realize_sacd_track_blocking(&iso, track_index, area, &staging_root)
        })
        .await
        .map_err(|err| ConvertError::Realize(format!("SACD extraction task failed: {err}")))??,
    };

    if cancel.is_cancelled() {
        return Err(ConvertError::Realize("cancelled".to_string()));
    }

    Ok(output)
}

fn realize_sacd_track_blocking(
    iso: &Path,
    track_index: u32,
    area: SacdArea,
    staging_root: &Path,
) -> Result<PathBuf, ConvertError> {
    let metadata = parse_sacd_iso(iso).map_err(sacd_error_to_convert)?;
    let area_info = sacd_area_info(&metadata, area).ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "requested SACD area {} is not present in {}",
            sacd_area_label(area),
            iso.display()
        ))
    })?;
    let entry = area_info.tracks.get(track_index as usize).ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "SACD track index {} outside area {} track count {}",
            track_index,
            sacd_area_label(area),
            area_info.tracks.len()
        ))
    })?;

    if entry.length_lsn == 0 {
        return Err(ConvertError::TrackValidation(format!(
            "SACD track index {} has zero sectors",
            track_index
        )));
    }
    if !matches!(area_info.header.channel_count, 2 | 5 | 6) {
        return Err(ConvertError::TrackValidation(format!(
            "SACD area {} has unsupported channel count {}",
            sacd_area_label(area),
            area_info.header.channel_count
        )));
    }

    let start_lsn = u64::from(entry.start_lsn);
    let end_lsn = start_lsn
        .checked_add(u64::from(entry.length_lsn))
        .ok_or_else(|| {
            ConvertError::TrackValidation("SACD track sector range overflows".to_string())
        })?;

    let realized_dir = staging_root.join("realized-sacd-tracks");
    fs::create_dir_all(&realized_dir)?;
    let expectation = DsfExpectation::from_area(area_info, entry.duration);
    let out_path = realized_dir.join(sacd_track_output_name(iso, area, track_index, entry));

    if dsf_output_is_ready_for(&out_path, expectation) {
        return Ok(out_path);
    }

    let tmp_path = unique_sacd_tmp_path(&out_path);
    let _ = fs::remove_file(&tmp_path);

    let extraction_result = (|| {
        let mut iso_reader = IsoReader::open(iso).map_err(|err| {
            ConvertError::Realize(format!("could not open SACD ISO {}: {err}", iso.display()))
        })?;
        let mut writer = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;

        let options = ExtractOptions::new(
            start_lsn,
            end_lsn,
            area_info.header.channel_count,
            OutputFormat::Dsf,
        );
        let stats = extract_track(&mut iso_reader, &mut writer, options).map_err(|err| {
            ConvertError::Realize(format!(
                "SACD extraction failed for {} track {}: {err}",
                iso.display(),
                track_index + 1
            ))
        })?;
        writer.sync_all()?;
        drop(writer);

        validate_sacd_realization(&tmp_path, area_info, entry.duration, stats)?;
        Ok(())
    })();

    if let Err(err) = extraction_result {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    match fs::rename(&tmp_path, &out_path) {
        Ok(()) => Ok(out_path),
        Err(first_err) if out_path.exists() => {
            let _ = fs::remove_file(&out_path);
            fs::rename(&tmp_path, &out_path).map_err(|second_err| {
                ConvertError::Io(io::Error::new(
                    second_err.kind(),
                    format!(
                        "failed to replace {} after successful SACD extraction: {}; first rename error: {}",
                        out_path.display(),
                        second_err,
                        first_err
                    ),
                ))
            })?;
            Ok(out_path)
        }
        Err(err) => {
            let _ = fs::remove_file(&tmp_path);
            Err(ConvertError::Io(err))
        }
    }
}

fn sacd_area_info(metadata: &SacdMetadata, area: SacdArea) -> Option<&AreaInfo> {
    match area {
        SacdArea::Stereo => metadata.stereo.as_ref(),
        SacdArea::MultiChannel => metadata.multi_channel.as_ref(),
    }
}

fn sacd_area_label(area: SacdArea) -> &'static str {
    match area {
        SacdArea::Stereo => "stereo",
        SacdArea::MultiChannel => "multichannel",
    }
}

fn sacd_track_output_name(
    iso: &Path,
    area: SacdArea,
    track_index: u32,
    entry: &TrackEntry,
) -> String {
    let stem = sanitize_segment_component(
        iso.file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("sacd"),
    );
    let path_hash = stable_path_hash(iso);
    format!(
        "{stem}_{path_hash:016x}_{}_track_{:03}_{:08x}_{:08x}.dsf",
        sacd_area_label(area),
        track_index + 1,
        entry.start_lsn,
        entry.length_lsn
    )
}

fn unique_sacd_tmp_path(out_path: &Path) -> PathBuf {
    let file_name = out_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("track.dsf");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    out_path.with_file_name(format!(".{file_name}.{}.{}.tmp", std::process::id(), stamp))
}

#[derive(Debug, Clone, Copy)]
struct DsfExpectation {
    channel_count: u32,
    sample_frequency: u32,
    sample_count: u64,
}

impl DsfExpectation {
    fn from_area(area_info: &AreaInfo, duration: PlayTime) -> Self {
        Self {
            channel_count: u32::from(area_info.header.channel_count),
            sample_frequency: SACD_SAMPLE_RATE_HZ,
            sample_count: sacd_dsf_sample_count(duration),
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct DsfHeader {
    file_size: u64,
    metadata_offset: u64,
    channel_count: u32,
    sample_frequency: u32,
    bits_per_sample: u32,
    sample_count: u64,
    block_size_per_channel: u32,
    data_chunk_size: u64,
}

fn dsf_output_is_ready_for(path: &Path, expectation: DsfExpectation) -> bool {
    validate_dsf_container(path, Some(expectation)).is_ok()
}

fn validate_sacd_realization(
    path: &Path,
    area_info: &AreaInfo,
    duration: PlayTime,
    stats: ExtractStats,
) -> Result<(), ConvertError> {
    if stats.frames_read == 0 || stats.audio_bytes == 0 {
        return Err(ConvertError::TrackValidation(format!(
            "SACD extraction produced no audio for {}",
            path.display()
        )));
    }
    if stats.audio_bytes % u64::from(area_info.header.channel_count) != 0 {
        return Err(ConvertError::TrackValidation(format!(
            "SACD extraction produced audio bytes not divisible by channel count: {} bytes / {} channels",
            stats.audio_bytes,
            area_info.header.channel_count
        )));
    }

    let expectation = DsfExpectation::from_area(area_info, duration);
    validate_dsf_container(path, Some(expectation))?;

    let expected_frames = u64::from(playtime_to_frame_count(duration));
    if expected_frames > 0 {
        let delta = stats.frames_read.abs_diff(expected_frames);
        if delta > 1 {
            return Err(ConvertError::TrackValidation(format!(
                "SACD frame count drift: expected {expected_frames}, got {}, allowed 1",
                stats.frames_read
            )));
        }
    }

    // Duration sanity check: compare extracted byte-derived sample count
    // against TOC-derived expectation with ±1 TOC frame tolerance.
    let stats_sample_count = stats
        .audio_bytes
        .checked_div(u64::from(area_info.header.channel_count))
        .unwrap_or(0)
        .saturating_mul(8);
    if expectation.sample_count != 0 {
        let one_toc_frame_samples = u64::from(SACD_SAMPLE_RATE_HZ / 75);
        let delta = stats_sample_count.abs_diff(expectation.sample_count);
        if delta > one_toc_frame_samples {
            return Err(ConvertError::TrackValidation(format!(
                "SACD DSF sample-count mismatch: expected ~{}, got {}, delta {} exceeds 1-frame tolerance {}",
                expectation.sample_count,
                stats_sample_count,
                delta,
                one_toc_frame_samples,
            )));
        }
    }

    Ok(())
}

fn validate_dsf_container(
    path: &Path,
    expectation: Option<DsfExpectation>,
) -> Result<DsfHeader, ConvertError> {
    let metadata = fs::metadata(path).map_err(|err| {
        ConvertError::TrackValidation(format!(
            "realized SACD track missing: {} ({err})",
            path.display()
        ))
    })?;
    if !metadata.is_file() || metadata.len() <= 92 {
        return Err(ConvertError::TrackValidation(format!(
            "realized SACD track is empty or missing DSF payload: {}",
            path.display()
        )));
    }

    let mut file = fs::File::open(path)?;
    let mut header = [0_u8; 92];
    use std::io::Read;
    file.read_exact(&mut header)?;

    if &header[0..4] != b"DSD " || &header[28..32] != b"fmt " || &header[80..84] != b"data" {
        return Err(ConvertError::TrackValidation(format!(
            "realized SACD track is not a complete DSF container: {}",
            path.display()
        )));
    }

    let parsed = DsfHeader {
        file_size: read_u64_le(&header, 12),
        metadata_offset: read_u64_le(&header, 20),
        channel_count: read_u32_le(&header, 52),
        sample_frequency: read_u32_le(&header, 56),
        bits_per_sample: read_u32_le(&header, 60),
        sample_count: read_u64_le(&header, 64),
        block_size_per_channel: read_u32_le(&header, 72),
        data_chunk_size: read_u64_le(&header, 84),
    };

    if parsed.file_size != metadata.len() {
        return Err(ConvertError::TrackValidation(format!(
            "DSF header file size {} does not match actual size {} for {}",
            parsed.file_size,
            metadata.len(),
            path.display()
        )));
    }
    if parsed.bits_per_sample != 1 || parsed.block_size_per_channel == 0 {
        return Err(ConvertError::TrackValidation(format!(
            "DSF header has invalid DSD format fields for {}",
            path.display()
        )));
    }
    if parsed.data_chunk_size < 12 || parsed.data_chunk_size > parsed.file_size.saturating_sub(80) {
        return Err(ConvertError::TrackValidation(format!(
            "DSF data chunk size is invalid for {}",
            path.display()
        )));
    }
    if parsed.metadata_offset != 0 && parsed.metadata_offset >= parsed.file_size {
        return Err(ConvertError::TrackValidation(format!(
            "DSF metadata offset is outside the file for {}",
            path.display()
        )));
    }

    if let Some(expectation) = expectation {
        if parsed.channel_count != expectation.channel_count {
            return Err(ConvertError::TrackValidation(format!(
                "DSF channel count mismatch: expected {}, got {} for {}",
                expectation.channel_count,
                parsed.channel_count,
                path.display()
            )));
        }
        if parsed.sample_frequency != expectation.sample_frequency {
            return Err(ConvertError::TrackValidation(format!(
                "DSF sample frequency mismatch: expected {}, got {} for {}",
                expectation.sample_frequency,
                parsed.sample_frequency,
                path.display()
            )));
        }
        // SACD TOC PlayTime has 75 fps granularity. DSF sample counts
        // are derived from realized sector/frame extraction and can
        // differ by one TOC frame. Treat PlayTime-derived samples as
        // a duration sanity check, not an exact sample-count invariant.
        if expectation.sample_count != 0 {
            let one_toc_frame_samples = u64::from(expectation.sample_frequency / 75);
            let delta = parsed.sample_count.abs_diff(expectation.sample_count);
            if delta > one_toc_frame_samples {
                return Err(ConvertError::TrackValidation(format!(
                    "DSF sample count mismatch: expected ~{}, got {}, delta {} exceeds 1-frame tolerance {} for {}",
                    expectation.sample_count,
                    parsed.sample_count,
                    delta,
                    one_toc_frame_samples,
                    path.display()
                )));
            }
        }
    }

    Ok(parsed)
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64_le(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn sacd_dsf_sample_count(time: PlayTime) -> u64 {
    u64::from(playtime_to_frame_count(time)) * u64::from(SACD_SAMPLE_RATE_HZ)
        / u64::from(SACD_FRAME_RATE)
}

fn playtime_to_frame_count(time: PlayTime) -> u32 {
    u32::from(time.minutes) * 60 * SACD_FRAME_RATE
        + u32::from(time.seconds) * SACD_FRAME_RATE
        + u32::from(time.frames)
}

fn sacd_error_to_convert(err: SacdError) -> ConvertError {
    match err {
        SacdError::NotSacdIso => ConvertError::Realize(
            "SACD ISO is encrypted, corrupt, or missing Master TOC magic".to_string(),
        ),
        SacdError::Malformed(message) if looks_encrypted_sacd(&message) => {
            ConvertError::Realize("SACD ISO is encrypted or scrambled".to_string())
        }
        SacdError::Malformed(message) => ConvertError::Realize(message),
        SacdError::TooSmall { .. } => ConvertError::Realize(err.to_string()),
        SacdError::Io(message) => ConvertError::Realize(message),
    }
}

fn looks_encrypted_sacd(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("encrypted") || lower.contains("scrambled") || lower.contains("cipher")
}

#[cfg(test)]
pub(crate) mod sacd_stage_test_support {
    use super::*;

    pub(crate) fn output_name_for_test(
        path: &Path,
        area: SacdArea,
        track_index: u32,
        start_lsn: u32,
        length_lsn: u32,
    ) -> String {
        let entry = TrackEntry {
            start_lsn,
            length_lsn,
            ..TrackEntry::default()
        };
        sacd_track_output_name(path, area, track_index, &entry)
    }

    pub(crate) fn playtime_frames_for_test(minutes: u8, seconds: u8, frames: u8) -> u32 {
        playtime_to_frame_count(PlayTime {
            minutes,
            seconds,
            frames,
        })
    }

    pub(crate) fn dsf_ready_for_test(
        path: &Path,
        channel_count: u32,
        sample_frequency: u32,
        sample_count: u64,
    ) -> bool {
        dsf_output_is_ready_for(
            path,
            DsfExpectation {
                channel_count,
                sample_frequency,
                sample_count,
            },
        )
    }

    pub(crate) fn dsf_sample_count_for_test(minutes: u8, seconds: u8, frames: u8) -> u64 {
        sacd_dsf_sample_count(PlayTime {
            minutes,
            seconds,
            frames,
        })
    }
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
    if let Some(folder_template) = &req.naming.folder_template {
        validate_template(folder_template).map_err(PlanError::InvalidTemplate)?;
    }

    let output_root = normalize_path(&req.output_root);
    let album_dir = if req.naming.per_album_subdir {
        match &req.naming.folder_template {
            Some(tmpl) => {
                let rendered = render_folder_template(tmpl, source, &req.settings.target_format);
                output_root.join(rendered)
            }
            None => {
                let album_component = sanitize_component(
                    source
                        .album_metadata
                        .album
                        .as_deref()
                        .or_else(|| source.container.file_stem().and_then(|s| s.to_str()))
                        .unwrap_or("Album"),
                );
                output_root.join(album_component)
            }
        }
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
        let rel = render_track_template(&req.naming.template, source, track, &req.settings.target_format)?;
        reject_escaping_path(&rel).map_err(PlanError::InvalidTemplate)?;

        let mut final_path = normalize_path(&album_dir.join(rel));
        append_default_extension(&mut final_path, &req.settings.target_format, req.container_extension.as_deref());
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

        entries.push(PlannedTrackOutput {
            track_id: track.id.clone(),
            final_path,
        });
    }

    Ok(AlbumPlan { album_dir, entries })
}

// ===========================================================================
// Convert / merge / metadata / replaygain / features  (PR 4–6 bodies)
// ===========================================================================

/// Convert every planned track. Per-track failures are represented in
/// `TrackRecord`s; successful artifacts only contain tracks that encoded.

/// Worker result for one encoded track. Records are sorted back into source
/// order after concurrent execution so durable logs remain deterministic.
#[derive(Debug)]
pub struct ScheduledTrackOutput {
    pub index: usize,
    pub record: TrackRecord,
    pub artifact: Option<TrackArtifact>,
    pub ok: bool,
    pub metadata_satisfaction: PlannedMetadataSatisfaction,
}

/// Result of an extraction/split work unit. The scheduler submits this to the
/// same shared queue as encode work, so SACD/CUE realization and encoding are
/// separate dependency steps instead of hidden serial work inside encoding.
#[derive(Debug)]
pub struct ScheduledRealizedTrack {
    pub index: usize,
    pub track: PreparedTrack,
    pub final_path: PathBuf,
    pub realized_path: PathBuf,
    pub req: PipelineRequest,
    pub staging_root: PathBuf,
    pub staging_job: String,
    pub convert_root: PathBuf,
    pub cancel: CancellationToken,
}

/// Build a deterministic failed track output for scheduler boundary failures.
///
/// Worker-pool execution errors are still track-level conversion failures, not
/// process-wide scheduler failures. Converting them here keeps album failure
/// policy, durable logs, and post-processing gates in one code path.
pub fn scheduled_worker_failure_output(
    index: usize,
    track: &PreparedTrack,
    realized_input: Option<PathBuf>,
    output_file: Option<PathBuf>,
    error: String,
) -> ScheduledTrackOutput {
    ScheduledTrackOutput {
        index,
        record: TrackRecord {
            track_id: track.id.clone(),
            outcome: TrackOutcome::Err(error),
            source_ref: track.source_ref.clone(),
            realized_input,
            output_file,
            commands: Vec::new(),
            bytes_in: None,
            bytes_out: None,
            duration: None,
        },
        artifact: None,
        ok: false,
        metadata_satisfaction: PlannedMetadataSatisfaction::none(),
    }
}

pub async fn convert_tracks(
    source: &PreparedSource,
    plan: &AlbumPlan,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> ConvertStageResult {
    convert_tracks_with_reporter(source, plan, req, staging, runner, cancel, None).await
}

async fn convert_tracks_with_reporter(
    source: &PreparedSource,
    plan: &AlbumPlan,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    reporter: Option<&dyn PipelineReporter>,
) -> ConvertStageResult {
    let tool_paths: HashMap<String, PathBuf> = HashMap::new();
    convert_tracks_with_reporter_with_tool_paths(
        source,
        plan,
        req,
        staging,
        runner,
        cancel,
        reporter,
        &tool_paths,
        None,
    )
    .await
}

async fn convert_tracks_with_reporter_with_tool_paths(
    source: &PreparedSource,
    plan: &AlbumPlan,
    req: &PipelineRequest,
    staging: &StagingDir,
    _runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    reporter: Option<&dyn PipelineReporter>,
    tool_paths: &HashMap<String, PathBuf>,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
) -> ConvertStageResult {
    let planned: BTreeMap<_, _> = plan
        .entries
        .iter()
        .map(|entry| (entry.track_id.clone(), entry.final_path.clone()))
        .collect();
    let convert_root = staging.root.join("converted");
    if let Err(err) = fs::create_dir_all(&convert_root) {
        let error = format!("could not create conversion staging directory: {err}");
        let records = source
            .tracks
            .iter()
            .map(|track| failed_track_record(track, None, None, Vec::new(), error.clone()))
            .collect();
        return ConvertStageResult {
            tracks: records,
            artifacts: ArtifactSet {
                audio: AudioArtifacts::Tracks(Vec::new()),
                sidecars: Vec::new(),
            },
            record: stage_record(PipelineStage::Convert, StageOutcome::Failed(error)),
        };
    }

    // Fallback entry used by direct unit tests and legacy callers. The normal
    // queue path uses the processor-level shared pool and calls
    // `encode_track_for_scheduler` for each ready track. Keeping this fallback
    // serial avoids nested worker pools and worker overcommit.
    let total_tracks = source.tracks.len();
    let total_expected_samples = total_expected_samples(source);
    let mut progress_tracker = OperationProgressTracker::new(req.item_id.clone(), PipelineStage::Convert, reporter);
    let mut records = Vec::with_capacity(total_tracks);
    let mut artifacts = Vec::new();
    let mut completed_expected_samples = 0_u64;

    for (track_index, track) in source.tracks.iter().cloned().enumerate() {
        let Some(final_path) = planned.get(&track.id).cloned() else {
            records.push(failed_track_record(
                &track,
                None,
                None,
                Vec::new(),
                format!("missing planned output for track {}", track.id.source_ordinal),
            ));
            continue;
        };
        let start_fraction = convert_progress_fraction(
            completed_expected_samples,
            total_expected_samples,
            track_index,
            total_tracks,
        );
        progress_tracker
            .estimated(
                start_fraction,
                convert_track_message("Starting", track_index + 1, total_tracks, &track, Some(final_path.as_path())),
            )
            .await;
        let output = convert_one_track_work(
            track_index,
            track.clone(),
            final_path,
            req.clone(),
            staging.root.clone(),
            staging.job_id.clone(),
            convert_root.clone(),
            tool_paths.clone(),
            cancel.clone(),
            tool_concurrency_limits.clone(),
            reporter,
        )
        .await
        .unwrap_or_else(|err| ScheduledTrackOutput {
            index: track_index,
            record: failed_track_record(&track, None, None, Vec::new(), err),
            artifact: None,
            ok: false,
            metadata_satisfaction: PlannedMetadataSatisfaction::none(),
        });
        completed_expected_samples = advance_expected_samples(completed_expected_samples, &track);
        let progress = convert_progress_fraction(
            completed_expected_samples,
            total_expected_samples,
            track_index + 1,
            total_tracks,
        );
        progress_tracker
            .estimated(
                progress,
                convert_track_message("Finished", track_index + 1, total_tracks, &track, output.artifact.as_ref().map(|artifact| artifact.final_path.as_path())),
            )
            .await;
        if let Some(artifact) = output.artifact.clone() {
            artifacts.push(artifact);
        }
        records.push(output.record);
        if cancel.is_cancelled() {
            break;
        }
    }

    let failed = records.iter().any(|record| matches!(record.outcome, TrackOutcome::Err(_)));
    ConvertStageResult {
        tracks: records,
        artifacts: ArtifactSet {
            audio: AudioArtifacts::Tracks(artifacts),
            sidecars: Vec::new(),
        },
        record: stage_record(
            PipelineStage::Convert,
            if failed && req.failure_policy == FailurePolicy::FailAlbumOnAnyTrackFailure {
                StageOutcome::Failed("one or more tracks failed".to_string())
            } else {
                StageOutcome::Ok
            },
        ),
    }
}

async fn convert_one_track_work(
    track_index: usize,
    track: PreparedTrack,
    final_path: PathBuf,
    req: PipelineRequest,
    staging_root: PathBuf,
    staging_job: String,
    convert_root: PathBuf,
    tool_paths: HashMap<String, PathBuf>,
    cancel: CancellationToken,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
    reporter: Option<&dyn PipelineReporter>,
) -> Result<ScheduledTrackOutput, String> {
    let staging = StagingDir::borrowed(staging_root, staging_job);
    let runner = RealToolRunner::new(tool_paths.clone());
    let staged_path = staged_audio_path(&convert_root, &final_path, &track.id, &req.settings.target_format);
    let mut progress_tracker = OperationProgressTracker::new(req.item_id.clone(), PipelineStage::Convert, reporter);
    let realized_input = match realize_track_with_tool_limits(
        &track.source_ref,
        &req,
        &staging,
        &runner,
        &cancel,
        tool_concurrency_limits.clone(),
        Some(&mut progress_tracker),
    )
    .await
    {
        Ok(path) => path,
        Err(err) => {
            let record = failed_track_record(&track, None, Some(staged_path), Vec::new(), err.to_string());
            return Ok(ScheduledTrackOutput { index: track_index, record, artifact: None, ok: false, metadata_satisfaction: PlannedMetadataSatisfaction::none() });
        }
    };

    if let Some(parent) = staged_path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            let record = failed_track_record(
                &track,
                Some(realized_input),
                Some(staged_path),
                Vec::new(),
                format!("could not create output directory: {err}"),
            );
            return Ok(ScheduledTrackOutput { index: track_index, record, artifact: None, ok: false, metadata_satisfaction: PlannedMetadataSatisfaction::none() });
        }
    }

    let bytes_in = file_len(&realized_input);
    let executed = execute_planned_track_conversion(
        &req,
        &track,
        &realized_input,
        &staged_path,
        &convert_root,
        &runner,
        &cancel,
        &tool_paths,
        tool_concurrency_limits.clone(),
        &mut progress_tracker,
        0.0,
        1.0,
    )
    .await;

    match executed {
        Ok(executed) => {
            let bytes_out = file_len(&staged_path);
            if bytes_out.unwrap_or(0) == 0 {
                let error = format!("planner did not produce output: {}", staged_path.display());
                let record = failed_track_record(
                    &track,
                    Some(realized_input),
                    Some(staged_path),
                    executed.commands,
                    error,
                );
                Ok(ScheduledTrackOutput { index: track_index, record, artifact: None, ok: false, metadata_satisfaction: PlannedMetadataSatisfaction::none() })
            } else {
                let actual_samples = validate_encoded_output_with_tool_limits(
                    &staged_path,
                    track.expected_samples,
                    &req.settings.target_format,
                    &runner,
                    &cancel,
                    tool_concurrency_limits.as_ref(),
                )
                .await;
                let record = TrackRecord {
                    track_id: track.id.clone(),
                    outcome: TrackOutcome::Ok,
                    source_ref: track.source_ref.clone(),
                    realized_input: Some(realized_input),
                    output_file: Some(staged_path.clone()),
                    commands: executed.commands,
                    bytes_in,
                    bytes_out,
                    duration: Some(executed.elapsed),
                };
                let artifact = TrackArtifact {
                    planned_command_hash: executed.command_hash,
                    track_id: track.id.clone(),
                    staged_path,
                    final_path,
                    samples: actual_samples.or(track.expected_samples),
                    metadata_satisfaction: executed.metadata_satisfaction,
                    metadata_required: executed.metadata_required,
                };
                Ok(ScheduledTrackOutput { index: track_index, record, artifact: Some(artifact), ok: true, metadata_satisfaction: executed.metadata_satisfaction })
            }
        }
        Err(err) => {
            let commands = command_from_convert_error(&err);
            let record = failed_track_record(
                &track,
                Some(realized_input),
                Some(staged_path),
                commands,
                err.to_string(),
            );
            Ok(ScheduledTrackOutput { index: track_index, record, artifact: None, ok: false, metadata_satisfaction: PlannedMetadataSatisfaction::none() })
        }
    }
}

#[allow(dead_code)]
fn track_expected_duration(track: &PreparedTrack) -> Option<Duration> {
    if track.sample_rate == 0 {
        return None;
    }
    let samples = track.expected_samples?;
    if samples == 0 {
        return None;
    }
    Some(Duration::from_secs_f64(
        samples as f64 / track.sample_rate as f64,
    ))
}
fn total_expected_samples(source: &PreparedSource) -> Option<u64> {
    let mut total = 0_u64;
    for track in &source.tracks {
        let samples = track.expected_samples?;
        total = total.saturating_add(samples);
    }
    if total == 0 {
        None
    } else {
        Some(total)
    }
}

fn advance_expected_samples(completed: u64, track: &PreparedTrack) -> u64 {
    completed.saturating_add(track.expected_samples.unwrap_or(0))
}

fn convert_progress_fraction(
    completed_expected_samples: u64,
    total_expected_samples: Option<u64>,
    completed_tracks: usize,
    total_tracks: usize,
) -> f32 {
    if let Some(total) = total_expected_samples.filter(|total| *total > 0) {
        return (completed_expected_samples as f32 / total as f32).clamp(0.0, 1.0);
    }
    if total_tracks == 0 {
        0.0
    } else {
        (completed_tracks as f32 / total_tracks as f32).clamp(0.0, 1.0)
    }
}

fn convert_track_message(
    action: &str,
    track_number: usize,
    total_tracks: usize,
    track: &PreparedTrack,
    final_path: Option<&Path>,
) -> String {
    format!(
        "{} track {} of {}: {}",
        action,
        track_number,
        total_tracks,
        progress_track_label(track, final_path)
    )
}

#[allow(dead_code)]
fn convert_track_failure_message(
    track_number: usize,
    total_tracks: usize,
    track: &PreparedTrack,
    final_path: Option<&Path>,
    error: &str,
) -> String {
    format!(
        "Track {} of {} failed: {} ({})",
        track_number,
        total_tracks,
        progress_track_label(track, final_path),
        error
    )
}

fn progress_track_label(track: &PreparedTrack, final_path: Option<&Path>) -> String {
    if let Some(title) = track
        .metadata
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return title.to_string();
    }
    if let Some(stem) = final_path
        .and_then(|path| path.file_stem())
        .and_then(|stem| stem.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return stem.to_string();
    }
    format!("Track {:02}", track.id.track_number)
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
    merge_tracks_with_tool_limits(artifacts, req, staging, runner, cancel, None).await
}

pub async fn merge_tracks_with_tool_limits(
    artifacts: ArtifactSet,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
) -> Result<(ArtifactSet, StageRecord), MergeError> {
    if !req.merge {
        return Ok((
            artifacts,
            StageRecord {
                stage: PipelineStage::Merge,
                outcome: StageOutcome::Skipped,
            },
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
            planned_command_hash: t.planned_command_hash.clone(),
        };
        return Ok((
            ArtifactSet {
                audio: AudioArtifacts::Merged(merged),
                sidecars,
            },
            StageRecord {
                stage: PipelineStage::Merge,
                outcome: StageOutcome::Ok,
            },
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
            "-f".into(),
            "concat".into(),
            "-safe".into(),
            "0".into(),
            "-i".into(),
            concat_list.display().to_string(),
            "-c".into(),
            "copy".into(),
            merged_staged.display().to_string(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(3600),
    };

    let merge_result =
        run_tool_command_with_concurrency(cmd, runner, cancel, tool_concurrency_limits.as_ref())
            .await;
    let _ = fs::remove_file(&concat_list);

    if let Err(e) = merge_result {
        let _ = fs::remove_file(&merged_staged);
        return Err(e.into());
    }

    // Validate merged output via ffprobe.
    let probe_cmd = ToolCommand {
        binary: ToolBinary::Ffprobe,
        args: vec![
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            "a:0".into(),
            "-show_entries".into(),
            "stream=sample_rate,duration".into(),
            "-show_entries".into(),
            "format=duration".into(),
            "-of".into(),
            "json".into(),
            merged_staged.display().to_string(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(30),
    };

    let probe_output = match run_tool_command_with_concurrency(
        probe_cmd,
        runner,
        cancel,
        tool_concurrency_limits.as_ref(),
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            let _ = fs::remove_file(&merged_staged);
            return Err(e.into());
        }
    };

    let (actual_sample_rate, actual_duration) = parse_merge_probe(&probe_output.stdout_tail)?;
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

    let source_tracks: Vec<TrackId> = track_artifacts.iter().map(|t| t.track_id.clone()).collect();

    // Compute a stable hash of the merge command plan for manifest rerun identity.
    // Source track identities, per-track planned hashes, and target format capture
    // the merge identity. The concat list content is excluded because it contains
    // volatile staging paths that change on every run.
    let merge_command_hash = {
        #[derive(serde::Serialize)]
        struct MergeCommandSignature<'a> {
            schema_version: u32,
            mode: &'static str,
            source_tracks: &'a [TrackId],
            source_command_hashes: Vec<Option<String>>,
            target_format: &'a tonepoet_pipeline::AudioFormat,
        }
        let source_hashes: Vec<Option<String>> = track_artifacts
            .iter()
            .map(|t| t.planned_command_hash.clone())
            .collect();
        let sig = MergeCommandSignature {
            schema_version: 1,
            mode: "ffmpeg-concat-demuxer",
            source_tracks: &source_tracks,
            source_command_hashes: source_hashes,
            target_format: &req.settings.target_format,
        };
        serde_json::to_vec(&sig)
            .map(|bytes| super::manifest::sha256_hex(&bytes))
            .ok()
    };

    let merged = MergedArtifact {
        staged_path: merged_staged,
        final_path: merged_final,
        total_samples: actual_samples,
        source_tracks,
        planned_command_hash: merge_command_hash,
    };

    Ok((
        ArtifactSet {
            audio: AudioArtifacts::Merged(merged),
            sidecars,
        },
        StageRecord {
            stage: PipelineStage::Merge,
            outcome: StageOutcome::Ok,
        },
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


fn planner_metadata_already_satisfied(
    artifacts: &ArtifactSet,
    source: &PreparedSource,
    req: &PipelineRequest,
) -> bool {
    if req.merge {
        return false;
    }

    let source_level_required = metadata_obligations_for_request(req, source);
    match &artifacts.audio {
        AudioArtifacts::Tracks(tracks) => {
            !tracks.is_empty()
                && tracks.iter().all(|track| {
                    // Planner-owned requirements are per realized track because
                    // SOURCE_AUDIO_MD5 depends on parsed SourceInfo::audio_md5,
                    // not on source kind or path extension. Authoritative
                    // materializer tags remain source-level/orchestrator-owned.
                    let required = track.metadata_required.merge(PlannedMetadataSatisfaction {
                        authoritative_tags_applied: source_level_required.authoritative_tags_applied,
                        ..PlannedMetadataSatisfaction::none()
                    });
                    !required.any()
                        || !orchestrator_metadata_stage_required(
                            track.metadata_satisfaction,
                            req.stages.metadata,
                            required,
                        )
                })
        }
        AudioArtifacts::Merged(_) => false,
    }
}

/// Apply metadata tags to staged audio artifacts.
pub async fn apply_metadata(
    artifacts: &ArtifactSet,
    source: &PreparedSource,
    req: &PipelineRequest,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<StageRecord, MetadataError> {
    apply_metadata_with_tool_limits(artifacts, source, req, runner, cancel, None).await
}

pub async fn apply_metadata_with_tool_limits(
    artifacts: &ArtifactSet,
    source: &PreparedSource,
    req: &PipelineRequest,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
) -> Result<StageRecord, MetadataError> {
    if req.stages.metadata == StageRequirement::Disabled {
        return Ok(StageRecord {
            stage: PipelineStage::Metadata,
            outcome: StageOutcome::Skipped,
        });
    }

    match &artifacts.audio {
        AudioArtifacts::Tracks(tracks) => {
            for artifact in tracks {
                if cancel.is_cancelled() {
                    return Err(MetadataError::Tool(ToolRunnerError::Cancelled {
                        command: CommandRecord {
                            binary: ToolBinary::Metaflac,
                            sanitized_args: vec![],
                            cwd: None,
                            env_keys: vec![],
                            exit: None,
                            stdout_tail: String::new(),
                            stderr_tail: String::new(),
                            elapsed: Duration::ZERO,
                        },
                    }));
                }
                let meta = source
                    .tracks
                    .iter()
                    .find(|t| t.id == artifact.track_id)
                    .map(|t| &t.metadata);
                if let Some(meta) = meta {
                    tag_audio_file(
                        &artifact.staged_path,
                        meta,
                        &source.album_metadata,
                        runner,
                        cancel,
                        tool_concurrency_limits.as_ref(),
                    )
                    .await?;
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
            tag_audio_file(
                &merged.staged_path,
                &album_as_track,
                &source.album_metadata,
                runner,
                cancel,
                tool_concurrency_limits.as_ref(),
            )
            .await?;
        }
    }

    Ok(StageRecord {
        stage: PipelineStage::Metadata,
        outcome: StageOutcome::Ok,
    })
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
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<(), MetadataError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let mut tags: Vec<(String, String)> = Vec::new();
    if let Some(ref v) = meta.title {
        push_tag_value(&mut tags, "TITLE", v);
    }
    if let Some(ref v) = meta.artist {
        push_tag_value(&mut tags, "ARTIST", v);
    }
    if let Some(ref v) = meta.album_artist {
        push_tag_value(&mut tags, "ALBUMARTIST", v);
    }
    let album_tag = album.extra.get("album_tag_override").or(album.album.as_ref());
    if let Some(v) = album_tag {
        push_tag_value(&mut tags, "ALBUM", v);
    }
    if let Some(ref v) = meta.genre {
        push_tag_value(&mut tags, "GENRE", v);
    }
    if let Some(ref v) = meta.date {
        push_tag_value(&mut tags, "DATE", v);
    }
    if let Some(n) = meta.track_number {
        push_tag_value(&mut tags, "TRACKNUMBER", &n.to_string());
    }
    if let Some(n) = meta.disc_number {
        push_tag_value(&mut tags, "DISCNUMBER", &n.to_string());
    }
    if let Some(ref v) = meta.comment {
        push_tag_value(&mut tags, "COMMENT", v);
    }

    // PR 8 CUE-specific metadata. The materializer preserves these fields in
    // TrackMetadata/AlbumMetadata, and the metadata stage writes them through
    // so published split files remain self-describing.
    if let Some(ref v) = meta.composer {
        push_tag_value(&mut tags, "COMPOSER", v);
    }
    if let Some(ref v) = meta.performer {
        push_tag_value(&mut tags, "PERFORMER", v);
    }
    if let Some(ref v) = meta.isrc {
        push_tag_value(&mut tags, "ISRC", v);
    }
    if let Some(ref v) = meta.publisher {
        push_tag_value(&mut tags, "PUBLISHER", v);
    }
    if let Some(ref v) = meta.copyright {
        push_tag_value(&mut tags, "COPYRIGHT", v);
    }
    if meta.pre_emphasis {
        push_tag_value(&mut tags, "PRE_EMPHASIS", "1");
        push_tag_value(&mut tags, "CUE_FLAGS", "PRE");
    }
    if album.total_tracks > 0 {
        push_tag_value(&mut tags, "TOTALTRACKS", &album.total_tracks.to_string());
    }
    if let Some(n) = album.total_discs {
        push_tag_value(&mut tags, "TOTALDISCS", &n.to_string());
    }
    if let Some(n) = album.disc_number {
        push_tag_value(&mut tags, "DISCNUMBER", &n.to_string());
    }
    if let Some(v) = album.extra.get("catalog") {
        push_tag_value(&mut tags, "CATALOG", v);
    }
    for (key, value) in &album.extra {
        let tag_key = cue_extra_tag_key("ALBUM", key);
        push_tag_value(&mut tags, &tag_key, value);
    }
    for (key, value) in &meta.extra {
        let tag_key = cue_extra_tag_key("TRACK", key);
        push_tag_value(&mut tags, &tag_key, value);
    }

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
            let tmp = path.with_extension(format!(
                "tmp.{}",
                path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
            ));
            let mut args = vec!["-y".into(), "-i".into(), path.display().to_string()];
            for (k, v) in &tags {
                args.push("-metadata".into());
                args.push(format!("{}={}", k.to_lowercase(), v));
            }
            args.push("-c".into());
            args.push("copy".into());
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

    run_tool_command_with_concurrency(cmd, runner, cancel, tool_concurrency_limits)
        .await
        .map_err(MetadataError::Tool)?;

    if matches!(ext.as_str(), "mp3" | "m4a" | "aac" | "wav" | "aiff" | "aif") {
        let tmp = path.with_extension(format!(
            "tmp.{}",
            path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
        ));
        if tmp.exists() {
            fs::rename(&tmp, path)?;
        }
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
    apply_replaygain_with_tool_limits(artifacts, req, runner, cancel, None).await
}

pub async fn apply_replaygain_with_tool_limits(
    artifacts: &ArtifactSet,
    req: &PipelineRequest,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
) -> Result<StageRecord, ReplayGainError> {
    if req.stages.replaygain == StageRequirement::Disabled {
        return Ok(StageRecord {
            stage: PipelineStage::ReplayGain,
            outcome: StageOutcome::Skipped,
        });
    }

    let mut args = Vec::new();
    match &artifacts.audio {
        AudioArtifacts::Tracks(tracks) => {
            if tracks.is_empty() {
                return Ok(StageRecord {
                    stage: PipelineStage::ReplayGain,
                    outcome: StageOutcome::Skipped,
                });
            }
            args.push("-a".to_string());
            args.push("-k".to_string());
            args.push("-s".to_string());
            args.push("i".to_string());
            for t in tracks {
                args.push(t.staged_path.display().to_string());
            }
        }
        AudioArtifacts::Merged(merged) => {
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
        timeout: Duration::from_secs(600),
    };
    run_tool_command_with_concurrency(cmd, runner, cancel, tool_concurrency_limits.as_ref())
        .await
        .map_err(ReplayGainError::Tool)?;
    Ok(StageRecord {
        stage: PipelineStage::ReplayGain,
        outcome: StageOutcome::Ok,
    })
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
            StageRecord {
                stage: PipelineStage::Features,
                outcome: StageOutcome::Skipped,
            },
        ));
    }

    // Derive album_dir from the first audio artifact's final path so sidecars
    // land in the same directory. Using output_root directly fails when
    // per_album_subdir is true because audio files are one level deeper.
    let album_dir = match &artifacts.audio {
        AudioArtifacts::Tracks(tracks) => tracks
            .first()
            .and_then(|t| t.final_path.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| req.output_root.clone()),
        AudioArtifacts::Merged(merged) => merged
            .final_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| req.output_root.clone()),
    };

    let log_staged = staging.root.join("conversion.log");
    let log_content = build_conversion_log(outcome, source, req);
    fs::write(&log_staged, &log_content)?;
    artifacts.sidecars.push(SidecarArtifact {
        kind: SidecarKind::ConversionLog,
        staged_path: log_staged,
        final_path: album_dir.join("conversion.log"),
    });

    if req.stages.generate_cue && source.tracks.len() > 1 {
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
        StageRecord {
            stage: PipelineStage::Features,
            outcome: StageOutcome::Ok,
        },
    ))
}

fn build_conversion_log(
    outcome: &AlbumOutcome,
    source: &PreparedSource,
    req: &PipelineRequest,
) -> String {
    let mut log = String::new();
    let tracks = collect_outcome_tracks(outcome);
    let source_tracks_by_ordinal = build_source_track_index(source);
    let successful_count = tracks
        .iter()
        .filter(|track| matches!(track.outcome, TrackOutcome::Ok))
        .count();
    let failed_count = tracks.len().saturating_sub(successful_count);
    let total_track_count = source.tracks.len().max(tracks.len());
    let total_bytes_in = tracks
        .iter()
        .filter_map(|track| track.bytes_in)
        .fold(0_u64, u64::saturating_add);
    let total_bytes_out = tracks
        .iter()
        .filter_map(|track| track.bytes_out)
        .fold(0_u64, u64::saturating_add);
    let missing_input_sizes = tracks
        .iter()
        .filter(|track| track.bytes_in.is_none())
        .count();
    let missing_output_sizes = tracks
        .iter()
        .filter(|track| track.bytes_out.is_none())
        .count();
    let missing_durations = tracks
        .iter()
        .filter(|track| track.duration.is_none())
        .count();
    let total_duration = total_track_duration(&tracks);

    log.push_str("TONEPOET CONVERSION LOG\n");
    log.push_str("=======================\n");
    push_kv_line(
        &mut log,
        "Generated (UTC)",
        chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string(),
    );
    push_kv_line(&mut log, "Job ID", &req.job_id);
    push_kv_line(&mut log, "Item ID", &req.item_id);
    log.push('\n');

    log.push_str("Source Information\n");
    log.push_str("------------------\n");
    push_kv_line(&mut log, "Container path", path_log_value(&req.container));
    push_kv_line(&mut log, "Source kind", source_kind_label(source.kind));
    push_kv_line(&mut log, "Track count", source.tracks.len().to_string());
    push_optional_kv_line(
        &mut log,
        "Album artist",
        source.album_metadata.album_artist.as_deref(),
    );
    push_optional_kv_line(&mut log, "Album", source.album_metadata.album.as_deref());
    push_optional_kv_line(
        &mut log,
        "Year",
        conversion_log_album_year(&source.album_metadata),
    );
    push_optional_kv_line(&mut log, "Genre", source.album_metadata.genre.as_deref());
    push_optional_kv_line(
        &mut log,
        "Catalog number",
        conversion_log_catalog_number(&source.album_metadata.extra),
    );
    log.push('\n');

    log.push_str("Conversion Settings\n");
    log.push_str("-------------------\n");
    push_kv_line(&mut log, "Target format", req.settings.target_format.display_name());
    push_kv_line(&mut log, "Preferred tool", format!("{:?}", req.settings.preferred_tool));
    push_kv_line(&mut log, "Target sample rate", format!("{:?}", req.settings.target_sample_rate));
    push_kv_line(&mut log, "Target bit depth", format!("{:?}", req.settings.target_bit_depth));
    push_kv_line(&mut log, "Resample quality", format!("{:?}", req.settings.resample_quality));
    push_kv_line(&mut log, "Nyquist transition", format!("{:?}", req.settings.nyquist_transition));
    push_kv_line(&mut log, "Dither type", format!("{:?}", req.settings.dither_type));
    push_kv_line(&mut log, "Force encode", yes_no(req.settings.force_encode));
    push_kv_line(&mut log, "FLAC compression", req.settings.flac.compression_level.to_string());
    push_kv_line(&mut log, "MP3 mode", format!("{:?}", req.settings.mp3.mode));
    push_kv_line(&mut log, "MP3 bitrate", format!("{} kbps", req.settings.mp3.bitrate_kbps));
    push_kv_line(&mut log, "AAC profile", format!("{:?}", req.settings.aac.profile));
    push_kv_line(&mut log, "AAC bitrate", format!("{} kbps", req.settings.aac.bitrate_kbps));
    push_kv_line(&mut log, "Opus bitrate", format!("{} kbps", req.settings.opus.bitrate_kbps));
    push_kv_line(&mut log, "WavPack mode", format!("{:?}", req.settings.wavpack.mode));
    push_kv_line(&mut log, "Merge mode", yes_no(req.merge));
    push_kv_line(
        &mut log,
        "Metadata",
        stage_requirement_label(req.stages.metadata),
    );
    push_kv_line(
        &mut log,
        "ReplayGain",
        stage_requirement_label(req.stages.replaygain),
    );
    push_kv_line(
        &mut log,
        "Features",
        stage_requirement_label(req.stages.features),
    );
    match &req.naming.folder_template {
        Some(template) => push_kv_line(&mut log, "Folder template", template),
        None => push_kv_line(&mut log, "Folder template", "album-name fallback"),
    }
    push_kv_line(&mut log, "Filename template", &req.naming.template);
    log.push('\n');

    log.push_str("Per-Track Results\n");
    log.push_str("-----------------\n");
    if tracks.is_empty() {
        log.push_str("No track records were produced.\n");
    } else {
        for record in &tracks {
            append_track_log(
                &mut log,
                record,
                source_tracks_by_ordinal
                    .get(&record.track_id.source_ordinal)
                    .copied(),
            );
        }
    }
    log.push('\n');

    log.push_str("Stage Summary\n");
    log.push_str("-------------\n");
    let stages = outcome_stage_records(outcome);
    if stages.is_empty() {
        log.push_str("No stage records were produced.\n");
    } else {
        for stage in stages {
            push_kv_line(
                &mut log,
                pipeline_stage_label(stage.stage),
                stage_outcome_label(&stage.outcome),
            );
        }
    }
    log.push('\n');

    log.push_str("Overall Summary\n");
    log.push_str("---------------\n");
    push_kv_line(
        &mut log,
        "Total tracks",
        format!(
            "{successful_count} successful / {failed_count} failed / {total_track_count} total"
        ),
    );
    append_total_size_line(
        &mut log,
        total_bytes_in,
        total_bytes_out,
        missing_input_sizes,
        missing_output_sizes,
    );
    append_total_duration_line(&mut log, total_duration, missing_durations);
    push_kv_line(
        &mut log,
        "Result",
        outcome_result_label(outcome, successful_count, failed_count, total_track_count),
    );
    log.push('\n');

    log.push_str("Log generated by tonepoet\n");
    log
}

fn build_source_track_index<'a>(source: &'a PreparedSource) -> BTreeMap<u32, &'a PreparedTrack> {
    let mut tracks_by_ordinal = BTreeMap::new();
    for track in &source.tracks {
        tracks_by_ordinal
            .entry(track.id.source_ordinal)
            .or_insert(track);
    }
    tracks_by_ordinal
}

fn append_track_log(log: &mut String, record: &TrackRecord, prepared: Option<&PreparedTrack>) {
    log.push_str(&escape_log_value(&track_display_label(record, prepared)));
    log.push('\n');
    match &record.outcome {
        TrackOutcome::Ok => log.push_str("  Status: Success\n"),
        TrackOutcome::Err(error) => {
            log.push_str("  Status: Failure\n");
            push_kv_line(log, "  Error", error);
        }
    }

    if let Some(track) = prepared {
        push_optional_kv_line(log, "  Artist", track.metadata.artist.as_deref());
        push_optional_kv_line(log, "  Composer", track.metadata.composer.as_deref());
        let mut source_audio = format_sample_rate(track.sample_rate);
        if let Some(bit_depth) = track.bit_depth {
            source_audio.push_str(&format!(", {bit_depth}-bit"));
        }
        if let Some(expected_samples) = track.expected_samples {
            source_audio.push_str(&format!(", {expected_samples} expected samples"));
        }
        push_kv_line(log, "  Source audio", source_audio);
    }

    push_kv_line(
        log,
        "  Source ref",
        track_source_ref_label(&record.source_ref),
    );
    if let Some(realized_input) = &record.realized_input {
        push_kv_line(log, "  Realized input", path_log_value(realized_input));
    }
    if let Some(output_file) = &record.output_file {
        push_kv_line(log, "  Output file", path_log_value(output_file));
    }

    match (record.bytes_in, record.bytes_out) {
        (Some(bytes_in), Some(bytes_out)) => push_kv_line(
            log,
            "  Size",
            format!(
                "{} -> {} ({})",
                format_bytes(bytes_in),
                format_bytes(bytes_out),
                compression_ratio(bytes_in, bytes_out)
            ),
        ),
        (Some(bytes_in), None) => push_kv_line(log, "  Input size", format_bytes(bytes_in)),
        (None, Some(bytes_out)) => push_kv_line(log, "  Output size", format_bytes(bytes_out)),
        (None, None) => log.push_str("  Size: unknown\n"),
    }

    if let Some(duration) = record.duration {
        push_kv_line(log, "  Encode duration", format_duration(duration));
    }

    if record.commands.is_empty() {
        log.push_str("  Commands: none recorded\n");
    } else {
        log.push_str("  Commands:\n");
        for (index, command) in record.commands.iter().enumerate() {
            log.push_str(&format!(
                "    {}. {} [{}; {}]\n",
                index + 1,
                command_line_label(command),
                format_duration(command.elapsed),
                process_exit_label(command.exit)
            ));
        }
    }
    log.push('\n');
}

fn collect_outcome_tracks(outcome: &AlbumOutcome) -> Vec<&TrackRecord> {
    let mut tracks: Vec<&TrackRecord> = match outcome {
        AlbumOutcome::Complete { tracks, .. } => tracks.iter().collect(),
        AlbumOutcome::Partial {
            successful, failed, ..
        }
        | AlbumOutcome::Blocked {
            successful, failed, ..
        } => successful.iter().chain(failed.iter()).collect(),
    };
    tracks.sort_by_key(|track| {
        (
            track.track_id.source_ordinal,
            track.track_id.disc_number.unwrap_or(0),
            track.track_id.track_number,
        )
    });
    tracks
}

fn outcome_stage_records(outcome: &AlbumOutcome) -> &[StageRecord] {
    match outcome {
        AlbumOutcome::Complete { stages, .. }
        | AlbumOutcome::Partial { stages, .. }
        | AlbumOutcome::Blocked { stages, .. } => stages,
    }
}

fn total_track_duration(tracks: &[&TrackRecord]) -> Duration {
    let mut total = Duration::ZERO;
    for duration in tracks.iter().filter_map(|track| track.duration) {
        total = total
            .checked_add(duration)
            .unwrap_or_else(|| Duration::from_secs(u64::MAX));
    }
    total
}

fn append_total_size_line(
    log: &mut String,
    total_bytes_in: u64,
    total_bytes_out: u64,
    missing_input_sizes: usize,
    missing_output_sizes: usize,
) {
    if total_bytes_in == 0 && total_bytes_out == 0 {
        log.push_str("Total size: unknown\n");
        return;
    }

    let mut line = format!(
        "{} -> {}",
        format_bytes(total_bytes_in),
        format_bytes(total_bytes_out)
    );
    if total_bytes_in > 0 {
        line.push_str(&format!(
            " ({})",
            compression_ratio(total_bytes_in, total_bytes_out)
        ));
    } else {
        line.push_str(" (ratio unavailable)");
    }
    if missing_input_sizes > 0 || missing_output_sizes > 0 {
        line.push_str(&format!(
            " [partial data: {missing_input_sizes} missing input size(s), {missing_output_sizes} missing output size(s)]"
        ));
    }
    push_kv_line(log, "Total size", line);
}

fn append_total_duration_line(
    log: &mut String,
    total_duration: Duration,
    missing_durations: usize,
) {
    let mut line = format_duration(total_duration);
    if missing_durations > 0 {
        line.push_str(&format!(
            " [partial data: {missing_durations} missing duration(s)]"
        ));
    }
    push_kv_line(log, "Total conversion time", line);
}

fn track_display_label(record: &TrackRecord, prepared: Option<&PreparedTrack>) -> String {
    let track_number = prepared
        .and_then(|track| track.metadata.track_number)
        .unwrap_or(record.track_id.track_number);
    let label = match record.track_id.disc_number {
        Some(disc_number) => format!("Disc {disc_number}, Track {track_number}"),
        None => format!("Track {track_number}"),
    };
    match prepared
        .and_then(|track| track.metadata.title.as_deref())
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        Some(title) => format!("{label}: {title}"),
        None => format!("{label} (untitled)"),
    }
}

fn command_line_label(command: &CommandRecord) -> String {
    let mut parts = Vec::with_capacity(command.sanitized_args.len() + 1);
    parts.push(command.binary.default_name().to_string());
    parts.extend(
        command
            .sanitized_args
            .iter()
            .map(|arg| quote_command_arg(arg)),
    );
    parts.join(" ")
}

fn quote_command_arg(arg: &str) -> String {
    let arg = escape_log_value(arg);
    if arg.is_empty() {
        return "''".to_string();
    }
    if arg.chars().all(|ch| {
        ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                '/' | '.' | '_' | '-' | '=' | ':' | ',' | '+' | '@' | '%'
            )
    }) {
        return arg;
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

fn process_exit_label(exit: Option<super::tool::ProcessExit>) -> String {
    match exit {
        Some(super::tool::ProcessExit::Code(0)) => "exit 0".to_string(),
        Some(super::tool::ProcessExit::Code(code)) => format!("exit {code} (error)"),
        Some(super::tool::ProcessExit::Signal(signal)) => format!("killed by signal {signal}"),
        Some(super::tool::ProcessExit::Unknown) | None => "exit unknown".to_string(),
    }
}

fn outcome_result_label(
    outcome: &AlbumOutcome,
    successful_count: usize,
    failed_count: usize,
    total_track_count: usize,
) -> String {
    match outcome {
        AlbumOutcome::Complete { .. } => {
            format!("Complete ({successful_count}/{total_track_count} ok)")
        }
        AlbumOutcome::Partial { .. } => {
            format!("Partial ({successful_count}/{total_track_count} ok, {failed_count} failed)")
        }
        AlbumOutcome::Blocked { reason, .. } => {
            format!("Blocked ({})", block_reason_label(reason))
        }
    }
}

fn block_reason_label(reason: &BlockReason) -> String {
    match reason {
        BlockReason::TrackFailures => "track failures".to_string(),
        BlockReason::RequiredStageFailure(stage) => {
            format!("required stage failed: {}", pipeline_stage_label(*stage))
        }
        BlockReason::MaterializeFailed => "materialize failed".to_string(),
        BlockReason::PlanFailed => "output planning failed".to_string(),
        BlockReason::PublishFailed => "publish failed".to_string(),
        BlockReason::DurableLogFailed => "durable log failed".to_string(),
        BlockReason::Cancelled => "cancelled".to_string(),
    }
}

fn pipeline_stage_label(stage: PipelineStage) -> &'static str {
    match stage {
        PipelineStage::Materialize => "Materialize",
        PipelineStage::PlanOutputs => "PlanOutputs",
        PipelineStage::Convert => "Convert",
        PipelineStage::Merge => "Merge",
        PipelineStage::Metadata => "Metadata",
        PipelineStage::ReplayGain => "ReplayGain",
        PipelineStage::Features => "Features",
        PipelineStage::Publish => "Publish",
        PipelineStage::DurableLog => "DurableLog",
    }
}

fn stage_outcome_label(outcome: &StageOutcome) -> String {
    match outcome {
        StageOutcome::Ok => "Ok".to_string(),
        StageOutcome::Skipped => "Skipped".to_string(),
        StageOutcome::Failed(error) => format!("Failed ({})", escape_log_value(error)),
    }
}

fn stage_requirement_label(requirement: StageRequirement) -> &'static str {
    match requirement {
        StageRequirement::Enabled => "Enabled",
        StageRequirement::Disabled => "Disabled",
    }
}

fn source_kind_label(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::SingleFile => "SingleFile",
        SourceKind::SevenZip => "SevenZip",
        SourceKind::CueImage => "CueImage",
        SourceKind::SacdIso => "SacdIso",
    }
}

fn track_source_ref_label(source_ref: &TrackSourceRef) -> String {
    match source_ref {
        TrackSourceRef::StagedFile(path) => format!("staged file {}", path_log_value(path)),
        TrackSourceRef::ImageSegment {
            image,
            start_sample,
            samples,
        } => format!(
            "image segment {} (start sample {start_sample}, {samples} samples)",
            path_log_value(image)
        ),
        TrackSourceRef::SacdTrack {
            iso,
            track_index,
            area,
        } => format!(
            "SACD track {} from {} ({:?})",
            track_index + 1,
            path_log_value(iso),
            area
        ),
    }
}

fn push_kv_line(log: &mut String, label: &str, value: impl AsRef<str>) {
    log.push_str(label);
    log.push_str(": ");
    log.push_str(&escape_log_value(value.as_ref()));
    log.push('\n');
}

fn push_optional_kv_line(log: &mut String, label: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        push_kv_line(log, label, value);
    }
}

fn path_log_value(path: &Path) -> String {
    escape_log_value(&path.display().to_string())
}

fn escape_log_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push_str(&format!("\\u{{{:x}}}", ch as u32)),
            ch => escaped.push(ch),
        }
    }
    escaped
}

fn conversion_log_album_year(metadata: &AlbumMetadata) -> Option<&str> {
    metadata.date.as_deref().and_then(first_four_digit_run)
}

fn first_four_digit_run(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    if bytes.len() < 4 {
        return None;
    }
    for start in 0..=bytes.len() - 4 {
        if bytes[start..start + 4]
            .iter()
            .all(|byte| byte.is_ascii_digit())
        {
            return value.get(start..start + 4);
        }
    }
    None
}

fn conversion_log_catalog_number(extra: &BTreeMap<String, String>) -> Option<&str> {
    for (key, value) in extra {
        let normalized = normalize_extra_key(key);
        if matches!(
            normalized.as_str(),
            "catno"
                | "catnumber"
                | "catalog"
                | "catalogid"
                | "catalogno"
                | "catalognumber"
                | "catalogue"
                | "catalogueid"
                | "catalogueno"
                | "cataloguenumber"
                | "sacdalbumcatalognumber"
        ) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

fn normalize_extra_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "Yes"
    } else {
        "No"
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0_usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let millis = duration.subsec_millis();
    if seconds == 0 {
        if millis == 0 {
            return "0s".to_string();
        }
        return format!("0.{millis:03}s");
    }
    if seconds < 60 {
        if millis == 0 {
            return format!("{seconds}s");
        }
        return format!("{seconds}.{millis:03}s");
    }
    let minutes = seconds / 60;
    let remaining_seconds = seconds % 60;
    if minutes < 60 {
        return format!("{minutes}m {remaining_seconds}s");
    }
    let hours = minutes / 60;
    let remaining_minutes = minutes % 60;
    format!("{hours}h {remaining_minutes}m {remaining_seconds}s")
}

fn compression_ratio(bytes_in: u64, bytes_out: u64) -> String {
    if bytes_in == 0 {
        return "n/a".to_string();
    }
    let input = bytes_in as f64;
    let output = bytes_out as f64;
    if bytes_out < bytes_in {
        format!("{:.1}% smaller", (1.0 - output / input) * 100.0)
    } else if bytes_out > bytes_in {
        format!("{:.1}% larger", (output / input - 1.0) * 100.0)
    } else {
        "0.0% change".to_string()
    }
}
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
                let filename = t
                    .staged_path
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or("unknown");
                cue.push_str(&format!("FILE \"{}\" WAVE\n", filename));
                cue.push_str(&format!("  TRACK {:02} AUDIO\n", i + 1));
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
            let filename = merged
                .staged_path
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
    manifest: Option<&super::manifest::ConversionManifest>,
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
                return Err(PublishError::DestinationExists(
                    plan.album_dir.display().to_string(),
                ));
            }
            OverwritePolicy::ReplaceWithBackup | OverwritePolicy::AlwaysRedo => {
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
            // Manifest-based policies are handled by the orchestrator rerun gate
            // before reaching publish. If we get here, proceed with publish.
            OverwritePolicy::SkipIfManifestMatch
            | OverwritePolicy::VerifyIfManifestMatch => {
                // The rerun gate already decided to proceed (not skip).
                // Treat as replace-with-backup for the publish step.
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

    // Write manifest into the temp dir before atomic rename; expose the final
    // post-publish path in PublishedAlbum so PipelineReport cannot contain a
    // stale temp-directory manifest path.
    let manifest_path = if let Some(manifest) = manifest {
        match super::manifest::write_manifest_for_publish(&temp_dir, &plan.album_dir, manifest) {
            Ok(_written_manifest_path) => Some(super::manifest::manifest_path(&plan.album_dir)),
            Err(err) => {
                log::warn!("manifest write failed (non-fatal): {err}");
                None
            }
        }
    } else {
        None
    };

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
        manifest_path,
    })
}

// ===========================================================================
// Durable log  (PR 6 body; PR 4 ships a minimal interim body)
// ===========================================================================

/// Write the interim durable JSON report used by PR 4.
pub fn write_durable_log(report: &PipelineReport, log: &LogPolicy) -> Result<PathBuf, LogError> {
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

fn enrich_source_with_label_info(source: &mut PreparedSource, container: &Path, req: &PipelineRequest) {
    super::source_heuristics::maybe_enrich(
        source,
        container,
        req.source.archive_password.as_ref().map(|s| s.expose()),
        req.naming.folder_template.as_deref(),
    );
    super::label_resolver::enrich_with_label_info(
        &mut source.album_metadata,
        container,
        super::label_resolver::dictionary_label_resolver(),
    );
}


// ===========================================================================
// Scheduler split points
// ===========================================================================

/// Materialization result used by the processor-level shared scheduler.
pub enum ScheduledMaterialization {
    Ready(ScheduledAlbum),
    Finished(PipelineReport),
}

/// Album state held by the global scheduler between materialization, track
/// encoding, and album-level post-processing. The run lock stays alive for the
/// whole album so two workers cannot mutate the same staging root.
pub struct ScheduledAlbum {
    pub req: PipelineRequest,
    pub item_id: String,
    pub staging: StagingDir,
    pub source: PreparedSource,
    pub plan: AlbumPlan,
    pub stages: Vec<StageRecord>,
    _run_lock: FileLock,
}

impl ScheduledAlbum {
    pub fn track_count(&self) -> usize {
        self.source.tracks.len()
    }

    pub fn allow_partial(&self) -> bool {
        self.req.failure_policy == FailurePolicy::AllowPartialAlbum
    }

    pub fn convert_root(&self) -> PathBuf {
        self.staging.root.join("converted")
    }

    pub fn planned_final_path(&self, track_id: &TrackId) -> Option<PathBuf> {
        self.plan
            .entries
            .iter()
            .find(|entry| &entry.track_id == track_id)
            .map(|entry| entry.final_path.clone())
    }
}

/// Run validation, staging setup, source materialization, and output planning.
/// Track conversion is intentionally not performed here; the caller submits
/// each ready track as an independent shared-pool work unit.
pub async fn prepare_pipeline_item_for_scheduler(
    req: PipelineRequest,
    runner: &dyn ToolRunner,
    reporter: &dyn PipelineReporter,
    cancel: &CancellationToken,
    tool_paths: &HashMap<String, PathBuf>,
) -> ScheduledMaterialization {
    let item_id = req.item_id.clone();
    let mut stages = Vec::new();
    let source: Option<PreparedSource> = None;
    let plan: Option<AlbumPlan> = None;

    if cancel.is_cancelled() {
        let outcome = AlbumOutcome::Blocked {
            successful: Vec::new(),
            failed: Vec::new(),
            stages,
            reason: BlockReason::Cancelled,
        };
        return ScheduledMaterialization::Finished(
            finalize_report(&req, reporter, source, plan, None, None, outcome).await,
        );
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
        return ScheduledMaterialization::Finished(
            finalize_report(&req, reporter, source, plan, None, None, outcome).await,
        );
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
        return ScheduledMaterialization::Finished(
            finalize_report(&req, reporter, source, plan, None, None, outcome).await,
        );
    }

    let run_lock = match acquire_run_lock(&staging_parent, &req.job_id, &req.item_id) {
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
            return ScheduledMaterialization::Finished(
                finalize_report(&req, reporter, source, plan, None, None, outcome).await,
            );
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
        return ScheduledMaterialization::Finished(
            finalize_report(&req, reporter, source, plan, None, None, outcome).await,
        );
    }

    emit_stage_started(reporter, &item_id, PipelineStage::Materialize).await;
    let materialized = match detect_source_kind(&req) {
        Ok(kind) => match materializer_for(kind) {
            Ok(materializer) => {
                materializer
                    .materialize(&req, &staging, runner, Some(reporter), tool_paths, cancel)
                    .await
            }
            Err(err) => Err(MaterializeError::Parse(err.to_string())),
        },
        Err(err) => Err(MaterializeError::Parse(err.to_string())),
    };

    let mut prepared = match materialized {
        Ok(prepared) => {
            let record = stage_record(PipelineStage::Materialize, StageOutcome::Ok);
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            prepared
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
            return ScheduledMaterialization::Finished(
                finalize_report(&req, reporter, None, None, None, None, outcome).await,
            );
        }
    };

    if cancel.is_cancelled() {
        let outcome = AlbumOutcome::Blocked {
            successful: Vec::new(),
            failed: Vec::new(),
            stages,
            reason: BlockReason::Cancelled,
        };
        return ScheduledMaterialization::Finished(
            finalize_report(&req, reporter, Some(prepared), None, None, None, outcome).await,
        );
    }

    enrich_source_with_label_info(&mut prepared, &req.container, &req);

    emit_stage_started(reporter, &item_id, PipelineStage::PlanOutputs).await;
    let album_plan = match plan_outputs(&prepared, &req) {
        Ok(album_plan) => {
            let record = stage_record(PipelineStage::PlanOutputs, StageOutcome::Ok);
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            album_plan
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
            return ScheduledMaterialization::Finished(
                finalize_report(&req, reporter, Some(prepared), None, None, None, outcome).await,
            );
        }
    };

    ScheduledMaterialization::Ready(ScheduledAlbum {
        item_id,
        req,
        staging,
        source: prepared,
        plan: album_plan,
        stages,
        _run_lock: run_lock,
    })
}

/// Encode one planned track. The caller controls scheduling; this function only
/// realizes the selected track, runs the planner-selected command chain, and
/// returns a deterministic record/artifact pair.
pub async fn encode_track_for_scheduler(
    track_index: usize,
    track: PreparedTrack,
    final_path: PathBuf,
    req: PipelineRequest,
    staging_root: PathBuf,
    staging_job: String,
    convert_root: PathBuf,
    tool_paths: HashMap<String, PathBuf>,
    reporter: &dyn PipelineReporter,
    cancel: CancellationToken,
) -> Result<ScheduledTrackOutput, String> {
    encode_track_for_scheduler_with_tool_limits(
        track_index,
        track,
        final_path,
        req,
        staging_root,
        staging_job,
        convert_root,
        tool_paths,
        None,
        reporter,
        cancel,
    )
    .await
}

pub async fn encode_track_for_scheduler_with_tool_limits(
    track_index: usize,
    track: PreparedTrack,
    final_path: PathBuf,
    req: PipelineRequest,
    staging_root: PathBuf,
    staging_job: String,
    convert_root: PathBuf,
    tool_paths: HashMap<String, PathBuf>,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
    reporter: &dyn PipelineReporter,
    cancel: CancellationToken,
) -> Result<ScheduledTrackOutput, String> {
    convert_one_track_work(
        track_index,
        track,
        final_path,
        req,
        staging_root,
        staging_job,
        convert_root,
        tool_paths,
        cancel,
        tool_concurrency_limits,
        Some(reporter),
    )
    .await
}

/// Realize one image-segment or SACD track as its own scheduler work unit.
/// Failures become normal failed track outputs so album policy, logging, and
/// durable reports stay deterministic.
pub async fn realize_track_for_scheduler(
    track_index: usize,
    track: PreparedTrack,
    final_path: PathBuf,
    req: PipelineRequest,
    staging_root: PathBuf,
    staging_job: String,
    convert_root: PathBuf,
    tool_paths: HashMap<String, PathBuf>,
    reporter: &dyn PipelineReporter,
    cancel: CancellationToken,
) -> Result<ScheduledRealizedTrack, ScheduledTrackOutput> {
    realize_track_for_scheduler_with_tool_limits(
        track_index,
        track,
        final_path,
        req,
        staging_root,
        staging_job,
        convert_root,
        tool_paths,
        None,
        reporter,
        cancel,
    )
    .await
}

pub async fn realize_track_for_scheduler_with_tool_limits(
    track_index: usize,
    track: PreparedTrack,
    final_path: PathBuf,
    req: PipelineRequest,
    staging_root: PathBuf,
    staging_job: String,
    convert_root: PathBuf,
    tool_paths: HashMap<String, PathBuf>,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
    reporter: &dyn PipelineReporter,
    cancel: CancellationToken,
) -> Result<ScheduledRealizedTrack, ScheduledTrackOutput> {
    let staging = StagingDir::borrowed(staging_root.clone(), staging_job.clone());
    let runner = RealToolRunner::new(tool_paths);
    let staged_path = staged_audio_path(&convert_root, &final_path, &track.id, &req.settings.target_format);
    let mut progress_tracker = OperationProgressTracker::new(req.item_id.clone(), PipelineStage::Convert, Some(reporter));
    match realize_track_with_tool_limits(
        &track.source_ref,
        &req,
        &staging,
        &runner,
        &cancel,
        tool_concurrency_limits,
        Some(&mut progress_tracker),
    )
    .await
    {
        Ok(realized_path) => Ok(ScheduledRealizedTrack {
            index: track_index,
            track,
            final_path,
            realized_path,
            req,
            staging_root,
            staging_job,
            convert_root,
            cancel,
        }),
        Err(err) => {
            let record = failed_track_record(&track, None, Some(staged_path), Vec::new(), err.to_string());
            Err(ScheduledTrackOutput { index: track_index, record, artifact: None, ok: false, metadata_satisfaction: PlannedMetadataSatisfaction::none() })
        }
    }
}

/// Encode a track that a prior scheduler work unit already realized. This is
/// used for CUE segment splits and SACD track extraction so realization and
/// encoding can occupy independent shared-pool slots.
pub async fn encode_realized_track_for_scheduler(
    realized: ScheduledRealizedTrack,
    tool_paths: HashMap<String, PathBuf>,
    reporter: &dyn PipelineReporter,
) -> Result<ScheduledTrackOutput, String> {
    encode_realized_track_for_scheduler_with_tool_limits(realized, tool_paths, None, reporter).await
}

pub async fn encode_realized_track_for_scheduler_with_tool_limits(
    realized: ScheduledRealizedTrack,
    tool_paths: HashMap<String, PathBuf>,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
    reporter: &dyn PipelineReporter,
) -> Result<ScheduledTrackOutput, String> {
    let runner = RealToolRunner::new(tool_paths.clone());
    let staged_path = staged_audio_path(
        &realized.convert_root,
        &realized.final_path,
        &realized.track.id,
        &realized.req.settings.target_format,
    );
    let mut progress_tracker = OperationProgressTracker::new(realized.req.item_id.clone(), PipelineStage::Convert, Some(reporter));

    if let Some(parent) = staged_path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            let record = failed_track_record(
                &realized.track,
                Some(realized.realized_path),
                Some(staged_path),
                Vec::new(),
                format!("could not create output directory: {err}"),
            );
            return Ok(ScheduledTrackOutput { index: realized.index, record, artifact: None, ok: false, metadata_satisfaction: PlannedMetadataSatisfaction::none() });
        }
    }

    let bytes_in = file_len(&realized.realized_path);
    let executed = execute_planned_track_conversion(
        &realized.req,
        &realized.track,
        &realized.realized_path,
        &staged_path,
        &realized.convert_root,
        &runner,
        &realized.cancel,
        &tool_paths,
        tool_concurrency_limits.clone(),
        &mut progress_tracker,
        0.0,
        1.0,
    )
    .await;

    match executed {
        Ok(executed) => {
            let bytes_out = file_len(&staged_path);
            if bytes_out.unwrap_or(0) == 0 {
                let error = format!("planner did not produce output: {}", staged_path.display());
                let record = failed_track_record(
                    &realized.track,
                    Some(realized.realized_path),
                    Some(staged_path),
                    executed.commands,
                    error,
                );
                Ok(ScheduledTrackOutput { index: realized.index, record, artifact: None, ok: false, metadata_satisfaction: PlannedMetadataSatisfaction::none() })
            } else {
                let actual_samples = validate_encoded_output_with_tool_limits(
                    &staged_path,
                    realized.track.expected_samples,
                    &realized.req.settings.target_format,
                    &runner,
                    &realized.cancel,
                    tool_concurrency_limits.as_ref(),
                )
                .await;
                let record = TrackRecord {
                    track_id: realized.track.id.clone(),
                    outcome: TrackOutcome::Ok,
                    source_ref: realized.track.source_ref.clone(),
                    realized_input: Some(realized.realized_path),
                    output_file: Some(staged_path.clone()),
                    commands: executed.commands,
                    bytes_in,
                    bytes_out,
                    duration: Some(executed.elapsed),
                };
                let artifact = TrackArtifact {
                    track_id: realized.track.id.clone(),
                    staged_path,
                    final_path: realized.final_path,
                    samples: actual_samples.or(realized.track.expected_samples),
                    metadata_satisfaction: executed.metadata_satisfaction,
                    metadata_required: executed.metadata_required,
                    planned_command_hash: executed.command_hash,
                };
                Ok(ScheduledTrackOutput { index: realized.index, record, artifact: Some(artifact), ok: true, metadata_satisfaction: executed.metadata_satisfaction })
            }
        }
        Err(err) => {
            let commands = command_from_convert_error(&err);
            let record = failed_track_record(
                &realized.track,
                Some(realized.realized_path),
                Some(staged_path),
                commands,
                err.to_string(),
            );
            Ok(ScheduledTrackOutput { index: realized.index, record, artifact: None, ok: false, metadata_satisfaction: PlannedMetadataSatisfaction::none() })
        }
    }
}


fn push_stage_and_reaggregate(mut outcome: AlbumOutcome, record: StageRecord, _policy: FailurePolicy) -> AlbumOutcome {
    push_stage(&mut outcome, record);
    outcome
}

fn convert_result_from_scheduled_outputs(
    source: &PreparedSource,
    outputs: Vec<ScheduledTrackOutput>,
    req: &PipelineRequest,
) -> ConvertStageResult {
    let mut records_by_index: Vec<Option<TrackRecord>> = vec![None; source.tracks.len()];
    let mut artifacts_by_index: Vec<Option<TrackArtifact>> = vec![None; source.tracks.len()];
    for output in outputs {
        if output.index < records_by_index.len() {
            records_by_index[output.index] = Some(output.record);
            artifacts_by_index[output.index] = output.artifact;
        }
    }

    let mut records = Vec::with_capacity(source.tracks.len());
    let mut artifacts = Vec::new();
    for (index, track) in source.tracks.iter().enumerate() {
        if let Some(record) = records_by_index.get_mut(index).and_then(Option::take) {
            records.push(record);
        } else {
            records.push(failed_track_record(
                track,
                None,
                None,
                Vec::new(),
                "track worker did not produce a result".to_string(),
            ));
        }
        if let Some(artifact) = artifacts_by_index.get_mut(index).and_then(Option::take) {
            artifacts.push(artifact);
        }
    }

    let failed = records.iter().any(|record| matches!(record.outcome, TrackOutcome::Err(_)));
    ConvertStageResult {
        tracks: records,
        artifacts: ArtifactSet {
            audio: AudioArtifacts::Tracks(artifacts),
            sidecars: Vec::new(),
        },
        record: stage_record(
            PipelineStage::Convert,
            if failed && req.failure_policy == FailurePolicy::FailAlbumOnAnyTrackFailure {
                StageOutcome::Failed("one or more tracks failed".to_string())
            } else {
                StageOutcome::Ok
            },
        ),
    }
}

/// Run album-level stages after every scheduled track has completed. This is
/// the post-processing gate for merge, metadata, ReplayGain, feature analysis,
/// publish, and durable log. Album-mode ReplayGain can only enter here because
/// the scheduler calls this function after all track work has reported back.
pub async fn finish_pipeline_album_for_scheduler(
    album: ScheduledAlbum,
    track_outputs: Vec<ScheduledTrackOutput>,
    runner: &dyn ToolRunner,
    reporter: &dyn PipelineReporter,
    cancel: &CancellationToken,
) -> PipelineReport {
    finish_pipeline_album_for_scheduler_with_tool_limits(
        album,
        track_outputs,
        runner,
        reporter,
        cancel,
        None,
    )
    .await
}

pub async fn finish_pipeline_album_for_scheduler_with_tool_limits(
    album: ScheduledAlbum,
    track_outputs: Vec<ScheduledTrackOutput>,
    runner: &dyn ToolRunner,
    reporter: &dyn PipelineReporter,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
) -> PipelineReport {
    let req = album.req;
    let item_id = req.item_id.clone();
    let staging = album.staging;
    let source_value = album.source;
    let plan_value = album.plan;
    let mut stages = album.stages;
    let source = Some(source_value.clone());
    let plan = Some(plan_value.clone());
    let mut published = None;

    emit_stage_started(reporter, &item_id, PipelineStage::Convert).await;
    let converted = convert_result_from_scheduled_outputs(&source_value, track_outputs, &req);
    emit_stage_finished(reporter, &item_id, converted.record.clone()).await;
    stages.push(converted.record.clone());
    let tracks = converted.tracks.clone();
    let mut artifacts = Some(converted.artifacts);

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
        match merge_tracks_with_tool_limits(
            artifacts.take().expect("artifacts present"),
            &req,
            &staging,
            runner,
            cancel,
            tool_concurrency_limits.clone(),
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
        if planner_metadata_already_satisfied(
            artifacts.as_ref().expect("artifacts present"),
            source.as_ref().expect("source present"),
            &req,
        ) {
            let record = stage_record(
                PipelineStage::Metadata,
                StageOutcome::Skipped,
            );
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
        } else {
            match apply_metadata_with_tool_limits(
                artifacts.as_ref().expect("artifacts present"),
                source.as_ref().expect("source present"),
                &req,
                runner,
                cancel,
                tool_concurrency_limits.clone(),
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
        }
    } else {
        let record = stage_record(PipelineStage::Metadata, StageOutcome::Skipped);
        emit_stage_finished(reporter, &item_id, record.clone()).await;
        stages.push(record);
    }

    if req.stages.replaygain == StageRequirement::Enabled {
        emit_stage_started(reporter, &item_id, PipelineStage::ReplayGain).await;
        match apply_replaygain_with_tool_limits(
            artifacts.as_ref().expect("artifacts present"),
            &req,
            runner,
            cancel,
            tool_concurrency_limits.clone(),
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

    current_outcome = aggregate_album_outcome(tracks, stages, req.failure_policy);
    if cancel.is_cancelled() {
        current_outcome = cancelled_outcome_from(current_outcome);
        return finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await;
    }
    if matches!(current_outcome, AlbumOutcome::Blocked { .. }) {
        return finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await;
    }

    // Build manifest from audio artifacts only when the publish policy
    // requests it. The manifest is used by the rerun gate to detect
    // identical conversions; most users don't need it.
    let conversion_manifest = if req.publish.write_manifest {
        match build_manifest_for_album(&req, &source_value, &artifacts, &plan_value) {
            Ok(manifest) => Some(manifest),
            Err(err) => {
                log::warn!("manifest build failed (non-fatal): {err}");
                None
            }
        }
    } else {
        None
    };

    emit_stage_started(reporter, &item_id, PipelineStage::Publish).await;
    match artifacts
        .as_ref()
        .ok_or(PublishError::StagingMissing)
        .and_then(|artifact_set| build_publish_plan(artifact_set, &req))
        .and_then(|publish_plan| publish_album_output(staging, &publish_plan, req.publish.clone(), conversion_manifest.as_ref()))
    {
        Ok(album) => {
            let record = stage_record(PipelineStage::Publish, StageOutcome::Ok);
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            current_outcome = push_stage_and_reaggregate(current_outcome, record, req.failure_policy);
            published = Some(album);
        }
        Err(err) => {
            let record = stage_record(PipelineStage::Publish, StageOutcome::Failed(err.to_string()));
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            current_outcome = push_stage_and_reaggregate(current_outcome, record, req.failure_policy);
            current_outcome = AlbumOutcome::Blocked {
                successful: successful_tracks_from(&current_outcome),
                failed: failed_tracks_from(&current_outcome),
                stages: stages_from(&current_outcome),
                reason: BlockReason::PublishFailed,
            };
            return finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await;
        }
    }

    finalize_report(&req, reporter, source, plan, artifacts, published, current_outcome).await
}

/// Run the staged pipeline for one queue item. `process_item` does not call
/// this yet; PR 4 freezes this orchestration shape for later PRs.
pub async fn run_pipeline_item(
    req: PipelineRequest,
    runner: &dyn ToolRunner,
    reporter: &dyn PipelineReporter,
    cancel: &CancellationToken,
) -> PipelineReport {
    let tool_paths: HashMap<String, PathBuf> = HashMap::new();
    run_pipeline_item_with_tool_paths(req, runner, reporter, cancel, &tool_paths).await
}

pub async fn run_pipeline_item_with_tool_paths(
    req: PipelineRequest,
    runner: &dyn ToolRunner,
    reporter: &dyn PipelineReporter,
    cancel: &CancellationToken,
    tool_paths: &HashMap<String, PathBuf>,
) -> PipelineReport {
    run_pipeline_item_with_tool_paths_and_tool_limits(
        req,
        runner,
        reporter,
        cancel,
        tool_paths,
        None,
    )
    .await
}

pub async fn run_pipeline_item_with_tool_paths_and_tool_limits(
    req: PipelineRequest,
    runner: &dyn ToolRunner,
    reporter: &dyn PipelineReporter,
    cancel: &CancellationToken,
    tool_paths: &HashMap<String, PathBuf>,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
) -> PipelineReport {
    let item_id = req.item_id.clone();
    let mut source = None;
    let mut plan = None;
    let mut artifacts = None;
    let mut published = None;
    #[allow(unused_assignments)]
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
        let record = stage_record(
            PipelineStage::Materialize,
            StageOutcome::Failed(err.to_string()),
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
            let record = stage_record(
                PipelineStage::Materialize,
                StageOutcome::Failed(err.to_string()),
            );
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            let outcome = AlbumOutcome::Blocked {
                successful: Vec::new(),
                failed: Vec::new(),
                stages,
                reason: BlockReason::MaterializeFailed,
            };
            return finalize_report(&req, reporter, source, plan, artifacts, published, outcome)
                .await;
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
            Ok(materializer) => {
                materializer
                    .materialize(&req, &staging, runner, Some(reporter), tool_paths, cancel)
                    .await
            }
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
            let record = stage_record(
                PipelineStage::Materialize,
                StageOutcome::Failed(err.to_string()),
            );
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            let outcome = AlbumOutcome::Blocked {
                successful: Vec::new(),
                failed: Vec::new(),
                stages,
                reason,
            };
            return finalize_report(&req, reporter, source, plan, artifacts, published, outcome)
                .await;
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

    // Enrich album metadata with label/pressing info before planning.
    // Tag-sourced values take priority; the resolver only fills gaps.
    if let Some(ref mut src) = source {
        enrich_source_with_label_info(src, &req.container, &req);
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
            let record = stage_record(
                PipelineStage::PlanOutputs,
                StageOutcome::Failed(err.to_string()),
            );
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            let outcome = AlbumOutcome::Blocked {
                successful: Vec::new(),
                failed: Vec::new(),
                stages,
                reason: BlockReason::PlanFailed,
            };
            return finalize_report(&req, reporter, source, plan, artifacts, published, outcome)
                .await;
        }
    }

    emit_stage_started(reporter, &item_id, PipelineStage::Convert).await;
    let converted = convert_tracks_with_reporter_with_tool_paths(
        source.as_ref().expect("source present"),
        plan.as_ref().expect("plan present"),
        &req,
        &staging,
        runner,
        cancel,
        Some(reporter),
        tool_paths,
        tool_concurrency_limits.clone(),
    )
    .await;
    emit_stage_finished(reporter, &item_id, converted.record.clone()).await;
    stages.push(converted.record.clone());
    tracks = converted.tracks.clone();
    artifacts = Some(converted.artifacts);

    let mut current_outcome =
        aggregate_album_outcome(tracks.clone(), stages.clone(), req.failure_policy);
    if cancel.is_cancelled() {
        current_outcome = cancelled_outcome_from(current_outcome);
        return finalize_report(
            &req,
            reporter,
            source,
            plan,
            artifacts,
            published,
            current_outcome,
        )
        .await;
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
        return finalize_report(
            &req,
            reporter,
            source,
            plan,
            artifacts,
            published,
            current_outcome,
        )
        .await;
    }

    if req.merge {
        emit_stage_started(reporter, &item_id, PipelineStage::Merge).await;
        match merge_tracks_with_tool_limits(
            artifacts.take().expect("artifacts present"),
            &req,
            &staging,
            runner,
            cancel,
            tool_concurrency_limits.clone(),
        )
        .await
        {
            Ok((merged_artifacts, record)) => {
                emit_stage_finished(reporter, &item_id, record.clone()).await;
                stages.push(record);
                artifacts = Some(merged_artifacts);
            }
            Err(err) => {
                let record =
                    stage_record(PipelineStage::Merge, StageOutcome::Failed(err.to_string()));
                emit_stage_finished(reporter, &item_id, record.clone()).await;
                stages.push(record);
                current_outcome =
                    aggregate_album_outcome(tracks.clone(), stages.clone(), req.failure_policy);
                return finalize_report(
                    &req,
                    reporter,
                    source,
                    plan,
                    artifacts,
                    published,
                    current_outcome,
                )
                .await;
            }
        }
    } else {
        let record = stage_record(PipelineStage::Merge, StageOutcome::Skipped);
        emit_stage_finished(reporter, &item_id, record.clone()).await;
        stages.push(record);
    }

    if req.stages.metadata == StageRequirement::Enabled {
        emit_stage_started(reporter, &item_id, PipelineStage::Metadata).await;
        if planner_metadata_already_satisfied(
            artifacts.as_ref().expect("artifacts present"),
            source.as_ref().expect("source present"),
            &req,
        ) {
            let record = stage_record(PipelineStage::Metadata, StageOutcome::Skipped);
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
        } else {
            match apply_metadata_with_tool_limits(
                artifacts.as_ref().expect("artifacts present"),
                source.as_ref().expect("source present"),
                &req,
                runner,
                cancel,
                tool_concurrency_limits.clone(),
            )
            .await
            {
                Ok(record) => {
                    emit_stage_finished(reporter, &item_id, record.clone()).await;
                    stages.push(record);
                }
                Err(err) => {
                    let record = stage_record(
                        PipelineStage::Metadata,
                        StageOutcome::Failed(err.to_string()),
                    );
                    emit_stage_finished(reporter, &item_id, record.clone()).await;
                    stages.push(record);
                    current_outcome =
                        aggregate_album_outcome(tracks.clone(), stages.clone(), req.failure_policy);
                    return finalize_report(
                        &req,
                        reporter,
                        source,
                        plan,
                        artifacts,
                        published,
                        current_outcome,
                    )
                    .await;
                }
            }
        }
    } else {
        let record = stage_record(PipelineStage::Metadata, StageOutcome::Skipped);
        emit_stage_finished(reporter, &item_id, record.clone()).await;
        stages.push(record);
    }

    if req.stages.replaygain == StageRequirement::Enabled {
        emit_stage_started(reporter, &item_id, PipelineStage::ReplayGain).await;
        match apply_replaygain_with_tool_limits(
            artifacts.as_ref().expect("artifacts present"),
            &req,
            runner,
            cancel,
            tool_concurrency_limits.clone(),
        )
        .await
        {
            Ok(record) => {
                emit_stage_finished(reporter, &item_id, record.clone()).await;
                stages.push(record);
            }
            Err(err) => {
                let record = stage_record(
                    PipelineStage::ReplayGain,
                    StageOutcome::Failed(err.to_string()),
                );
                emit_stage_finished(reporter, &item_id, record.clone()).await;
                stages.push(record);
                current_outcome =
                    aggregate_album_outcome(tracks.clone(), stages.clone(), req.failure_policy);
                return finalize_report(
                    &req,
                    reporter,
                    source,
                    plan,
                    artifacts,
                    published,
                    current_outcome,
                )
                .await;
            }
        }
    } else {
        let record = stage_record(PipelineStage::ReplayGain, StageOutcome::Skipped);
        emit_stage_finished(reporter, &item_id, record.clone()).await;
        stages.push(record);
    }

    current_outcome = aggregate_album_outcome(tracks.clone(), stages.clone(), req.failure_policy);
    if matches!(current_outcome, AlbumOutcome::Blocked { .. }) {
        return finalize_report(
            &req,
            reporter,
            source,
            plan,
            artifacts,
            published,
            current_outcome,
        )
        .await;
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
                let record = stage_record(
                    PipelineStage::Features,
                    StageOutcome::Failed(err.to_string()),
                );
                emit_stage_finished(reporter, &item_id, record.clone()).await;
                stages.push(record);
                current_outcome =
                    aggregate_album_outcome(tracks.clone(), stages.clone(), req.failure_policy);
                return finalize_report(
                    &req,
                    reporter,
                    source,
                    plan,
                    artifacts,
                    published,
                    current_outcome,
                )
                .await;
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
        return finalize_report(
            &req,
            reporter,
            source,
            plan,
            artifacts,
            published,
            current_outcome,
        )
        .await;
    }
    if matches!(current_outcome, AlbumOutcome::Blocked { .. }) {
        return finalize_report(
            &req,
            reporter,
            source,
            plan,
            artifacts,
            published,
            current_outcome,
        )
        .await;
    }

    emit_stage_started(reporter, &item_id, PipelineStage::Publish).await;
    match artifacts
        .as_ref()
        .ok_or(PublishError::StagingMissing)
        .and_then(|artifact_set| build_publish_plan(artifact_set, &req))
        .and_then(|publish_plan| publish_album_output(staging, &publish_plan, req.publish.clone(), None))
    {
        Ok(album) => {
            let record = stage_record(PipelineStage::Publish, StageOutcome::Ok);
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            published = Some(album);
        }
        Err(err) => {
            let record = stage_record(
                PipelineStage::Publish,
                StageOutcome::Failed(err.to_string()),
            );
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            current_outcome = AlbumOutcome::Blocked {
                successful: successful_tracks_from(&current_outcome),
                failed: failed_tracks_from(&current_outcome),
                stages,
                reason: BlockReason::PublishFailed,
            };
            return finalize_report(
                &req,
                reporter,
                source,
                plan,
                artifacts,
                published,
                current_outcome,
            )
            .await;
        }
    }

    current_outcome = aggregate_album_outcome(tracks, stages, req.failure_policy);
    finalize_report(
        &req,
        reporter,
        source,
        plan,
        artifacts,
        published,
        current_outcome,
    )
    .await
}

fn pipeline_report_manifest_path(published: &Option<PublishedAlbum>) -> Option<std::path::PathBuf> {
    published
        .as_ref()
        .and_then(|album| album.manifest_path.clone())
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
    let should_write = req.log.write_json_log
        && match &outcome {
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
            settings_fingerprint: None,
            manifest_path: pipeline_report_manifest_path(&published),
        };
        // Write the log alongside the album artifacts when possible,
        // fall back to the configured log root for blocked/failed jobs.
        let effective_log = match &published {
            Some(album) => LogPolicy {
                root: album.album_dir.clone(),
                ..req.log.clone()
            },
            None => req.log.clone(),
        };
        match write_durable_log(&report_to_write, &effective_log) {
            Ok(path) => {
                durable_log = Some(path);
                outcome = logged_outcome;
                emit_stage_finished(reporter, &item_id, ok_record).await;
            }
            Err(err) => {
                let error_text = err.to_string();
                let terminal_error =
                    durable_log_failure_terminal_error(&outcome, published.as_ref(), &error_text);
                let record =
                    stage_record(PipelineStage::DurableLog, StageOutcome::Failed(error_text));
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
        .emit(PipelineEvent::Terminal { item_id, status })
        .await;

    PipelineReport {
        request: RedactedPipelineRequest::from(req),
        source,
        plan,
        artifacts,
        manifest_path: pipeline_report_manifest_path(&published),
        published,
        outcome,
        durable_log,
        settings_fingerprint: None,
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
        return AlbumOutcome::Complete {
            tracks: successful,
            stages,
        };
    }

    match policy {
        FailurePolicy::FailAlbumOnAnyTrackFailure => AlbumOutcome::Blocked {
            successful,
            failed,
            stages,
            reason: BlockReason::TrackFailures,
        },
        FailurePolicy::AllowPartialAlbum => AlbumOutcome::Partial {
            successful,
            failed,
            stages,
        },
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
    let output_path = published.map(|p| p.album_dir.clone()).unwrap_or_default();
    let log_path = durable_log.map(|p| p.to_path_buf());

    match outcome {
        AlbumOutcome::Complete { .. } => ConversionStatus::Completed {
            output_path,
            log_path,
        },
        AlbumOutcome::Partial {
            successful, failed, ..
        } => ConversionStatus::Partial {
            output_path,
            successful: successful.len() as u32,
            failed: failed.len() as u32,
            log_path: log_path.unwrap_or_default(),
        },
        AlbumOutcome::Blocked { reason, stages, .. } => {
            let stage_error = stages.iter().rev().find_map(|r| match &r.outcome {
                StageOutcome::Failed(err) => Some(format!("{:?}: {}", r.stage, err)),
                _ => None,
            });
            let error = match stage_error {
                Some(detail) => detail,
                None => format!("album blocked: {:?}", reason),
            };
            ConversionStatus::Failed { error, log_path }
        }
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

async fn emit_stage_started(reporter: &dyn PipelineReporter, item_id: &str, stage: PipelineStage) {
    reporter
        .emit(PipelineEvent::StageStarted {
            item_id: item_id.to_string(),
            stage,
        })
        .await;
}

async fn emit_stage_finished(reporter: &dyn PipelineReporter, item_id: &str, record: StageRecord) {
    reporter
        .emit(PipelineEvent::StageFinished {
            item_id: item_id.to_string(),
            record,
        })
        .await;
}

fn validate_template(template: &str) -> Result<(), String> {
    // Only validate structure. Unknown tokens are resolved from extra maps at render time.
    let mut rest = template;
    while let Some(start) = rest.find('%') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('%') else {
            return Err("unclosed % token".to_string());
        };
        let token = &rest[..end];
        if token.is_empty() {
            return Err("empty token %%".to_string());
        }
        rest = &rest[end + 1..];
    }
    Ok(())
}

fn render_track_template(
    template: &str,
    source: &PreparedSource,
    track: &PreparedTrack,
    format: &PlannerAudioFormat,
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
        .map(|artist| sanitize_component(&super::label_resolver::canonicalize_artist(artist)))
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let album_artist = source
        .album_metadata
        .album_artist
        .as_deref()
        .map(|artist| sanitize_component(&super::label_resolver::canonicalize_artist(artist)))
        .unwrap_or_else(|| artist.clone());
    let raw_album = source.album_metadata.album.as_deref().unwrap_or("Album");
    let (album, title_extra) = if template.contains("%TITLE_EXTRA%") {
        match extract_title_extra(raw_album) {
            Some((clean, extra)) => (sanitize_component(&clean), sanitize_component(&extra)),
            None => (sanitize_component(raw_album), String::new()),
        }
    } else {
        (sanitize_component(raw_album), String::new())
    };
    let disc = track
        .id
        .disc_number
        .or(track.metadata.disc_number)
        .unwrap_or(1);
    let n = track.metadata.track_number.unwrap_or(track.id.track_number);
    let year = source
        .album_metadata
        .date
        .as_deref()
        .and_then(extract_year_from_date)
        .unwrap_or_default();
    let genre = track
        .metadata
        .genre
        .as_deref()
        .or(source.album_metadata.genre.as_deref())
        .map(sanitize_component)
        .unwrap_or_default();
    let composer = track
        .metadata
        .composer
        .as_deref()
        .map(sanitize_component)
        .unwrap_or_default();
    let catalog = catalog_value(&source.album_metadata.extra)
        .map(sanitize_component)
        .unwrap_or_default();
    let sample_rate = sanitize_component(&format_sample_rate(track.sample_rate));
    let bit_depth = track
        .bit_depth
        .map(|depth| depth.to_string())
        .map(|value| sanitize_component(&value))
        .unwrap_or_default();
    let isrc = track
        .metadata
        .isrc
        .as_deref()
        .map(sanitize_component)
        .unwrap_or_default();

    let mut rendered = template.to_string();
    let nn = format!("{n:02}");
    rendered = rendered.replace("%NN%", &nn);
    rendered = rendered.replace("%TRACKNN%", &nn);
    let n_str = n.to_string();
    rendered = rendered.replace("%N%", &n_str);
    rendered = rendered.replace("%TRACKN%", &n_str);
    rendered = rendered.replace("%TRACK%", &n_str);
    rendered = rendered.replace("%TITLE%", &title);
    rendered = rendered.replace("%ARTIST%", &artist);
    rendered = rendered.replace("%ALBUM_ARTIST%", &album_artist);
    rendered = rendered.replace("%ALBUM%", &album);
    rendered = rendered.replace("%TITLE_EXTRA%", &title_extra);
    rendered = rendered.replace("%DISC%", &disc.to_string());
    rendered = rendered.replace("%FORMAT%", format.display_name());
    rendered = rendered.replace("%YEAR%", &year);
    rendered = rendered.replace("%GENRE%", &genre);
    rendered = rendered.replace("%COMPOSER%", &composer);
    rendered = rendered.replace("%CATALOG%", &catalog);
    rendered = rendered.replace("%SAMPLERATE%", &sample_rate);
    rendered = rendered.replace("%BITDEPTH%", &bit_depth);
    rendered = rendered.replace("%ISRC%", &isrc);
    rendered = resolve_extra_tokens(
        &rendered,
        Some(&track.metadata.extra),
        &source.album_metadata.extra,
    );

    let rel = PathBuf::from(rendered);
    if rel.as_os_str().is_empty() {
        return Err(PlanError::InvalidTemplate(
            "template rendered empty".to_string(),
        ));
    }
    Ok(rel)
}

fn render_folder_template(template: &str, source: &PreparedSource, format: &PlannerAudioFormat) -> PathBuf {
    let artist = source
        .album_metadata
        .album_artist
        .as_deref()
        .or_else(|| {
            source
                .tracks
                .iter()
                .find_map(|track| track.metadata.artist.as_deref())
        })
        .map(|artist| sanitize_component(&super::label_resolver::canonicalize_artist(artist)))
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let raw_album = source
        .album_metadata
        .album
        .as_deref()
        .or_else(|| source.container.file_stem().and_then(|s| s.to_str()))
        .unwrap_or("Album");
    let (album, title_extra) = if template.contains("%TITLE_EXTRA%") {
        match extract_title_extra(raw_album) {
            Some((clean, extra)) => (sanitize_component(&clean), sanitize_component(&extra)),
            None => (sanitize_component(raw_album), String::new()),
        }
    } else {
        (sanitize_component(raw_album), String::new())
    };
    let year = source
        .album_metadata
        .date
        .as_deref()
        .and_then(extract_year_from_date)
        .unwrap_or_default();
    let genre = source
        .album_metadata
        .genre
        .as_deref()
        .map(sanitize_component)
        .unwrap_or_default();
    let catalog = catalog_value(&source.album_metadata.extra)
        .map(sanitize_component)
        .unwrap_or_default();
    let sample_rate = source
        .tracks
        .first()
        .map(|track| sanitize_component(&format_sample_rate(track.sample_rate)))
        .unwrap_or_default();
    let bit_depth = source
        .tracks
        .first()
        .and_then(|track| track.bit_depth)
        .map(|depth| depth.to_string())
        .map(|value| sanitize_component(&value))
        .unwrap_or_default();

    let mut rendered = template.to_string();
    rendered = rendered.replace("%ARTIST%", &artist);
    rendered = rendered.replace("%ALBUM_ARTIST%", &artist);
    rendered = rendered.replace("%ALBUM%", &album);
    rendered = rendered.replace("%TITLE_EXTRA%", &title_extra);
    rendered = rendered.replace("%YEAR%", &year);
    rendered = rendered.replace("%GENRE%", &genre);
    rendered = rendered.replace("%CATALOG%", &catalog);
    rendered = rendered.replace("%FORMAT%", format.display_name());
    rendered = rendered.replace("%SAMPLERATE%", &sample_rate);
    rendered = rendered.replace("%BITDEPTH%", &bit_depth);
    rendered = resolve_extra_tokens(&rendered, None, &source.album_metadata.extra);

    path_from_template_components(&rendered)
}

fn resolve_extra_tokens(
    rendered: &str,
    track_extra: Option<&BTreeMap<String, String>>,
    album_extra: &BTreeMap<String, String>,
) -> String {
    let mut output = String::with_capacity(rendered.len());
    let mut rest = rendered;

    while let Some(start) = rest.find('%') {
        output.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        let Some(end) = rest.find('%') else {
            output.push('%');
            output.push_str(rest);
            return output;
        };

        let token = &rest[..end];
        let key = token.to_ascii_lowercase();
        let value = track_extra
            .and_then(|extra| extra.get(&key))
            .or_else(|| album_extra.get(&key))
            .map(|value| sanitize_component(value))
            .unwrap_or_default();
        output.push_str(&value);
        rest = &rest[end + 1..];
    }

    output.push_str(rest);
    output
}

fn path_from_template_components(rendered: &str) -> PathBuf {
    let mut path = PathBuf::new();
    for component in rendered
        .split('/')
        .map(|component| component.trim().trim_matches('.').trim())
        .filter(|component| !component.is_empty())
    {
        path.push(component);
    }
    path
}

/// Extract metadata from a trailing parenthetical in an album name.
///
/// Scans for the last `(content)` optionally followed by `[bracket]` at the
/// end of the string. If the parenthetical content contains a recognized
/// metadata identifier (catalog prefix, format keyword, or audiophile label),
/// returns `Some((clean_album, extra_content))`. Otherwise returns `None`.
///
/// Only called when `%TITLE_EXTRA%` appears in the template.
fn extract_title_extra(album: &str) -> Option<(String, String)> {
    // Find the last '(' that has a matching ')'
    let mut depth = 0_i32;
    let mut last_open = None;
    let mut last_close = None;
    for (i, ch) in album.char_indices().rev() {
        match ch {
            ')' => {
                depth += 1;
                if last_close.is_none() {
                    last_close = Some(i);
                }
            }
            '(' => {
                depth -= 1;
                if depth == 0 {
                    last_open = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }

    let open = last_open?;
    let close = last_close?;
    if close <= open {
        return None;
    }

    // Don't strip parentheticals at the very start of the album name
    // (e.g., "(Ain't That) Good News")
    if open == 0 {
        return None;
    }

    let content = &album[open + 1..close];
    if content.trim().is_empty() {
        return None;
    }

    // Check if the content contains a recognized metadata identifier
    if !super::label_resolver::contains_metadata_identifier(content) {
        return None;
    }

    // Strip the parenthetical and any trailing bracket like [ISO], [FLAC]
    let before = album[..open].trim_end();
    let after = album[close + 1..].trim_start();

    // Strip trailing [...] if present
    let clean = if after.starts_with('[') {
        if let Some(bracket_end) = after.find(']') {
            let remainder = after[bracket_end + 1..].trim();
            if remainder.is_empty() {
                before.to_string()
            } else {
                format!("{} {}", before, remainder)
            }
        } else {
            before.to_string()
        }
    } else if after.is_empty() {
        before.to_string()
    } else {
        format!("{} {}", before, after)
    };

    Some((clean, content.trim().to_string()))
}

fn catalog_value(extra: &BTreeMap<String, String>) -> Option<&str> {
    extra
        .get("catalog")
        .or_else(|| extra.get("catalognumber"))
        .or_else(|| extra.get("sacd_album_catalog_number"))
        .map(String::as_str)
}

fn extract_year_from_date(date: &str) -> Option<String> {
    let mut run = String::new();
    for ch in date.chars() {
        if ch.is_ascii_digit() {
            run.push(ch);
            if run.len() == 4 {
                return Some(run);
            }
        } else {
            run.clear();
        }
    }
    None
}

fn format_sample_rate(hz: u32) -> String {
    const DSD64_HZ: u32 = 2_822_400;
    if hz >= DSD64_HZ && hz % DSD64_HZ == 0 {
        return format!("DSD{}", 64 * (hz / DSD64_HZ));
    }
    if hz % 1_000 == 0 {
        return format!("{}kHz", hz / 1_000);
    }
    if hz % 100 == 0 {
        return format!("{:.1}kHz", hz as f64 / 1_000.0);
    }
    let mut khz = format!("{:.3}", hz as f64 / 1_000.0);
    while khz.contains('.') && khz.ends_with('0') {
        khz.pop();
    }
    if khz.ends_with('.') {
        khz.pop();
    }
    format!("{khz}kHz")
}

fn sanitize_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            ch if ch.is_control() => ' ',
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

fn append_default_extension(
    path: &mut PathBuf,
    format: &PlannerAudioFormat,
    container_extension: Option<&str>,
) {
    let ext = container_extension.unwrap_or(format.extension());
    if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case(ext))
        .unwrap_or(false)
    {
        return;
    }
    path.set_extension(ext);
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
    format: &PlannerAudioFormat,
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

fn command_from_convert_error(err: &ConvertError) -> Vec<CommandRecord> {
    match err {
        ConvertError::Tool(tool_err) => command_from_tool_error(tool_err).into_iter().collect(),
        _ => Vec::new(),
    }
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
        return Err(PublishError::PathOutsideOutputRoot(
            final_path.display().to_string(),
        ));
    }
    let key = normalized_collision_key(&final_path);
    if !seen.insert(key) {
        return Err(PublishError::DestinationExists(
            final_path.display().to_string(),
        ));
    }
    entries.push(PublishEntry {
        staged_path,
        final_path,
        role,
    });
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
        let rel = final_path
            .strip_prefix(output_root)
            .map_err(|_| PublishError::PathOutsideOutputRoot(final_path.display().to_string()))?;
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
    acquire_file_lock(&lock_path, "album directory is locked by another process").map_err(|err| {
        match err {
            LockAcquireError::Busy(message) => PublishError::DestinationExists(message),
            LockAcquireError::Io(err) => PublishError::Io(err),
        }
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
    acquire_file_lock(&lock_path, "pipeline item is already running").map_err(|err| match err {
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
        Ok(()) => Ok(FileLock {
            file,
            path: lock_path.to_path_buf(),
        }),
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
    path: PathBuf,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = fs::remove_file(&self.path);
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

/// Build a conversion manifest from audio artifacts and source metadata.
/// Handles both per-track and merged artifact sets.
fn build_manifest_for_album(
    req: &PipelineRequest,
    source: &PreparedSource,
    artifacts: &Option<ArtifactSet>,
    album_plan: &AlbumPlan,
) -> Result<super::manifest::ConversionManifest, String> {
    use super::manifest::{TrackIdentity, ValidationStatus};
    use super::manifest_builder::{build_conversion_manifest, ManifestBuildInput, ManifestTrackBuildInput};

    let artifact_set = artifacts.as_ref().ok_or("no artifacts available for manifest")?;
    let album_relative = |final_path: &std::path::Path| -> Result<std::path::PathBuf, String> {
        super::manifest::album_relative_output_path(&album_plan.album_dir, final_path).map_err(|err| {
            format!(
                "manifest output path must be inside album dir {}: {} ({err})",
                album_plan.album_dir.display(),
                final_path.display(),
            )
        })
    };

    let track_inputs = match &artifact_set.audio {
        AudioArtifacts::Tracks(track_artifacts) => {
            let mut inputs = Vec::with_capacity(track_artifacts.len());
            for artifact in track_artifacts {
                let source_path = source
                    .tracks
                    .iter()
                    .find(|t| t.id == artifact.track_id)
                    .map(|t| match &t.source_ref {
                        TrackSourceRef::StagedFile(p) => p.clone(),
                        TrackSourceRef::ImageSegment { image, .. } => image.clone(),
                        TrackSourceRef::SacdTrack { iso, .. } => iso.clone(),
                    })
                    .unwrap_or_else(|| req.container.clone());

                inputs.push(ManifestTrackBuildInput {
                    source_path,
                    source_audio_md5: None,
                    track_identity: TrackIdentity {
                        source_ordinal: artifact.track_id.source_ordinal as usize,
                        disc_number: artifact.track_id.disc_number,
                        track_number: Some(artifact.track_id.track_number),
                    },
                    planned_command_hash: artifact.planned_command_hash.clone().unwrap_or_default(),
                    album_relative_output_path: album_relative(&artifact.final_path)?,
                    staged_output_path: artifact.staged_path.clone(),
                    validation_status: ValidationStatus::Passed,
                    record_output_hash: req.settings.verification.verify_after_encode,
                });
            }
            inputs
        }
        AudioArtifacts::Merged(merged) => {
            let planned_command_hash = merged
                .planned_command_hash
                .clone()
                .ok_or_else(|| "merged artifact missing planned_command_hash".to_string())?;

            vec![ManifestTrackBuildInput::merged_output(
                req.container.clone(),
                planned_command_hash,
                album_relative(&merged.final_path)?,
                merged.staged_path.clone(),
                ValidationStatus::Passed,
                req.settings.verification.verify_after_encode,
            )]
        }
    };

    build_conversion_manifest(ManifestBuildInput {
        album_dir: album_plan.album_dir.clone(),
        settings: req.settings.clone(),
        tracks: track_inputs,
    })
    .map_err(|err| format!("manifest build: {err}"))
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

#[cfg(test)]
mod conversion_log_tests {
    use super::*;

    fn log_test_source() -> PreparedSource {
        let mut album_extra = BTreeMap::new();
        album_extra.insert("catalog_number".to_string(), "CAT-123".to_string());
        PreparedSource {
            container: PathBuf::from("/tmp/input.7z"),
            kind: SourceKind::SevenZip,
            tracks: vec![
                PreparedTrack {
                    id: TrackId {
                        source_ordinal: 1,
                        disc_number: Some(1),
                        track_number: 1,
                    },
                    source_ref: TrackSourceRef::StagedFile(PathBuf::from("/stage/01.wav")),
                    metadata: TrackMetadata {
                        title: Some("One".to_string()),
                        artist: Some("Artist One".to_string()),
                        composer: Some("Composer One".to_string()),
                        track_number: Some(1),
                        disc_number: Some(1),
                        ..TrackMetadata::default()
                    },
                    expected_samples: Some(44_100),
                    sample_rate: 44_100,
                    bit_depth: Some(24),
                },
                PreparedTrack {
                    id: TrackId {
                        source_ordinal: 2,
                        disc_number: Some(1),
                        track_number: 2,
                    },
                    source_ref: TrackSourceRef::StagedFile(PathBuf::from("/stage/02.wav")),
                    metadata: TrackMetadata {
                        title: None,
                        track_number: Some(2),
                        disc_number: Some(1),
                        ..TrackMetadata::default()
                    },
                    expected_samples: None,
                    sample_rate: 96_000,
                    bit_depth: Some(24),
                },
            ],
            album_metadata: AlbumMetadata {
                album: Some("Test Album".to_string()),
                album_artist: Some("Test Artist".to_string()),
                genre: Some("Jazz".to_string()),
                date: Some("2025-12-31".to_string()),
                total_tracks: 2,
                extra: album_extra,
                ..AlbumMetadata::default()
            },
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SevenZip,
                source_sha256: None,
                tool_versions: BTreeMap::new(),
                extracted_at: chrono::Utc::now(),
            },
        }
    }

    fn log_test_request() -> PipelineRequest {
        PipelineRequest {
            job_id: "job-1".to_string(),
            item_id: "item-1".to_string(),
            container: PathBuf::from("/tmp/input.7z"),
            source: SourceOptions {
                archive_password: None,
                sacd_area: None,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection: TrackSelection::All,
            },
            settings: tonepoet_pipeline::PipelineSettings::default(),
            worker_count: None,
            merge: false,
            output_root: PathBuf::from("/out"),
            naming: NamingPolicy {
                template: "%NN% - %TITLE%".to_string(),
                folder_template: Some("%ARTIST%/%ALBUM%".to_string()),
                per_album_subdir: true,
                collision_policy: NamingCollisionPolicy::Fail,
            },
            publish: PublishPolicy {
                overwrite: OverwritePolicy::FailIfExists,
                same_filesystem_required: false,
                write_manifest: false,
            },
            log: LogPolicy {
                root: PathBuf::from("/out/.tonepoet-logs"),
                write_for_blocked: true,
                write_json_log: false,
            },
            stages: StagePolicy {
                metadata: StageRequirement::Enabled,
                replaygain: StageRequirement::Disabled,
                features: StageRequirement::Enabled,
                generate_cue: false,
            },
            failure_policy: FailurePolicy::AllowPartialAlbum,
            container_extension: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }

    fn command_record() -> CommandRecord {
        CommandRecord {
            binary: ToolBinary::Ffmpeg,
            sanitized_args: vec![
                "-i".to_string(),
                "/tmp/in file.wav".to_string(),
                "/tmp/out.flac".to_string(),
            ],
            cwd: None,
            env_keys: vec![],
            exit: Some(super::super::tool::ProcessExit::Code(0)),
            stdout_tail: "ignored stdout".to_string(),
            stderr_tail: "ignored stderr".to_string(),
            elapsed: Duration::from_secs(65),
        }
    }

    fn ok_record() -> TrackRecord {
        TrackRecord {
            track_id: TrackId {
                source_ordinal: 1,
                disc_number: Some(1),
                track_number: 1,
            },
            outcome: TrackOutcome::Ok,
            source_ref: TrackSourceRef::StagedFile(PathBuf::from("/stage/01.wav")),
            realized_input: Some(PathBuf::from("/realized/01.wav")),
            output_file: Some(PathBuf::from("/encoded/01.flac")),
            commands: vec![command_record()],
            bytes_in: Some(2048),
            bytes_out: Some(1024),
            duration: Some(Duration::from_secs(65)),
        }
    }

    fn failed_record() -> TrackRecord {
        TrackRecord {
            track_id: TrackId {
                source_ordinal: 2,
                disc_number: Some(1),
                track_number: 2,
            },
            outcome: TrackOutcome::Err("encode failed".to_string()),
            source_ref: TrackSourceRef::StagedFile(PathBuf::from("/stage/02.wav")),
            realized_input: Some(PathBuf::from("/realized/02.wav")),
            output_file: None,
            commands: vec![],
            bytes_in: Some(4096),
            bytes_out: None,
            duration: None,
        }
    }

    fn stage_records() -> Vec<StageRecord> {
        vec![
            StageRecord {
                stage: PipelineStage::Materialize,
                outcome: StageOutcome::Ok,
            },
            StageRecord {
                stage: PipelineStage::ReplayGain,
                outcome: StageOutcome::Skipped,
            },
            StageRecord {
                stage: PipelineStage::Features,
                outcome: StageOutcome::Ok,
            },
        ]
    }

    #[test]
    fn build_conversion_log_complete_contains_required_sections() {
        let source = log_test_source();
        let req = log_test_request();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };
        let log = build_conversion_log(&outcome, &source, &req);

        assert!(log.contains("TONEPOET CONVERSION LOG"));
        assert!(log.contains("Source Information"));
        assert!(log.contains("Conversion Settings"));
        assert!(log.contains("Per-Track Results"));
        assert!(log.contains("Stage Summary"));
        assert!(log.contains("Overall Summary"));
        assert!(log.contains("Job ID: job-1"));
        assert!(log.contains("Item ID: item-1"));
        assert!(log.contains("Catalog number: CAT-123"));
        assert!(log.contains("Target format: FLAC"));
        assert!(log.contains("Result: Complete"));
        assert!(log.contains("Log generated by tonepoet"));
    }

    #[test]
    fn build_conversion_log_partial_shows_successful_and_failed_tracks() {
        let source = log_test_source();
        let req = log_test_request();
        let outcome = AlbumOutcome::Partial {
            successful: vec![ok_record()],
            failed: vec![failed_record()],
            stages: stage_records(),
        };
        let log = build_conversion_log(&outcome, &source, &req);

        assert!(log.contains("Disc 1, Track 1: One"));
        assert!(log.contains("Status: Success"));
        assert!(log.contains("Disc 1, Track 2 (untitled)"));
        assert!(log.contains("Status: Failure"));
        assert!(log.contains("Error: encode failed"));
        assert!(log.contains("Result: Partial (1/2 ok, 1 failed)"));
    }

    #[test]
    fn build_conversion_log_blocked_shows_reason() {
        let source = log_test_source();
        let req = log_test_request();
        let outcome = AlbumOutcome::Blocked {
            successful: vec![],
            failed: vec![failed_record()],
            stages: vec![StageRecord {
                stage: PipelineStage::Convert,
                outcome: StageOutcome::Failed("convert failed".to_string()),
            }],
            reason: BlockReason::RequiredStageFailure(PipelineStage::Convert),
        };
        let log = build_conversion_log(&outcome, &source, &req);

        assert!(log.contains("Result: Blocked (required stage failed: Convert)"));
        assert!(log.contains("Convert: Failed (convert failed)"));
    }

    #[test]
    fn per_track_details_include_sizes_duration_and_command_info() {
        let source = log_test_source();
        let req = log_test_request();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };
        let log = build_conversion_log(&outcome, &source, &req);

        assert!(log.contains("Source audio: 44.1kHz, 24-bit, 44100 expected samples"));
        assert!(log.contains("Size: 2.0 KB -> 1.0 KB (50.0% smaller)"));
        assert!(log.contains("Encode duration: 1m 5s"));
        assert!(log.contains("ffmpeg -i '/tmp/in file.wav' /tmp/out.flac [1m 5s; exit 0]"));
        assert!(!log.contains("ignored stdout"));
        assert!(!log.contains("ignored stderr"));
    }

    #[test]
    fn stage_summary_includes_all_stage_records() {
        let source = log_test_source();
        let req = log_test_request();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };
        let log = build_conversion_log(&outcome, &source, &req);

        assert!(log.contains("Materialize: Ok"));
        assert!(log.contains("ReplayGain: Skipped"));
        assert!(log.contains("Features: Ok"));
    }

    #[test]
    fn log_values_escape_control_characters_without_dropping_text() {
        assert_eq!(
            escape_log_value("line1\nline2\ttail"),
            "line1\\nline2\\ttail"
        );
        let mut record = ok_record();
        record.outcome = TrackOutcome::Err("first line\nsecond line".to_string());
        record.commands[0].sanitized_args = vec!["arg\nnext".to_string()];
        let outcome = AlbumOutcome::Partial {
            successful: vec![],
            failed: vec![record],
            stages: vec![],
        };
        let log = build_conversion_log(&outcome, &log_test_source(), &log_test_request());

        assert!(log.contains("Error: first line\\nsecond line"));
        assert!(log.contains("'arg\\nnext'"));
    }

    #[test]
    fn helper_formatters_have_expected_output() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1_572_864), "1.5 MB");
        assert_eq!(format_duration(Duration::from_millis(250)), "0.250s");
        assert_eq!(format_duration(Duration::from_secs(5)), "5s");
        assert_eq!(format_duration(Duration::from_secs(65)), "1m 5s");
        assert_eq!(compression_ratio(1000, 500), "50.0% smaller");
        assert_eq!(compression_ratio(1000, 1250), "25.0% larger");
        assert_eq!(compression_ratio(1000, 1000), "0.0% change");
        assert_eq!(compression_ratio(0, 1000), "n/a");
    }
}
#[cfg(test)]
mod naming_template_tests {
    use super::*;

    fn template_source() -> PreparedSource {
        let mut album_extra = BTreeMap::new();
        album_extra.insert("catalog".to_string(), "CK-1234".to_string());
        PreparedSource {
            container: PathBuf::from("/tmp/container.7z"),
            kind: SourceKind::SevenZip,
            tracks: vec![PreparedTrack {
                id: TrackId {
                    source_ordinal: 1,
                    disc_number: Some(1),
                    track_number: 1,
                },
                source_ref: TrackSourceRef::StagedFile(PathBuf::from("/stage/01.flac")),
                metadata: TrackMetadata {
                    title: Some("Right Off".to_string()),
                    artist: Some("Miles/Davis".to_string()),
                    genre: Some("Jazz".to_string()),
                    composer: Some("Miles Davis".to_string()),
                    track_number: Some(1),
                    disc_number: Some(1),
                    isrc: Some("USSM17100001".to_string()),
                    extra: BTreeMap::from([("catalognumber".to_string(), "CAT/999".to_string())]),
                    ..TrackMetadata::default()
                },
                expected_samples: Some(1000),
                sample_rate: 44_100,
                bit_depth: Some(24),
            }],
            album_metadata: AlbumMetadata {
                album: Some("A Tribute to Jack Johnson".to_string()),
                album_artist: Some("Miles Davis".to_string()),
                genre: Some("Fusion".to_string()),
                date: Some("March 1971".to_string()),
                total_tracks: 1,
                extra: album_extra,
                ..AlbumMetadata::default()
            },
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SevenZip,
                source_sha256: None,
                tool_versions: BTreeMap::new(),
                extracted_at: chrono::Utc::now(),
            },
        }
    }

    fn template_request(folder_template: Option<String>) -> PipelineRequest {
        PipelineRequest {
            job_id: "job-test".to_string(),
            item_id: "item-test".to_string(),
            container: PathBuf::from("/tmp/container.7z"),
            source: SourceOptions {
                archive_password: None,
                sacd_area: None,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection: TrackSelection::All,
            },
            settings: tonepoet_pipeline::PipelineSettings::default(),
            worker_count: None,
            merge: false,
            output_root: PathBuf::from("/out"),
            naming: NamingPolicy {
                template: "%NN% - %TITLE%".to_string(),
                folder_template,
                per_album_subdir: true,
                collision_policy: NamingCollisionPolicy::Fail,
            },
            publish: PublishPolicy {
                overwrite: OverwritePolicy::FailIfExists,
                same_filesystem_required: false,
                write_manifest: false,
            },
            log: LogPolicy {
                root: PathBuf::from("/out/.tonepoet-logs"),
                write_for_blocked: true,
                write_json_log: false,
            },
            stages: StagePolicy {
                metadata: StageRequirement::Enabled,
                replaygain: StageRequirement::Disabled,
                features: StageRequirement::Enabled,
                generate_cue: false,
            },
            failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
            container_extension: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }

    #[test]
    fn plan_outputs_without_folder_template_keeps_album_name_fallback() {
        let source = template_source();
        let req = template_request(None);
        let plan = plan_outputs(&source, &req).unwrap();
        assert_eq!(
            plan.album_dir,
            PathBuf::from("/out/A Tribute to Jack Johnson")
        );
        assert_eq!(
            plan.entries[0].final_path,
            PathBuf::from("/out/A Tribute to Jack Johnson/01 - Right Off.flac")
        );
    }

    #[test]
    fn plan_outputs_with_folder_template_sets_nested_album_dir_and_track_paths() {
        let source = template_source();
        let req = template_request(Some("%ARTIST%/%ALBUM% (%YEAR%)".to_string()));
        let plan = plan_outputs(&source, &req).unwrap();
        assert_eq!(
            plan.album_dir,
            PathBuf::from("/out/Miles Davis/A Tribute to Jack Johnson (1971)")
        );
        assert_eq!(
            plan.entries[0].final_path,
            PathBuf::from("/out/Miles Davis/A Tribute to Jack Johnson (1971)/01 - Right Off.flac")
        );
    }

    #[test]
    fn render_folder_template_preserves_template_slashes() {
        let source = template_source();
        assert_eq!(
            render_folder_template("%ARTIST%/%ALBUM% (%YEAR%)", &source, &tonepoet_pipeline::AudioFormat::Flac),
            PathBuf::from("Miles Davis/A Tribute to Jack Johnson (1971)")
        );
    }

    #[test]
    fn render_folder_template_keeps_empty_year_parentheses() {
        let mut source = template_source();
        source.album_metadata.date = None;
        assert_eq!(
            render_folder_template("%ARTIST%/%ALBUM% (%YEAR%)", &source, &tonepoet_pipeline::AudioFormat::Flac),
            PathBuf::from("Miles Davis/A Tribute to Jack Johnson ()")
        );
    }

    #[test]
    fn render_folder_template_resolves_custom_album_extra_tokens() {
        let mut source = template_source();
        source
            .album_metadata
            .extra
            .insert("catalognumber".to_string(), "CK/1234".to_string());
        assert_eq!(
            render_folder_template(
                "%ARTIST%/%CATALOGNUMBER%/%ALBUM%",
                &source,
                &tonepoet_pipeline::AudioFormat::Flac
            ),
            PathBuf::from("Miles Davis/CK 1234/A Tribute to Jack Johnson")
        );
    }

    #[test]
    fn render_track_template_expands_new_builtins_and_custom_extras() {
        let source = template_source();
        let path = render_track_template(
            "%NN% - %TITLE% - %YEAR% - %GENRE% - %CATALOG% - %SAMPLERATE% - %BITDEPTH% - %CATALOGNUMBER% - %NONEXISTENT%",
            &source,
            &source.tracks[0],
            &tonepoet_pipeline::AudioFormat::Flac,
        )
        .unwrap();
        assert_eq!(
            path,
            PathBuf::from("01 - Right Off - 1971 - Jazz - CK-1234 - 44.1kHz - 24 - CAT 999 - ")
        );
    }

    #[test]
    fn validate_template_is_structural_only() {
        assert!(validate_template("%UNKNOWN%/%BARCODE%").is_ok());
        assert_eq!(
            validate_template("%UNKNOWN").unwrap_err(),
            "unclosed % token"
        );
        assert_eq!(validate_template("%%").unwrap_err(), "empty token %%");
    }

    #[test]
    fn folder_template_sanitizes_value_slashes_but_preserves_template_slashes() {
        let mut source = template_source();
        source.album_metadata.album_artist = Some("Miles/Davis".to_string());
        assert_eq!(
            render_folder_template("%ARTIST%/%ALBUM%", &source, &tonepoet_pipeline::AudioFormat::Flac),
            PathBuf::from("Miles Davis/A Tribute to Jack Johnson")
        );
    }

    #[test]
    fn resolver_enrichment_feeds_folder_template_extra_tokens() {
        let mut source = template_source();
        source
            .album_metadata
            .extra
            .insert("catalog".to_string(), "UCCQ-1234".to_string());
        source
            .album_metadata
            .extra
            .insert("media".to_string(), "CD".to_string());

        super::super::label_resolver::enrich_with_label_info(
            &mut source.album_metadata,
            Path::new("Miles Davis - Album.7z"),
            super::super::label_resolver::dictionary_label_resolver(),
        );

        assert_eq!(
            render_folder_template("%COUNTRY%/%ARTIST%/%ALBUM%", &source, &tonepoet_pipeline::AudioFormat::Flac),
            PathBuf::from("Japan/Miles Davis/A Tribute to Jack Johnson")
        );
    }

    #[test]
    fn resolver_enrichment_feeds_pressing_token_without_overwriting_label() {
        let mut source = template_source();
        source
            .album_metadata
            .extra
            .insert("label".to_string(), "MoFi".to_string());
        source
            .album_metadata
            .extra
            .insert("media".to_string(), "CD".to_string());

        super::super::label_resolver::enrich_with_label_info(
            &mut source.album_metadata,
            Path::new("Miles Davis - Album.7z"),
            super::super::label_resolver::dictionary_label_resolver(),
        );

        assert_eq!(
            source.album_metadata.extra.get("label").map(String::as_str),
            Some("MoFi")
        );
        assert_eq!(
            source
                .album_metadata
                .extra
                .get("pressing")
                .map(String::as_str),
            Some("MFSL")
        );
        assert_eq!(
            render_folder_template("%PRESSING%/%ARTIST%/%ALBUM%", &source, &tonepoet_pipeline::AudioFormat::Flac),
            PathBuf::from("MFSL/Miles Davis/A Tribute to Jack Johnson")
        );
    }

    #[test]
    fn folder_template_canonicalizes_artist_casing() {
        let mut source = template_source();
        source.album_metadata.album_artist = Some("miles davis".to_string());
        assert_eq!(
            render_folder_template("%ARTIST%/%ALBUM%", &source, &tonepoet_pipeline::AudioFormat::Flac),
            PathBuf::from("Miles Davis/A Tribute to Jack Johnson")
        );
    }

    #[test]
    fn track_template_canonicalizes_track_artist_casing() {
        let mut source = template_source();
        source.album_metadata.album_artist = None;
        source.tracks[0].metadata.artist = Some("bill evans trio".to_string());
        let path = render_track_template(
            "%ARTIST% - %TITLE%",
            &source,
            &source.tracks[0],
            &tonepoet_pipeline::AudioFormat::Flac,
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("Bill Evans Trio - Right Off"));
    }

    #[test]
    fn track_template_canonicalizes_album_artist_casing() {
        let mut source = template_source();
        source.album_metadata.album_artist = Some("miles davis".to_string());
        let path = render_track_template(
            "%ALBUM_ARTIST% - %TITLE%",
            &source,
            &source.tracks[0],
            &tonepoet_pipeline::AudioFormat::Flac,
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("Miles Davis - Right Off"));
    }

    #[test]
    fn pipeline_enrichment_before_output_planning_produces_folder_name() {
        let mut source = template_source();
        source.album_metadata.album_artist = Some("miles davis".to_string());
        source.album_metadata.extra.clear();
        source
            .album_metadata
            .extra
            .insert("media".to_string(), "LP".to_string());

        let mut req = template_request(Some("%COUNTRY%/%PRESSING%/%ARTIST%/%ALBUM%".to_string()));
        req.container = PathBuf::from("/tmp/The Beatles - Let It Be QRP.7z");

        enrich_source_with_label_info(&mut source, &req.container, &req);
        let plan = plan_outputs(&source, &req).unwrap();

        assert_eq!(
            source
                .album_metadata
                .extra
                .get("country")
                .map(String::as_str),
            Some("US")
        );
        assert_eq!(
            source
                .album_metadata
                .extra
                .get("pressing")
                .map(String::as_str),
            Some("US QRP Press LP")
        );
        assert_eq!(
            plan.album_dir,
            PathBuf::from("/out/US/US QRP Press LP/Miles Davis/A Tribute to Jack Johnson")
        );
        assert_eq!(
            plan.entries[0].final_path,
            PathBuf::from(
                "/out/US/US QRP Press LP/Miles Davis/A Tribute to Jack Johnson/01 - Right Off.flac"
            )
        );
    }

    #[test]
    fn sample_rate_formatter_handles_pcm_and_dsd() {
        assert_eq!(format_sample_rate(44_100), "44.1kHz");
        assert_eq!(format_sample_rate(48_000), "48kHz");
        assert_eq!(format_sample_rate(96_000), "96kHz");
        assert_eq!(format_sample_rate(2_822_400), "DSD64");
        assert_eq!(format_sample_rate(5_644_800), "DSD128");
    }
}

#[cfg(test)]
mod title_extra_tests {
    use super::*;

    #[test]
    fn strips_catalog_prefix_parenthetical() {
        let (album, extra) =
            extract_title_extra("A Tribute to Jack Johnson (SME JSACD SRGS-4504) [ISO]").unwrap();
        assert_eq!(album, "A Tribute to Jack Johnson");
        assert_eq!(extra, "SME JSACD SRGS-4504");
    }

    #[test]
    fn strips_shm_sacd_parenthetical() {
        let (album, extra) = extract_title_extra("Aja (Japan / SHM SACD ISO)").unwrap();
        assert_eq!(album, "Aja");
        assert_eq!(extra, "Japan / SHM SACD ISO");
    }

    #[test]
    fn strips_sacd_version_parenthetical() {
        let (album, extra) = extract_title_extra("All I Got (SACD 2.0)").unwrap();
        assert_eq!(album, "All I Got");
        assert_eq!(extra, "SACD 2.0");
    }

    #[test]
    fn strips_label_sacd_parenthetical() {
        let (album, extra) =
            extract_title_extra("Amused to Death (Analogue Productions SACD) [ISO]").unwrap();
        assert_eq!(album, "Amused to Death");
        assert_eq!(extra, "Analogue Productions SACD");
    }

    #[test]
    fn strips_mfsl_parenthetical() {
        let (album, extra) =
            extract_title_extra("Dark Side of the Moon (MFSL LP / 24-96)").unwrap();
        assert_eq!(album, "Dark Side of the Moon");
        assert_eq!(extra, "MFSL LP / 24-96");
    }

    #[test]
    fn preserves_live_at_parenthetical() {
        assert!(extract_title_extra("Live at the Apollo").is_none());
    }

    #[test]
    fn preserves_alternate_take_parenthetical() {
        assert!(extract_title_extra("All The Things You Are (alternate take)").is_none());
    }

    #[test]
    fn preserves_mono_parenthetical() {
        assert!(extract_title_extra("A Legal Matter (Mono)").is_none());
    }

    #[test]
    fn preserves_country_only_parenthetical() {
        assert!(extract_title_extra("Aftermath (US)").is_none());
    }

    #[test]
    fn preserves_leading_parenthetical() {
        assert!(extract_title_extra("(Ain't That) Good News").is_none());
    }

    #[test]
    fn strips_last_parenthetical_only() {
        let (album, extra) = extract_title_extra("Aftermath (US) (ABKCO Hybrid SACD ISO)").unwrap();
        assert_eq!(album, "Aftermath (US)");
        assert_eq!(extra, "ABKCO Hybrid SACD ISO");
    }

    #[test]
    fn no_parenthetical_returns_none() {
        assert!(extract_title_extra("Dark Side of the Moon").is_none());
    }

    #[test]
    fn folder_template_without_title_extra_preserves_album() {
        let mut source = template_source();
        source.album_metadata.album =
            Some("A Tribute to Jack Johnson (SME JSACD SRGS-4504) [ISO]".to_string());
        let result = render_folder_template("%ARTIST% - %ALBUM%", &source, &tonepoet_pipeline::AudioFormat::Flac);
        // Without %TITLE_EXTRA%, album is unchanged (just sanitized)
        assert!(result
            .to_string_lossy()
            .contains("A Tribute to Jack Johnson (SME JSACD SRGS-4504)"));
    }

    #[test]
    fn folder_template_with_title_extra_strips_album() {
        let mut source = template_source();
        source.album_metadata.album =
            Some("A Tribute to Jack Johnson (SME JSACD SRGS-4504) [ISO]".to_string());
        let result = render_folder_template(
            "%ARTIST% - %ALBUM% {%TITLE_EXTRA%}",
            &source,
            &tonepoet_pipeline::AudioFormat::Flac,
        );
        let s = result.to_string_lossy();
        assert!(
            s.contains("A Tribute to Jack Johnson"),
            "album should be clean: {s}"
        );
        assert!(
            s.contains("SME JSACD SRGS-4504"),
            "extra should contain catalog: {s}"
        );
        assert!(!s.contains("[ISO]"), "bracket should be stripped: {s}");
    }

    #[test]
    fn track_template_with_title_extra_strips_album() {
        let mut source = template_source();
        source.album_metadata.album = Some("Dark Side of the Moon (MFSL SACD) [ISO]".to_string());
        source.tracks[0].metadata.title = Some("Time".to_string());
        let result = render_track_template(
            "%NN% - %TITLE% [%ALBUM%] {%TITLE_EXTRA%}",
            &source,
            &source.tracks[0],
            &tonepoet_pipeline::AudioFormat::Flac,
        )
        .unwrap();
        let s = result.to_string_lossy();
        assert!(s.contains("Dark Side of the Moon"), "album stripped: {s}");
        assert!(s.contains("MFSL SACD"), "extra populated: {s}");
        assert!(!s.contains("[ISO]"), "bracket stripped: {s}");
    }

    fn template_source() -> PreparedSource {
        PreparedSource {
            container: PathBuf::from("/tmp/test.iso"),
            kind: SourceKind::SacdIso,
            tracks: vec![PreparedTrack {
                id: TrackId {
                    source_ordinal: 1,
                    track_number: 1,
                    disc_number: None,
                },
                source_ref: TrackSourceRef::StagedFile(PathBuf::from("/tmp/track.dsf")),
                metadata: TrackMetadata {
                    artist: Some("Miles Davis".to_string()),
                    ..Default::default()
                },
                expected_samples: None,
                sample_rate: 2_822_400,
                bit_depth: None,
            }],
            album_metadata: AlbumMetadata {
                album: Some("A Tribute to Jack Johnson".to_string()),
                album_artist: Some("Miles Davis".to_string()),
                ..Default::default()
            },
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SacdIso,
                source_sha256: None,
                tool_versions: BTreeMap::new(),
                extracted_at: chrono::Utc::now(),
            },
        }
    }
}

#[cfg(test)]
mod rich_progress_stage_tests {
    use super::*;

    #[test]
    fn item_count_progress_is_used_when_sample_metadata_is_missing() {
        assert_eq!(convert_progress_fraction(0, None, 0, 4), 0.0);
        assert_eq!(convert_progress_fraction(0, None, 2, 4), 0.5);
        assert_eq!(convert_progress_fraction(0, None, 4, 4), 1.0);
    }

    #[test]
    fn sample_weighted_progress_prefers_samples_when_available() {
        assert_eq!(convert_progress_fraction(250, Some(1_000), 1, 2), 0.25);
        assert_eq!(convert_progress_fraction(750, Some(1_000), 1, 2), 0.75);
    }
}

// CHUNK_2_1_3_FAILURE_CANCELLATION_TESTS_BEGIN
#[cfg(test)]
mod chunk_2_1_3_postprocessing_gate_and_phase_tests {
    use super::*;
    use crate::convert::pipeline::manifest::{manifest_path, read_manifest, ValidationStatus};
    use crate::convert::pipeline::reporter::{PipelineEvent, PipelineReporter, RecordingReporter};
    use crate::convert::pipeline::tool::blocking_test_runner::{
        tool_gate, BlockingToolRunner, ToolBehavior,
    };
    use crate::convert::pipeline::tool::{CommandRecord, ProcessExit, StubToolRunner, ToolBinary, ToolOutput};
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Mutex;
    use tempfile::TempDir;
    use async_trait::async_trait;
    use tonepoet_pipeline::PipelineSettings;

    struct AlbumFixture {
        _temp: TempDir,
        album: ScheduledAlbum,
        track_ids: Vec<TrackId>,
        staged_paths: Vec<PathBuf>,
        final_paths: Vec<PathBuf>,
        album_dir: PathBuf,
        log_root: PathBuf,
    }

    fn track_id(source_ordinal: u32) -> TrackId {
        TrackId {
            source_ordinal,
            disc_number: None,
            track_number: source_ordinal + 1,
        }
    }

    fn request(
        root: &Path,
        policy: FailurePolicy,
        stages: StagePolicy,
        overwrite: OverwritePolicy,
    ) -> PipelineRequest {
        PipelineRequest {
            job_id: "job-2-1-3".to_string(),
            item_id: format!("item-{:?}", policy),
            container: root.join("input.flac"),
            source: SourceOptions {
                archive_password: None,
                sacd_area: None,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection: TrackSelection::All,
            },
            settings: PipelineSettings::default(),
            worker_count: Some(2),
            merge: false,
            output_root: root.join("out"),
            naming: NamingPolicy {
                template: "%NN% - %TITLE%".to_string(),
                folder_template: None,
                per_album_subdir: true,
                collision_policy: NamingCollisionPolicy::Fail,
            },
            publish: PublishPolicy {
                overwrite,
                same_filesystem_required: false,
                write_manifest: true,
            },
            log: LogPolicy {
                root: root.join("logs"),
                write_for_blocked: true,
                write_json_log: true,
            },
            stages,
            failure_policy: policy,
            container_extension: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }

    fn stage_policy(metadata: bool, replaygain: bool, features: bool) -> StagePolicy {
        StagePolicy {
            metadata: if metadata { StageRequirement::Enabled } else { StageRequirement::Disabled },
            replaygain: if replaygain { StageRequirement::Enabled } else { StageRequirement::Disabled },
            features: if features { StageRequirement::Enabled } else { StageRequirement::Disabled },
            generate_cue: features,
        }
    }

    fn prepared_source(root: &Path, track_ids: &[TrackId]) -> PreparedSource {
        let tracks = track_ids
            .iter()
            .map(|id| PreparedTrack {
                id: id.clone(),
                source_ref: TrackSourceRef::StagedFile(root.join(format!("source-{}.flac", id.source_ordinal))),
                metadata: TrackMetadata {
                    title: Some(format!("Track {}", id.track_number)),
                    track_number: Some(id.track_number),
                    ..TrackMetadata::default()
                },
                expected_samples: Some(44_100),
                sample_rate: 44_100,
                bit_depth: Some(16),
            })
            .collect::<Vec<_>>();

        PreparedSource {
            container: root.join("input.flac"),
            kind: SourceKind::SingleFile,
            tracks,
            album_metadata: AlbumMetadata {
                album: Some("Gate Test".to_string()),
                total_tracks: track_ids.len() as u32,
                ..AlbumMetadata::default()
            },
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SingleFile,
                source_sha256: None,
                tool_versions: BTreeMap::new(),
                extracted_at: chrono::Utc::now(),
            },
        }
    }

    fn track_record(id: TrackId, ok: bool, output_file: Option<PathBuf>) -> TrackRecord {
        TrackRecord {
            track_id: id.clone(),
            outcome: if ok {
                TrackOutcome::Ok
            } else {
                TrackOutcome::Err("encoder failed".to_string())
            },
            source_ref: TrackSourceRef::StagedFile(PathBuf::from(format!("source-{}.flac", id.source_ordinal))),
            realized_input: None,
            output_file,
            commands: Vec::new(),
            bytes_in: None,
            bytes_out: None,
            duration: None,
        }
    }

    fn fixture(
        policy: FailurePolicy,
        track_count: usize,
        stages: StagePolicy,
        overwrite: OverwritePolicy,
    ) -> AlbumFixture {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        std::fs::create_dir_all(root.join("out")).expect("output root");
        std::fs::create_dir_all(root.join("logs")).expect("log root");
        std::fs::write(root.join("input.flac"), b"container").expect("container");

        let req = request(root, policy, stages, overwrite);
        let staging_parent = root.join(".tonepoet-staging");
        let run_lock = acquire_run_lock(&staging_parent, &req.job_id, &req.item_id).expect("run lock");
        let staging = StagingDir::new(staging_parent.join("job-2-1-3"), req.job_id.clone());
        let track_ids = (0..track_count).map(|index| track_id(index as u32)).collect::<Vec<_>>();
        let source = prepared_source(root, &track_ids);
        // Create source files so manifest builder can stat them.
        for id in &track_ids {
            std::fs::write(root.join(format!("source-{}.flac", id.source_ordinal)), b"source").expect("source file");
        }
        let album_dir = root.join("out").join("Gate Test");
        let mut staged_paths = Vec::new();
        let mut final_paths = Vec::new();
        let mut entries = Vec::new();
        for (index, id) in track_ids.iter().enumerate() {
            let final_path = album_dir.join(format!("{:02}.flac", index + 1));
            let staged_path = staging.root.join("converted").join(format!("{:02}.flac", index + 1));
            std::fs::create_dir_all(staged_path.parent().expect("stage parent")).expect("stage parent");
            std::fs::write(&staged_path, format!("encoded-{index}")).expect("staged success");
            entries.push(PlannedTrackOutput {
                track_id: id.clone(),
                final_path: final_path.clone(),
            });
            staged_paths.push(staged_path);
            final_paths.push(final_path);
        }
        let log_root = req.log.root.clone();
        let plan = AlbumPlan {
            album_dir: album_dir.clone(),
            entries,
        };

        AlbumFixture {
            _temp: temp,
            album: ScheduledAlbum {
                req,
                item_id: "item".to_string(),
                staging,
                source,
                plan,
                stages: Vec::new(),
                _run_lock: run_lock,
            },
            track_ids,
            staged_paths,
            final_paths,
            album_dir,
            log_root,
        }
    }

    fn successful_output(fixture: &AlbumFixture, index: usize) -> ScheduledTrackOutput {
        let id = fixture.track_ids[index].clone();
        ScheduledTrackOutput {
            index,
            record: track_record(id.clone(), true, Some(fixture.staged_paths[index].clone())),
            artifact: Some(TrackArtifact {
                track_id: id,
                staged_path: fixture.staged_paths[index].clone(),
                final_path: fixture.final_paths[index].clone(),
                samples: Some(44_100),
                metadata_satisfaction: PlannedMetadataSatisfaction::none(),
                metadata_required: PlannedMetadataSatisfaction::none(),
                planned_command_hash: None,
            }),
            ok: true,
            metadata_satisfaction: PlannedMetadataSatisfaction::none(),
        }
    }

    fn failed_output(fixture: &AlbumFixture, index: usize) -> ScheduledTrackOutput {
        ScheduledTrackOutput {
            index,
            record: track_record(fixture.track_ids[index].clone(), false, None),
            artifact: None,
            ok: false,
            metadata_satisfaction: PlannedMetadataSatisfaction::none(),
        }
    }

    fn stage_outcome(report: &PipelineReport, stage: PipelineStage) -> Option<&StageOutcome> {
        match &report.outcome {
            AlbumOutcome::Complete { stages, .. }
            | AlbumOutcome::Partial { stages, .. }
            | AlbumOutcome::Blocked { stages, .. } => {
                stages.iter().find(|record| record.stage == stage).map(|record| &record.outcome)
            }
        }
    }

    fn set_tag_values(args: &[String]) -> BTreeMap<String, String> {
        args.iter()
            .filter_map(|arg| {
                let payload = arg.strip_prefix("--set-tag=")?;
                let (key, value) = payload.split_once('=')?;
                Some((key.to_string(), value.to_string()))
            })
            .collect()
    }

    fn has_remove_tag(args: &[String], key: &str) -> bool {
        args.iter().any(|arg| arg == &format!("--remove-tag={key}"))
    }

    #[test]
    fn sacd_authoritative_metadata_prevents_md5_only_planner_skip() {
        let mut fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            1,
            stage_policy(true, false, false),
            OverwritePolicy::FailIfExists,
        );
        fixture.album.req.settings.metadata.transfer_tags = true;
        fixture.album.req.settings.metadata.preserve_artwork = true;
        fixture.album.req.settings.metadata.store_source_audio_md5 = true;
        fixture.album.source.kind = SourceKind::SacdIso;
        fixture.album.source.provenance.source_kind = SourceKind::SacdIso;
        fixture.album.source.tracks[0].source_ref = TrackSourceRef::SacdTrack {
            iso: fixture.album.req.container.clone(),
            track_index: 1,
            area: SacdArea::Stereo,
        };

        let required = metadata_obligations_for_request(&fixture.album.req, &fixture.album.source);
        assert!(!required.source_tags_transferred, "SACD source tags are supplied by sidecar/TOC metadata, not the generated DSF carrier");
        assert!(!required.artwork_transferred, "SACD artwork_transferred preservation is unsupported and must not be counted as a satisfiable obligation");
        assert!(!required.source_audio_md5_written, "SACD source-audio MD5 is unsupported because materialized DSF/DFF carriers have no FLAC STREAMINFO MD5");
        assert!(required.authoritative_tags_applied);

        let mut scheduled = successful_output(&fixture, 0);
        let mut artifact = scheduled.artifact.take().expect("successful artifact");
        artifact.metadata_satisfaction = PlannedMetadataSatisfaction {
            source_audio_md5_written: true,
            ..PlannedMetadataSatisfaction::none()
        };
        let artifacts = ArtifactSet {
            audio: AudioArtifacts::Tracks(vec![artifact]),
            sidecars: Vec::new(),
        };

        assert!(
            !planner_metadata_already_satisfied(&artifacts, &fixture.album.source, &fixture.album.req),
            "MD5-only satisfaction must not skip authoritative SACD metadata application"
        );
    }


    #[tokio::test]
    async fn sacd_md5_satisfaction_still_runs_metaflac_and_writes_sidecar_tags() {
        let mut fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            1,
            stage_policy(true, false, false),
            OverwritePolicy::FailIfExists,
        );
        fixture.album.req.settings.metadata.transfer_tags = true;
        fixture.album.req.settings.metadata.preserve_artwork = true;
        fixture.album.req.settings.metadata.store_source_audio_md5 = true;
        fixture.album.source.kind = SourceKind::SacdIso;
        fixture.album.source.provenance.source_kind = SourceKind::SacdIso;
        fixture.album.source.album_metadata = AlbumMetadata {
            album: Some("Rollins Plays for Bird (Analogue Productions SACD ISO)".to_string()),
            album_artist: Some("Sonny Rollins".to_string()),
            genre: Some("Jazz".to_string()),
            date: Some("1957".to_string()),
            total_tracks: 3,
            ..AlbumMetadata::default()
        };
        fixture.album.source.tracks[0].source_ref = TrackSourceRef::SacdTrack {
            iso: fixture.album.req.container.clone(),
            track_index: 1,
            area: SacdArea::Stereo,
        };
        fixture.album.source.tracks[0].metadata = TrackMetadata {
            title: Some("Medley: I Remember You...".to_string()),
            artist: Some("Sonny Rollins".to_string()),
            genre: Some("Jazz".to_string()),
            date: Some("1957".to_string()),
            track_number: Some(1),
            performer: Some("Sonny Rollins".to_string()),
            ..TrackMetadata::default()
        };

        let mut output = successful_output(&fixture, 0);
        output.metadata_satisfaction = PlannedMetadataSatisfaction {
            source_audio_md5_written: true,
            ..PlannedMetadataSatisfaction::none()
        };
        if let Some(artifact) = output.artifact.as_mut() {
            artifact.metadata_satisfaction = output.metadata_satisfaction;
        }
        let artifacts = ArtifactSet {
            audio: AudioArtifacts::Tracks(vec![output.artifact.as_ref().expect("artifact").clone()]),
            sidecars: Vec::new(),
        };
        assert!(
            !planner_metadata_already_satisfied(&artifacts, &fixture.album.source, &fixture.album.req),
            "source-audio MD5 satisfaction must not suppress SACD sidecar/TOC metadata writing"
        );

        let runner = BlockingToolRunner::with_behaviors([ToolBehavior::Succeed]);
        let reporter = RecordingReporter::new();
        let cancel = CancellationToken::new();
        let report = finish_pipeline_album_for_scheduler(
            fixture.album,
            vec![output],
            &runner,
            &reporter,
            &cancel,
        )
        .await;

        assert!(matches!(report.outcome, AlbumOutcome::Complete { .. }));
        assert!(matches!(stage_outcome(&report, PipelineStage::Metadata), Some(StageOutcome::Ok)));
        let transcript = runner.transcript();
        assert_eq!(transcript.len(), 1, "metadata stage emits exactly one tag-writing command");
        assert_eq!(transcript[0].binary, ToolBinary::Metaflac);
        let args = &transcript[0].sanitized_args;
        let tags = set_tag_values(args);
        for (key, value) in [
            ("TITLE", "Medley: I Remember You..."),
            ("ARTIST", "Sonny Rollins"),
            ("ALBUM", "Rollins Plays for Bird (Analogue Productions SACD ISO)"),
            ("DATE", "1957"),
            ("GENRE", "Jazz"),
            ("TRACKNUMBER", "1"),
            ("TOTALTRACKS", "3"),
            ("PERFORMER", "Sonny Rollins"),
        ] {
            assert!(has_remove_tag(args, key), "metaflac removes stale {key} before setting it");
            assert_eq!(tags.get(key).map(String::as_str), Some(value), "required tag {key} is written");
        }
    }


    #[test]
    fn no_real_metadata_obligations_skip_metadata_stage() {
        let fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            1,
            stage_policy(true, false, false),
            OverwritePolicy::FailIfExists,
        );

        let scheduled = successful_output(&fixture, 0);
        let artifact = scheduled.artifact.expect("successful artifact");
        assert_eq!(artifact.metadata_required, PlannedMetadataSatisfaction::none());
        let artifacts = ArtifactSet {
            audio: AudioArtifacts::Tracks(vec![artifact]),
            sidecars: Vec::new(),
        };

        assert!(
            planner_metadata_already_satisfied(&artifacts, &fixture.album.source, &fixture.album.req),
            "when source-MD5 is unavailable and no authoritative/source metadata obligation remains, apply_metadata() has no work to do"
        );
    }

    #[test]
    fn single_file_exact_planner_metadata_satisfaction_can_skip() {
        let mut fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            1,
            stage_policy(true, false, false),
            OverwritePolicy::FailIfExists,
        );
        fixture.album.req.settings.metadata.transfer_tags = true;
        fixture.album.req.settings.metadata.preserve_artwork = true;
        fixture.album.req.settings.metadata.store_source_audio_md5 = true;

        let mut scheduled = successful_output(&fixture, 0);
        let mut artifact = scheduled.artifact.take().expect("successful artifact");
        artifact.metadata_required = PlannedMetadataSatisfaction {
            source_tags_transferred: true,
            artwork_transferred: true,
            source_audio_md5_written: true,
            authoritative_tags_applied: false,
        };
        artifact.metadata_satisfaction = artifact.metadata_required;
        let artifacts = ArtifactSet {
            audio: AudioArtifacts::Tracks(vec![artifact]),
            sidecars: Vec::new(),
        };

        assert!(
            planner_metadata_already_satisfied(&artifacts, &fixture.album.source, &fixture.album.req),
            "single-file source tag/artwork_transferred/MD5 satisfaction remains skippable"
        );
    }


    fn phase_request(root: &Path, container_name: &str) -> PipelineRequest {
        let container = root.join(container_name);
        std::fs::write(&container, b"container").expect("container");
        std::fs::create_dir_all(root.join("out")).expect("output root");
        std::fs::create_dir_all(root.join("logs")).expect("log root");
        let mut req = request(
            root,
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            stage_policy(false, false, false),
            OverwritePolicy::FailIfExists,
        );
        req.container = container;
        req.item_id = format!("phase-{}", container_name.replace('.', "-"));
        req
    }

    fn tree_has_entries(path: &Path) -> bool {
        let Ok(mut entries) = std::fs::read_dir(path) else {
            return false;
        };
        entries.next().is_some()
    }

    fn staging_root_for_request(req: &PipelineRequest) -> PathBuf {
        staging_parent_for(req).join(format!(
            "{}-{}",
            sanitize_component(&req.job_id),
            sanitize_component(&req.item_id)
        ))
    }

    struct CancellingReporter {
        cancel: CancellationToken,
        cancel_after_stage: PipelineStage,
        events: Mutex<Vec<PipelineEvent>>,
    }

    impl CancellingReporter {
        fn new(cancel: CancellationToken, cancel_after_stage: PipelineStage) -> Self {
            Self {
                cancel,
                cancel_after_stage,
                events: Mutex::new(Vec::new()),
            }
        }

        fn events(&self) -> Vec<PipelineEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl PipelineReporter for CancellingReporter {
        async fn emit(&self, event: PipelineEvent) {
            if matches!(
                &event,
                PipelineEvent::StageFinished { record, .. }
                    if record.stage == self.cancel_after_stage && matches!(record.outcome, StageOutcome::Ok)
            ) {
                self.cancel.cancel();
            }
            self.events.lock().unwrap().push(event);
        }
    }


    #[tokio::test]
    async fn realized_image_segment_waits_on_ffmpeg_family_limit_before_runner() {
        let temp = tempfile::tempdir().expect("temp dir");
        let req = phase_request(temp.path(), "input.flac");
        let image = temp.path().join("image.flac");
        std::fs::write(&image, b"fake image").expect("image file");
        let staging = StagingDir::new(temp.path().join("staging"), req.job_id.clone());
        std::fs::create_dir_all(&staging.root).expect("staging root");
        let limits = std::sync::Arc::new(ToolConcurrencyLimits::new(4, 1, 4, 4));
        let held_ffmpeg = limits.hold_ffmpeg_permit_for_test().await;
        assert_eq!(limits.ffmpeg_available_permits_for_test(), 0);

        let src = TrackSourceRef::ImageSegment {
            image,
            start_sample: 0,
            samples: 44_100,
        };
        let (gate, blocker) = tool_gate();
        let runner = std::sync::Arc::new(BlockingToolRunner::with_behaviors([
            ToolBehavior::BlockThenSucceed(blocker),
        ]));
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let run_runner = runner.clone();
        let run_limits = limits.clone();
        let handle = tokio::spawn(async move {
            realize_track_with_tool_limits(
                &src,
                &req,
                &staging,
                run_runner.as_ref(),
                &run_cancel,
                Some(run_limits),
                None,
            )
            .await
        });

        let not_started = tokio::time::timeout(std::time::Duration::from_millis(25), gate.wait_started()).await;
        assert!(
            not_started.is_err(),
            "realized image-segment FFmpeg work must wait on the shared FFmpeg-family semaphore before runner.run starts"
        );
        assert!(runner.transcript().is_empty());

        cancel.cancel();
        let err = tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("blocked realization wakes after cancellation")
            .expect("realization task joins")
            .expect_err("cancelled semaphore wait aborts realization");
        assert!(err.to_string().contains("cancelled"));
        drop(held_ffmpeg);
    }

    #[tokio::test]
    async fn cancellation_before_materialization_enters_no_phase_and_leaves_no_staging() {
        let temp = tempfile::tempdir().expect("temp dir");
        let req = phase_request(temp.path(), "input.flac");
        let staging_parent = staging_parent_for(&req);
        let staging_root = staging_root_for_request(&req);
        let runner = BlockingToolRunner::new();
        let reporter = RecordingReporter::new();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let materialized = prepare_pipeline_item_for_scheduler(
            req,
            &runner,
            &reporter,
            &cancel,
            &HashMap::new(),
        )
        .await;

        let report = match materialized {
            ScheduledMaterialization::Finished(report) => report,
            ScheduledMaterialization::Ready(_) => panic!("pre-cancelled item must not become schedulable"),
        };
        assert!(matches!(
            report.outcome,
            AlbumOutcome::Blocked { reason: BlockReason::Cancelled, .. }
        ));
        assert!(report.source.is_none());
        assert!(report.plan.is_none());
        assert!(report.artifacts.is_none());
        assert!(report.published.is_none());
        assert!(runner.transcript().is_empty());
        assert!(!staging_root.exists());
        assert!(!tree_has_entries(&staging_parent), "no staging files are created before materialization starts");
    }

    #[tokio::test]
    async fn cancellation_during_archive_materialization_cancels_runner_and_drops_staging() {
        let temp = tempfile::tempdir().expect("temp dir");
        let req = phase_request(temp.path(), "input.7z");
        let staging_root = staging_root_for_request(&req);
        let (gate, blocker) = tool_gate();
        let runner = std::sync::Arc::new(BlockingToolRunner::with_behaviors([
            ToolBehavior::BlockThenSucceed(blocker),
        ]));
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let run_runner = runner.clone();
        let handle = tokio::spawn(async move {
            let reporter = RecordingReporter::new();
            prepare_pipeline_item_for_scheduler(
                req,
                run_runner.as_ref(),
                &reporter,
                &run_cancel,
                &HashMap::new(),
            )
            .await
        });

        let release = gate.wait_started().await;
        cancel.cancel();
        let materialized = handle.await.expect("materialization task joins");
        drop(release);

        let report = match materialized {
            ScheduledMaterialization::Finished(report) => report,
            ScheduledMaterialization::Ready(_) => panic!("cancelled materialization must not become schedulable"),
        };
        assert!(matches!(
            report.outcome,
            AlbumOutcome::Blocked { reason: BlockReason::Cancelled, .. }
        ));
        let transcript = runner.transcript();
        assert_eq!(transcript.len(), 1, "archive materializer started one child command");
        assert_eq!(transcript[0].exit, None, "blocked child was cancelled rather than completed");
        assert!(report.source.is_none());
        assert!(report.plan.is_none());
        assert!(report.published.is_none());
        assert!(!staging_root.exists(), "staging root is dropped after materialization cancellation");
    }

    #[tokio::test]
    async fn cancellation_after_materialization_before_planning_uses_real_prepare_boundary() {
        let temp = tempfile::tempdir().expect("temp dir");
        let req = phase_request(temp.path(), "input.flac");
        let staging_root = staging_root_for_request(&req);
        let runner = StubToolRunner::new();
        // Push a valid ffprobe response so the single-file materializer succeeds.
        runner.push_output(ToolOutput {
            exit: ProcessExit::Code(0),
            stdout_tail: r#"{"streams":[{"sample_rate":"44100","duration":"300.0","bits_per_raw_sample":"16"}],"format":{"duration":"300.0"}}"#.to_string(),
            stderr_tail: String::new(),
            elapsed: Duration::from_millis(10),
            command: CommandRecord {
                binary: ToolBinary::Ffprobe,
                sanitized_args: vec!["input.flac".to_string()],
                cwd: None,
                env_keys: vec![],
                exit: Some(ProcessExit::Code(0)),
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                elapsed: Duration::from_millis(10),
            },
        });
        let cancel = CancellationToken::new();
        let reporter = CancellingReporter::new(cancel.clone(), PipelineStage::Materialize);

        let materialized = prepare_pipeline_item_for_scheduler(
            req,
            &runner,
            &reporter,
            &cancel,
            &HashMap::new(),
        )
        .await;

        let report = match materialized {
            ScheduledMaterialization::Finished(report) => report,
            ScheduledMaterialization::Ready(_) => panic!("post-materialization cancellation must not reach planning readiness"),
        };
        assert!(matches!(
            report.outcome,
            AlbumOutcome::Blocked { reason: BlockReason::Cancelled, .. }
        ));
        assert!(report.source.is_some(), "materialized source is retained in the report");
        assert!(report.plan.is_none(), "planning did not run after materialization-triggered cancellation");
        assert!(report.artifacts.is_none());
        assert!(report.published.is_none());
        assert!(reporter.events().iter().any(|event| matches!(
            event,
            PipelineEvent::StageFinished { record, .. }
                if record.stage == PipelineStage::Materialize && matches!(record.outcome, StageOutcome::Ok)
        )));
        assert!(!reporter.events().iter().any(|event| matches!(
            event,
            PipelineEvent::StageStarted { stage: PipelineStage::PlanOutputs, .. }
        )));
        assert!(!staging_root.exists(), "owned staging dir is dropped when prepare exits cancelled");
    }

    #[tokio::test]
    async fn all_tracks_ok_runs_metadata_replaygain_features_and_publish() {
        let fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            1,
            stage_policy(true, true, true),
            OverwritePolicy::FailIfExists,
        );
        let runner = BlockingToolRunner::with_behaviors([
            ToolBehavior::Succeed,
            ToolBehavior::Succeed,
        ]);
        let reporter = RecordingReporter::new();
        let cancel = CancellationToken::new();
        let outputs = vec![successful_output(&fixture, 0)];

        let report = finish_pipeline_album_for_scheduler(
            fixture.album,
            outputs,
            &runner,
            &reporter,
            &cancel,
        )
        .await;

        assert!(matches!(report.outcome, AlbumOutcome::Complete { .. }));
        // SingleFile FLAC→FLAC: planner transfers source tags via ffmpeg, so
        // the orchestrator metadata stage is correctly skipped (no authoritative
        // materializer metadata to apply).
        assert!(matches!(stage_outcome(&report, PipelineStage::Metadata), Some(StageOutcome::Skipped)));
        assert!(matches!(stage_outcome(&report, PipelineStage::ReplayGain), Some(StageOutcome::Ok)));
        assert!(matches!(stage_outcome(&report, PipelineStage::Features), Some(StageOutcome::Ok)));
        assert!(matches!(stage_outcome(&report, PipelineStage::Publish), Some(StageOutcome::Ok)));
        let transcript = runner.transcript();
        assert_eq!(transcript.len(), 1);
        assert_eq!(transcript[0].binary, ToolBinary::Loudgain);
        assert!(report.published.is_some());
    }

    #[tokio::test]
    async fn five_track_fail_fast_blocks_after_convert_and_runs_no_postprocessing_tools() {
        let fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            5,
            stage_policy(true, true, true),
            OverwritePolicy::FailIfExists,
        );
        let runner = BlockingToolRunner::new();
        let reporter = RecordingReporter::new();
        let cancel = CancellationToken::new();
        let outputs = (0..5)
            .map(|index| if index == 1 { failed_output(&fixture, index) } else { successful_output(&fixture, index) })
            .collect::<Vec<_>>();
        let final_paths = fixture.final_paths.clone();

        let report = finish_pipeline_album_for_scheduler(
            fixture.album,
            outputs,
            &runner,
            &reporter,
            &cancel,
        )
        .await;

        assert!(matches!(
            report.outcome,
            AlbumOutcome::Blocked {
                reason: BlockReason::RequiredStageFailure(PipelineStage::Convert),
                ..
            }
        ));
        assert!(runner.transcript().is_empty(), "post-processing tools did not run");
        assert!(report.published.is_none());
        assert!(final_paths.iter().all(|path| !path.exists()));
        assert!(!manifest_path(&fixture.album_dir).exists(), "blocked fail-fast album writes no manifest");
    }

    #[tokio::test]
    async fn five_track_allow_partial_publishes_only_four_survivors() {
        let fixture = fixture(
            FailurePolicy::AllowPartialAlbum,
            5,
            stage_policy(false, false, false),
            OverwritePolicy::FailIfExists,
        );
        let runner = BlockingToolRunner::new();
        let reporter = RecordingReporter::new();
        let cancel = CancellationToken::new();
        let outputs = (0..5)
            .map(|index| if index == 1 { failed_output(&fixture, index) } else { successful_output(&fixture, index) })
            .collect::<Vec<_>>();
        let final_paths = fixture.final_paths.clone();

        let report = finish_pipeline_album_for_scheduler(
            fixture.album,
            outputs,
            &runner,
            &reporter,
            &cancel,
        )
        .await;

        assert!(matches!(report.outcome, AlbumOutcome::Partial { .. }));
        assert_eq!(
            report.published.as_ref().map(|published| published.entries.len()),
            Some(4)
        );
        assert!(final_paths[0].exists());
        assert!(!final_paths[1].exists());
        assert!(final_paths[2].exists());
        assert!(final_paths[3].exists());
        assert!(final_paths[4].exists());

        // Manifest records only published tracks (failed tracks have no artifact/output).
        let manifest = read_manifest(&fixture.album_dir)
            .expect("partial manifest read succeeds")
            .expect("allow-partial publish writes a manifest");
        assert_eq!(manifest.total_tracks, 4, "manifest records 4 published tracks");
        assert_eq!(manifest.tracks.len(), 4);
        assert_eq!(
            manifest.tracks.iter()
                .filter(|track| track.validation_status == ValidationStatus::Passed)
                .count(),
            4,
            "all manifest entries are successful (failed tracks not in manifest)"
        );
    }

    #[tokio::test]
    async fn all_tracks_cancelled_blocks_with_cancelled_and_runs_no_postprocessing() {
        let fixture = fixture(
            FailurePolicy::AllowPartialAlbum,
            3,
            stage_policy(true, true, true),
            OverwritePolicy::FailIfExists,
        );
        let runner = BlockingToolRunner::new();
        let reporter = RecordingReporter::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let outputs = (0..3).map(|index| failed_output(&fixture, index)).collect::<Vec<_>>();

        let report = finish_pipeline_album_for_scheduler(
            fixture.album,
            outputs,
            &runner,
            &reporter,
            &cancel,
        )
        .await;

        assert!(matches!(
            report.outcome,
            AlbumOutcome::Blocked {
                reason: BlockReason::Cancelled,
                ..
            }
        ));
        assert!(runner.transcript().is_empty());
        assert!(report.published.is_none());
        assert!(!manifest_path(&fixture.album_dir).exists(), "cancelled album writes no manifest");
    }

    #[tokio::test]
    async fn metadata_failure_blocks_replaygain_features_and_publish() {
        let mut fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            1,
            stage_policy(true, true, true),
            OverwritePolicy::FailIfExists,
        );
        // Use SACD source so authoritative metadata is required and the
        // metadata stage actually runs (SingleFile sources skip it because
        // the planner already transfers source tags).
        fixture.album.source.kind = SourceKind::SacdIso;
        fixture.album.source.provenance.source_kind = SourceKind::SacdIso;
        fixture.album.source.album_metadata.album = Some("Test Album".to_string());
        fixture.album.source.tracks[0].source_ref = TrackSourceRef::SacdTrack {
            iso: fixture.album.req.container.clone(),
            track_index: 0,
            area: SacdArea::Stereo,
        };
        let runner = BlockingToolRunner::with_behaviors([
            ToolBehavior::FailWithStderr("metadata failed".to_string()),
        ]);
        let reporter = RecordingReporter::new();
        let cancel = CancellationToken::new();
        let outputs = vec![successful_output(&fixture, 0)];

        let report = finish_pipeline_album_for_scheduler(
            fixture.album,
            outputs,
            &runner,
            &reporter,
            &cancel,
        )
        .await;

        assert!(matches!(stage_outcome(&report, PipelineStage::Metadata), Some(StageOutcome::Failed(_))));
        assert!(stage_outcome(&report, PipelineStage::ReplayGain).is_none());
        assert!(stage_outcome(&report, PipelineStage::Features).is_none());
        assert!(stage_outcome(&report, PipelineStage::Publish).is_none());
        assert!(matches!(report.outcome, AlbumOutcome::Blocked { .. }));
        assert_eq!(runner.transcript().len(), 1);
        assert!(report.published.is_none());
    }

    #[tokio::test]
    async fn replaygain_failure_blocks_features_and_publish() {
        let fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            1,
            stage_policy(false, true, true),
            OverwritePolicy::FailIfExists,
        );
        let runner = BlockingToolRunner::with_behaviors([
            ToolBehavior::FailWithStderr("replaygain failed".to_string()),
        ]);
        let reporter = RecordingReporter::new();
        let cancel = CancellationToken::new();
        let outputs = vec![successful_output(&fixture, 0)];

        let report = finish_pipeline_album_for_scheduler(
            fixture.album,
            outputs,
            &runner,
            &reporter,
            &cancel,
        )
        .await;

        assert!(matches!(stage_outcome(&report, PipelineStage::ReplayGain), Some(StageOutcome::Failed(_))));
        assert!(stage_outcome(&report, PipelineStage::Features).is_none());
        assert!(stage_outcome(&report, PipelineStage::Publish).is_none());
        assert!(matches!(report.outcome, AlbumOutcome::Blocked { .. }));
        assert_eq!(runner.transcript().len(), 1);
        assert_eq!(runner.transcript()[0].binary, ToolBinary::Loudgain);
        assert!(report.published.is_none());
    }

    #[tokio::test]
    async fn publish_failure_blocks_and_still_writes_durable_log() {
        let fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            1,
            stage_policy(false, false, false),
            OverwritePolicy::FailIfExists,
        );
        std::fs::create_dir_all(&fixture.album_dir).expect("existing album dir");
        std::fs::write(fixture.album_dir.join("old.flac"), b"old").expect("old output");
        let runner = BlockingToolRunner::new();
        let reporter = RecordingReporter::new();
        let cancel = CancellationToken::new();
        let outputs = vec![successful_output(&fixture, 0)];
        let log_root = fixture.log_root.clone();

        let report = finish_pipeline_album_for_scheduler(
            fixture.album,
            outputs,
            &runner,
            &reporter,
            &cancel,
        )
        .await;

        assert!(matches!(
            report.outcome,
            AlbumOutcome::Blocked {
                reason: BlockReason::PublishFailed,
                ..
            }
        ));
        assert!(matches!(stage_outcome(&report, PipelineStage::Publish), Some(StageOutcome::Failed(_))));
        let durable_log = report.durable_log.as_ref().expect("durable log path");
        assert!(durable_log.starts_with(log_root));
        assert!(durable_log.exists());
        assert!(fixture.album_dir.join("old.flac").exists());
        assert!(report.published.is_none());
    }

    #[tokio::test]
    async fn cancellation_during_metadata_runs_no_later_postprocessing() {
        let mut fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            1,
            stage_policy(true, true, true),
            OverwritePolicy::FailIfExists,
        );
        // Use SACD source so authoritative metadata is required and the
        // metadata stage actually runs.
        fixture.album.source.kind = SourceKind::SacdIso;
        fixture.album.source.provenance.source_kind = SourceKind::SacdIso;
        fixture.album.source.album_metadata.album = Some("Test Album".to_string());
        fixture.album.source.tracks[0].source_ref = TrackSourceRef::SacdTrack {
            iso: fixture.album.req.container.clone(),
            track_index: 0,
            area: SacdArea::Stereo,
        };
        let (gate, blocker) = tool_gate();
        let runner = std::sync::Arc::new(BlockingToolRunner::with_behaviors([
            ToolBehavior::BlockThenSucceed(blocker),
        ]));
        let reporter = RecordingReporter::new();
        let cancel = CancellationToken::new();
        let outputs = vec![successful_output(&fixture, 0)];
        let run_cancel = cancel.clone();
        let run_runner = runner.clone();
        let handle = tokio::spawn(async move {
            finish_pipeline_album_for_scheduler(
                fixture.album,
                outputs,
                run_runner.as_ref(),
                &reporter,
                &run_cancel,
            )
            .await
        });

        let release = gate.wait_started().await;
        cancel.cancel();
        let report = handle.await.expect("finish task joins");
        drop(release);

        assert!(matches!(stage_outcome(&report, PipelineStage::Metadata), Some(StageOutcome::Failed(_))));
        assert!(stage_outcome(&report, PipelineStage::ReplayGain).is_none());
        assert!(stage_outcome(&report, PipelineStage::Publish).is_none());
        assert!(report.published.is_none());
        assert_eq!(runner.transcript().len(), 1);
    }

    #[tokio::test]
    async fn cancellation_during_replaygain_runs_no_features_or_publish() {
        let fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            1,
            stage_policy(false, true, true),
            OverwritePolicy::FailIfExists,
        );
        let (gate, blocker) = tool_gate();
        let runner = std::sync::Arc::new(BlockingToolRunner::with_behaviors([
            ToolBehavior::BlockThenSucceed(blocker),
        ]));
        let reporter = RecordingReporter::new();
        let cancel = CancellationToken::new();
        let outputs = vec![successful_output(&fixture, 0)];
        let run_cancel = cancel.clone();
        let run_runner = runner.clone();
        let handle = tokio::spawn(async move {
            finish_pipeline_album_for_scheduler(
                fixture.album,
                outputs,
                run_runner.as_ref(),
                &reporter,
                &run_cancel,
            )
            .await
        });

        let release = gate.wait_started().await;
        cancel.cancel();
        let report = handle.await.expect("finish task joins");
        drop(release);

        assert!(matches!(stage_outcome(&report, PipelineStage::ReplayGain), Some(StageOutcome::Failed(_))));
        assert!(stage_outcome(&report, PipelineStage::Features).is_none());
        assert!(stage_outcome(&report, PipelineStage::Publish).is_none());
        assert!(report.published.is_none());
        assert_eq!(runner.transcript().len(), 1);
    }


    #[tokio::test]
    async fn cancellation_after_features_before_publish_writes_no_final_output_or_manifest() {
        let fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            1,
            stage_policy(false, false, true),
            OverwritePolicy::FailIfExists,
        );
        let runner = BlockingToolRunner::new();
        let cancel = CancellationToken::new();
        let reporter = CancellingReporter::new(cancel.clone(), PipelineStage::Features);
        let outputs = vec![successful_output(&fixture, 0)];
        let final_paths = fixture.final_paths.clone();

        let report = finish_pipeline_album_for_scheduler(
            fixture.album,
            outputs,
            &runner,
            &reporter,
            &cancel,
        )
        .await;

        assert!(matches!(stage_outcome(&report, PipelineStage::Features), Some(StageOutcome::Ok)));
        assert!(stage_outcome(&report, PipelineStage::Publish).is_none());
        assert!(matches!(
            report.outcome,
            AlbumOutcome::Blocked { reason: BlockReason::Cancelled, .. }
        ));
        assert!(report.published.is_none());
        assert!(final_paths.iter().all(|path| !path.exists()));
        assert!(!manifest_path(&fixture.album_dir).exists(), "publish-boundary cancellation writes no manifest");
    }

    fn publish_album_output_with_test_cancel_after_backup(
        staging: StagingDir,
        plan: &PublishPlan,
        policy: PublishPolicy,
        cancel: &CancellationToken,
    ) -> std::result::Result<PublishedAlbum, PublishError> {
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
        if let Err(err) = std::fs::create_dir_all(&temp_dir) {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(PublishError::Io(err));
        }

        let mut published_entries = Vec::with_capacity(plan.entries.len());
        for entry in &plan.entries {
            if !entry.staged_path.exists() {
                let _ = std::fs::remove_dir_all(&temp_dir);
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
                if let Err(err) = std::fs::create_dir_all(parent) {
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
            let bytes = match std::fs::metadata(&staged_final) {
                Ok(metadata) => metadata.len(),
                Err(err) => return cleanup_publish_temp(&temp_dir, PublishError::Io(err)),
            };
            published_entries.push(PublishedEntry {
                final_path: entry.final_path.clone(),
                role: entry.role.clone(),
                bytes,
            });
        }

        if plan.album_dir.exists() {
            match policy.overwrite {
                OverwritePolicy::FailIfExists => {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    return Err(PublishError::DestinationExists(
                        plan.album_dir.display().to_string(),
                    ));
                }
                OverwritePolicy::ReplaceWithBackup
                | OverwritePolicy::AlwaysRedo
                | OverwritePolicy::SkipIfManifestMatch
                | OverwritePolicy::VerifyIfManifestMatch => {
                    if let Err(err) = write_publish_marker(&marker_path, &backup_dir) {
                        let _ = std::fs::remove_dir_all(&temp_dir);
                        return Err(err);
                    }
                    std::fs::rename(&plan.album_dir, &backup_dir).map_err(|err| {
                        let _ = std::fs::remove_dir_all(&temp_dir);
                        let _ = std::fs::remove_file(&marker_path);
                        PublishError::BackupFailed(format!(
                            "{} -> {}: {err}",
                            plan.album_dir.display(),
                            backup_dir.display()
                        ))
                    })?;
                    cancel.cancel();
                    return Err(PublishError::AtomicRename(format!(
                        "cancelled after backup before final publish rename: marker {} backup {} temp {}",
                        marker_path.display(),
                        backup_dir.display(),
                        temp_dir.display()
                    )));
                }
            }
        }

        if cancel.is_cancelled() {
            return Err(PublishError::AtomicRename(
                "cancelled before final publish rename".to_string(),
            ));
        }
        std::fs::rename(&temp_dir, &plan.album_dir).map_err(|err| {
            let _ = std::fs::remove_dir_all(&temp_dir);
            PublishError::AtomicRename(format!(
                "{} -> {}: {err}",
                temp_dir.display(),
                plan.album_dir.display()
            ))
        })?;
        sync_parent_dir_best_effort(&plan.album_dir);
        drop(staging);
        Ok(PublishedAlbum {
            album_dir: plan.album_dir.clone(),
            entries: published_entries,
            manifest_path: None,
        })
    }

    #[test]
    fn cancellation_during_publish_after_backup_leaves_recoverable_state_and_no_corrupt_final_output() {
        let fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            1,
            stage_policy(false, false, false),
            OverwritePolicy::ReplaceWithBackup,
        );
        let parent = fixture.album_dir.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&fixture.album_dir).expect("existing album dir");
        std::fs::write(fixture.album_dir.join("old.flac"), b"old output").expect("old output");

        let marker_path = parent.join(".Gate Test.publish-in-progress");
        let plan = PublishPlan {
            album_dir: fixture.album_dir.clone(),
            entries: vec![PublishEntry {
                staged_path: fixture.staged_paths[0].clone(),
                final_path: fixture.final_paths[0].clone(),
                role: PublishRole::Audio,
            }],
        };
        let publish = fixture.album.req.publish.clone();
        let staging = fixture.album.staging;
        let cancel = CancellationToken::new();

        let err = publish_album_output_with_test_cancel_after_backup(staging, &plan, publish.clone(), &cancel)
            .expect_err("test seam cancels after backup and before final rename");

        assert!(matches!(err, PublishError::AtomicRename(_)));
        assert!(cancel.is_cancelled());
        assert!(marker_path.exists(), "mid-publish cancellation left recovery marker");
        let marker_text = std::fs::read_to_string(&marker_path).expect("marker text");
        let backup_dir = backup_dir_from_marker(&fixture.album_dir, &marker_path, &marker_text)
            .expect("marker names a valid backup");
        assert!(backup_dir.exists(), "backup exists for rollback");
        assert!(backup_dir.join("old.flac").exists(), "old album contents are backed up");
        assert!(!fixture.album_dir.exists(), "half-published final album dir is absent until repair");
        assert!(!fixture.final_paths[0].exists(), "new final output is not exposed before repair");

        let retry_staging_root = parent.join("retry-staging");
        let retry_staging = StagingDir::new(retry_staging_root.clone(), "retry-job".to_string());
        let retry_staged = retry_staging.root.join("converted").join("01.flac");
        std::fs::create_dir_all(retry_staged.parent().unwrap()).expect("retry staging parent");
        std::fs::write(&retry_staged, b"new output").expect("retry staged output");
        let retry_plan = PublishPlan {
            album_dir: fixture.album_dir.clone(),
            entries: vec![PublishEntry {
                staged_path: retry_staged,
                final_path: fixture.final_paths[0].clone(),
                role: PublishRole::Audio,
            }],
        };

        let published = publish_album_output(retry_staging, &retry_plan, publish, None)
            .expect("normal publish repairs marker then publishes retry output");

        assert!(!marker_path.exists());
        assert_eq!(published.entries.len(), 1);
        assert_eq!(std::fs::read(&fixture.final_paths[0]).unwrap(), b"new output");
        assert!(std::fs::read_dir(&parent)
            .expect("parent entries")
            .filter_map(|entry| entry.ok())
            .all(|entry| !entry.file_name().to_string_lossy().starts_with(".Gate Test.tmp-")));
    }

    #[test]
    fn interrupted_publish_recovery_restores_backup_before_new_publish() {
        let fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            1,
            stage_policy(false, false, false),
            OverwritePolicy::ReplaceWithBackup,
        );
        let parent = fixture.album_dir.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&parent).expect("album parent");
        let backup_dir = parent.join(".Gate Test.backup-test");
        std::fs::create_dir_all(&backup_dir).expect("backup dir");
        std::fs::write(backup_dir.join("restored.flac"), b"restored").expect("backup file");
        let marker_path = parent.join(".Gate Test.publish-in-progress");
        let marker = serde_json::json!({
            "version": 1,
            "album_dir_name": "Gate Test",
            "backup_dir_name": ".Gate Test.backup-test"
        });
        std::fs::write(&marker_path, serde_json::to_vec_pretty(&marker).unwrap()).expect("marker");

        let plan = PublishPlan {
            album_dir: fixture.album_dir.clone(),
            entries: vec![PublishEntry {
                staged_path: fixture.staged_paths[0].clone(),
                final_path: fixture.final_paths[0].clone(),
                role: PublishRole::Audio,
            }],
        };
        let publish = fixture.album.req.publish.clone();
        let staging = fixture.album.staging;
        let published = publish_album_output(staging, &plan, publish, None)
            .expect("publish repairs marker then publishes");

        assert!(!marker_path.exists());
        assert!(published.album_dir.join("01.flac").exists());
        assert!(!backup_dir.exists(), "repair consumed the old backup before publish");
    }

    #[test]
    fn scheduled_worker_failure_output_has_failed_shape_required_by_mid_chain_contract() {
        let fixture = fixture(
            FailurePolicy::FailAlbumOnAnyTrackFailure,
            1,
            stage_policy(false, false, false),
            OverwritePolicy::FailIfExists,
        );
        let output = scheduled_worker_failure_output(
            0,
            &fixture.album.source.tracks[0],
            Some(fixture.staged_paths[0].clone()),
            Some(fixture.final_paths[0].clone()),
            "planned command 2 failed".to_string(),
        );

        assert!(!output.ok);
        assert!(output.artifact.is_none());
        assert!(matches!(output.record.outcome, TrackOutcome::Err(_)));
        assert_eq!(output.index, 0);
    }
}
// CHUNK_2_1_3_FAILURE_CANCELLATION_TESTS_END

#[cfg(test)]
mod validate_encoded_output_tests {
    use super::*;
    use crate::convert::pipeline::tool::{StubToolRunner, ToolOutput, ProcessExit, CommandRecord};

    fn ffprobe_exact_json(sample_rate: u32, total_samples: u64) -> String {
        format!(
            r#"{{"streams":[{{"sample_rate":"{sample_rate}","duration_ts":{total_samples},"time_base":"1/{sample_rate}"}}],"format":{{}}}}"#
        )
    }

    fn ffprobe_approx_json(sample_rate: u32, duration_secs: f64) -> String {
        format!(
            r#"{{"streams":[{{"sample_rate":"{sample_rate}","duration":"{duration_secs}"}}],"format":{{"duration":"{duration_secs}"}}}}"#
        )
    }

    fn stub_with_probe(json: &str) -> StubToolRunner {
        let runner = StubToolRunner::new();
        runner.push_output(ToolOutput {
            exit: ProcessExit::Code(0),
            stdout_tail: json.to_string(),
            stderr_tail: String::new(),
            elapsed: Duration::from_millis(1),
            command: CommandRecord {
                binary: ToolBinary::Ffprobe,
                sanitized_args: vec![],
                cwd: None,
                env_keys: vec![],
                exit: Some(ProcessExit::Code(0)),
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                elapsed: Duration::from_millis(1),
            },
        });
        runner
    }

    #[tokio::test]
    async fn lossless_target_with_matching_samples_returns_actual() {
        let temp = tempfile::tempdir().expect("temp dir");
        let out = temp.path().join("track.flac");
        std::fs::write(&out, b"fake-flac").expect("write");
        let runner = stub_with_probe(&ffprobe_exact_json(44100, 1_000_000));
        let cancel = CancellationToken::new();

        let result = validate_encoded_output(
            &out,
            Some(1_000_000),
            &tonepoet_pipeline::AudioFormat::Flac,
            &runner,
            &cancel,
        )
        .await;

        assert_eq!(result, Some(1_000_000));
    }

    #[tokio::test]
    async fn lossless_target_with_drifted_samples_returns_actual_and_warns() {
        let temp = tempfile::tempdir().expect("temp dir");
        let out = temp.path().join("track.wav");
        std::fs::write(&out, b"fake-wav").expect("write");
        // Probe returns 1,000,100 but expected is 1,000,000 — drift of 100
        let runner = stub_with_probe(&ffprobe_exact_json(44100, 1_000_100));
        let cancel = CancellationToken::new();

        let result = validate_encoded_output(
            &out,
            Some(1_000_000),
            &tonepoet_pipeline::AudioFormat::Wav,
            &runner,
            &cancel,
        )
        .await;

        // Returns actual (probed) value, not expected
        assert_eq!(result, Some(1_000_100));
    }

    #[tokio::test]
    async fn lossy_target_skips_validation_returns_expected() {
        let temp = tempfile::tempdir().expect("temp dir");
        let out = temp.path().join("track.mp3");
        std::fs::write(&out, b"fake-mp3").expect("write");
        // No probe should be called — runner has no responses queued
        let runner = StubToolRunner::new();
        let cancel = CancellationToken::new();

        let result = validate_encoded_output(
            &out,
            Some(1_000_000),
            &tonepoet_pipeline::AudioFormat::Mp3,
            &runner,
            &cancel,
        )
        .await;

        assert_eq!(result, Some(1_000_000));
        // Verify no probe was attempted
        assert_eq!(runner.transcript().len(), 0);
    }

    #[tokio::test]
    async fn dsd_target_skips_validation_returns_expected() {
        let temp = tempfile::tempdir().expect("temp dir");
        let out = temp.path().join("track.dsf");
        std::fs::write(&out, b"fake-dsf").expect("write");
        let runner = StubToolRunner::new();
        let cancel = CancellationToken::new();

        let result = validate_encoded_output(
            &out,
            Some(1_000_000),
            &tonepoet_pipeline::AudioFormat::Dsf,
            &runner,
            &cancel,
        )
        .await;

        assert_eq!(result, Some(1_000_000));
        assert_eq!(runner.transcript().len(), 0);
    }

    #[tokio::test]
    async fn no_expected_samples_returns_none() {
        let temp = tempfile::tempdir().expect("temp dir");
        let out = temp.path().join("track.flac");
        std::fs::write(&out, b"fake-flac").expect("write");
        let runner = StubToolRunner::new();
        let cancel = CancellationToken::new();

        let result = validate_encoded_output(
            &out,
            None,
            &tonepoet_pipeline::AudioFormat::Flac,
            &runner,
            &cancel,
        )
        .await;

        assert_eq!(result, None);
        assert_eq!(runner.transcript().len(), 0);
    }

    #[tokio::test]
    async fn approximate_probe_allows_small_drift() {
        let temp = tempfile::tempdir().expect("temp dir");
        let out = temp.path().join("track.flac");
        std::fs::write(&out, b"fake-flac").expect("write");
        // Expected 1,000,000. Duration 22.685 sec → round(22.685 * 44100) = 1,000,409.
        // Drift = 409, allowed = 44100/75 = 588. 409 < 588 → within tolerance.
        let runner = stub_with_probe(&ffprobe_approx_json(44100, 22.685));
        let cancel = CancellationToken::new();

        let result = validate_encoded_output(
            &out,
            Some(1_000_000),
            &tonepoet_pipeline::AudioFormat::Flac,
            &runner,
            &cancel,
        )
        .await;

        assert!(result.is_some());
        let actual = result.unwrap();
        // Probed value should reflect the approximate duration, not the expected value
        assert_ne!(actual, 1_000_000, "approximate probe should return a different value");
        assert!(actual.abs_diff(1_000_000) <= 588, "drift {} should be within tolerance 588", actual.abs_diff(1_000_000));
    }
}
