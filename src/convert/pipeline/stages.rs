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
use super::materializer_dvda::{is_dvda_candidate, DvdaAudioMaterializer};
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
use tonepoet_pipeline::{
    AacProfile, AudioFormat as PlannerAudioFormat, BitDepthTarget, DitherType,
    DsdLowpassMethod, DsdRate, DsdToPcmGainMode, Mp3Mode, NyquistTransition,
    OpusContentType, PcmBitDepth, PreferredTool, RateTarget, ResampleQuality,
    SoxSincPhase, SsrcPdfType, SsrcProfile, WavPackMode,
};
use crate::tui::sacd::{
    parse_sacd_iso, AreaInfo, PlayTime, SacdError, SacdMetadata, TrackEntry, SACD_FRAME_RATE,
    SACD_SAMPLE_RATE_HZ,
};
use sacd_rs::dsd_file::{validate_dsd_stream, DsdValidationMode, DsdValidationOptions, DsdValidationReport};
use sacd_rs::extract::{extract_track_with_area_frame_format, ExtractIntegrityOptions, ExtractReport, ExtractStats, OutputFormat};
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
    if !req.container.is_file() && !req.container.is_dir() {
        return Err(RequestValidationError::InvalidOutputRoot(format!(
            "container is neither a regular file nor a directory: {}",
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
    if is_dvda_candidate(req)? {
        return Ok(SourceKind::DvdAudio);
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
        SourceKind::DvdAudio => Ok(Box::new(DvdaAudioMaterializer)),
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
    realize_track_with_tool_limits_and_stats(
        src,
        req,
        staging,
        runner,
        cancel,
        tool_concurrency_limits,
        progress_tracker,
    )
    .await
    .map(|realized| realized.path)
}

async fn realize_track_with_tool_limits_and_stats(
    src: &TrackSourceRef,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
    progress_tracker: Option<&mut OperationProgressTracker<'_>>,
) -> Result<RealizedTrackInfo, ConvertError> {
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
            Ok(RealizedTrackInfo {
                path: path.clone(),
                dsd_dst_stats: dsd_dst_stats_from_file(path, Some(file_len(path).unwrap_or(0)), None),
            })
        }
        TrackSourceRef::CueSegmentCarrier { path, carrier, .. } => {
            if !path.exists() {
                return Err(ConvertError::TrackValidation(format!(
                    "staged CUE segment carrier does not exist: {}",
                    path.display()
                )));
            }
            if !path.is_file() {
                return Err(ConvertError::TrackValidation(format!(
                    "staged CUE segment carrier is not a regular file: {}",
                    path.display()
                )));
            }
            if *carrier != CueSegmentCarrier::PcmS32LeWav {
                return Err(ConvertError::TrackValidation(format!(
                    "unsupported staged CUE segment carrier {:?} at {}",
                    carrier,
                    path.display()
                )));
            }
            Ok(RealizedTrackInfo::without_stats(path.clone()))
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
            .map(RealizedTrackInfo::without_stats)
        }
        TrackSourceRef::SacdTrack {
            iso,
            track_index,
            area,
        } => realize_sacd_track(iso, *track_index, *area, &req.settings.target_format, staging, cancel, progress_tracker).await,
        TrackSourceRef::DvdaTrack { .. } => Err(ConvertError::UnsupportedTrackSource),
    }
}

fn cue_segment_output_name(image: &Path, start_sample: u64, samples: u64) -> String {
    let stem = sanitize_segment_component(
        image
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("image"),
    );
    format!("{stem}_{start_sample:012}_{samples:012}.wav")
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
        "-vn".into(),
        "-sn".into(),
        "-dn".into(),
        "-af".into(),
        filter.to_string(),
        "-f".into(),
        "wav".into(),
        "-c:a".into(),
        "pcm_s32le".into(),
        out_path.to_string_lossy().into_owned(),
    ]
}

#[cfg(test)]
mod cue_image_segment_command_tests {
    use super::*;

    #[test]
    fn cue_segment_command_decodes_to_pcm_s32le_wav_without_stream_copy() {
        let args = cut_segment_ffmpeg_args(
            Path::new("album.flac"),
            "atrim=start_sample=0:end_sample=44100,asetpts=PTS-STARTPTS",
            Path::new("segment.wav"),
        );

        assert!(has_adjacent_arg(&args, "-map", "0:a:0"));
        assert!(has_adjacent_arg(&args, "-f", "wav"));
        assert!(has_adjacent_arg(&args, "-c:a", "pcm_s32le"));
        assert!(args.iter().any(|arg| arg == "-vn"));
        assert!(args.iter().any(|arg| arg == "-sn"));
        assert!(args.iter().any(|arg| arg == "-dn"));
        assert!(!has_adjacent_arg(&args, "-map", "0:v?"));
        assert!(!has_adjacent_arg(&args, "-map_metadata", "0"));
        assert!(!has_adjacent_arg(&args, "-c:a", "copy"));
        assert!(!has_adjacent_arg(&args, "-c:a", "flac"));
    }

    #[test]
    fn legacy_image_segment_realization_uses_wav_carrier_name_not_flac() {
        let name = cue_segment_output_name(Path::new("album.flac"), 0, 44_100);
        assert!(name.ends_with(".wav"));
        assert!(!name.ends_with(".flac"));
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
/// Lossless PCM-preserving targets (FLAC, WAV, AIFF, WavPack, ALAC) must
/// preserve the staged CUE segment's sample count after final encode. Exact
/// `duration_ts` probes must match exactly. Duration-only probes get a narrow
/// one-millisecond sample tolerance to cover container/probe rounding. Lossy
/// formats and DSD are skipped because codec padding or a different sample
/// model makes strict comparison invalid.
///
/// Returns the actual probed sample count on success, the original expected
/// sample count when validation is intentionally skipped for a non-lossless
/// target, or a track-validation error when a lossless final output drifts.
#[allow(dead_code)]
async fn validate_encoded_output(
    out_path: &Path,
    expected_samples: Option<u64>,
    target_format: &tonepoet_pipeline::AudioFormat,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<Option<u64>, ConvertError> {
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
) -> Result<Option<u64>, ConvertError> {
    let Some(expected) = expected_samples else {
        return Ok(None);
    };

    if matches!(
        target_format,
        tonepoet_pipeline::AudioFormat::Dsf | tonepoet_pipeline::AudioFormat::Dff
    ) || !target_format.is_pcm_lossless()
    {
        return Ok(Some(expected));
    }

    let probe = probe_realized_segment_with_tool_limits(
        out_path,
        runner,
        cancel,
        tool_concurrency_limits,
    )
    .await
    .map_err(|err| {
        ConvertError::TrackValidation(format!(
            "post-encode sample validation failed for lossless output {}: {err}",
            out_path.display()
        ))
    })?;

    let actual = probe.samples.ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "post-encode sample validation failed for lossless output {}: ffprobe returned no sample count or duration",
            out_path.display()
        ))
    })?;

    let delta = actual.abs_diff(expected);
    let allowed = encoded_output_sample_tolerance(&probe);

    if delta > allowed {
        return Err(ConvertError::TrackValidation(format!(
            "post-encode sample drift for lossless output {}: expected {expected}, got {actual}, allowed {allowed}",
            out_path.display()
        )));
    }

    Ok(Some(actual))
}

fn encoded_output_sample_tolerance(probe: &RealizedProbe) -> u64 {
    if probe.exact {
        0
    } else {
        // Duration-only probes are a fallback for containers/tools that do not
        // expose exact `duration_ts`; allow at most one millisecond of sample
        // rounding at the probed sample rate.
        (probe.sample_rate / 1000).max(1) as u64
    }
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
    target_format: &tonepoet_pipeline::AudioFormat,
    staging: &StagingDir,
    cancel: &CancellationToken,
    progress_tracker: Option<&mut OperationProgressTracker<'_>>,
) -> Result<RealizedTrackInfo, ConvertError> {
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
    let target_format = target_format.clone();

    let output = match progress_tracker {
        Some(tracker) => {
            heartbeat::run_with_heartbeat(
                async {
                    let iso = iso.clone();
                    let staging_root = staging_root.clone();
                    let target_format = target_format.clone();
                    tokio::task::spawn_blocking(move || {
                        realize_sacd_track_blocking(&iso, track_index, area, &target_format, &staging_root)
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
            realize_sacd_track_blocking(&iso, track_index, area, &target_format, &staging_root)
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
    target_format: &tonepoet_pipeline::AudioFormat,
    staging_root: &Path,
) -> Result<RealizedTrackInfo, ConvertError> {
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

    let output_format = match target_format {
        tonepoet_pipeline::AudioFormat::Dff => OutputFormat::Dff,
        _ => OutputFormat::Dsf,
    };
    let output_ext = match output_format {
        OutputFormat::Dff => "dff",
        _ => "dsf",
    };

    let realized_dir = staging_root.join("realized-sacd-tracks");
    fs::create_dir_all(&realized_dir)?;
    let out_path = realized_dir.join(sacd_track_output_name(iso, area, track_index, entry, output_ext));

    if sacd_output_is_ready(&out_path, output_format, area_info, entry.duration) {
        let stats = dsd_dst_stats_from_file(
            &out_path,
            None,
            Some(file_len(&out_path).unwrap_or(0)),
        );
        return Ok(RealizedTrackInfo {
            path: out_path,
            dsd_dst_stats: stats,
        });
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

        let options = area_info
            .track_extract_options(track_index as usize, output_format)
            .map_err(|err| ConvertError::TrackValidation(err.to_string()))?;
        let report = extract_track_with_area_frame_format(
            &mut iso_reader,
            &mut writer,
            options,
            area_info.extraction_frame_format(),
            ExtractIntegrityOptions::strict(),
        )
        .map_err(|err| {
            ConvertError::Realize(format!(
                "SACD extraction failed for {} track {}: {err}",
                iso.display(),
                track_index + 1
            ))
        })?;
        if report.integrity_loss_detected() {
            return Err(ConvertError::Realize(format!(
                "SACD extraction lost integrity for {} track {}: {:?}",
                iso.display(),
                track_index + 1,
                report.integrity
            )));
        }
        writer.sync_all()?;
        drop(writer);

        validate_sacd_realization(&tmp_path, output_format, area_info, entry.duration, report.stats)?;
        Ok(dsd_dst_stats_from_extract_report(&report, Some(file_len(&tmp_path).unwrap_or(0))))
    })();

    let mut dsd_dst_stats = match extraction_result {
        Ok(stats) => stats,
        Err(err) => {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }
    };

    match fs::rename(&tmp_path, &out_path) {
        Ok(()) => {
            if let Some(stats) = dsd_dst_stats.as_mut() {
                stats.bytes_written = file_len(&out_path).unwrap_or(stats.bytes_written);
            }
            Ok(RealizedTrackInfo {
                path: out_path,
                dsd_dst_stats,
            })
        }
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
            if let Some(stats) = dsd_dst_stats.as_mut() {
                stats.bytes_written = file_len(&out_path).unwrap_or(stats.bytes_written);
            }
            Ok(RealizedTrackInfo {
                path: out_path,
                dsd_dst_stats,
            })
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
    ext: &str,
) -> String {
    let stem = sanitize_segment_component(
        iso.file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("sacd"),
    );
    let path_hash = stable_path_hash(iso);
    format!(
        "{stem}_{path_hash:016x}_{}_track_{:03}_{:08x}_{:08x}.{ext}",
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
        .unwrap_or("track");
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

fn sacd_output_is_ready(
    path: &Path,
    format: OutputFormat,
    area_info: &AreaInfo,
    duration: PlayTime,
) -> bool {
    match format {
        OutputFormat::Dsf => {
            let expectation = DsfExpectation::from_area(area_info, duration);
            dsf_output_is_ready_for(path, expectation)
        }
        _ => {
            // For DFF/DFF-DST: basic existence + non-empty check.
            // The extraction already validates via ExtractStats.
            path.metadata()
                .map(|m| m.is_file() && m.len() > 0)
                .unwrap_or(false)
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
    format: OutputFormat,
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
    if matches!(format, OutputFormat::Dsf) {
        validate_dsf_container(path, Some(expectation))?;
    }

    let expected_frames = u64::from(playtime_to_frame_count(duration));
    if expected_frames > 0 {
        let delta = stats.frames_read.abs_diff(expected_frames);
        if delta > 1 {
            log::warn!(
                "SACD frame count drift for {}: TOC expected {}, extractor emitted {}",
                path.display(),
                expected_frames,
                stats.frames_read
            );
        }
    }

    // Duration sanity check: TOC-derived duration can drift from the
    // byte-derived DSF sample count. Report the drift but keep the realized
    // file when container structure and extracted audio bytes are valid.
    let stats_sample_count = stats
        .audio_bytes
        .checked_div(u64::from(area_info.header.channel_count))
        .unwrap_or(0)
        .saturating_mul(8);
    if expectation.sample_count != 0 {
        let one_toc_frame_samples = u64::from(SACD_SAMPLE_RATE_HZ / 75);
        let delta = stats_sample_count.abs_diff(expectation.sample_count);
        if delta > one_toc_frame_samples {
            log::warn!(
                "SACD DSF sample-count drift for {}: TOC expected approximately {}, extractor emitted {}, delta {} samples",
                path.display(),
                expectation.sample_count,
                stats_sample_count,
                delta,
            );
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
        // come from realized DSD payload bytes, so TOC-derived duration is
        // advisory rather than a container invariant.
        if expectation.sample_count != 0 {
            let one_toc_frame_samples = u64::from(expectation.sample_frequency / 75);
            let delta = parsed.sample_count.abs_diff(expectation.sample_count);
            if delta > one_toc_frame_samples {
                log::warn!(
                    "DSF sample-count drift for {}: TOC expected approximately {}, header contains {}, delta {} samples",
                    path.display(),
                    expectation.sample_count,
                    parsed.sample_count,
                    delta,
                );
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
        sacd_track_output_name(path, area, track_index, &entry, "dsf")
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
    validate_final_container_extension(&req.settings.target_format, req.container_extension.as_deref())
        .map_err(PlanError::InvalidTemplate)?;

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
    pub realized_dsd_dst_stats: Option<DsdDstPipelineStats>,
    pub req: PipelineRequest,
    pub staging_root: PathBuf,
    pub staging_job: String,
    pub convert_root: PathBuf,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone)]
struct RealizedTrackInfo {
    path: PathBuf,
    dsd_dst_stats: Option<DsdDstPipelineStats>,
}

impl RealizedTrackInfo {
    fn without_stats(path: PathBuf) -> Self {
        Self {
            path,
            dsd_dst_stats: None,
        }
    }
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
            dsd_dst_stats: None,
        },
        artifact: None,
        ok: false,
        metadata_satisfaction: PlannedMetadataSatisfaction::none(),
    }
}

fn blocked_track_records(source: &PreparedSource, reason: &str) -> Vec<TrackRecord> {
    source
        .tracks
        .iter()
        .map(|track| TrackRecord {
            track_id: track.id.clone(),
            outcome: TrackOutcome::Blocked(reason.to_string()),
            source_ref: track.source_ref.clone(),
            realized_input: None,
            output_file: None,
            commands: Vec::new(),
            bytes_in: None,
            bytes_out: None,
            duration: None,
            dsd_dst_stats: None,
        })
        .collect()
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

    let failed = records.iter().any(|record| !matches!(record.outcome, TrackOutcome::Ok));
    let record = convert_stage_record_for_tracks(
        &records,
        if failed && req.failure_policy == FailurePolicy::FailAlbumOnAnyTrackFailure {
            StageOutcome::Failed("one or more tracks failed".to_string())
        } else {
            StageOutcome::Ok
        },
    );
    ConvertStageResult {
        tracks: records,
        artifacts: ArtifactSet {
            audio: AudioArtifacts::Tracks(artifacts),
            sidecars: Vec::new(),
        },
        record,
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
    let realized = match realize_track_with_tool_limits_and_stats(
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
        Ok(realized) => realized,
        Err(err) => {
            let record = failed_track_record(&track, None, Some(staged_path), Vec::new(), err.to_string());
            return Ok(ScheduledTrackOutput { index: track_index, record, artifact: None, ok: false, metadata_satisfaction: PlannedMetadataSatisfaction::none() });
        }
    };
    let realized_input = realized.path;
    let realized_dsd_dst_stats = realized.dsd_dst_stats;

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
                let actual_samples = match validate_encoded_output_with_tool_limits(
                    &staged_path,
                    track.expected_samples,
                    &req.settings.target_format,
                    &runner,
                    &cancel,
                    tool_concurrency_limits.as_ref(),
                )
                .await
                {
                    Ok(samples) => samples,
                    Err(err) => {
                        let commands = command_from_convert_error(&err);
                        let record = failed_track_record(
                            &track,
                            Some(realized_input),
                            Some(staged_path),
                            commands,
                            err.to_string(),
                        );
                        return Ok(ScheduledTrackOutput { index: track_index, record, artifact: None, ok: false, metadata_satisfaction: PlannedMetadataSatisfaction::none() });
                    }
                };
                let mut dsd_dst_stats = realized_dsd_dst_stats;
                merge_optional_dsd_dst_stats(
                    &mut dsd_dst_stats,
                    dsd_dst_stats_from_file(&staged_path, None, bytes_out),
                );
                apply_byte_totals_to_stats(&mut dsd_dst_stats, bytes_in, bytes_out);
                let mut commands = executed.commands;
                append_dsd_dst_stats_to_command_descriptions(&mut commands, dsd_dst_stats.as_ref());
                let record = TrackRecord {
                    track_id: track.id.clone(),
                    outcome: TrackOutcome::Ok,
                    source_ref: track.source_ref.clone(),
                    realized_input: Some(realized_input),
                    output_file: Some(staged_path.clone()),
                    commands,
                    bytes_in,
                    bytes_out,
                    duration: Some(executed.elapsed),
                    dsd_dst_stats,
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
            let error = err.to_string();
            let commands = err.commands;
            let record = failed_track_record(
                &track,
                Some(realized_input),
                Some(staged_path),
                commands,
                error,
            );
            Ok(ScheduledTrackOutput { index: track_index, record, artifact: None, ok: false, metadata_satisfaction: PlannedMetadataSatisfaction::none() })
        }
    }
}

#[allow(dead_code)]
fn track_expected_duration(track: &PreparedTrack) -> Option<Duration> {
    let sample_rate = track.scalar_sample_rate()?;
    let samples = track.expected_samples?;
    if samples == 0 {
        return None;
    }
    Some(Duration::from_secs_f64(samples as f64 / sample_rate as f64))
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
                dsd_dst_stats: None,
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
                dsd_dst_stats: None,
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
            dsd_dst_stats: None,
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

#[derive(Debug, Clone)]
struct CueArtworkSidecar {
    path: PathBuf,
    mime_type: Option<String>,
}

fn cue_artwork_sidecar_from_album_metadata(album: &AlbumMetadata) -> Option<CueArtworkSidecar> {
    let path = album.extra.get(CUE_ARTWORK_PATH_EXTRA_KEY)?.trim();
    if path.is_empty() {
        return None;
    }
    Some(CueArtworkSidecar {
        path: PathBuf::from(path),
        mime_type: album
            .extra
            .get(CUE_ARTWORK_MIME_EXTRA_KEY)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
    })
}

/// Apply metadata tags and CUE artwork to staged audio artifacts.
///
/// CUE tracks use an audio-only PCM S32 WAV carrier. When the materializer
/// extracted original image artwork into a sidecar, this stage owns the
/// post-encode re-injection step for target containers that have a concrete
/// writer here. Unsupported target families are deliberately skipped with a
/// warning rather than pretending the carrier preserved artwork.
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
            dsd_dst_stats: None,
        });
    }

    let cue_artwork = if req.settings.metadata.preserve_artwork {
        cue_artwork_sidecar_from_album_metadata(&source.album_metadata)
    } else {
        None
    };

    match &artifacts.audio {
        AudioArtifacts::Tracks(tracks) => {
            for artifact in tracks {
                if cancel.is_cancelled() {
                    return Err(MetadataError::Tool(ToolRunnerError::Cancelled {
                        command: CommandRecord {
                            description: None,
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
                if let Some(artwork) = cue_artwork.as_ref() {
                    embed_cue_artwork_for_file(
                        &artifact.staged_path,
                        artwork,
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
            if let Some(artwork) = cue_artwork.as_ref() {
                embed_cue_artwork_for_file(
                    &merged.staged_path,
                    artwork,
                    runner,
                    cancel,
                    tool_concurrency_limits.as_ref(),
                )
                .await?;
            }
        }
    }

    Ok(StageRecord {
        stage: PipelineStage::Metadata,
        outcome: StageOutcome::Ok,
        dsd_dst_stats: None,
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

fn authoritative_metadata_tags(meta: &TrackMetadata, album: &AlbumMetadata) -> Vec<(String, String)> {
    let mut tags: Vec<(String, String)> = Vec::new();
    if let Some(ref v) = meta.title {
        push_tag_value(&mut tags, "TITLE", v);
    }
    if let Some(ref v) = meta.artist {
        push_tag_value(&mut tags, "ARTIST", v);
    }
    if let Some(v) = meta.album_artist.as_ref().or(album.album_artist.as_ref()) {
        push_tag_value(&mut tags, "ALBUMARTIST", v);
    }
    let album_tag = album.extra.get("album_tag_override").or(album.album.as_ref());
    if let Some(v) = album_tag {
        push_tag_value(&mut tags, "ALBUM", v);
    }
    if let Some(ref v) = meta.genre {
        push_tag_value(&mut tags, "GENRE", v);
    } else if let Some(ref v) = album.genre {
        push_tag_value(&mut tags, "GENRE", v);
    }
    if let Some(ref v) = meta.date {
        push_tag_value(&mut tags, "DATE", v);
    } else if let Some(ref v) = album.date {
        push_tag_value(&mut tags, "DATE", v);
    }
    if let Some(n) = meta.track_number {
        push_tag_value(&mut tags, "TRACKNUMBER", &n.to_string());
    }
    if let Some(n) = meta.disc_number.or(album.disc_number) {
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
    if let Some(v) = album.extra.get("catalog") {
        push_tag_value(&mut tags, "CATALOG", v);
    }
    for (key, value) in &album.extra {
        if key == "album_tag_override"
            || key == CUE_ARTWORK_PATH_EXTRA_KEY
            || key == CUE_ARTWORK_MIME_EXTRA_KEY
            || key == CUE_ARTWORK_SOURCE_EXTRA_KEY
            || key == CUE_ARTWORK_UNSUPPORTED_EXTRA_KEY
        {
            continue;
        }
        let tag_key = cue_extra_tag_key("ALBUM", key);
        push_tag_value(&mut tags, &tag_key, value);
    }
    for (key, value) in &meta.extra {
        let tag_key = cue_extra_tag_key("TRACK", key);
        push_tag_value(&mut tags, &tag_key, value);
    }

    tags
}

fn tag_value<'a>(tags: &'a [(String, String)], key: &str) -> Option<&'a str> {
    tags.iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.as_str())
}

fn ffmpeg_metadata_key(key: &str) -> String {
    match key {
        "ALBUMARTIST" => "album_artist".to_string(),
        "PRE_EMPHASIS" => "pre_emphasis".to_string(),
        "CUE_FLAGS" => "cue_flags".to_string(),
        _ => key.to_ascii_lowercase(),
    }
}

fn ffmpeg_metadata_value_for_number(
    tags: &[(String, String)],
    number_key: &str,
    total_key: &str,
) -> Option<String> {
    let number = tag_value(tags, number_key)?;
    match tag_value(tags, total_key) {
        Some(total) if !total.trim().is_empty() => Some(format!("{number}/{total}")),
        _ => Some(number.to_string()),
    }
}

fn ffmpeg_authoritative_metadata_tags(tags: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (key, value) in tags {
        match key.as_str() {
            "TRACKNUMBER" => {
                if let Some(track) = ffmpeg_metadata_value_for_number(tags, "TRACKNUMBER", "TOTALTRACKS") {
                    push_tag_value(&mut out, "track", &track);
                }
            }
            "DISCNUMBER" => {
                if let Some(disc) = ffmpeg_metadata_value_for_number(tags, "DISCNUMBER", "TOTALDISCS") {
                    push_tag_value(&mut out, "disc", &disc);
                }
            }
            "TOTALTRACKS" | "TOTALDISCS" => {}
            _ => push_tag_value(&mut out, &ffmpeg_metadata_key(key), value),
        }
    }
    out
}

fn metadata_rewrite_temp_path(path: &Path) -> io::Result<PathBuf> {
    let parent = parent_dir_or_current(path);
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("audio");
    let ext = path.extension().and_then(|value| value.to_str()).unwrap_or("tmp");
    let prefix = format!(".{file_name}.tonepoet-metadata.");
    let suffix = format!(".tmp.{ext}");
    let temp = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(&suffix)
        .tempfile_in(parent)?;
    temp.into_temp_path().keep().map_err(|err| err.error)
}

fn sync_file_before_metadata_replace(path: &Path) -> io::Result<()> {
    let file = fs::OpenOptions::new().read(true).open(path)?;
    file.sync_all()
}

fn sync_parent_dir(path: &Path) -> io::Result<()> {
    let parent = parent_dir_or_current(path);
    let dir = fs::File::open(parent)?;
    dir.sync_all()
}

fn replace_rewritten_metadata_file(path: &Path, tmp: &Path) -> io::Result<()> {
    if !tmp.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("metadata rewrite did not create temporary file: {}", tmp.display()),
        ));
    }
    let metadata = fs::metadata(tmp)?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("metadata rewrite produced an empty or non-file temporary output: {}", tmp.display()),
        ));
    }

    sync_file_before_metadata_replace(tmp)?;
    // The rewrite temp is created in the same directory as the target, so this
    // rename is same-filesystem. On POSIX platforms it atomically replaces an
    // existing target and never exposes a target-absent window.
    fs::rename(tmp, path)?;
    sync_parent_dir(path)
}

const AUTHORITATIVE_CUE_MANAGED_TAG_KEYS: &[&str] = &[
    "TITLE",
    "ARTIST",
    "ALBUMARTIST",
    "ALBUM",
    "GENRE",
    "DATE",
    "TRACKNUMBER",
    "TOTALTRACKS",
    "DISCNUMBER",
    "TOTALDISCS",
    "COMMENT",
    "COMPOSER",
    "PERFORMER",
    "ISRC",
    "PUBLISHER",
    "COPYRIGHT",
    "PRE_EMPHASIS",
    "CUE_FLAGS",
    "CATALOG",
];

const TONEPOET_MANAGED_DYNAMIC_TAG_PREFIXES: &[&str] = &[
    "TONEPOET_ALBUM_",
    "TONEPOET_TRACK_",
];

fn normalize_metadata_key(key: &str) -> String {
    key.trim().to_ascii_uppercase()
}

fn is_tonepoet_managed_dynamic_tag_key(key: &str) -> bool {
    let normalized = normalize_metadata_key(key);
    TONEPOET_MANAGED_DYNAMIC_TAG_PREFIXES
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
}

fn parse_native_tag_keys(text: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for line in text.lines() {
        let Some((key, _)) = line
            .split_once('=')
            .or_else(|| line.split_once(':'))
        else {
            continue;
        };
        let key = normalize_metadata_key(key);
        if key.is_empty() || key == "VENDOR STRING" {
            continue;
        }
        keys.insert(key);
    }
    keys
}

fn native_existing_tag_list_command(path: &Path, ext: &str) -> Option<ToolCommand> {
    let (binary, args) = match ext {
        "flac" => (
            ToolBinary::Metaflac,
            vec!["--export-tags-to=-".to_string(), path.display().to_string()],
        ),
        "opus" | "ogg" => (
            ToolBinary::Opustags,
            vec![path.display().to_string()],
        ),
        "wv" => (
            ToolBinary::Wvtag,
            vec!["--list".to_string(), path.display().to_string()],
        ),
        _ => return None,
    };

    Some(ToolCommand {
        binary,
        args,
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(30),
    })
}

async fn native_existing_tag_keys(
    path: &Path,
    ext: &str,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<BTreeSet<String>, MetadataError> {
    let Some(cmd) = native_existing_tag_list_command(path, ext) else {
        return Ok(BTreeSet::new());
    };

    let output = run_tool_command_with_concurrency(cmd, runner, cancel, tool_concurrency_limits)
        .await
        .map_err(MetadataError::Tool)?;
    Ok(parse_native_tag_keys(&output.stdout_tail))
}

fn authoritative_cue_managed_tag_delete_keys(
    tags: &[(String, String)],
    existing_keys: &BTreeSet<String>,
) -> Vec<String> {
    let mut keys = Vec::new();
    let mut seen = BTreeSet::new();

    for key in AUTHORITATIVE_CUE_MANAGED_TAG_KEYS {
        let key = normalize_metadata_key(key);
        if seen.insert(key.clone()) {
            keys.push(key);
        }
    }

    // Current tags include dynamically modeled CUE extras such as
    // TONEPOET_ALBUM_* and TONEPOET_TRACK_*. They are authoritative for this
    // run and must be rewritten without duplicates.
    for (key, _) in tags {
        let key = normalize_metadata_key(key);
        if seen.insert(key.clone()) {
            keys.push(key);
        }
    }

    // Changed-input convergence requires deleting old Tonepoet-owned dynamic
    // keys that are present on the file but absent from this run's payload.
    // Do not delete arbitrary user tags; only the owned prefixes below are
    // considered managed dynamic CUE metadata.
    for key in existing_keys {
        if is_tonepoet_managed_dynamic_tag_key(key) && seen.insert(key.clone()) {
            keys.push(key.clone());
        }
    }

    keys
}

fn metaflac_tag_args(
    path: &Path,
    tags: &[(String, String)],
    existing_keys: &BTreeSet<String>,
) -> Vec<String> {
    let mut args = Vec::new();
    for key in authoritative_cue_managed_tag_delete_keys(tags, existing_keys) {
        args.push(format!("--remove-tag={key}"));
    }
    for (k, v) in tags {
        args.push(format!("--set-tag={}={}", k, v));
    }
    args.push(path.display().to_string());
    args
}

fn opustags_tag_args(
    path: &Path,
    tags: &[(String, String)],
    existing_keys: &BTreeSet<String>,
) -> Vec<String> {
    let mut args = Vec::new();
    for key in authoritative_cue_managed_tag_delete_keys(tags, existing_keys) {
        args.push("--delete".into());
        args.push(key);
    }
    for (k, v) in tags {
        args.push("-s".into());
        args.push(format!("{}={}", k, v));
    }
    args.push("--in-place".into());
    args.push(path.display().to_string());
    args
}

fn wvtag_tag_args(
    path: &Path,
    tags: &[(String, String)],
    existing_keys: &BTreeSet<String>,
) -> Vec<String> {
    let mut args = vec!["-q".to_string()];
    for key in authoritative_cue_managed_tag_delete_keys(tags, existing_keys) {
        args.push("-d".into());
        args.push(key);
    }
    for (k, v) in tags {
        args.push("-w".into());
        args.push(format!("{}={}", k, v));
    }
    args.push(path.display().to_string());
    args
}

fn ffmpeg_metadata_rewrite_args(
    path: &Path,
    tmp: &Path,
    tags: &[(String, String)],
) -> Vec<String> {
    let mut args = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "error".into(),
        "-i".into(),
        path.display().to_string(),
        "-map".into(),
        "0".into(),
        "-map_metadata".into(),
        "-1".into(),
    ];
    for (k, v) in ffmpeg_authoritative_metadata_tags(tags) {
        args.push("-metadata".into());
        args.push(format!("{}={}", k, v));
    }
    args.push("-c".into());
    args.push("copy".into());
    args.push(tmp.display().to_string());
    args
}

fn metadata_tag_command(
    path: &Path,
    ext: &str,
    tags: &[(String, String)],
    existing_keys: &BTreeSet<String>,
) -> Result<(ToolCommand, Option<PathBuf>), MetadataError> {
    let (binary, args, tmp_path) = match ext {
        "flac" => (ToolBinary::Metaflac, metaflac_tag_args(path, tags, existing_keys), None),
        "opus" | "ogg" => (ToolBinary::Opustags, opustags_tag_args(path, tags, existing_keys), None),
        "wv" => (ToolBinary::Wvtag, wvtag_tag_args(path, tags, existing_keys), None),
        "mp3" | "m4a" | "aac" | "wav" | "aiff" | "aif" => {
            let tmp = metadata_rewrite_temp_path(path)?;
            let args = ffmpeg_metadata_rewrite_args(path, &tmp, tags);
            (ToolBinary::Ffmpeg, args, Some(tmp))
        }
        _ => return Err(MetadataError::UnsupportedTagFormat(ext.to_string())),
    };

    let timeout = match binary {
        ToolBinary::Ffmpeg => Duration::from_secs(60),
        _ => Duration::from_secs(30),
    };

    Ok((
        ToolCommand {
            binary,
            args,
            secret_args: vec![],
            cwd: None,
            env: vec![],
            timeout,
        },
        tmp_path,
    ))
}


fn ffmpeg_artwork_rewrite_args(
    path: &Path,
    artwork: &CueArtworkSidecar,
    tmp: &Path,
    ext: &str,
) -> Vec<String> {
    let mut args = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "error".into(),
        "-i".into(),
        path.display().to_string(),
        "-i".into(),
        artwork.path.display().to_string(),
        "-map".into(),
        "0:a".into(),
        "-map_metadata".into(),
        "0".into(),
        "-map_chapters".into(),
        "0".into(),
        "-map".into(),
        "1:v:0".into(),
        "-c:a".into(),
        "copy".into(),
        "-c:v".into(),
        "copy".into(),
        "-disposition:v:0".into(),
        "attached_pic".into(),
        "-metadata:s:v".into(),
        "title=Album cover".into(),
        "-metadata:s:v".into(),
        "comment=Cover (front)".into(),
    ];

    match ext {
        "mp3" => {
            args.push("-id3v2_version".into());
            args.push("3".into());
        }
        "m4a" | "mp4" => {
            args.push("-f".into());
            args.push("ipod".into());
        }
        _ => {}
    }

    args.push(tmp.display().to_string());
    args
}

fn wvtag_artwork_args(path: &Path, artwork: &CueArtworkSidecar) -> Vec<String> {
    vec![
        "-q".to_string(),
        "-d".to_string(),
        "Cover Art (Front)".to_string(),
        "--write-binary-tag".to_string(),
        format!("Cover Art (Front)=@{}", artwork.path.display()),
        path.display().to_string(),
    ]
}

fn cue_artwork_embed_command(
    path: &Path,
    ext: &str,
    artwork: &CueArtworkSidecar,
) -> Result<Option<(ToolCommand, Option<PathBuf>)>, MetadataError> {
    let (binary, args, tmp_path, timeout) = match ext {
        "flac" | "mp3" | "m4a" | "mp4" => {
            let tmp = metadata_rewrite_temp_path(path)?;
            (
                ToolBinary::Ffmpeg,
                ffmpeg_artwork_rewrite_args(path, artwork, &tmp, ext),
                Some(tmp),
                Duration::from_secs(90),
            )
        }
        "wv" => (
            ToolBinary::Wvtag,
            wvtag_artwork_args(path, artwork),
            None,
            Duration::from_secs(30),
        ),
        // WAV/AIFF artwork conventions are not portable, raw AAC has no MP4
        // cover atom, and Opus/Ogg needs a METADATA_BLOCK_PICTURE writer rather
        // than FFmpeg attached-picture stream-copy. Keep these unsupported
        // cases explicit and non-failing.
        "wav" | "wave" | "aiff" | "aif" | "aac" | "opus" | "ogg" => return Ok(None),
        _ => return Ok(None),
    };

    Ok(Some((
        ToolCommand {
            binary,
            args,
            secret_args: vec![],
            cwd: None,
            env: vec![],
            timeout,
        },
        tmp_path,
    )))
}

async fn embed_cue_artwork_for_file(
    path: &Path,
    artwork: &CueArtworkSidecar,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<(), MetadataError> {
    if !artwork.path.is_file() {
        return Err(MetadataError::UnsupportedTagFormat(format!(
            "CUE artwork sidecar is missing: {}",
            artwork.path.display()
        )));
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let Some((cmd, tmp_path)) = cue_artwork_embed_command(path, &ext, artwork)? else {
        log::warn!(
            "CUE artwork sidecar {} is available, but target {} does not have an implemented post-encode artwork writer on this path",
            artwork.path.display(),
            path.display()
        );
        return Ok(());
    };

    let result = run_tool_command_with_concurrency(cmd, runner, cancel, tool_concurrency_limits)
        .await
        .map_err(MetadataError::Tool);

    if let Err(err) = result {
        if let Some(tmp) = tmp_path.as_ref() {
            let _ = fs::remove_file(tmp);
        }
        return Err(err);
    }

    if let Some(tmp) = tmp_path.as_ref() {
        replace_rewritten_metadata_file(path, tmp)?;
    }

    Ok(())
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

    let tags = authoritative_metadata_tags(meta, album);
    if tags.is_empty() {
        return Ok(());
    }

    let existing_keys = native_existing_tag_keys(
        path,
        &ext,
        runner,
        cancel,
        tool_concurrency_limits,
    )
    .await?;
    let (cmd, tmp_path) = metadata_tag_command(path, &ext, &tags, &existing_keys)?;
    let result = run_tool_command_with_concurrency(cmd, runner, cancel, tool_concurrency_limits)
        .await
        .map_err(MetadataError::Tool);

    if let Err(err) = result {
        if let Some(tmp) = tmp_path.as_ref() {
            let _ = fs::remove_file(tmp);
        }
        return Err(err);
    }

    if let Some(tmp) = tmp_path.as_ref() {
        replace_rewritten_metadata_file(path, tmp)?;
    }

    Ok(())
}

#[cfg(test)]
mod metadata_writer_command_tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::process::Command as ProcessCommand;

    fn assert_pair(args: &[String], left: &str, right: &str) {
        assert!(
            args.windows(2).any(|window| window[0] == left && window[1] == right),
            "missing adjacent args {left:?} {right:?}: {args:?}"
        );
    }

    fn sample_metadata() -> (TrackMetadata, AlbumMetadata) {
        let mut album_extra = BTreeMap::new();
        album_extra.insert("catalog".to_string(), "ABC-123".to_string());
        album_extra.insert("album_tag_override".to_string(), "Cue Album Override".to_string());
        album_extra.insert("label".to_string(), "Example Label".to_string());
        album_extra.insert(CUE_ARTWORK_PATH_EXTRA_KEY.to_string(), "staging/cue-artwork/cover.jpg".to_string());
        album_extra.insert(CUE_ARTWORK_MIME_EXTRA_KEY.to_string(), "image/jpeg".to_string());

        let mut track_extra = BTreeMap::new();
        track_extra.insert("index_00".to_string(), "00:00:32".to_string());

        (
            TrackMetadata {
                title: Some("Cue Track".to_string()),
                artist: Some("Cue Performer".to_string()),
                performer: Some("Cue Performer".to_string()),
                composer: Some("Cue Composer".to_string()),
                genre: Some("Fusion".to_string()),
                date: Some("2026".to_string()),
                track_number: Some(3),
                isrc: Some("USRC17607839".to_string()),
                comment: Some("Cue note".to_string()),
                pre_emphasis: true,
                extra: track_extra,
                ..TrackMetadata::default()
            },
            AlbumMetadata {
                album: Some("Cue Album".to_string()),
                album_artist: Some("Cue Album Artist".to_string()),
                total_tracks: 12,
                total_discs: Some(2),
                disc_number: Some(2),
                extra: album_extra,
                ..AlbumMetadata::default()
            },
        )
    }

    #[test]
    fn authoritative_tags_cover_cue_album_track_and_number_fields() {
        let (track, album) = sample_metadata();
        let tags = authoritative_metadata_tags(&track, &album);

        assert!(tags.contains(&("TITLE".to_string(), "Cue Track".to_string())));
        assert!(tags.contains(&("ARTIST".to_string(), "Cue Performer".to_string())));
        assert!(tags.contains(&("ALBUMARTIST".to_string(), "Cue Album Artist".to_string())));
        assert!(tags.contains(&("ALBUM".to_string(), "Cue Album Override".to_string())));
        assert!(tags.contains(&("TRACKNUMBER".to_string(), "3".to_string())));
        assert!(tags.contains(&("TOTALTRACKS".to_string(), "12".to_string())));
        assert!(tags.contains(&("DISCNUMBER".to_string(), "2".to_string())));
        assert!(tags.contains(&("TOTALDISCS".to_string(), "2".to_string())));
        assert!(tags.contains(&("ISRC".to_string(), "USRC17607839".to_string())));
        assert!(tags.contains(&("CATALOG".to_string(), "ABC-123".to_string())));
        assert!(tags.contains(&("PERFORMER".to_string(), "Cue Performer".to_string())));
        assert!(tags.contains(&("PRE_EMPHASIS".to_string(), "1".to_string())));
        assert!(!tags.iter().any(|(key, _)| key == "TONEPOET_ALBUM_ALBUM_TAG_OVERRIDE"));
        assert!(!tags.iter().any(|(key, _)| key == "TONEPOET_ALBUM_TONEPOET_CUE_ARTWORK_PATH"));
        assert!(!tags.iter().any(|(key, _)| key == "TONEPOET_ALBUM_TONEPOET_CUE_ARTWORK_MIME"));
    }

    #[test]
    fn ffmpeg_tags_use_container_native_track_disc_and_album_artist_keys() {
        let (track, album) = sample_metadata();
        let tags = ffmpeg_authoritative_metadata_tags(&authoritative_metadata_tags(&track, &album));

        assert!(tags.contains(&("title".to_string(), "Cue Track".to_string())));
        assert!(tags.contains(&("album_artist".to_string(), "Cue Album Artist".to_string())));
        assert!(tags.contains(&("track".to_string(), "3/12".to_string())));
        assert!(tags.contains(&("disc".to_string(), "2/2".to_string())));
        assert!(!tags.iter().any(|(key, _)| key == "tracknumber"));
        assert!(!tags.iter().any(|(key, _)| key == "totaltracks"));
    }

    #[test]
    fn ffmpeg_rewrite_args_clear_input_metadata_preserve_streams_and_copy_codecs() {
        let (track, album) = sample_metadata();
        let tags = authoritative_metadata_tags(&track, &album);
        let args = ffmpeg_metadata_rewrite_args(
            Path::new("track.m4a"),
            Path::new(".track.m4a.tmp.m4a"),
            &tags,
        );

        assert_pair(&args, "-map", "0");
        assert_pair(&args, "-map_metadata", "-1");
        assert_pair(&args, "-metadata", "album_artist=Cue Album Artist");
        assert_pair(&args, "-metadata", "track=3/12");
        assert_pair(&args, "-metadata", "disc=2/2");
        assert_pair(&args, "-c", "copy");
        assert_eq!(args.last().map(String::as_str), Some(".track.m4a.tmp.m4a"));
    }

    #[test]
    fn native_managed_dynamic_key_discovery_is_prefix_scoped() {
        let keys = parse_native_tag_keys(
            "TITLE=Old\nTONEPOET_ALBUM_STALE=old\nTONEPOET_TRACK_OLD=old\nUSER_NOTE=keep\nVendor String: ignored\n",
        );
        assert!(keys.contains("TONEPOET_ALBUM_STALE"));
        assert!(keys.contains("TONEPOET_TRACK_OLD"));
        assert!(keys.contains("USER_NOTE"));

        let delete_keys = authoritative_cue_managed_tag_delete_keys(
            &[("TITLE".to_string(), "New".to_string())],
            &keys,
        );
        assert!(delete_keys.contains(&"TONEPOET_ALBUM_STALE".to_string()));
        assert!(delete_keys.contains(&"TONEPOET_TRACK_OLD".to_string()));
        assert!(!delete_keys.contains(&"USER_NOTE".to_string()));
    }

    #[test]
    fn native_writer_commands_delete_full_managed_universe_before_writing_present_values() {
        let tags = vec![
            ("TITLE".to_string(), "Cue Track".to_string()),
            ("ARTIST".to_string(), "Cue Performer".to_string()),
        ];

        let existing = BTreeSet::from([
            "TONEPOET_ALBUM_STALE_DYNAMIC".to_string(),
            "USER_NOTE".to_string(),
        ]);
        let flac = metaflac_tag_args(Path::new("track.flac"), &tags, &existing);
        assert!(flac.iter().any(|arg| arg == "--remove-tag=COMMENT"));
        assert!(flac.iter().any(|arg| arg == "--remove-tag=CATALOG"));
        assert!(flac.iter().any(|arg| arg == "--remove-tag=TONEPOET_ALBUM_STALE_DYNAMIC"));
        assert!(!flac.iter().any(|arg| arg == "--remove-tag=USER_NOTE"));
        assert_pair(&flac, "--set-tag=TITLE=Cue Track", "--set-tag=ARTIST=Cue Performer");
        assert!(
            flac.iter().position(|arg| arg == "--remove-tag=COMMENT").unwrap()
                < flac.iter().position(|arg| arg == "--set-tag=TITLE=Cue Track").unwrap(),
            "metaflac must delete stale managed keys before setting current values"
        );

        let opus = opustags_tag_args(Path::new("track.opus"), &tags, &existing);
        assert_pair(&opus, "--delete", "COMMENT");
        assert_pair(&opus, "--delete", "CATALOG");
        assert_pair(&opus, "--delete", "TONEPOET_ALBUM_STALE_DYNAMIC");
        assert!(!opus.windows(2).any(|pair| pair[0] == "--delete" && pair[1] == "USER_NOTE"));
        assert_pair(&opus, "-s", "TITLE=Cue Track");
        assert!(
            opus.iter().position(|arg| arg == "--delete").unwrap()
                < opus.iter().position(|arg| arg == "-s").unwrap(),
            "opustags must delete stale managed keys before setting current values"
        );

        let wavpack = wvtag_tag_args(Path::new("track.wv"), &tags, &existing);
        assert_pair(&wavpack, "-d", "COMMENT");
        assert_pair(&wavpack, "-d", "CATALOG");
        assert_pair(&wavpack, "-d", "TONEPOET_ALBUM_STALE_DYNAMIC");
        assert!(!wavpack.windows(2).any(|pair| pair[0] == "-d" && pair[1] == "USER_NOTE"));
        assert_pair(&wavpack, "-w", "TITLE=Cue Track");
        assert_pair(&wavpack, "-w", "ARTIST=Cue Performer");
        assert!(
            wavpack.iter().position(|arg| arg == "-d").unwrap()
                < wavpack.iter().position(|arg| arg == "-w").unwrap(),
            "wvtag must delete stale managed keys before setting current values"
        );
    }

    fn executable_on_path(name: &str) -> bool {
        let Some(paths) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
    }

    fn ffmpeg_encoder_available(name: &str) -> bool {
        let Ok(output) = ProcessCommand::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .output()
        else {
            return false;
        };
        if !output.status.success() {
            return false;
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.split_whitespace().any(|field| field == name))
    }

    fn run_checked(tool: &str, args: &[String]) {
        let output = ProcessCommand::new(tool)
            .args(args)
            .output()
            .unwrap_or_else(|err| panic!("failed to run {tool}: {err}"));
        assert!(
            output.status.success(),
            "{tool} failed with status {:?}\nstdout:\n{}\nstderr:\n{}\nargs: {:?}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            args
        );
    }

    fn run_stdout(tool: &str, args: &[String]) -> String {
        let output = ProcessCommand::new(tool)
            .args(args)
            .output()
            .unwrap_or_else(|err| panic!("failed to run {tool}: {err}"));
        assert!(
            output.status.success(),
            "{tool} failed with status {:?}\nstdout:\n{}\nstderr:\n{}\nargs: {:?}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            args
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn tag_line_key_counts(text: &str) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for line in text.lines() {
            let key = line
                .split_once('=')
                .or_else(|| line.split_once(':'))
                .map(|(key, _)| key.trim().to_ascii_uppercase());
            let Some(key) = key else {
                continue;
            };
            if key.is_empty() || key == "VENDOR STRING" {
                continue;
            }
            *counts.entry(key).or_insert(0) += 1;
        }
        counts
    }

    fn tag_line_values(text: &str) -> BTreeMap<String, Vec<String>> {
        let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for line in text.lines() {
            let Some((key, value)) = line
                .split_once('=')
                .or_else(|| line.split_once(':'))
            else {
                continue;
            };
            let key = key.trim().to_ascii_uppercase();
            if key.is_empty() || key == "VENDOR STRING" {
                continue;
            }
            values.entry(key).or_default().push(value.trim().to_string());
        }
        values
    }

    fn assert_single_tag_value(
        values: &BTreeMap<String, Vec<String>>,
        key: &str,
        expected: &str,
    ) {
        let actual = values.get(key).cloned().unwrap_or_default();
        assert_eq!(
            actual,
            vec![expected.to_string()],
            "expected {key} to have exactly one authoritative value"
        );
    }

    fn temp_test_dir(prefix: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after Unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{prefix}-{suffix}"));
        fs::create_dir_all(&dir).expect("create temp test directory");
        dir
    }

    fn create_sine_audio(path: &Path, codec_args: &[&str]) {
        let mut args = vec![
            "-y".to_string(),
            "-hide_banner".to_string(),
            "-nostdin".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-f".to_string(),
            "lavfi".to_string(),
            "-i".to_string(),
            "sine=frequency=1000:sample_rate=44100:duration=0.25".to_string(),
        ];
        args.extend(codec_args.iter().map(|arg| (*arg).to_string()));
        args.push(path.display().to_string());
        run_checked("ffmpeg", &args);
    }

    #[test]
    fn native_flac_writer_clears_stale_managed_keys_on_real_file_when_tools_are_available() {
        if !executable_on_path("ffmpeg")
            || !executable_on_path("metaflac")
            || !ffmpeg_encoder_available("flac")
        {
            eprintln!("skipping real-file FLAC authoritative metadata test; ffmpeg with flac encoder and metaflac are required");
            return;
        }

        let dir = temp_test_dir("tonepoet-metaflac-authoritative");
        let flac_path = dir.join("track.flac");
        create_sine_audio(&flac_path, &["-c:a", "flac"]);

        let seed_tags = vec![
            ("TITLE".to_string(), "Old Cue Track".to_string()),
            ("COMMENT".to_string(), "stale comment".to_string()),
            ("CATALOG".to_string(), "STALE-CATALOG".to_string()),
            ("TONEPOET_ALBUM_OLD_DYNAMIC".to_string(), "stale dynamic".to_string()),
            ("USER_NOTE".to_string(), "preserve me".to_string()),
        ];
        let current_tags = vec![
            ("TITLE".to_string(), "Cue Track".to_string()),
            ("ARTIST".to_string(), "Cue Performer".to_string()),
            ("TONEPOET_TRACK_NEW_DYNAMIC".to_string(), "current dynamic".to_string()),
        ];

        run_checked("metaflac", &metaflac_tag_args(&flac_path, &seed_tags, &BTreeSet::new()));
        let args = metaflac_tag_args(&flac_path, &current_tags, &parse_native_tag_keys(&run_stdout("metaflac", &["--export-tags-to=-".to_string(), flac_path.display().to_string()])));
        run_checked("metaflac", &args);
        run_checked("metaflac", &args);

        let stdout = run_stdout("metaflac", &[
            "--export-tags-to=-".to_string(),
            flac_path.display().to_string(),
        ]);
        let counts = tag_line_key_counts(&stdout);
        assert_eq!(counts.get("TITLE").copied().unwrap_or(0), 1, "TITLE should appear once: {counts:?}");
        assert_eq!(counts.get("ARTIST").copied().unwrap_or(0), 1, "ARTIST should appear once: {counts:?}");
        assert_eq!(counts.get("COMMENT").copied().unwrap_or(0), 0, "stale COMMENT must be cleared: {counts:?}");
        assert_eq!(counts.get("CATALOG").copied().unwrap_or(0), 0, "stale CATALOG must be cleared: {counts:?}");
        assert_eq!(counts.get("TONEPOET_ALBUM_OLD_DYNAMIC").copied().unwrap_or(0), 0, "stale Tonepoet dynamic key must be cleared: {counts:?}");
        assert_eq!(counts.get("TONEPOET_TRACK_NEW_DYNAMIC").copied().unwrap_or(0), 1, "current Tonepoet dynamic key should appear once: {counts:?}");
        assert_eq!(counts.get("USER_NOTE").copied().unwrap_or(0), 1, "unrelated user tag should survive: {counts:?}");
        let values = tag_line_values(&stdout);
        assert_single_tag_value(&values, "TITLE", "Cue Track");
        assert_single_tag_value(&values, "USER_NOTE", "preserve me");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn native_opus_writer_clears_stale_managed_keys_on_real_file_when_tools_are_available() {
        if !executable_on_path("ffmpeg")
            || !executable_on_path("opustags")
            || !ffmpeg_encoder_available("libopus")
        {
            eprintln!("skipping real-file Opus authoritative metadata test; ffmpeg with libopus and opustags are required");
            return;
        }

        let dir = temp_test_dir("tonepoet-opustags-authoritative");
        let opus_path = dir.join("track.opus");
        create_sine_audio(&opus_path, &["-c:a", "libopus", "-b:a", "64k"]);

        let seed_tags = vec![
            ("TITLE".to_string(), "Old Cue Track".to_string()),
            ("COMMENT".to_string(), "stale comment".to_string()),
            ("CATALOG".to_string(), "STALE-CATALOG".to_string()),
            ("TONEPOET_ALBUM_OLD_DYNAMIC".to_string(), "stale dynamic".to_string()),
            ("USER_NOTE".to_string(), "preserve me".to_string()),
        ];
        let current_tags = vec![
            ("TITLE".to_string(), "Cue Track".to_string()),
            ("ARTIST".to_string(), "Cue Performer".to_string()),
            ("TONEPOET_TRACK_NEW_DYNAMIC".to_string(), "current dynamic".to_string()),
        ];

        run_checked("opustags", &opustags_tag_args(&opus_path, &seed_tags, &BTreeSet::new()));
        let args = opustags_tag_args(&opus_path, &current_tags, &parse_native_tag_keys(&run_stdout("opustags", &[opus_path.display().to_string()])));
        run_checked("opustags", &args);
        run_checked("opustags", &args);

        let stdout = run_stdout("opustags", &[opus_path.display().to_string()]);
        let counts = tag_line_key_counts(&stdout);
        assert_eq!(counts.get("TITLE").copied().unwrap_or(0), 1, "TITLE should appear once: {counts:?}");
        assert_eq!(counts.get("ARTIST").copied().unwrap_or(0), 1, "ARTIST should appear once: {counts:?}");
        assert_eq!(counts.get("COMMENT").copied().unwrap_or(0), 0, "stale COMMENT must be cleared: {counts:?}");
        assert_eq!(counts.get("CATALOG").copied().unwrap_or(0), 0, "stale CATALOG must be cleared: {counts:?}");
        assert_eq!(counts.get("TONEPOET_ALBUM_OLD_DYNAMIC").copied().unwrap_or(0), 0, "stale Tonepoet dynamic key must be cleared: {counts:?}");
        assert_eq!(counts.get("TONEPOET_TRACK_NEW_DYNAMIC").copied().unwrap_or(0), 1, "current Tonepoet dynamic key should appear once: {counts:?}");
        assert_eq!(counts.get("USER_NOTE").copied().unwrap_or(0), 1, "unrelated user tag should survive: {counts:?}");
        let values = tag_line_values(&stdout);
        assert_single_tag_value(&values, "TITLE", "Cue Track");
        assert_single_tag_value(&values, "USER_NOTE", "preserve me");

        let _ = fs::remove_dir_all(&dir);
    }

    fn read_le_u32(bytes: &[u8], offset: usize) -> usize {
        u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("four-byte little-endian field"),
        ) as usize
    }

    fn apev2_item_key_counts(bytes: &[u8]) -> BTreeMap<String, usize> {
        let footer = bytes
            .windows(8)
            .rposition(|window| window == b"APETAGEX")
            .expect("WavPack file should contain an APEv2 tag footer");
        assert!(footer + 32 <= bytes.len(), "truncated APEv2 footer");

        let tag_size = read_le_u32(bytes, footer + 12);
        let item_count = read_le_u32(bytes, footer + 16);
        assert!(tag_size >= 32, "APEv2 tag size must include at least the footer");
        assert!(tag_size <= footer + 32, "APEv2 tag size points before start of file");

        let mut pos = footer + 32 - tag_size;
        if pos + 32 <= footer && &bytes[pos..pos + 8] == b"APETAGEX" {
            pos += 32;
        }

        let mut counts = BTreeMap::new();
        for _ in 0..item_count {
            assert!(pos + 8 <= footer, "truncated APEv2 item header");
            let value_size = read_le_u32(bytes, pos);
            pos += 8;

            let key_end = bytes[pos..footer]
                .iter()
                .position(|byte| *byte == 0)
                .map(|relative| pos + relative)
                .expect("APEv2 item key should be NUL-terminated");
            let key = String::from_utf8_lossy(&bytes[pos..key_end]).to_ascii_uppercase();
            pos = key_end + 1;
            assert!(pos + value_size <= footer, "truncated APEv2 item value");
            pos += value_size;

            *counts.entry(key).or_insert(0) += 1;
        }

        counts
    }

    #[test]
    fn wavpack_managed_metadata_is_duplicate_free_on_real_file_when_tools_are_available() {
        if !executable_on_path("ffmpeg")
            || !executable_on_path("wvtag")
            || !ffmpeg_encoder_available("wavpack")
        {
            eprintln!("skipping real-file WavPack metadata idempotency test; ffmpeg with wavpack encoder and wvtag are required");
            return;
        }

        let dir = temp_test_dir("tonepoet-wvtag-authoritative");
        let wavpack_path = dir.join("track.wv");
        create_sine_audio(&wavpack_path, &["-c:a", "wavpack"]);

        let seed_tags = vec![
            ("TITLE".to_string(), "Old Cue Track".to_string()),
            ("COMMENT".to_string(), "stale comment".to_string()),
            ("CATALOG".to_string(), "STALE-CATALOG".to_string()),
            ("TONEPOET_ALBUM_OLD_DYNAMIC".to_string(), "stale dynamic".to_string()),
            ("USER_NOTE".to_string(), "preserve me".to_string()),
        ];
        let current_tags = vec![
            ("TITLE".to_string(), "Cue Track".to_string()),
            ("ARTIST".to_string(), "Cue Performer".to_string()),
            ("TONEPOET_TRACK_NEW_DYNAMIC".to_string(), "current dynamic".to_string()),
        ];

        run_checked("wvtag", &wvtag_tag_args(&wavpack_path, &seed_tags, &BTreeSet::new()));
        let wvtag_args = wvtag_tag_args(&wavpack_path, &current_tags, &apev2_item_key_counts(&fs::read(&wavpack_path).expect("read tagged WavPack file")).keys().cloned().collect::<BTreeSet<_>>());
        run_checked("wvtag", &wvtag_args);
        run_checked("wvtag", &wvtag_args);

        let bytes = fs::read(&wavpack_path).expect("read tagged WavPack file");
        let counts = apev2_item_key_counts(&bytes);
        for key in ["TITLE", "ARTIST"] {
            assert_eq!(
                counts.get(key).copied().unwrap_or(0),
                1,
                "managed key {key} must appear exactly once in the final APEv2 tag: {counts:?}"
            );
        }
        for key in ["COMMENT", "CATALOG", "TONEPOET_ALBUM_OLD_DYNAMIC"] {
            assert_eq!(
                counts.get(key).copied().unwrap_or(0),
                0,
                "stale managed key {key} must be cleared from the final APEv2 tag: {counts:?}"
            );
        }
        assert_eq!(
            counts.get("TONEPOET_TRACK_NEW_DYNAMIC").copied().unwrap_or(0),
            1,
            "current Tonepoet dynamic key should appear once: {counts:?}"
        );
        assert_eq!(
            counts.get("USER_NOTE").copied().unwrap_or(0),
            1,
            "unrelated user tag should survive: {counts:?}"
        );
        let stdout = run_stdout("wvtag", &["-l".to_string(), wavpack_path.display().to_string()]);
        let values = tag_line_values(&stdout);
        assert_single_tag_value(&values, "TITLE", "Cue Track");
        assert_single_tag_value(&values, "USER_NOTE", "preserve me");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ffmpeg_metadata_command_uses_random_same_directory_sidecar_temp_file_for_replacement() {
        let dir = temp_test_dir("tonepoet-metadata-temp");
        let path = dir.join("track.mp3");
        let (track, album) = sample_metadata();
        let tags = authoritative_metadata_tags(&track, &album);
        let (cmd, tmp) = metadata_tag_command(&path, "mp3", &tags, &BTreeSet::new())
            .expect("mp3 metadata command");
        let tmp = tmp.expect("ffmpeg metadata rewrite must use a temp file");

        assert!(matches!(cmd.binary, ToolBinary::Ffmpeg));
        assert_eq!(tmp.parent(), Some(dir.as_path()));
        assert!(matches!(
            tmp.extension().and_then(|ext| ext.to_str()),
            Some("mp3")
        ));
        assert!(
            tmp.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".tonepoet-metadata.") && name.ends_with(".tmp.mp3")),
            "temp path should use a random same-directory rewrite name with the target extension: {}",
            tmp.display()
        );
        assert_pair(&cmd.args, "-map_metadata", "-1");
        assert_pair(&cmd.args, "-c", "copy");

        let _ = fs::remove_file(&tmp);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ffmpeg_metadata_temp_paths_are_collision_resistant() {
        let dir = temp_test_dir("tonepoet-metadata-temp-unique");
        let path = dir.join("track.m4a");
        let first = metadata_rewrite_temp_path(&path).expect("first temp path");
        let second = metadata_rewrite_temp_path(&path).expect("second temp path");

        assert_ne!(first, second, "metadata rewrite temp paths must not be pid-deterministic");
        assert_eq!(first.parent(), Some(dir.as_path()));
        assert_eq!(second.parent(), Some(dir.as_path()));
        assert!(first.file_name().and_then(|name| name.to_str()).unwrap_or_default().ends_with(".tmp.m4a"));
        assert!(second.file_name().and_then(|name| name.to_str()).unwrap_or_default().ends_with(".tmp.m4a"));

        let _ = fs::remove_file(first);
        let _ = fs::remove_file(second);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn metadata_rewrite_replaces_target_atomically_and_syncs_visible_state() {
        let dir = temp_test_dir("tonepoet-metadata-replace");
        let target = dir.join("track.mp3");
        fs::write(&target, b"old audio").expect("write old target");
        let tmp = metadata_rewrite_temp_path(&target).expect("temp path");
        fs::write(&tmp, b"new audio").expect("write rewritten temp");

        replace_rewritten_metadata_file(&target, &tmp).expect("replace rewritten file");

        assert_eq!(fs::read(&target).expect("read replaced target"), b"new audio");
        assert!(!tmp.exists(), "temp file should be consumed by replacement");
        assert!(
            fs::read_dir(&dir)
                .expect("read temp dir")
                .all(|entry| !entry.expect("dir entry").file_name().to_string_lossy().contains(".bak")),
            "metadata replacement should not create backup windows or backup files"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn metadata_rewrite_rejects_empty_temporary_output() {
        let dir = temp_test_dir("tonepoet-metadata-empty-temp");
        let target = dir.join("track.mp3");
        fs::write(&target, b"old audio").expect("write old target");
        let tmp = metadata_rewrite_temp_path(&target).expect("temp path");

        let err = replace_rewritten_metadata_file(&target, &tmp)
            .expect_err("empty metadata rewrite temp must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&target).expect("old target should remain"), b"old audio");

        let _ = fs::remove_file(tmp);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cue_artwork_embedding_commands_replace_existing_artwork_by_container() {
        let dir = temp_test_dir("tonepoet-artwork-command");
        let artwork = CueArtworkSidecar {
            path: PathBuf::from("cover.jpg"),
            mime_type: Some("image/jpeg".to_string()),
        };

        for (target, ext) in [("track.flac", "flac"), ("track.mp3", "mp3"), ("track.m4a", "m4a")] {
            let path = dir.join(target);
            let (cmd, tmp) = cue_artwork_embed_command(&path, ext, &artwork)
                .expect("artwork command allocation should succeed")
                .expect("container supports post-encode CUE artwork embedding");
            assert!(matches!(cmd.binary, ToolBinary::Ffmpeg));
            let tmp = tmp.expect("FFmpeg artwork embedding must use sidecar temp replacement");
            assert_eq!(tmp.parent(), Some(dir.as_path()));
            assert!(
                tmp.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(".tonepoet-metadata.") && name.ends_with(&format!(".tmp.{ext}"))),
                "artwork rewrite temp should preserve target extension: {}",
                tmp.display()
            );
            assert_pair(&cmd.args, "-map", "0:a");
            assert_pair(&cmd.args, "-map", "1:v:0");
            assert!(!cmd.args.windows(2).any(|pair| pair[0] == "-map" && pair[1] == "0:v"));
            assert_pair(&cmd.args, "-disposition:v:0", "attached_pic");
            assert_pair(&cmd.args, "-c:a", "copy");
            assert_pair(&cmd.args, "-c:v", "copy");
            let _ = fs::remove_file(tmp);
        }

        let (m4a_cmd, m4a_tmp) = cue_artwork_embed_command(&dir.join("track.m4a"), "m4a", &artwork)
            .expect("m4a artwork command allocation")
            .expect("m4a artwork command");
        assert_pair(&m4a_cmd.args, "-f", "ipod");
        if let Some(tmp) = m4a_tmp { let _ = fs::remove_file(tmp); }

        let (mp3_cmd, mp3_tmp) = cue_artwork_embed_command(&dir.join("track.mp3"), "mp3", &artwork)
            .expect("mp3 artwork command allocation")
            .expect("mp3 artwork command");
        assert_pair(&mp3_cmd.args, "-id3v2_version", "3");
        if let Some(tmp) = mp3_tmp { let _ = fs::remove_file(tmp); }

        let (wv_cmd, wv_tmp) = cue_artwork_embed_command(&dir.join("track.wv"), "wv", &artwork)
            .expect("WavPack artwork command allocation")
            .expect("WavPack artwork command");
        assert!(matches!(wv_cmd.binary, ToolBinary::Wvtag));
        assert!(wv_tmp.is_none());
        assert_pair(&wv_cmd.args, "-d", "Cover Art (Front)");
        assert_pair(&wv_cmd.args, "--write-binary-tag", "Cover Art (Front)=@cover.jpg");

        for ext in ["wav", "aiff", "aif", "aac", "opus", "ogg"] {
            assert!(
                cue_artwork_embed_command(&dir.join(format!("track.{ext}")), ext, &artwork)
                    .expect("unsupported artwork command allocation should not fail")
                    .is_none(),
                "{ext} artwork must stay explicitly unsupported on this path"
            );
        }

        let _ = fs::remove_dir_all(&dir);
    }

}

#[cfg(test)]
mod cue_real_output_matrix_tests {
    use super::*;
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::process::Command as ProcessCommand;

    const EXPECTED_DURATION_SECONDS: f64 = 1.50;
    const LOSSY_DURATION_TOLERANCE_SECONDS: f64 = 0.15;

    #[derive(Clone)]
    struct MatrixCase {
        name: &'static str,
        format: tonepoet_pipeline::AudioFormat,
        extension: &'static str,
        container_contains: &'static [&'static str],
        codec: &'static str,
        required_encoder: Option<&'static str>,
        required_taggers: &'static [&'static str],
        artwork_supported: ArtworkExpectation,
        supports_album_artist: bool,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ArtworkExpectation {
        EmbeddedPicture,
        WavPackApeBinary,
        Unsupported,
    }

    fn matrix_cases() -> Vec<MatrixCase> {
        vec![
            MatrixCase {
                name: "cue_to_flac",
                format: tonepoet_pipeline::AudioFormat::Flac,
                extension: "flac",
                container_contains: &["flac"],
                codec: "flac",
                required_encoder: Some("flac"),
                required_taggers: &["metaflac"],
                artwork_supported: ArtworkExpectation::EmbeddedPicture,
                supports_album_artist: true,
            },
            MatrixCase {
                name: "cue_to_wav",
                format: tonepoet_pipeline::AudioFormat::Wav,
                extension: "wav",
                container_contains: &["wav"],
                codec: "pcm_s16le",
                required_encoder: Some("pcm_s16le"),
                required_taggers: &[],
                artwork_supported: ArtworkExpectation::Unsupported,
                supports_album_artist: false,
            },
            MatrixCase {
                name: "cue_to_wavpack",
                format: tonepoet_pipeline::AudioFormat::WavPack,
                extension: "wv",
                container_contains: &["wv", "wavpack"],
                codec: "wavpack",
                required_encoder: Some("wavpack"),
                required_taggers: &["wvtag"],
                artwork_supported: ArtworkExpectation::WavPackApeBinary,
                supports_album_artist: true,
            },
            MatrixCase {
                name: "cue_to_opus",
                format: tonepoet_pipeline::AudioFormat::Opus,
                extension: "opus",
                container_contains: &["ogg"],
                codec: "opus",
                required_encoder: Some("libopus"),
                required_taggers: &["opustags"],
                artwork_supported: ArtworkExpectation::Unsupported,
                supports_album_artist: true,
            },
            MatrixCase {
                name: "cue_to_aac_m4a",
                format: tonepoet_pipeline::AudioFormat::Aac,
                extension: "m4a",
                container_contains: &["mov", "mp4", "m4a", "3gp", "3g2", "mj2"],
                codec: "aac",
                required_encoder: Some("libfdk_aac"),
                required_taggers: &[],
                artwork_supported: ArtworkExpectation::EmbeddedPicture,
                supports_album_artist: true,
            },
            MatrixCase {
                name: "cue_to_mp3",
                format: tonepoet_pipeline::AudioFormat::Mp3,
                extension: "mp3",
                container_contains: &["mp3"],
                codec: "mp3",
                required_encoder: Some("libmp3lame"),
                required_taggers: &[],
                artwork_supported: ArtworkExpectation::EmbeddedPicture,
                supports_album_artist: true,
            },
            MatrixCase {
                name: "cue_to_alac_m4a",
                format: tonepoet_pipeline::AudioFormat::Alac,
                extension: "m4a",
                container_contains: &["mov", "mp4", "m4a", "3gp", "3g2", "mj2"],
                codec: "alac",
                required_encoder: Some("alac"),
                required_taggers: &[],
                artwork_supported: ArtworkExpectation::EmbeddedPicture,
                supports_album_artist: true,
            },
        ]
    }

    fn executable_on_path(name: &str) -> bool {
        let Some(paths) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
    }

    fn ffmpeg_encoder_available(name: &str) -> bool {
        let Ok(output) = ProcessCommand::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .output()
        else {
            return false;
        };
        output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.split_whitespace().any(|field| field == name))
    }

    fn env_flag_enabled(value: Option<&str>) -> bool {
        value
            .map(|raw| matches!(raw.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
    }

    fn cue_matrix_strict_mode() -> bool {
        env_flag_enabled(std::env::var("TONEPOET_CUE_MATRIX_STRICT").ok().as_deref())
    }

    fn case_unavailability_reasons(case: &MatrixCase) -> Vec<String> {
        let mut reasons = Vec::new();
        if !executable_on_path("ffmpeg") {
            reasons.push("missing ffmpeg executable".to_string());
        }
        if !executable_on_path("ffprobe") {
            reasons.push("missing ffprobe executable".to_string());
        }
        if executable_on_path("ffmpeg") {
            if let Some(encoder) = case.required_encoder {
                if !ffmpeg_encoder_available(encoder) {
                    reasons.push(format!("missing ffmpeg encoder {encoder}"));
                }
            }
        }
        for tool in case.required_taggers {
            if !executable_on_path(tool) {
                reasons.push(format!("missing {tool} executable"));
            }
        }
        reasons
    }


    fn run_output(tool: &str, args: &[String]) -> String {
        let output = ProcessCommand::new(tool)
            .args(args)
            .output()
            .unwrap_or_else(|err| panic!("failed to run {tool}: {err}"));
        assert!(
            output.status.success(),
            "{tool} failed with status {:?}\nstdout:\n{}\nstderr:\n{}\nargs: {:?}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            args
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn run_checked(tool: &str, args: &[String]) {
        let _ = run_output(tool, args);
    }

    fn create_fixture_image_and_cue(dir: &Path) -> (PathBuf, PathBuf) {
        let cover = dir.join("cover.jpg");
        run_checked(
            "ffmpeg",
            &[
                "-y".to_string(),
                "-hide_banner".to_string(),
                "-nostdin".to_string(),
                "-loglevel".to_string(),
                "error".to_string(),
                "-f".to_string(),
                "lavfi".to_string(),
                "-i".to_string(),
                "color=c=red:s=64x64:d=0.10".to_string(),
                "-frames:v".to_string(),
                "1".to_string(),
                cover.display().to_string(),
            ],
        );

        let image = dir.join("album.flac");
        run_checked(
            "ffmpeg",
            &[
                "-y".to_string(),
                "-hide_banner".to_string(),
                "-nostdin".to_string(),
                "-loglevel".to_string(),
                "error".to_string(),
                "-f".to_string(),
                "lavfi".to_string(),
                "-i".to_string(),
                format!("sine=frequency=440:sample_rate=44100:duration={EXPECTED_DURATION_SECONDS}"),
                "-i".to_string(),
                cover.display().to_string(),
                "-map".to_string(),
                "0:a:0".to_string(),
                "-map".to_string(),
                "1:v:0".to_string(),
                "-c:a".to_string(),
                "flac".to_string(),
                "-sample_fmt".to_string(),
                "s16".to_string(),
                "-c:v".to_string(),
                "copy".to_string(),
                "-disposition:v:0".to_string(),
                "attached_pic".to_string(),
                image.display().to_string(),
            ],
        );

        let cue = dir.join("album.cue");
        std::fs::write(
            &cue,
            r#"REM GENRE "Fusion"
REM DATE "2026"
REM CATALOG ABC1234567890
PERFORMER "Cue Album Artist"
TITLE "Cue Album"
FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Cue Track"
    PERFORMER "Cue Artist"
    ISRC USRC17607839
    INDEX 01 00:00:00
"#,
        )
        .expect("write CUE sheet");

        (image, cue)
    }

    fn request_for_case(root: &Path, image: &Path, case: &MatrixCase) -> PipelineRequest {
        let mut settings = tonepoet_pipeline::PipelineSettings::default();
        settings.target_format = case.format.clone();
        settings.force_encode = true;
        settings.metadata.transfer_tags = false;
        settings.metadata.preserve_artwork = true;
        settings.metadata.store_source_audio_md5 = false;
        settings.target_bit_depth = tonepoet_pipeline::BitDepthTarget::Pcm(tonepoet_pipeline::PcmBitDepth::Int16);

        PipelineRequest {
            job_id: format!("matrix-{}", case.name),
            item_id: format!("matrix-{}", case.name),
            container: image.to_path_buf(),
            source: SourceOptions {
                archive_password: None,
                sacd_area: Some(SacdArea::Stereo),
                dvda_group_selection: DvdaGroupSelection::Default,
                dvda_group: None,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection: TrackSelection::All,
            },
            settings,
            worker_count: Some(1),
            merge: false,
            output_root: root.join("out"),
            naming: NamingPolicy {
                template: "%NN% - %TITLE%".to_string(),
                folder_template: None,
                per_album_subdir: false,
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
            },
            stages: StagePolicy {
                metadata: StageRequirement::Enabled,
                replaygain: StageRequirement::Disabled,
                features: StageRequirement::Disabled,
                generate_cue: false,
            },
            failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
            container_extension: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }

    fn ffprobe_json(path: &Path) -> Value {
        let stdout = run_output(
            "ffprobe",
            &[
                "-v".to_string(),
                "error".to_string(),
                "-show_streams".to_string(),
                "-show_format".to_string(),
                "-of".to_string(),
                "json".to_string(),
                path.display().to_string(),
            ],
        );
        serde_json::from_str(&stdout).expect("ffprobe JSON should parse")
    }

    fn audio_stream<'a>(probe: &'a Value) -> &'a Value {
        probe["streams"]
            .as_array()
            .and_then(|streams| {
                streams.iter().find(|stream| {
                    stream["codec_type"].as_str() == Some("audio")
                })
            })
            .expect("output should contain an audio stream")
    }

    fn format_tag_map(probe: &Value) -> BTreeMap<String, String> {
        let mut tags = BTreeMap::new();
        // Format-level tags (most containers: FLAC, WAV, MP3, M4A, etc.)
        if let Some(obj) = probe.pointer("/format/tags").and_then(|value| value.as_object()) {
            for (key, value) in obj {
                if let Some(value) = value.as_str() {
                    tags.insert(key.to_ascii_uppercase(), value.to_string());
                }
            }
        }
        // Stream-level tags (OGG/Opus stores Vorbis comments on the stream)
        if let Some(streams) = probe.get("streams").and_then(|value| value.as_array()) {
            for stream in streams {
                if stream["codec_type"].as_str() == Some("audio") {
                    if let Some(obj) = stream.get("tags").and_then(|value| value.as_object()) {
                        for (key, value) in obj {
                            if let Some(value) = value.as_str() {
                                tags.entry(key.to_ascii_uppercase()).or_insert_with(|| value.to_string());
                            }
                        }
                    }
                }
            }
        }
        tags
    }

    fn attached_picture_count(probe: &Value) -> usize {
        probe["streams"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|stream| stream["codec_type"].as_str() == Some("video"))
            .filter(|stream| {
                stream
                    .pointer("/disposition/attached_pic")
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0)
                    == 1
            })
            .count()
    }

    fn assert_matrix_duration_or_sample_count(case: &MatrixCase, audio: &Value, probe: &Value) {
        let sample_rate = audio["sample_rate"]
            .as_str()
            .and_then(|value| value.parse::<u32>().ok())
            .expect("ffprobe should expose audio sample_rate");
        let expected_samples = (EXPECTED_DURATION_SECONDS * f64::from(sample_rate)).round() as u64;

        if case.format.is_pcm_lossless() {
            if let Some(actual_samples) = samples_from_stream_duration_ts(audio, sample_rate) {
                assert_eq!(
                    actual_samples,
                    expected_samples,
                    "{} lossless output sample count drifted: expected {expected_samples}, got {actual_samples}",
                    case.name
                );
                return;
            }

            let duration = probed_duration_seconds(audio, probe);
            let actual_samples = (duration * f64::from(sample_rate)).round() as u64;
            let allowed = (sample_rate / 1000).max(1) as u64;
            let delta = actual_samples.abs_diff(expected_samples);
            assert!(
                delta <= allowed,
                "{} lossless output duration-only sample drift too large: expected {expected_samples} samples ({EXPECTED_DURATION_SECONDS}s), got {actual_samples} samples ({duration}s), allowed {allowed} samples",
                case.name
            );
        } else {
            let duration = probed_duration_seconds(audio, probe);
            let delta = (duration - EXPECTED_DURATION_SECONDS).abs();
            assert!(
                delta <= LOSSY_DURATION_TOLERANCE_SECONDS,
                "{} lossy output duration drift too large: expected {EXPECTED_DURATION_SECONDS}, got {duration}, delta {delta}",
                case.name
            );
        }
    }

    fn probed_duration_seconds(audio: &Value, probe: &Value) -> f64 {
        audio["duration"]
            .as_str()
            .and_then(|value| value.parse::<f64>().ok())
            .or_else(|| {
                probe
                    .pointer("/format/duration")
                    .and_then(|value| value.as_str())
                    .and_then(|value| value.parse::<f64>().ok())
            })
            .expect("ffprobe should expose duration")
    }

    fn assert_container_codec_duration_and_metadata(case: &MatrixCase, path: &Path, probe: &Value) {
        assert!(path.exists(), "{} output should exist: {}", case.name, path.display());
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some(case.extension),
            "{} output extension should match the selected container",
            case.name
        );
        let format_name = probe
            .pointer("/format/format_name")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        assert!(
            case.container_contains.iter().any(|needle| format_name.contains(needle)),
            "{} container mismatch: expected one of {:?}, got {format_name:?}",
            case.name,
            case.container_contains
        );
        let audio = audio_stream(probe);
        assert_eq!(
            audio["codec_name"].as_str(),
            Some(case.codec),
            "{} audio codec mismatch",
            case.name
        );
        assert_matrix_duration_or_sample_count(case, audio, probe);

        let tags = format_tag_map(probe);
        assert_tag_value(&tags, "TITLE", "Cue Track", case.name);
        assert_tag_value(&tags, "ARTIST", "Cue Artist", case.name);
        assert_tag_value(&tags, "ALBUM", "Cue Album", case.name);
        if case.supports_album_artist {
            assert_any_tag_value(&tags, &["ALBUMARTIST", "ALBUM_ARTIST"], "Cue Album Artist", case.name);
        }
        assert_any_tag_value(&tags, &["TRACKNUMBER", "TRACK"], "1", case.name);
        assert_any_tag_value(&tags, &["DATE", "YEAR"], "2026", case.name);
        assert_tag_value(&tags, "GENRE", "Fusion", case.name);
    }

    fn assert_tag_value(tags: &BTreeMap<String, String>, key: &str, expected: &str, case_name: &str) {
        let actual = tags.get(key).unwrap_or_else(|| {
            panic!("{case_name} missing metadata key {key}; tags were {tags:?}")
        });
        assert!(
            actual.as_str() == expected || actual.split('/').next() == Some(expected),
            "{case_name} metadata key {key} mismatch: expected {expected:?}, got {actual:?}"
        );
    }

    fn assert_any_tag_value(
        tags: &BTreeMap<String, String>,
        keys: &[&str],
        expected: &str,
        case_name: &str,
    ) {
        for key in keys {
            if let Some(actual) = tags.get(*key) {
                assert!(
                    actual.as_str() == expected || actual.split('/').next() == Some(expected),
                    "{case_name} metadata key {key} mismatch: expected {expected:?}, got {actual:?}"
                );
                return;
            }
        }
        panic!("{case_name} missing any of metadata keys {keys:?}; tags were {tags:?}");
    }

    fn assert_artwork_behavior(case: &MatrixCase, path: &Path, probe: &Value) {
        match case.artwork_supported {
            ArtworkExpectation::EmbeddedPicture => {
                assert_eq!(
                    attached_picture_count(probe),
                    1,
                    "{} should contain exactly one attached picture after repeated metadata/artwork stages",
                    case.name
                );
            }
            ArtworkExpectation::WavPackApeBinary => {
                let counts = apev2_item_key_counts(&std::fs::read(path).expect("read WavPack output"));
                assert_eq!(
                    counts.get("COVER ART (FRONT)").copied().unwrap_or(0),
                    1,
                    "{} should contain exactly one managed WavPack cover-art tag after repeated metadata/artwork stages: {counts:?}",
                    case.name
                );
            }
            ArtworkExpectation::Unsupported => {
                assert_eq!(
                    attached_picture_count(probe),
                    0,
                    "{} has explicitly unsupported artwork and should not accidentally preserve carrier/source artwork",
                    case.name
                );
            }
        }
    }

    fn assert_native_tag_duplicate_free(case: &MatrixCase, path: &Path) {
        match &case.format {
            tonepoet_pipeline::AudioFormat::Flac => {
                let stdout = run_output("metaflac", &["--export-tags-to=-".to_string(), path.display().to_string()]);
                let counts = key_value_line_counts(&stdout);
                assert_managed_key_counts_once(case.name, &counts, &["TITLE", "ARTIST", "ALBUM", "GENRE", "DATE", "TRACKNUMBER"]);
            }
            tonepoet_pipeline::AudioFormat::Opus => {
                let stdout = run_output("opustags", &[path.display().to_string()]);
                let counts = key_value_line_counts(&stdout);
                assert_managed_key_counts_once(case.name, &counts, &["TITLE", "ARTIST", "ALBUM", "GENRE", "DATE", "TRACKNUMBER"]);
            }
            tonepoet_pipeline::AudioFormat::WavPack => {
                let counts = apev2_item_key_counts(&std::fs::read(path).expect("read WavPack output"));
                assert_managed_key_counts_once(case.name, &counts, &["TITLE", "ARTIST", "ALBUM", "GENRE", "DATE", "TRACKNUMBER"]);
            }
            tonepoet_pipeline::AudioFormat::Mp3 => {
                let counts = id3v2_frame_counts(&std::fs::read(path).expect("read MP3 output"));
                assert_native_key_counts_at_most_once(
                    case.name,
                    &counts,
                    &[
                        "TIT2", "TPE1", "TPE2", "TALB", "TCON", "TRCK", "TPOS", "TDRC", "TYER",
                        "TCOM", "TPE3", "TSRC", "TPUB", "TCOP", "COMM",
                    ],
                );
                assert_managed_key_counts_once(case.name, &counts, &["TIT2", "TPE1", "TALB", "TCON", "TRCK"]);
                if case.artwork_supported == ArtworkExpectation::EmbeddedPicture {
                    assert_eq!(
                        counts.get("APIC").copied().unwrap_or(0),
                        1,
                        "{} should contain exactly one ID3 APIC frame after repeated artwork rewrites: {counts:?}",
                        case.name
                    );
                }
            }
            tonepoet_pipeline::AudioFormat::Aac | tonepoet_pipeline::AudioFormat::Alac => {
                let counts = mp4_ilst_atom_counts(&std::fs::read(path).expect("read MP4/M4A output"));
                assert_native_key_counts_at_most_once(
                    case.name,
                    &counts,
                    &["A9nam", "A9ART", "aART", "A9alb", "A9gen", "A9day", "trkn", "disk", "desc", "cprt", "covr"],
                );
                assert_managed_key_counts_once(case.name, &counts, &["A9nam", "A9ART", "A9alb", "A9gen", "A9day", "trkn"]);
                if case.artwork_supported == ArtworkExpectation::EmbeddedPicture {
                    assert_eq!(
                        counts.get("covr").copied().unwrap_or(0),
                        1,
                        "{} should contain exactly one MP4 covr atom after repeated artwork rewrites: {counts:?}",
                        case.name
                    );
                }
            }
            tonepoet_pipeline::AudioFormat::Wav => {
                let counts = riff_info_key_counts(&std::fs::read(path).expect("read WAV output"));
                assert_native_key_counts_at_most_once(
                    case.name,
                    &counts,
                    &["INAM", "IART", "IPRD", "IGNR", "ICRD", "IPRT", "ICMT", "ISBJ", "ICOP"],
                );
                assert_managed_key_counts_once(case.name, &counts, &["INAM", "IART", "IPRD", "IGNR", "ICRD"]);
            }
            tonepoet_pipeline::AudioFormat::Aiff => {
                let counts = aiff_metadata_chunk_counts(&std::fs::read(path).expect("read AIFF output"));
                assert_native_key_counts_at_most_once(case.name, &counts, &["NAME", "AUTH", "ANNO", "(C) ", "ID3 "]);
            }
            _ => {}
        }
    }

    fn key_value_line_counts(text: &str) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for line in text.lines() {
            let Some((key, _value)) = line.split_once('=') else {
                continue;
            };
            *counts.entry(key.trim().to_ascii_uppercase()).or_insert(0) += 1;
        }
        counts
    }

    fn assert_managed_key_counts_once(case_name: &str, counts: &BTreeMap<String, usize>, keys: &[&str]) {
        for key in keys {
            assert_eq!(
                counts.get(*key).copied().unwrap_or(0),
                1,
                "{case_name} managed key {key} should appear exactly once after repeated metadata stage runs: {counts:?}"
            );
        }
    }
    fn assert_native_key_counts_at_most_once(case_name: &str, counts: &BTreeMap<String, usize>, keys: &[&str]) {
        for key in keys {
            assert!(
                counts.get(*key).copied().unwrap_or(0) <= 1,
                "{case_name} native metadata key {key} should not be duplicated after repeated metadata stage runs: {counts:?}"
            );
        }
    }

    fn read_be_u32(bytes: &[u8], offset: usize) -> usize {
        u32::from_be_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("four-byte big-endian field"),
        ) as usize
    }

    fn read_be_u64(bytes: &[u8], offset: usize) -> usize {
        u64::from_be_bytes(
            bytes[offset..offset + 8]
                .try_into()
                .expect("eight-byte big-endian field"),
        ) as usize
    }

    fn read_syncsafe_u32(bytes: &[u8], offset: usize) -> usize {
        ((bytes[offset] as usize & 0x7f) << 21)
            | ((bytes[offset + 1] as usize & 0x7f) << 14)
            | ((bytes[offset + 2] as usize & 0x7f) << 7)
            | (bytes[offset + 3] as usize & 0x7f)
    }

    fn id3v2_frame_counts(bytes: &[u8]) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        if bytes.len() < 10 || &bytes[0..3] != b"ID3" {
            return counts;
        }

        let major_version = bytes[3];
        let flags = bytes[5];
        let tag_size = read_syncsafe_u32(bytes, 6);
        let end = (10 + tag_size).min(bytes.len());
        let mut pos = 10;

        if flags & 0x40 != 0 {
            if major_version == 4 && pos + 4 <= end {
                let extended_size = read_syncsafe_u32(bytes, pos);
                pos = pos.saturating_add(extended_size);
            } else if major_version == 3 && pos + 4 <= end {
                let extended_size = read_be_u32(bytes, pos);
                pos = pos.saturating_add(4 + extended_size);
            }
        }

        while pos + 10 <= end {
            let id_bytes = &bytes[pos..pos + 4];
            if id_bytes.iter().all(|byte| *byte == 0) {
                break;
            }
            if !id_bytes
                .iter()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
            {
                break;
            }

            let frame_size = if major_version == 4 {
                read_syncsafe_u32(bytes, pos + 4)
            } else {
                read_be_u32(bytes, pos + 4)
            };
            if frame_size == 0 || pos + 10 + frame_size > end {
                break;
            }

            let id = String::from_utf8_lossy(id_bytes).to_string();
            *counts.entry(id).or_insert(0) += 1;
            pos += 10 + frame_size;
        }

        counts
    }

    fn mp4_atom_code(code: &[u8]) -> String {
        if code.len() == 4 && code[0] == 0xa9 && code[1..].iter().all(|byte| byte.is_ascii()) {
            format!("A9{}", String::from_utf8_lossy(&code[1..]))
        } else {
            String::from_utf8_lossy(code).to_string()
        }
    }

    fn walk_mp4_boxes(bytes: &[u8], mut pos: usize, end: usize, in_ilst: bool, counts: &mut BTreeMap<String, usize>) {
        while pos + 8 <= end {
            let size32 = read_be_u32(bytes, pos);
            let code = &bytes[pos + 4..pos + 8];
            let mut header_size = 8usize;
            let size = match size32 {
                0 => end.saturating_sub(pos),
                1 if pos + 16 <= end => {
                    header_size = 16;
                    read_be_u64(bytes, pos + 8)
                }
                _ => size32,
            };
            if size < header_size || pos + size > end {
                break;
            }

            let atom = mp4_atom_code(code);
            let data_start = pos + header_size;
            let data_end = pos + size;
            if in_ilst {
                *counts.entry(atom).or_insert(0) += 1;
            } else if code == b"moov" || code == b"udta" {
                walk_mp4_boxes(bytes, data_start, data_end, false, counts);
            } else if code == b"meta" && data_start + 4 <= data_end {
                walk_mp4_boxes(bytes, data_start + 4, data_end, false, counts);
            } else if code == b"ilst" {
                walk_mp4_boxes(bytes, data_start, data_end, true, counts);
            }

            pos += size;
        }
    }

    fn mp4_ilst_atom_counts(bytes: &[u8]) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        walk_mp4_boxes(bytes, 0, bytes.len(), false, &mut counts);
        counts
    }

    fn riff_info_key_counts(bytes: &[u8]) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return counts;
        }

        let mut pos = 12usize;
        while pos + 8 <= bytes.len() {
            let id = &bytes[pos..pos + 4];
            let size = read_le_u32(bytes, pos + 4);
            let data_start = pos + 8;
            let data_end = data_start.saturating_add(size).min(bytes.len());
            if id == b"LIST" && data_start + 4 <= data_end && &bytes[data_start..data_start + 4] == b"INFO" {
                let mut item_pos = data_start + 4;
                while item_pos + 8 <= data_end {
                    let key = String::from_utf8_lossy(&bytes[item_pos..item_pos + 4]).to_ascii_uppercase();
                    let item_size = read_le_u32(bytes, item_pos + 4);
                    *counts.entry(key).or_insert(0) += 1;
                    let step = 8 + item_size + (item_size & 1);
                    if step == 0 || item_pos + step > data_end + 1 {
                        break;
                    }
                    item_pos += step;
                }
            }
            let step = 8 + size + (size & 1);
            if step == 0 {
                break;
            }
            pos += step;
        }
        counts
    }

    fn aiff_metadata_chunk_counts(bytes: &[u8]) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        if bytes.len() < 12 || &bytes[0..4] != b"FORM" || (&bytes[8..12] != b"AIFF" && &bytes[8..12] != b"AIFC") {
            return counts;
        }

        let mut pos = 12usize;
        while pos + 8 <= bytes.len() {
            let key = String::from_utf8_lossy(&bytes[pos..pos + 4]).to_ascii_uppercase();
            let size = read_be_u32(bytes, pos + 4);
            let is_id3 = key == "ID3 ";
            if matches!(key.as_str(), "NAME" | "AUTH" | "ANNO" | "(C) " | "ID3 ") {
                *counts.entry(key).or_insert(0) += 1;
            }
            if is_id3 && pos + 8 + size <= bytes.len() {
                let id3_counts = id3v2_frame_counts(&bytes[pos + 8..pos + 8 + size]);
                for (frame, count) in id3_counts {
                    *counts.entry(frame).or_insert(0) += count;
                }
            }
            let step = 8 + size + (size & 1);
            if step == 0 {
                break;
            }
            pos += step;
        }
        counts
    }

    fn read_le_u32(bytes: &[u8], offset: usize) -> usize {
        u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("four-byte little-endian field"),
        ) as usize
    }

    fn apev2_item_key_counts(bytes: &[u8]) -> BTreeMap<String, usize> {
        let footer = bytes
            .windows(8)
            .rposition(|window| window == b"APETAGEX")
            .expect("file should contain an APEv2 tag footer");
        assert!(footer + 32 <= bytes.len(), "truncated APEv2 footer");

        let tag_size = read_le_u32(bytes, footer + 12);
        let item_count = read_le_u32(bytes, footer + 16);
        assert!(tag_size >= 32, "APEv2 tag size must include at least the footer");
        assert!(tag_size <= footer + 32, "APEv2 tag size points before start of file");

        let mut pos = footer + 32 - tag_size;
        if pos + 32 <= footer && &bytes[pos..pos + 8] == b"APETAGEX" {
            pos += 32;
        }

        let mut counts = BTreeMap::new();
        for _ in 0..item_count {
            assert!(pos + 8 <= footer, "truncated APEv2 item header");
            let value_size = read_le_u32(bytes, pos);
            pos += 8;

            let key_end = bytes[pos..footer]
                .iter()
                .position(|byte| *byte == 0)
                .map(|relative| pos + relative)
                .expect("APEv2 item key should be NUL-terminated");
            let key = String::from_utf8_lossy(&bytes[pos..key_end]).to_ascii_uppercase();
            pos = key_end + 1;
            assert!(pos + value_size <= footer, "truncated APEv2 item value");
            pos += value_size;

            *counts.entry(key).or_insert(0) += 1;
        }

        counts
    }

    fn find_extension_under(root: &Path, extension: &str) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(root) else {
            return found;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(find_extension_under(&path, extension));
            } else if path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
            {
                found.push(path);
            }
        }
        found
    }

    #[tokio::test]
    async fn cue_matrix_validates_real_outputs_when_external_tools_are_available() {
        let strict = cue_matrix_strict_mode();
        if !executable_on_path("ffmpeg") || !executable_on_path("ffprobe") {
            let message = "real CUE matrix output validation requires ffmpeg and ffprobe";
            if strict {
                panic!("{message}; TONEPOET_CUE_MATRIX_STRICT=1 forbids skipping required matrix infrastructure");
            }
            eprintln!("skipping real CUE matrix output validation; {message}");
            return;
        }

        let temp = tempfile::tempdir().expect("temp dir");
        let (image, _cue) = create_fixture_image_and_cue(temp.path());
        let cases = matrix_cases();
        let required_case_count = cases.len();
        let mut exercised = Vec::new();
        let mut skipped = Vec::new();

        for case in cases {
            let unavailable = case_unavailability_reasons(&case);
            if !unavailable.is_empty() {
                let reason = format!("{}: {}", case.name, unavailable.join(", "));
                if strict {
                    skipped.push(reason);
                } else {
                    eprintln!("skipping {reason}");
                }
                continue;
            }

            let case_root = temp.path().join(case.name);
            std::fs::create_dir_all(case_root.join("out")).expect("case output root");
            std::fs::create_dir_all(case_root.join("logs")).expect("case log root");
            let req = request_for_case(&case_root, &image, &case);
            let staging = StagingDir::new(case_root.join("staging"), req.job_id.clone());
            let runner = RealToolRunner::new(HashMap::new());
            let cancel = CancellationToken::new();

            let source = CueImageMaterializer
                .materialize(&req, &staging, &runner, None, &HashMap::new(), &cancel)
                .await
                .unwrap_or_else(|err| panic!("{} CUE materialization failed: {err}", case.name));
            assert_eq!(source.kind, SourceKind::CueImage);
            assert_eq!(source.tracks.len(), 1);
            assert!(
                source.album_metadata.extra.contains_key(CUE_ARTWORK_PATH_EXTRA_KEY),
                "{} should extract original image artwork as a sidecar",
                case.name
            );
            let TrackSourceRef::CueSegmentCarrier { path, carrier, .. } = &source.tracks[0].source_ref else {
                panic!("{} should materialize a typed CUE segment carrier", case.name);
            };
            assert_eq!(*carrier, CueSegmentCarrier::PcmS32LeWav);
            assert_eq!(path.extension().and_then(|value| value.to_str()), Some("wav"));
            assert!(path.exists(), "{} staged PCM WAV segment should exist", case.name);
            assert!(
                find_extension_under(&staging.root.join("cue-segments"), "flac").is_empty(),
                "{} normal CUE path must not create an intermediate FLAC under cue-segments",
                case.name
            );

            let plan = plan_outputs(&source, &req).unwrap_or_else(|err| panic!("{} output planning failed: {err}", case.name));
            assert_eq!(
                plan.entries[0].final_path.extension().and_then(|value| value.to_str()),
                Some(case.extension),
                "{} planned final container extension mismatch",
                case.name
            );

            let converted = convert_tracks(&source, &plan, &req, &staging, &runner, &cancel).await;
            assert!(
                matches!(converted.record.outcome, StageOutcome::Ok),
                "{} conversion stage failed: {:?}",
                case.name,
                converted.tracks
            );
            let AudioArtifacts::Tracks(track_artifacts) = &converted.artifacts.audio else {
                panic!("{} should produce per-track artifacts", case.name);
            };
            assert_eq!(track_artifacts.len(), 1, "{} should produce one converted track", case.name);
            let output_path = track_artifacts[0].staged_path.clone();

            apply_metadata(&converted.artifacts, &source, &req, &runner, &cancel)
                .await
                .unwrap_or_else(|err| panic!("{} first metadata/artwork stage failed: {err}", case.name));
            let first_probe = ffprobe_json(&output_path);
            apply_metadata(&converted.artifacts, &source, &req, &runner, &cancel)
                .await
                .unwrap_or_else(|err| panic!("{} second metadata/artwork stage failed: {err}", case.name));
            let second_probe = ffprobe_json(&output_path);

            assert_container_codec_duration_and_metadata(&case, &output_path, &second_probe);
            assert_artwork_behavior(&case, &output_path, &second_probe);
            assert_eq!(
                format_tag_map(&first_probe),
                format_tag_map(&second_probe),
                "{} repeated metadata stage should be semantically idempotent",
                case.name
            );
            assert_eq!(
                attached_picture_count(&first_probe),
                attached_picture_count(&second_probe),
                "{} repeated artwork stage should not duplicate attached-picture streams",
                case.name
            );
            assert_native_tag_duplicate_free(&case, &output_path);
            exercised.push(case.name);
        }

        if strict {
            assert!(
                skipped.is_empty(),
                "strict CUE matrix mode requires every required target case to run; skipped cases: {skipped:?}"
            );
            assert_eq!(
                exercised.len(),
                required_case_count,
                "strict CUE matrix mode requires all required target cases to pass"
            );
        } else {
            assert!(
                !exercised.is_empty(),
                "real CUE matrix test did not exercise any target format; install ffmpeg/ffprobe plus at least one target encoder/tagger, or set TONEPOET_CUE_MATRIX_STRICT=1 in release validation"
            );
        }
        eprintln!("exercised real CUE matrix cases: {exercised:?}");
    }

    #[test]
    fn cue_matrix_strict_flag_parsing_is_explicit() {
        assert!(env_flag_enabled(Some("1")));
        assert!(env_flag_enabled(Some("true")));
        assert!(env_flag_enabled(Some("YES")));
        assert!(env_flag_enabled(Some("on")));
        assert!(!env_flag_enabled(Some("0")));
        assert!(!env_flag_enabled(Some("false")));
        assert!(!env_flag_enabled(Some("")));
        assert!(!env_flag_enabled(None));
    }
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
            dsd_dst_stats: None,
        });
    }

    let mut args = Vec::new();
    match &artifacts.audio {
        AudioArtifacts::Tracks(tracks) => {
            if tracks.is_empty() {
                return Ok(StageRecord {
                    stage: PipelineStage::ReplayGain,
                    outcome: StageOutcome::Skipped,
                    dsd_dst_stats: None,
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
        dsd_dst_stats: None,
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
                dsd_dst_stats: None,
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
    let settings_fingerprint = tonepoet_pipeline::fingerprint::settings_fingerprint(&req.settings);
    let log_content = build_conversion_log(
        outcome,
        source,
        req,
        &artifacts,
        Some(settings_fingerprint),
    );
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
            dsd_dst_stats: None,
        },
    ))
}

fn build_conversion_log(
    outcome: &AlbumOutcome,
    source: &PreparedSource,
    req: &PipelineRequest,
    artifacts: &ArtifactSet,
    settings_fingerprint: Option<tonepoet_pipeline::SettingsFingerprint>,
) -> String {
    let mut log = String::new();
    let tracks = collect_outcome_tracks(outcome);
    let source_tracks_by_ordinal = build_source_track_index(source);
    let artifacts_by_track_id = build_track_artifact_index(artifacts);
    let metadata_stage_result = metadata_stage_outcome(outcome);
    let resampling_applies = resampling_applies_for_source(source, &req.settings);
    let bit_depth_change_applies = bit_depth_change_applies_for_source(source, &req.settings);
    let dithering_applies = dithering_applies_for_source(source, &req.settings);
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
    if let Some(fingerprint) = settings_fingerprint {
        push_kv_line(&mut log, "Settings fingerprint", fingerprint.to_string());
    }
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
    append_source_blocking_lines(&mut log, source);
    log.push('\n');

    append_provenance_section(&mut log, &source.provenance);
    append_artwork_section(&mut log, source, req, outcome, artifacts);

    log.push_str("Conversion Settings\n");
    log.push_str("-------------------\n");
    append_conversion_settings_section(
        &mut log,
        source,
        req,
        resampling_applies,
        bit_depth_change_applies,
        dithering_applies,
    );
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
                artifacts_by_track_id.get(&record.track_id).copied(),
                req,
                metadata_stage_result,
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
            let mut summary = stage_outcome_label(&stage.outcome);
            if let Some(stats) = stage.dsd_dst_stats.as_ref() {
                summary.push_str("; ");
                summary.push_str(&format_dsd_dst_stats_inline(stats));
            }
            push_kv_line(
                &mut log,
                pipeline_stage_label(stage.stage),
                summary,
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

fn append_source_blocking_lines(log: &mut String, source: &PreparedSource) {
    let extra = &source.album_metadata.extra;
    let cppm = extra
        .get("dvda_cppm_detected")
        .map(|value| value == "true")
        .unwrap_or(false);
    let mkb = extra
        .get("dvda_mkb_present")
        .map(|value| value == "true")
        .unwrap_or(false);
    if !cppm && !mkb {
        return;
    }

    push_kv_line(log, "Copy protection", "CPPM blocked");
    if let Some(source) = extra.get("dvda_copy_protection_source") {
        push_kv_line(log, "Copy protection evidence", source);
    }
    if let Some(file) = extra.get("dvda_copy_protection_file") {
        push_kv_line(log, "Copy protection source", file);
    } else if mkb {
        push_kv_line(log, "Copy protection source", "DVDAUDIO.MKB");
    }
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

fn build_track_artifact_index<'a>(artifacts: &'a ArtifactSet) -> BTreeMap<TrackId, &'a TrackArtifact> {
    let mut artifacts_by_track_id = BTreeMap::new();
    if let AudioArtifacts::Tracks(tracks) = &artifacts.audio {
        for artifact in tracks {
            artifacts_by_track_id
                .entry(artifact.track_id.clone())
                .or_insert(artifact);
        }
    }
    artifacts_by_track_id
}

fn append_track_log(
    log: &mut String,
    record: &TrackRecord,
    prepared: Option<&PreparedTrack>,
    artifact: Option<&TrackArtifact>,
    req: &PipelineRequest,
    metadata_stage_result: Option<&StageOutcome>,
) {
    log.push_str(&escape_log_value(&track_display_label(record, prepared)));
    log.push('\n');
    match &record.outcome {
        TrackOutcome::Ok => log.push_str("  Status: Success\n"),
        TrackOutcome::Err(error) => {
            log.push_str("  Status: Failure\n");
            push_kv_line(log, "  Error", error);
        }
        TrackOutcome::Blocked(reason) => {
            log.push_str("  Status: Blocked\n");
            push_kv_line(log, "  Block reason", reason);
        }
    }

    if let Some(track) = prepared {
        push_optional_kv_line(log, "  Artist", track.metadata.artist.as_deref());
        push_optional_kv_line(log, "  Composer", track.metadata.composer.as_deref());
        push_kv_line(log, "  Source audio", source_audio_description(track));
        push_kv_line(log, "  Conversion", conversion_summary(track, req));
    }

    if let Some(pipeline) = planned_pipeline_label(&record.commands)
        .or_else(|| passthrough_pipeline_label(record, prepared, req))
    {
        push_kv_line(log, "  Pipeline", pipeline);
    }

    if let Some(metadata) = metadata_satisfaction_label(artifact, req.stages.metadata, metadata_stage_result) {
        push_kv_line(log, "  Metadata", metadata);
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

    if let Some(stats) = record.dsd_dst_stats.as_ref() {
        push_kv_line(log, "  DSD/DST stats", format_dsd_dst_stats_inline(stats));
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

fn append_provenance_section(log: &mut String, provenance: &ExtractionProvenance) {
    log.push_str("Provenance\n");
    log.push_str("----------\n");
    if let Some(sha256) = provenance
        .source_sha256
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        push_kv_line(log, "Source SHA-256", sha256);
    }
    push_kv_line(
        log,
        "Extracted at",
        provenance
            .extracted_at
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string(),
    );
    if !provenance.tool_versions.is_empty() {
        let versions = provenance
            .tool_versions
            .iter()
            .filter_map(|(tool, version)| {
                let tool = tool.trim();
                let version = version.trim();
                if tool.is_empty() || version.is_empty() {
                    None
                } else {
                    Some(format!("{tool} {version}"))
                }
            })
            .collect::<Vec<_>>();
        if !versions.is_empty() {
            push_kv_line(log, "Tool versions", versions.join(", "));
        }
    }
    log.push('\n');
}

fn append_artwork_section(
    log: &mut String,
    source: &PreparedSource,
    req: &PipelineRequest,
    outcome: &AlbumOutcome,
    artifacts: &ArtifactSet,
) {
    let Some(artwork) = cue_artwork_sidecar_from_album_metadata(&source.album_metadata) else {
        return;
    };
    let artwork_format = artwork
        .mime_type
        .as_deref()
        .map(artwork_mime_label)
        .unwrap_or("artwork");
    push_kv_line(
        log,
        "Artwork",
        format!(
            "extracted {artwork_format} from source image → {}",
            cue_artwork_log_outcome(req, outcome, artifacts)
        ),
    );
    log.push('\n');
}

fn append_conversion_settings_section(
    log: &mut String,
    source: &PreparedSource,
    req: &PipelineRequest,
    resampling_applies: bool,
    bit_depth_change_applies: bool,
    dithering_applies: bool,
) {
    let settings = &req.settings;
    push_kv_line(log, "Target format", settings.target_format.display_name());
    if resampling_applies {
        push_kv_line(
            log,
            "Target sample rate",
            target_sample_rate_setting_label(source, settings),
        );
        push_kv_line(log, "Preferred resampler tool", preferred_resampler_label(settings));
    }
    if bit_depth_change_applies {
        push_kv_line(
            log,
            "Target bit depth",
            bit_depth_target_label(settings.target_bit_depth),
        );
    }
    if dithering_applies {
        push_kv_line(log, "Dither type", dither_type_label(settings.dither_type));
    }
    push_kv_line(log, "Force encode", yes_no(settings.force_encode));
    push_kv_line(log, "Merge mode", yes_no(req.merge));

    append_target_format_settings(log, settings);
    if resampling_applies {
        append_resampler_settings(log, settings);
    }
    if source_is_dsd(source) || settings.target_format.is_dsd() {
        append_dsd_settings(log, source, settings);
    }

    push_kv_line(log, "Metadata", stage_requirement_label(req.stages.metadata));
    push_kv_line(log, "ReplayGain", stage_requirement_label(req.stages.replaygain));
    push_kv_line(log, "Features", stage_requirement_label(req.stages.features));
    match &req.naming.folder_template {
        Some(template) => push_kv_line(log, "Folder template", template),
        None => push_kv_line(log, "Folder template", "album-name fallback"),
    }
    push_kv_line(log, "Filename template", &req.naming.template);
}

fn append_target_format_settings(log: &mut String, settings: &tonepoet_pipeline::PipelineSettings) {
    match &settings.target_format {
        PlannerAudioFormat::Flac => {
            push_kv_line(log, "FLAC compression", settings.flac.compression_level.to_string());
        }
        PlannerAudioFormat::Mp3 => {
            push_kv_line(log, "MP3 mode", mp3_mode_label(settings.mp3.mode));
            push_kv_line(log, "MP3 bitrate", format!("{} kbps", settings.mp3.bitrate_kbps));
            match settings.mp3.mode {
                Mp3Mode::Vbr => push_kv_line(log, "MP3 VBR quality", settings.mp3.vbr_quality.to_string()),
                Mp3Mode::Cbr | Mp3Mode::Abr => {}
            }
        }
        PlannerAudioFormat::Aac => {
            push_kv_line(log, "AAC profile", aac_profile_label(settings.aac.profile));
            push_kv_line(log, "AAC bitrate", format!("{} kbps", settings.aac.bitrate_kbps));
        }
        PlannerAudioFormat::Opus => {
            push_kv_line(log, "Opus bitrate", format!("{} kbps", settings.opus.bitrate_kbps));
            push_kv_line(log, "Opus application", opus_content_type_label(settings.opus.content_type));
            push_kv_line(log, "Opus complexity", settings.opus.complexity.to_string());
        }
        PlannerAudioFormat::WavPack => {
            push_kv_line(log, "WavPack mode", wavpack_mode_label(settings.wavpack.mode));
            if settings.wavpack.hybrid {
                push_kv_line(
                    log,
                    "WavPack hybrid bitrate",
                    format!("{} kbps/ch", settings.wavpack.hybrid_bitrate_kbps),
                );
                push_kv_line(
                    log,
                    "WavPack correction file",
                    yes_no(settings.wavpack.correction_file),
                );
            }
        }
        PlannerAudioFormat::Alac
        | PlannerAudioFormat::Wav
        | PlannerAudioFormat::Aiff
        | PlannerAudioFormat::Dsf
        | PlannerAudioFormat::Dff
        | PlannerAudioFormat::Dts
        | PlannerAudioFormat::Ac3
        | PlannerAudioFormat::Custom { .. } => {}
    }
}

fn append_resampler_settings(log: &mut String, settings: &tonepoet_pipeline::PipelineSettings) {
    match preferred_resampler_family(settings) {
        ResamplerFamily::Ssrc => append_ssrc_settings(log, settings),
        ResamplerFamily::Sox => append_sox_resampler_settings(log, settings),
        ResamplerFamily::Soxr => append_soxr_resampler_settings(log, settings),
        ResamplerFamily::Auto => {
            push_kv_line(log, "Resample quality", resample_quality_label(settings.resample_quality));
            push_kv_line(log, "Nyquist transition", nyquist_transition_label(settings.nyquist_transition));
            append_non_default_resampler_overrides(log, settings);
        }
    }
}

fn append_ssrc_settings(log: &mut String, settings: &tonepoet_pipeline::PipelineSettings) {
    let ssrc = settings.ssrc;
    push_kv_line(log, "SSRC profile", ssrc_profile_label(ssrc.profile, settings.resample_quality, ssrc.insane_mode));
    if let Some(attenuation) = ssrc.attenuation_db {
        push_kv_line(log, "SSRC attenuation", format!("{} dB", decimal_label(attenuation)));
    }
    push_kv_line(log, "SSRC minimum phase", yes_no(ssrc.min_phase));
    if let Some(dither_id) = ssrc.dither_id {
        push_kv_line(log, "SSRC dither ID", dither_id.to_string());
    }
    if let Some(pdf_type) = ssrc.pdf_type {
        push_kv_line(log, "SSRC PDF type", ssrc_pdf_type_label(pdf_type));
    }
    push_kv_line(log, "SSRC insane mode", yes_no(ssrc.insane_mode));
}

fn append_sox_resampler_settings(log: &mut String, settings: &tonepoet_pipeline::PipelineSettings) {
    let sox = settings.sox_resampler;
    push_kv_line(log, "SoX quality", resample_quality_label(settings.resample_quality));
    push_kv_line(log, "SoX Nyquist transition", nyquist_transition_label(settings.nyquist_transition));
    if sox.chebyshev {
        push_kv_line(log, "SoX steep/Chebyshev", "yes");
    }
    if let Some(bandwidth) = sox.bandwidth_pct {
        push_kv_line(log, "SoX bandwidth", format!("{}%", decimal_label(bandwidth)));
    }
    if let Some(phase) = sox.phase {
        push_kv_line(log, "SoX phase response", phase.to_string());
    }
    if sox.allow_aliasing {
        push_kv_line(log, "SoX allow aliasing", "yes");
    }
    append_sox_sinc_settings(log, settings);
}

fn append_soxr_resampler_settings(log: &mut String, settings: &tonepoet_pipeline::PipelineSettings) {
    let soxr = settings.soxr_resampler;
    push_kv_line(
        log,
        "Soxr quality preset",
        resample_quality_label(settings.resample_quality),
    );
    if let Some(phase) = soxr.phase {
        push_kv_line(log, "Soxr phase response", phase.to_string());
    }
    if let Some(cutoff) = soxr.cutoff {
        push_kv_line(log, "Soxr cutoff override", decimal_label(cutoff));
    }
    if soxr.chebyshev {
        push_kv_line(log, "Soxr Chebyshev", "yes");
    }
}

fn append_non_default_resampler_overrides(log: &mut String, settings: &tonepoet_pipeline::PipelineSettings) {
    if settings.ssrc.force
        || settings.ssrc.insane_mode
        || settings.ssrc.profile.is_some()
        || settings.ssrc.attenuation_db.is_some()
        || settings.ssrc.min_phase
        || settings.ssrc.dither_id.is_some()
        || settings.ssrc.pdf_type.is_some()
    {
        append_ssrc_settings(log, settings);
    }
    if settings.sox_resampler.chebyshev
        || settings.sox_resampler.bandwidth_pct.is_some()
        || settings.sox_resampler.phase.is_some()
        || settings.sox_resampler.allow_aliasing
        || settings.sox_resampler.sinc_taps.is_some()
        || settings.sox_resampler.sinc_attenuation_db.is_some()
        || settings.sox_resampler.sinc_passband_hz.is_some()
        || settings.sox_resampler.sinc_transition_hz.is_some()
        || settings.sox_resampler.sinc_kaiser_beta.is_some()
        || settings.sox_resampler.sinc_phase.is_some()
    {
        append_sox_resampler_settings(log, settings);
    }
    if settings.soxr_resampler.chebyshev
        || settings.soxr_resampler.cutoff.is_some()
        || settings.soxr_resampler.phase.is_some()
    {
        append_soxr_resampler_settings(log, settings);
    }
}

fn append_sox_sinc_settings(log: &mut String, settings: &tonepoet_pipeline::PipelineSettings) {
    let sox = settings.sox_resampler;
    if sox.sinc_taps.is_none()
        && sox.sinc_attenuation_db.is_none()
        && sox.sinc_passband_hz.is_none()
        && sox.sinc_transition_hz.is_none()
        && sox.sinc_kaiser_beta.is_none()
        && sox.sinc_phase.is_none()
    {
        return;
    }
    if let Some(taps) = sox.sinc_taps {
        push_kv_line(log, "SoX sinc taps", taps.to_string());
    }
    if let Some(attenuation) = sox.sinc_attenuation_db {
        push_kv_line(log, "SoX sinc attenuation", format!("{attenuation} dB"));
    }
    if let Some(passband) = sox.sinc_passband_hz {
        push_kv_line(log, "SoX sinc passband", format!("{} Hz", decimal_label(passband)));
    }
    if let Some(transition) = sox.sinc_transition_hz {
        push_kv_line(log, "SoX sinc transition", format!("{} Hz", decimal_label(transition)));
    }
    if let Some(beta) = sox.sinc_kaiser_beta {
        push_kv_line(log, "SoX sinc Kaiser beta", decimal_label(beta));
    }
    if let Some(phase) = sox.sinc_phase {
        push_kv_line(log, "SoX sinc phase", sox_sinc_phase_label(phase));
    }
}

fn append_dsd_settings(
    log: &mut String,
    source: &PreparedSource,
    settings: &tonepoet_pipeline::PipelineSettings,
) {
    if source_is_dsd(source) && !settings.target_format.is_dsd() {
        push_kv_line(log, "DSD gain mode", dsd_gain_mode_label(settings.dsd.dsd_to_pcm_gain_mode));
        push_kv_line(
            log,
            "DSD auto gain margin",
            format!("{} dB", decimal_label(settings.dsd.dsd_to_pcm_auto_gain_margin_db)),
        );
        if let Some(gain) = settings.dsd.dsd_to_pcm_gain_db {
            push_kv_line(log, "DSD manual gain", format!("{} dB", decimal_label(gain)));
        }
        push_kv_line(
            log,
            "DSD→PCM lowpass method",
            dsd_lowpass_method_label(settings.dsd.dsd_to_pcm_lowpass),
        );
    }

    if settings.target_format.is_dsd() {
        push_kv_line(
            log,
            "PCM→DSD filter preset",
            format!("{:?}", settings.dsd.pcm_to_dsd_filter),
        );
    }
}

fn planned_pipeline_label(commands: &[CommandRecord]) -> Option<String> {
    if commands.is_empty() {
        return None;
    }
    let parts = commands
        .iter()
        .map(|command| {
            command
                .description
                .as_deref()
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| command_line_label(command))
        })
        .collect::<Vec<_>>();
    Some(parts.join(" → "))
}

fn passthrough_pipeline_label(
    record: &TrackRecord,
    prepared: Option<&PreparedTrack>,
    req: &PipelineRequest,
) -> Option<String> {
    if !record.commands.is_empty() || !matches!(record.outcome, TrackOutcome::Ok) {
        return None;
    }

    let track = prepared?;
    if source_audio_matches_target_for_passthrough(track, &req.settings) {
        Some("passthrough copy".to_string())
    } else {
        None
    }
}

fn source_audio_matches_target_for_passthrough(
    track: &PreparedTrack,
    settings: &tonepoet_pipeline::PipelineSettings,
) -> bool {
    if settings.force_encode || !source_track_format_matches_target(track, &settings.target_format) {
        return false;
    }

    let Some(target_rate) = resolved_target_rate_hz(track, settings) else {
        return false;
    };
    if track.scalar_sample_rate() != Some(target_rate) {
        return false;
    }

    if settings.target_format.is_dsd() {
        return true;
    }

    resolved_target_bit_depth(track, settings.target_bit_depth) == track.bit_depth
}

fn source_track_format_matches_target(track: &PreparedTrack, target: &PlannerAudioFormat) -> bool {
    let extension = source_ref_extension(&track.source_ref)
        .unwrap_or_default()
        .trim_start_matches('.')
        .to_ascii_lowercase();

    match target {
        PlannerAudioFormat::Flac => extension == "flac",
        PlannerAudioFormat::Mp3 => extension == "mp3",
        PlannerAudioFormat::Aac | PlannerAudioFormat::Alac => false,
        PlannerAudioFormat::Opus => matches!(extension.as_str(), "opus" | "ogg"),
        PlannerAudioFormat::WavPack => extension == "wv",
        PlannerAudioFormat::Wav => matches!(extension.as_str(), "wav" | "wave" | "rf64"),
        PlannerAudioFormat::Aiff => matches!(extension.as_str(), "aiff" | "aif"),
        PlannerAudioFormat::Dsf => extension == "dsf",
        PlannerAudioFormat::Dff => extension == "dff",
        PlannerAudioFormat::Dts => extension == "dts",
        PlannerAudioFormat::Ac3 => extension == "ac3",
        PlannerAudioFormat::Custom { .. } => false,
    }
}

fn metadata_satisfaction_label(
    artifact: Option<&TrackArtifact>,
    metadata_stage: StageRequirement,
    metadata_stage_result: Option<&StageOutcome>,
) -> Option<String> {
    let artifact = artifact?;
    let satisfied = artifact.metadata_satisfaction;
    let required = artifact.metadata_required;
    if !satisfied.any() || satisfied.satisfies(required) {
        return None;
    }

    let mut parts = metadata_satisfied_parts(satisfied);
    parts.extend(metadata_remaining_requirement_parts(
        metadata_stage,
        metadata_stage_result,
        satisfied,
        required,
    ));
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

fn metadata_satisfied_parts(value: PlannedMetadataSatisfaction) -> Vec<String> {
    let mut parts = Vec::new();
    if value.source_tags_transferred {
        parts.push("planner transferred source tags".to_string());
    }
    if value.artwork_transferred {
        parts.push("planner transferred artwork".to_string());
    }
    if value.source_audio_md5_written {
        parts.push("planner wrote source audio MD5".to_string());
    }
    if value.authoritative_tags_applied {
        parts.push("authoritative CUE tags already applied".to_string());
    }
    parts
}

fn metadata_remaining_requirement_parts(
    metadata_stage: StageRequirement,
    metadata_stage_result: Option<&StageOutcome>,
    satisfied: PlannedMetadataSatisfaction,
    required: PlannedMetadataSatisfaction,
) -> Vec<String> {
    let remaining = PlannedMetadataSatisfaction {
        source_tags_transferred: required.source_tags_transferred && !satisfied.source_tags_transferred,
        artwork_transferred: required.artwork_transferred && !satisfied.artwork_transferred,
        source_audio_md5_written: required.source_audio_md5_written && !satisfied.source_audio_md5_written,
        authoritative_tags_applied: required.authoritative_tags_applied && !satisfied.authoritative_tags_applied,
    };
    if !remaining.any() {
        return Vec::new();
    }

    let dimensions = metadata_dimension_labels(remaining).join(", ");
    let status = match metadata_stage {
        StageRequirement::Disabled => "metadata stage disabled".to_string(),
        StageRequirement::Enabled => match metadata_stage_result {
            Some(StageOutcome::Ok) => {
                "metadata stage completed; per-track writes not recorded".to_string()
            }
            Some(StageOutcome::Skipped) => "metadata stage skipped".to_string(),
            Some(StageOutcome::Failed(reason)) => {
                format!("metadata stage failed: {}", escape_log_value(reason))
            }
            None => "metadata stage outcome unavailable".to_string(),
        },
    };

    vec![format!("remaining required metadata: {dimensions} ({status})")]
}

fn metadata_dimension_labels(value: PlannedMetadataSatisfaction) -> Vec<&'static str> {
    let mut labels = Vec::new();
    if value.source_tags_transferred {
        labels.push("source tags");
    }
    if value.artwork_transferred {
        labels.push("artwork");
    }
    if value.source_audio_md5_written {
        labels.push("source audio MD5");
    }
    if value.authoritative_tags_applied {
        labels.push("authoritative CUE tags");
    }
    labels
}

fn conversion_summary(track: &PreparedTrack, req: &PipelineRequest) -> String {
    let source_rate = track.scalar_sample_rate();
    let source_depth = track.bit_depth;
    let source_format = source_track_format_label(track);
    let target_rate = resolved_target_rate_hz(track, &req.settings).or(source_rate);
    let target_depth = if req.settings.target_format.is_dsd() {
        None
    } else {
        resolved_target_bit_depth(track, req.settings.target_bit_depth)
    };
    let mut summary = format!(
        "{} {} -> {} {}",
        stream_description(source_depth, source_rate, Some(&source_format)),
        source_format,
        target_stream_description(track, target_depth, target_rate, &req.settings),
        req.settings.target_format.display_name(),
    );
    let mut transforms = Vec::new();
    if let (Some(source_rate), Some(target_rate)) = (source_rate, target_rate) {
        if source_rate != target_rate {
            transforms.push(format!("{} resampling", preferred_resampler_label(&req.settings)));
        }
    }
    if dither_applies(source_depth, target_depth, req.settings.dither_type) {
        transforms.push(format!("{} dither", dither_type_label(req.settings.dither_type)));
    }
    if !transforms.is_empty() {
        summary.push_str(&format!(" ({})", transforms.join(", ")));
    }
    summary
}

fn stream_description(
    bit_depth: Option<u32>,
    sample_rate: Option<u32>,
    dsd_format_hint: Option<&str>,
) -> String {
    if dsd_format_hint == Some("DSD") {
        if let Some(rate) = sample_rate.and_then(DsdRate::from_hz) {
            return dsd_rate_label(rate).to_string();
        }
    }
    let rate = sample_rate
        .map(format_sample_rate)
        .unwrap_or_else(|| "unknown rate".to_string());
    match bit_depth {
        Some(bits) => format!("{bits}-bit/{rate}"),
        None => rate,
    }
}

fn target_stream_description(
    track: &PreparedTrack,
    bit_depth: Option<u32>,
    sample_rate: Option<u32>,
    settings: &tonepoet_pipeline::PipelineSettings,
) -> String {
    if settings.target_format.is_dsd() {
        let target_dsd_rate = match settings.target_sample_rate {
            RateTarget::Dsd(rate) => Some(rate),
            RateTarget::Source => source_track_dsd_rate(track),
            RateTarget::PcmHz(_) => None,
        }
        .or_else(|| sample_rate.and_then(DsdRate::from_hz));
        if let Some(rate) = target_dsd_rate {
            return dsd_rate_label(rate).to_string();
        }
    }
    stream_description(bit_depth, sample_rate, None)
}

fn resampling_applies_for_source(
    source: &PreparedSource,
    settings: &tonepoet_pipeline::PipelineSettings,
) -> bool {
    source.tracks.iter().any(|track| {
        match (track.scalar_sample_rate(), resolved_target_rate_hz(track, settings)) {
            (Some(source_rate), Some(target_rate)) => source_rate != target_rate,
            _ => false,
        }
    })
}

fn bit_depth_change_applies_for_source(
    source: &PreparedSource,
    settings: &tonepoet_pipeline::PipelineSettings,
) -> bool {
    if settings.target_format.is_dsd() {
        return false;
    }
    source.tracks.iter().any(|track| {
        match (track.bit_depth, resolved_target_bit_depth(track, settings.target_bit_depth)) {
            (Some(source_depth), Some(target_depth)) => source_depth != target_depth,
            _ => false,
        }
    })
}

fn dithering_applies_for_source(
    source: &PreparedSource,
    settings: &tonepoet_pipeline::PipelineSettings,
) -> bool {
    if settings.target_format.is_dsd() {
        return false;
    }
    source.tracks.iter().any(|track| {
        dither_applies(
            track.bit_depth,
            resolved_target_bit_depth(track, settings.target_bit_depth),
            settings.dither_type,
        )
    })
}

fn dither_applies(source_depth: Option<u32>, target_depth: Option<u32>, dither: DitherType) -> bool {
    if dither == DitherType::None {
        return false;
    }
    match (source_depth, target_depth) {
        (Some(source_depth), Some(target_depth)) => target_depth < source_depth,
        _ => false,
    }
}

fn resolved_target_rate_hz(
    track: &PreparedTrack,
    settings: &tonepoet_pipeline::PipelineSettings,
) -> Option<u32> {
    match settings.target_sample_rate {
        RateTarget::Source if !settings.target_format.is_dsd() => source_track_dsd_rate(track)
            .map(DsdRate::default_pcm_target_hz)
            .or_else(|| track.scalar_sample_rate()),
        RateTarget::Source => track.scalar_sample_rate(),
        RateTarget::PcmHz(hz) => Some(hz),
        RateTarget::Dsd(rate) => Some(rate.hz()),
    }
}

fn resolved_target_bit_depth(track: &PreparedTrack, target: BitDepthTarget) -> Option<u32> {
    match target {
        BitDepthTarget::Source => track.bit_depth,
        BitDepthTarget::Pcm(depth) => Some(depth.bits()),
    }
}

fn source_is_dsd(source: &PreparedSource) -> bool {
    source.kind == SourceKind::SacdIso
        || source
            .tracks
            .iter()
            .any(|track| source_track_dsd_rate(track).is_some())
}

fn source_track_dsd_rate(track: &PreparedTrack) -> Option<DsdRate> {
    let rate = DsdRate::from_hz(track.scalar_sample_rate()?)?;
    if track.bit_depth.is_none() || matches!(track.source_ref, TrackSourceRef::SacdTrack { .. }) {
        Some(rate)
    } else {
        None
    }
}

fn source_audio_description(track: &PreparedTrack) -> String {
    if !track.source_audio.channel_groups.is_empty() {
        let coding = track
            .source_audio
            .coding
            .map(source_audio_coding_label)
            .unwrap_or("source");
        let groups = track
            .source_audio
            .channel_groups
            .iter()
            .map(channel_group_description)
            .collect::<Vec<_>>()
            .join("; ");
        let mut label = format!("{coding} [{groups}]");
        if let Some(expected_samples) = track.expected_samples {
            label.push_str(&format!(", {expected_samples} expected samples"));
        }
        return label;
    }

    let mut label = stream_description(track.bit_depth, track.scalar_sample_rate(), Some(&source_track_format_label(track)));
    if let Some(expected_samples) = track.expected_samples {
        label.push_str(&format!(", {expected_samples} expected samples"));
    }
    label
}

fn source_audio_coding_label(coding: SourceAudioCoding) -> &'static str {
    match coding {
        SourceAudioCoding::Pcm => "PCM",
        SourceAudioCoding::Dsd => "DSD",
        SourceAudioCoding::DvdaUnknown => "DVD-Audio",
        SourceAudioCoding::Unknown => "source",
    }
}

fn channel_group_description(group: &ChannelGroupDescriptor) -> String {
    let rate = group
        .sample_rate
        .map(format_sample_rate)
        .unwrap_or_else(|| "unknown rate".to_string());
    let depth = group
        .bit_depth
        .map(|bits| format!("{bits}-bit"))
        .unwrap_or_else(|| "unknown depth".to_string());
    let channels = group
        .assignment
        .as_deref()
        .map(str::to_string)
        .or_else(|| group.channels.map(|count| format!("{count}ch")))
        .unwrap_or_else(|| "unknown channels".to_string());
    format!("group {}: {channels}, {depth}/{rate}", group.group_nr)
}

fn source_track_format_label(track: &PreparedTrack) -> String {
    if source_track_dsd_rate(track).is_some() {
        return "DSD".to_string();
    }
    source_ref_extension(&track.source_ref)
        .as_deref()
        .map(audio_extension_label)
        .unwrap_or("source")
        .to_string()
}

fn source_ref_extension(source_ref: &TrackSourceRef) -> Option<String> {
    let path = match source_ref {
        TrackSourceRef::StagedFile(path) => path,
        TrackSourceRef::CueSegmentCarrier { source_image, .. } => source_image,
        TrackSourceRef::ImageSegment { image, .. } => image,
        TrackSourceRef::SacdTrack { .. } => return Some("dsd".to_string()),
        TrackSourceRef::DvdaTrack { .. } => return Some("dvda".to_string()),
    };
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
}

fn audio_extension_label(extension: &str) -> &'static str {
    match extension.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "flac" => "FLAC",
        "wav" | "wave" => "WAV",
        "aiff" | "aif" => "AIFF",
        "wv" => "WavPack",
        "mp3" => "MP3",
        "m4a" | "mp4" | "aac" => "AAC/ALAC",
        "opus" | "ogg" => "Opus",
        "dsf" | "dff" | "dsd" | "iso" => "DSD",
        _ => "source",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResamplerFamily {
    Auto,
    Ssrc,
    Sox,
    Soxr,
}

fn preferred_resampler_family(settings: &tonepoet_pipeline::PipelineSettings) -> ResamplerFamily {
    if settings.ssrc.force || settings.nyquist_transition == NyquistTransition::BrickWall {
        return ResamplerFamily::Ssrc;
    }
    match &settings.preferred_tool {
        PreferredTool::Ssrc => ResamplerFamily::Ssrc,
        PreferredTool::Sox => ResamplerFamily::Sox,
        PreferredTool::Ffmpeg => ResamplerFamily::Soxr,
        PreferredTool::Auto | PreferredTool::Custom(_) => ResamplerFamily::Auto,
    }
}

fn preferred_resampler_label(settings: &tonepoet_pipeline::PipelineSettings) -> &'static str {
    match preferred_resampler_family(settings) {
        ResamplerFamily::Ssrc => "SSRC",
        ResamplerFamily::Sox => "SoX",
        ResamplerFamily::Soxr => "soxr",
        ResamplerFamily::Auto => "Auto",
    }
}

fn target_sample_rate_setting_label(
    source: &PreparedSource,
    settings: &tonepoet_pipeline::PipelineSettings,
) -> String {
    match settings.target_sample_rate {
        RateTarget::PcmHz(hz) => format_sample_rate(hz),
        RateTarget::Dsd(rate) => dsd_rate_label(rate).to_string(),
        RateTarget::Source => {
            let labels = source
                .tracks
                .iter()
                .filter_map(|track| {
                    resolved_target_rate_hz(track, settings)
                        .filter(|target_rate| Some(*target_rate) != track.scalar_sample_rate())
                        .map(|target_rate| target_rate_setting_label_for_hz(target_rate, settings))
                })
                .collect::<BTreeSet<_>>();
            match labels.len() {
                0 => "source".to_string(),
                1 => labels.into_iter().next().unwrap_or_else(|| "source".to_string()),
                _ => format!(
                    "source-derived ({})",
                    labels.into_iter().collect::<Vec<_>>().join(", ")
                ),
            }
        }
    }
}

fn target_rate_setting_label_for_hz(
    rate_hz: u32,
    settings: &tonepoet_pipeline::PipelineSettings,
) -> String {
    if settings.target_format.is_dsd() {
        DsdRate::from_hz(rate_hz)
            .map(dsd_rate_label)
            .map(str::to_string)
            .unwrap_or_else(|| format_sample_rate(rate_hz))
    } else {
        format_sample_rate(rate_hz)
    }
}

fn bit_depth_target_label(target: BitDepthTarget) -> String {
    match target {
        BitDepthTarget::Source => "source".to_string(),
        BitDepthTarget::Pcm(depth) => pcm_bit_depth_label(depth).to_string(),
    }
}

fn pcm_bit_depth_label(depth: PcmBitDepth) -> &'static str {
    match depth {
        PcmBitDepth::Int8 => "8-bit",
        PcmBitDepth::Int16 => "16-bit",
        PcmBitDepth::Int24 => "24-bit",
        PcmBitDepth::Int32 => "32-bit integer",
        PcmBitDepth::Float32 => "32-bit float",
        PcmBitDepth::Float64 => "64-bit float",
    }
}

fn dsd_rate_label(rate: DsdRate) -> &'static str {
    match rate {
        DsdRate::Dsd64 => "DSD64",
        DsdRate::Dsd128 => "DSD128",
        DsdRate::Dsd256 => "DSD256",
        DsdRate::Dsd512 => "DSD512",
        DsdRate::Dsd1024 => "DSD1024",
    }
}

fn dither_type_label(value: DitherType) -> &'static str {
    match value {
        DitherType::None => "none",
        DitherType::Tpdf => "TPDF",
        DitherType::SlopedTpdf => "sloped TPDF",
        DitherType::Shibata => "Shibata",
        DitherType::Lipshitz => "Lipshitz",
        DitherType::FWeighted => "F-weighted",
        DitherType::ModifiedEWeighted => "modified E-weighted",
        DitherType::ImprovedEWeighted => "improved E-weighted",
        DitherType::Gesemann => "Gesemann",
        DitherType::LowShibata => "low-Shibata",
        DitherType::HighShibata => "high-Shibata",
    }
}

fn resample_quality_label(value: ResampleQuality) -> &'static str {
    match value {
        ResampleQuality::Low => "low",
        ResampleQuality::Medium => "medium",
        ResampleQuality::High => "high",
        ResampleQuality::VeryHigh => "very high",
        ResampleQuality::Ultra => "ultra",
        ResampleQuality::Insane => "insane",
    }
}

fn nyquist_transition_label(value: NyquistTransition) -> &'static str {
    match value {
        NyquistTransition::Gentle => "gentle",
        NyquistTransition::Medium => "medium",
        NyquistTransition::Steep => "steep",
        NyquistTransition::Sharp => "sharp",
        NyquistTransition::BrickWall => "brick-wall",
    }
}

fn mp3_mode_label(value: Mp3Mode) -> &'static str {
    match value {
        Mp3Mode::Cbr => "CBR",
        Mp3Mode::Vbr => "VBR",
        Mp3Mode::Abr => "ABR",
    }
}

fn aac_profile_label(value: AacProfile) -> &'static str {
    match value {
        AacProfile::LcAac => "LC-AAC",
        AacProfile::HeAac => "HE-AAC",
        AacProfile::HeAacV2 => "HE-AAC v2",
    }
}

fn opus_content_type_label(value: OpusContentType) -> &'static str {
    match value {
        OpusContentType::Auto => "auto",
        OpusContentType::Music => "music",
        OpusContentType::Speech => "speech",
    }
}

fn wavpack_mode_label(value: WavPackMode) -> &'static str {
    match value {
        WavPackMode::Normal => "normal",
        WavPackMode::Fast => "fast",
        WavPackMode::High => "high",
        WavPackMode::VeryHigh => "very high",
    }
}

fn ssrc_profile_label(
    profile: Option<SsrcProfile>,
    quality: ResampleQuality,
    insane_mode: bool,
) -> &'static str {
    if insane_mode {
        return "insane";
    }
    match profile {
        Some(SsrcProfile::Insane) => "insane",
        Some(SsrcProfile::High) => "high",
        Some(SsrcProfile::Long) => "long",
        Some(SsrcProfile::Standard) => "standard",
        Some(SsrcProfile::Short) => "short",
        Some(SsrcProfile::Fast) => "fast",
        Some(SsrcProfile::Lightning) => "lightning",
        None => resample_quality_label(quality),
    }
}

fn ssrc_pdf_type_label(value: SsrcPdfType) -> &'static str {
    match value {
        SsrcPdfType::Rectangular => "rectangular",
        SsrcPdfType::Triangular => "triangular",
    }
}

fn sox_sinc_phase_label(value: SoxSincPhase) -> &'static str {
    match value {
        SoxSincPhase::Linear => "linear",
        SoxSincPhase::Minimum => "minimum",
        SoxSincPhase::Intermediate => "intermediate",
    }
}

fn dsd_gain_mode_label(value: DsdToPcmGainMode) -> &'static str {
    match value {
        DsdToPcmGainMode::Disabled => "disabled",
        DsdToPcmGainMode::Auto => "auto",
        DsdToPcmGainMode::Manual => "manual",
    }
}

fn dsd_lowpass_method_label(value: DsdLowpassMethod) -> &'static str {
    match value {
        DsdLowpassMethod::Auto => "auto",
        DsdLowpassMethod::SoxUltra => "SoX ultra",
        DsdLowpassMethod::Sinc => "sinc",
    }
}

fn artwork_mime_label(mime: &str) -> &'static str {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "JPEG",
        "image/png" => "PNG",
        "image/webp" => "WebP",
        "image/gif" => "GIF",
        _ => "artwork",
    }
}

fn cue_artwork_log_outcome(
    req: &PipelineRequest,
    outcome: &AlbumOutcome,
    artifacts: &ArtifactSet,
) -> String {
    if !req.settings.metadata.preserve_artwork {
        return "skipped (artwork preservation disabled)".to_string();
    }

    if let Some(planner_transfer) = planner_artwork_transfer_label(artifacts) {
        if planner_transfer.complete {
            return planner_transfer.label;
        }
        return format!(
            "{}; {}",
            planner_transfer.label,
            cue_artwork_metadata_stage_outcome(req, outcome)
        );
    }

    cue_artwork_metadata_stage_outcome(req, outcome)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtworkTransferLabel {
    label: String,
    complete: bool,
}

fn planner_artwork_transfer_label(artifacts: &ArtifactSet) -> Option<ArtworkTransferLabel> {
    let AudioArtifacts::Tracks(tracks) = &artifacts.audio else {
        return None;
    };
    let total = tracks.len();
    if total == 0 {
        return None;
    }
    let transferred = tracks
        .iter()
        .filter(|track| track.metadata_satisfaction.artwork_transferred)
        .count();
    if transferred == 0 {
        return None;
    }
    if transferred == total {
        return Some(ArtworkTransferLabel {
            label: "planner transferred artwork into output".to_string(),
            complete: true,
        });
    }
    Some(ArtworkTransferLabel {
        label: format!("planner transferred artwork for {transferred}/{total} output(s)"),
        complete: false,
    })
}

fn cue_artwork_metadata_stage_outcome(req: &PipelineRequest, outcome: &AlbumOutcome) -> String {
    if req.stages.metadata == StageRequirement::Disabled {
        return "skipped (metadata stage disabled)".to_string();
    }
    if !cue_artwork_supported_by_target(&req.settings.target_format) {
        return format!(
            "skipped ({} container unsupported)",
            req.settings.target_format.display_name()
        );
    }

    match metadata_stage_outcome(outcome) {
        Some(StageOutcome::Ok) => {
            "metadata stage completed post-encode artwork embedding".to_string()
        }
        Some(StageOutcome::Skipped) => "not confirmed (metadata stage skipped)".to_string(),
        Some(StageOutcome::Failed(reason)) => {
            format!("not confirmed (metadata stage failed: {reason})")
        }
        None => "not confirmed (metadata stage outcome unavailable)".to_string(),
    }
}

fn metadata_stage_outcome(outcome: &AlbumOutcome) -> Option<&StageOutcome> {
    outcome_stage_records(outcome)
        .iter()
        .find(|stage| stage.stage == PipelineStage::Metadata)
        .map(|stage| &stage.outcome)
}

fn cue_artwork_supported_by_target(format: &PlannerAudioFormat) -> bool {
    matches!(
        format,
        PlannerAudioFormat::Flac
            | PlannerAudioFormat::Mp3
            | PlannerAudioFormat::Aac
            | PlannerAudioFormat::Alac
            | PlannerAudioFormat::WavPack
    )
}

fn decimal_label(value: f32) -> String {
    let mut text = format!("{value:.3}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
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
        BlockReason::EncryptedSource => "encrypted source".to_string(),
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
        SourceKind::DvdAudio => "DvdAudio",
    }
}

fn track_source_ref_label(source_ref: &TrackSourceRef) -> String {
    match source_ref {
        TrackSourceRef::StagedFile(path) => format!("staged file {}", path_log_value(path)),
        TrackSourceRef::CueSegmentCarrier {
            path,
            source_image,
            start_sample,
            samples,
            carrier,
        } => format!(
            "typed CUE segment carrier {} ({:?}, source {}, start sample {start_sample}, {samples} samples)",
            path_log_value(path),
            carrier,
            path_log_value(source_image)
        ),
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
        TrackSourceRef::DvdaTrack {
            volume_source,
            group_nr,
            title_set_nr,
            title_ordinal,
            group_track_ordinal,
            ats_track_nr,
            samg_track_nr,
            samg_ordinal,
            sector_address_space,
            ..
        } => match sector_address_space {
            DvdaSectorAddressSpace::AtsAobRelative { .. } => format!(
                "DVD-Audio group {group_nr} track {group_track_ordinal} ATS {} title {} chapter {} from {}",
                title_set_nr
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                title_ordinal
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                ats_track_nr
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                path_log_value(volume_source.original_container())
            ),
            DvdaSectorAddressSpace::SamgAbsolute => format!(
                "DVD-Audio group {group_nr} track {group_track_ordinal} SAMG track {} ordinal {} from {}",
                samg_track_nr
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                samg_ordinal
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                path_log_value(volume_source.original_container())
            ),
        },
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
        Err(MaterializeError::BlockedSource { message, blocked }) => {
            let blocked = *blocked;
            let record = stage_record(PipelineStage::Materialize, StageOutcome::Failed(message.clone()));
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            let failed = blocked_track_records(&blocked.source, &message);
            let outcome = AlbumOutcome::Blocked {
                successful: Vec::new(),
                failed,
                stages,
                reason: BlockReason::EncryptedSource,
            };
            return ScheduledMaterialization::Finished(
                finalize_report(
                    &req,
                    reporter,
                    Some(blocked.source),
                    None,
                    None,
                    None,
                    outcome,
                )
                .await,
            );
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
    match realize_track_with_tool_limits_and_stats(
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
        Ok(realized) => Ok(ScheduledRealizedTrack {
            index: track_index,
            track,
            final_path,
            realized_path: realized.path,
            realized_dsd_dst_stats: realized.dsd_dst_stats,
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
                let actual_samples = match validate_encoded_output_with_tool_limits(
                    &staged_path,
                    realized.track.expected_samples,
                    &realized.req.settings.target_format,
                    &runner,
                    &realized.cancel,
                    tool_concurrency_limits.as_ref(),
                )
                .await
                {
                    Ok(samples) => samples,
                    Err(err) => {
                        let commands = command_from_convert_error(&err);
                        let record = failed_track_record(
                            &realized.track,
                            Some(realized.realized_path),
                            Some(staged_path),
                            commands,
                            err.to_string(),
                        );
                        return Ok(ScheduledTrackOutput { index: realized.index, record, artifact: None, ok: false, metadata_satisfaction: PlannedMetadataSatisfaction::none() });
                    }
                };
                let mut dsd_dst_stats = realized.realized_dsd_dst_stats.clone();
                merge_optional_dsd_dst_stats(
                    &mut dsd_dst_stats,
                    dsd_dst_stats_from_file(&staged_path, None, bytes_out),
                );
                apply_byte_totals_to_stats(&mut dsd_dst_stats, bytes_in, bytes_out);
                let mut commands = executed.commands;
                append_dsd_dst_stats_to_command_descriptions(&mut commands, dsd_dst_stats.as_ref());
                let record = TrackRecord {
                    track_id: realized.track.id.clone(),
                    outcome: TrackOutcome::Ok,
                    source_ref: realized.track.source_ref.clone(),
                    realized_input: Some(realized.realized_path.clone()),
                    output_file: Some(staged_path.clone()),
                    commands,
                    bytes_in,
                    bytes_out,
                    duration: Some(executed.elapsed),
                    dsd_dst_stats,
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
            let error = err.to_string();
            let commands = err.commands;
            let record = failed_track_record(
                &realized.track,
                Some(realized.realized_path),
                Some(staged_path),
                commands,
                error,
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

    let failed = records.iter().any(|record| !matches!(record.outcome, TrackOutcome::Ok));
    let record = convert_stage_record_for_tracks(
        &records,
        if failed && req.failure_policy == FailurePolicy::FailAlbumOnAnyTrackFailure {
            StageOutcome::Failed("one or more tracks failed".to_string())
        } else {
            StageOutcome::Ok
        },
    );
    ConvertStageResult {
        tracks: records,
        artifacts: ArtifactSet {
            audio: AudioArtifacts::Tracks(artifacts),
            sidecars: Vec::new(),
        },
        record,
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
        Err(MaterializeError::BlockedSource { message, blocked }) => {
            let blocked = *blocked;
            let record = stage_record(
                PipelineStage::Materialize,
                StageOutcome::Failed(message.clone()),
            );
            emit_stage_finished(reporter, &item_id, record.clone()).await;
            stages.push(record);
            let failed = blocked_track_records(&blocked.source, &message);
            let outcome = AlbumOutcome::Blocked {
                successful: Vec::new(),
                failed,
                stages,
                reason: BlockReason::EncryptedSource,
            };
            return finalize_report(
                &req,
                reporter,
                Some(blocked.source),
                plan,
                artifacts,
                published,
                outcome,
            )
            .await;
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
    let settings_fingerprint = tonepoet_pipeline::fingerprint::settings_fingerprint(&req.settings);
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
            settings_fingerprint: Some(settings_fingerprint),
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
        settings_fingerprint: Some(settings_fingerprint),
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
    StageRecord {
        stage,
        outcome,
        dsd_dst_stats: None,
    }
}

fn convert_stage_record_for_tracks(tracks: &[TrackRecord], outcome: StageOutcome) -> StageRecord {
    StageRecord {
        stage: PipelineStage::Convert,
        outcome,
        dsd_dst_stats: aggregate_track_dsd_dst_stats(tracks),
    }
}

fn aggregate_track_dsd_dst_stats(tracks: &[TrackRecord]) -> Option<DsdDstPipelineStats> {
    let mut aggregate = DsdDstPipelineStats::default();
    for stats in tracks.iter().filter_map(|record| record.dsd_dst_stats.as_ref()) {
        aggregate.merge(stats);
    }
    if aggregate.is_empty() {
        None
    } else {
        Some(aggregate)
    }
}

fn merge_optional_dsd_dst_stats(
    base: &mut Option<DsdDstPipelineStats>,
    incoming: Option<DsdDstPipelineStats>,
) {
    let Some(incoming) = incoming else {
        return;
    };
    if incoming.is_empty() {
        return;
    }
    match base {
        Some(base) => base.merge(&incoming),
        None => *base = Some(incoming),
    }
}

fn apply_byte_totals_to_stats(
    stats: &mut Option<DsdDstPipelineStats>,
    bytes_in: Option<u64>,
    bytes_out: Option<u64>,
) {
    let Some(stats) = stats.as_mut() else {
        return;
    };
    if let Some(bytes_in) = bytes_in {
        stats.bytes_read = bytes_in;
    }
    if let Some(bytes_out) = bytes_out {
        stats.bytes_written = bytes_out;
    }
}

fn append_dsd_dst_stats_to_command_descriptions(
    commands: &mut [CommandRecord],
    stats: Option<&DsdDstPipelineStats>,
) {
    let Some(stats) = stats.filter(|stats| !stats.is_empty()) else {
        return;
    };
    let Some(command) = commands.first_mut() else {
        return;
    };
    let summary = format_dsd_dst_stats_inline(stats);
    command.description = Some(match command.description.take() {
        Some(existing) if !existing.trim().is_empty() => format!("{}; {}", existing.trim(), summary),
        _ => summary,
    });
}

fn dsd_dst_stats_from_file(
    path: &Path,
    bytes_read: Option<u64>,
    bytes_written: Option<u64>,
) -> Option<DsdDstPipelineStats> {
    if !is_dsd_container_path(path) {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let report = validate_dsd_stream(
        file,
        DsdValidationOptions {
            mode: DsdValidationMode::DecodeDst,
            max_frames: None,
        },
    );
    let mut stats = dsd_dst_stats_from_validation_report(&report);
    let output_role = bytes_written.is_some() && bytes_read.is_none();
    let input_role = bytes_read.is_some() && bytes_written.is_none();
    if output_role {
        stats.frames_read = 0;
        stats.frames_decoded = 0;
        stats.dst_decoded_frames = 0;
    }
    if input_role {
        stats.frames_emitted = 0;
    }
    stats.bytes_read = bytes_read.or_else(|| file_len(path)).unwrap_or(0);
    stats.bytes_written = bytes_written.unwrap_or(0);
    if stats.is_empty() {
        None
    } else {
        Some(stats)
    }
}

fn is_dsd_container_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "dsf" | "dff"))
        .unwrap_or(false)
}

fn dsd_dst_stats_from_validation_report(report: &DsdValidationReport) -> DsdDstPipelineStats {
    let mut stats = DsdDstPipelineStats {
        frames_read: report.frames_seen(),
        frames_decoded: report.dst_frames_seen,
        frames_emitted: report.dsd_frames_seen.saturating_add(report.dst_frames_seen),
        crc_checked: report.dstc_passed_frames.saturating_add(report.dstc_failed_frames),
        crc_passed: report.dstc_passed_frames,
        crc_failed: report.dstc_failed_frames.saturating_add(report.dstc_malformed_frames),
        crc_missing: report.dstc_no_crc_frames,
        dst_decoded_frames: report.dst_frames_seen,
        ..DsdDstPipelineStats::default()
    };
    if let Some(failure) = report.failures.first() {
        stats.first_error_frame = failure.frame_index;
        stats.first_error_offset = failure.offset;
    }
    stats
}

fn dsd_dst_stats_from_extract_report(
    report: &ExtractReport,
    bytes_written: Option<u64>,
) -> Option<DsdDstPipelineStats> {
    let mut stats = DsdDstPipelineStats {
        frames_read: report.stats.frames_read,
        frames_emitted: report.stats.frames_read,
        bytes_written: bytes_written.unwrap_or(report.stats.audio_bytes),
        ..DsdDstPipelineStats::default()
    };

    if let Some(writer) = report.dff_dst_stats() {
        stats.frames_emitted = writer.frames_written;
        stats.dst_passthrough_frames = writer.passthrough_frames_written;
        stats.dst_reencoded_frames = writer.predictive_frames_written;
        stats.dst_raw_fallback_frames = writer.raw_frames_written;
        stats.bytes_read = writer.total_raw_bytes;
        stats.bytes_written = writer.total_encoded_bytes;
    }

    if stats.is_empty() {
        None
    } else {
        Some(stats)
    }
}

fn format_dsd_dst_stats_inline(stats: &DsdDstPipelineStats) -> String {
    format!(
        "DSD/DST stats: frames read {}, decoded {}, emitted {}; CRC checked {} (passed {}, failed {}, missing {}); DST passthrough {}, decoded {}, reencoded {}, raw {}; bytes read {}, written {}{}",
        stats.frames_read,
        stats.frames_decoded,
        stats.frames_emitted,
        stats.crc_checked,
        stats.crc_passed,
        stats.crc_failed,
        stats.crc_missing,
        stats.dst_passthrough_frames,
        stats.dst_decoded_frames,
        stats.dst_reencoded_frames,
        stats.dst_raw_fallback_frames,
        stats.bytes_read,
        stats.bytes_written,
        first_dsd_dst_error_suffix(stats),
    )
}

fn first_dsd_dst_error_suffix(stats: &DsdDstPipelineStats) -> String {
    match (stats.first_error_frame, stats.first_error_offset) {
        (None, None) => String::new(),
        (frame, offset) => format!(
            "; first error frame {}, offset {}",
            frame.map(|value| value.to_string()).unwrap_or_else(|| "unknown".to_string()),
            offset.map(|value| value.to_string()).unwrap_or_else(|| "unknown".to_string()),
        ),
    }
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
    let sample_rate = track
        .scalar_sample_rate()
        .map(format_sample_rate)
        .map(|value| sanitize_component(&value))
        .unwrap_or_default();
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
        .and_then(|track| track.scalar_sample_rate())
        .map(format_sample_rate)
        .map(|value| sanitize_component(&value))
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

fn default_audio_container_extension<'a>(
    format: &'a PlannerAudioFormat,
    container_extension: Option<&'a str>,
) -> &'a str {
    match format {
        PlannerAudioFormat::Aac | PlannerAudioFormat::Alac => match container_extension {
            Some(extension) if matches!(extension.trim().to_ascii_lowercase().as_str(), "m4a" | "mp4") => {
                extension
            }
            _ => "m4a",
        },
        _ => match container_extension {
            Some(extension) if !extension.trim().is_empty() => extension,
            _ => format.extension(),
        },
    }
}

fn validate_final_container_extension(
    format: &PlannerAudioFormat,
    container_extension: Option<&str>,
) -> Result<(), String> {
    let Some(extension) = container_extension
        .map(str::trim)
        .filter(|extension| !extension.is_empty())
        .map(|extension| extension.to_ascii_lowercase())
    else {
        return Ok(());
    };

    match format {
        PlannerAudioFormat::Aac => match extension.as_str() {
            "m4a" | "mp4" => Ok(()),
            "aac" => Err(
                "AAC output is muxed as MP4/M4A by this pipeline; raw .aac output is not implemented, so use .m4a/.mp4 or add an explicit raw-AAC mode".to_string(),
            ),
            _ => Err(
                "AAC output must use an .m4a or .mp4 container extension unless an explicit raw-AAC mode is implemented".to_string(),
            ),
        },
        PlannerAudioFormat::Alac => match extension.as_str() {
            "m4a" | "mp4" => Ok(()),
            _ => Err("ALAC output must use an .m4a or .mp4 container extension".to_string()),
        },
        _ => Ok(()),
    }
}

fn append_default_extension(
    path: &mut PathBuf,
    format: &PlannerAudioFormat,
    container_extension: Option<&str>,
) {
    let ext = default_audio_container_extension(format, container_extension);
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
    let extension = final_path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.to_ascii_lowercase())
        .filter(|extension| match format {
            PlannerAudioFormat::Aac | PlannerAudioFormat::Alac => {
                matches!(extension.as_str(), "m4a" | "mp4")
            }
            _ => true,
        })
        .unwrap_or_else(|| default_audio_container_extension(format, None).to_string());

    convert_root.join(format!(
        "{:03}-{}.{}",
        id.source_ordinal,
        file_stem,
        extension
    ))
}

#[cfg(test)]
mod cue_container_extension_tests {
    use super::*;

    #[test]
    fn aac_and_alac_default_to_m4a_container() {
        let mut aac = PathBuf::from("03 - Track");
        append_default_extension(&mut aac, &PlannerAudioFormat::Aac, None);
        assert_eq!(aac.extension().and_then(|value| value.to_str()), Some("m4a"));

        let mut alac = PathBuf::from("03 - Track");
        append_default_extension(&mut alac, &PlannerAudioFormat::Alac, None);
        assert_eq!(alac.extension().and_then(|value| value.to_str()), Some("m4a"));
    }

    #[test]
    fn staged_aac_and_alac_paths_use_m4a_so_ffmpeg_selects_mp4_muxer() {
        let id = TrackId {
            source_ordinal: 3,
            disc_number: None,
            track_number: 3,
        };
        let convert_root = Path::new("convert");

        let aac = staged_audio_path(
            convert_root,
            Path::new("03 - Track.m4a"),
            &id,
            &PlannerAudioFormat::Aac,
        );
        assert_eq!(aac.extension().and_then(|value| value.to_str()), Some("m4a"));

        let alac = staged_audio_path(
            convert_root,
            Path::new("03 - Track.m4a"),
            &id,
            &PlannerAudioFormat::Alac,
        );
        assert_eq!(alac.extension().and_then(|value| value.to_str()), Some("m4a"));
    }

    #[test]
    fn explicit_container_extension_override_is_honored() {
        let mut wav = PathBuf::from("track");
        append_default_extension(&mut wav, &PlannerAudioFormat::Wav, Some("rf64"));
        assert_eq!(wav.extension().and_then(|value| value.to_str()), Some("rf64"));
    }

    #[test]
    fn explicit_raw_aac_container_extension_is_rejected() {
        let err = validate_final_container_extension(&PlannerAudioFormat::Aac, Some("aac"))
            .expect_err("raw AAC is not an implemented AAC container mode");
        assert!(err.contains("raw .aac output is not implemented"), "{err}");
        assert_eq!(default_audio_container_extension(&PlannerAudioFormat::Aac, Some("aac")), "m4a");
    }

    #[test]
    fn explicit_aac_mp4_containers_are_accepted() {
        validate_final_container_extension(&PlannerAudioFormat::Aac, Some("m4a")).unwrap();
        validate_final_container_extension(&PlannerAudioFormat::Aac, Some("mp4")).unwrap();
        validate_final_container_extension(&PlannerAudioFormat::Alac, Some("m4a")).unwrap();
        validate_final_container_extension(&PlannerAudioFormat::Alac, Some("mp4")).unwrap();
    }

    #[test]
    fn template_raw_aac_suffix_is_normalized_to_m4a_default() {
        let mut aac = PathBuf::from("03 - Track.aac");
        append_default_extension(&mut aac, &PlannerAudioFormat::Aac, None);
        assert_eq!(aac.extension().and_then(|value| value.to_str()), Some("m4a"));
    }

    #[test]
    fn staged_path_respects_final_path_extension_override() {
        let id = TrackId {
            source_ordinal: 4,
            disc_number: None,
            track_number: 4,
        };
        let staged = staged_audio_path(
            Path::new("convert"),
            Path::new("04 - Track.rf64"),
            &id,
            &PlannerAudioFormat::Wav,
        );
        assert_eq!(staged.extension().and_then(|value| value.to_str()), Some("rf64"));
    }


    #[test]
    fn staged_aac_path_does_not_inherit_raw_aac_suffix() {
        let id = TrackId {
            source_ordinal: 5,
            disc_number: None,
            track_number: 5,
        };
        let staged = staged_audio_path(
            Path::new("convert"),
            Path::new("05 - Track.aac"),
            &id,
            &PlannerAudioFormat::Aac,
        );
        assert_eq!(staged.extension().and_then(|value| value.to_str()), Some("m4a"));
    }
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
        dsd_dst_stats: None,
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
                        TrackSourceRef::CueSegmentCarrier { source_image, .. } => source_image.clone(),
                        TrackSourceRef::ImageSegment { image, .. } => image.clone(),
                        TrackSourceRef::SacdTrack { iso, .. } => iso.clone(),
                        TrackSourceRef::DvdaTrack { volume_source, .. } => {
                            volume_source.original_container().clone()
                        }
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
                    sample_rate: Some(44_100),
                    source_audio: SourceAudioDescriptor::from_scalar(
                        Some(44_100),
                        Some(24),
                        Some(SourceAudioCoding::Pcm),
                    ),
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
                    sample_rate: Some(96_000),
                    source_audio: SourceAudioDescriptor::from_scalar(
                        Some(96_000),
                        Some(24),
                        Some(SourceAudioCoding::Pcm),
                    ),
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
                dvda_group_selection: DvdaGroupSelection::Default,
                dvda_group: None,
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
            description: None,
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
            dsd_dst_stats: None,
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
            dsd_dst_stats: None,
        }
    }

    fn stage_records() -> Vec<StageRecord> {
        vec![
            StageRecord {
                stage: PipelineStage::Materialize,
                outcome: StageOutcome::Ok,
                dsd_dst_stats: None,
            },
            StageRecord {
                stage: PipelineStage::ReplayGain,
                outcome: StageOutcome::Skipped,
                dsd_dst_stats: None,
            },
            StageRecord {
                stage: PipelineStage::Features,
                outcome: StageOutcome::Ok,
                dsd_dst_stats: None,
            },
        ]
    }

    fn log_test_artifacts() -> ArtifactSet {
        ArtifactSet {
            audio: AudioArtifacts::Tracks(vec![
                TrackArtifact {
                    track_id: TrackId {
                        source_ordinal: 1,
                        disc_number: Some(1),
                        track_number: 1,
                    },
                    staged_path: PathBuf::from("/encoded/01.flac"),
                    final_path: PathBuf::from("/out/01.flac"),
                    samples: Some(44_100),
                    metadata_satisfaction: PlannedMetadataSatisfaction::none(),
                    metadata_required: PlannedMetadataSatisfaction::none(),
                    planned_command_hash: None,
                },
                TrackArtifact {
                    track_id: TrackId {
                        source_ordinal: 2,
                        disc_number: Some(1),
                        track_number: 2,
                    },
                    staged_path: PathBuf::from("/encoded/02.flac"),
                    final_path: PathBuf::from("/out/02.flac"),
                    samples: None,
                    metadata_satisfaction: PlannedMetadataSatisfaction::none(),
                    metadata_required: PlannedMetadataSatisfaction::none(),
                    planned_command_hash: None,
                },
            ]),
            sidecars: Vec::new(),
        }
    }

    #[test]
    fn build_conversion_log_complete_contains_required_sections() {
        let source = log_test_source();
        let req = log_test_request();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };
        let artifacts = log_test_artifacts();
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

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
        let artifacts = log_test_artifacts();
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

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
                dsd_dst_stats: None,
            }],
            reason: BlockReason::RequiredStageFailure(PipelineStage::Convert),
        };
        let artifacts = log_test_artifacts();
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

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
        let artifacts = log_test_artifacts();
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

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
        let artifacts = log_test_artifacts();
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains("Materialize: Ok"));
        assert!(log.contains("ReplayGain: Skipped"));
        assert!(log.contains("Features: Ok"));
    }

    #[test]
    fn format_aware_settings_include_only_target_codec_family() {
        let source = log_test_source();
        let artifacts = log_test_artifacts();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };

        let mut flac_req = log_test_request();
        flac_req.settings.target_format = PlannerAudioFormat::Flac;
        let flac_log = build_conversion_log(&outcome, &source, &flac_req, &artifacts, None);
        assert!(flac_log.contains("FLAC compression"));
        assert!(!flac_log.contains("MP3 mode"));
        assert!(!flac_log.contains("AAC profile"));
        assert!(!flac_log.contains("Opus bitrate"));
        assert!(!flac_log.contains("WavPack mode"));

        let mut mp3_req = log_test_request();
        mp3_req.settings.target_format = PlannerAudioFormat::Mp3;
        mp3_req.settings.mp3.mode = Mp3Mode::Cbr;
        let mp3_log = build_conversion_log(&outcome, &source, &mp3_req, &artifacts, None);
        assert!(mp3_log.contains("MP3 mode: CBR"));
        assert!(mp3_log.contains("MP3 bitrate: 320 kbps"));
        assert!(!mp3_log.contains("FLAC compression"));
        assert!(!mp3_log.contains("AAC profile"));
    }

    #[test]
    fn resampler_settings_are_printed_only_for_actual_rate_changes() {
        let source = log_test_source();
        let artifacts = log_test_artifacts();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };

        let req = log_test_request();
        let no_resample_log = build_conversion_log(&outcome, &source, &req, &artifacts, None);
        assert!(!no_resample_log.contains("Preferred resampler tool"));
        assert!(!no_resample_log.contains("SSRC profile"));

        let mut resample_req = log_test_request();
        resample_req.settings.target_sample_rate = RateTarget::PcmHz(48_000);
        resample_req.settings.preferred_tool = PreferredTool::Ssrc;
        resample_req.settings.ssrc.attenuation_db = Some(1.5);
        resample_req.settings.ssrc.min_phase = true;
        resample_req.settings.ssrc.dither_id = Some(2);
        resample_req.settings.ssrc.pdf_type = Some(SsrcPdfType::Triangular);
        let resample_log = build_conversion_log(&outcome, &source, &resample_req, &artifacts, None);
        assert!(resample_log.contains("Preferred resampler tool: SSRC"));
        assert!(resample_log.contains("SSRC attenuation: 1.5 dB"));
        assert!(resample_log.contains("SSRC minimum phase: Yes"));
        assert!(resample_log.contains("SSRC dither ID: 2"));
        assert!(resample_log.contains("SSRC PDF type: triangular"));
    }

    #[test]
    fn soxr_settings_use_precise_labels_and_do_not_invent_stopband() {
        let source = log_test_source();
        let artifacts = log_test_artifacts();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };

        let mut req = log_test_request();
        req.settings.target_sample_rate = RateTarget::PcmHz(48_000);
        req.settings.preferred_tool = PreferredTool::Ffmpeg;
        req.settings.resample_quality = ResampleQuality::VeryHigh;
        req.settings.soxr_resampler.cutoff = Some(0.97);
        req.settings.soxr_resampler.phase = Some(45);
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains("Preferred resampler tool: soxr"));
        assert!(log.contains("Soxr quality preset: very high"));
        assert!(log.contains("Soxr cutoff override: 0.97"));
        assert!(log.contains("Soxr phase response: 45"));
        assert!(!log.contains("Soxr precision"));
        assert!(!log.contains("Soxr passband end"));
        assert!(!log.contains("Soxr stopband begin"));
    }

    #[test]
    fn soxr_log_does_not_claim_identical_passband_and_stopband_from_single_cutoff() {
        let source = log_test_source();
        let artifacts = log_test_artifacts();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };

        let mut req = log_test_request();
        req.settings.target_sample_rate = RateTarget::PcmHz(48_000);
        req.settings.preferred_tool = PreferredTool::Ffmpeg;
        req.settings.soxr_resampler.cutoff = Some(0.91);
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains("Soxr cutoff override: 0.91"));
        assert!(!log.contains("Soxr passband end: 0.91"));
        assert!(!log.contains("Soxr stopband begin: 0.91"));
    }

    #[test]
    fn dsd_settings_are_printed_only_for_dsd_sources() {
        let artifacts = log_test_artifacts();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };
        let req = log_test_request();
        let pcm_log = build_conversion_log(&outcome, &log_test_source(), &req, &artifacts, None);
        assert!(!pcm_log.contains("DSD gain mode"));

        let mut dsd_source = log_test_source();
        dsd_source.kind = SourceKind::SacdIso;
        dsd_source.tracks[0].sample_rate = Some(2_822_400);
        dsd_source.tracks[0].bit_depth = None;
        let mut dsd_req = log_test_request();
        dsd_req.settings.dsd.dsd_to_pcm_gain_mode = DsdToPcmGainMode::Auto;
        let dsd_log = build_conversion_log(&outcome, &dsd_source, &dsd_req, &artifacts, None);
        assert!(dsd_log.contains("DSD gain mode: auto"));
        assert!(dsd_log.contains("DSD auto gain margin"));
        assert!(dsd_log.contains("DSD→PCM lowpass method"));
        assert!(!dsd_log.contains("DSD filter preset"));
        assert!(!dsd_log.contains("PCM→DSD filter preset"));
    }

    #[test]
    fn pcm_to_dsd_settings_use_specific_filter_preset_label() {
        let mut source = log_test_source();
        source.tracks[0].source_ref = TrackSourceRef::StagedFile(PathBuf::from("/stage/01.wav"));
        source.tracks[0].sample_rate = Some(88_200);
        source.tracks[0].bit_depth = Some(24);

        let mut req = log_test_request();
        req.settings.target_format = PlannerAudioFormat::Dsf;
        req.settings.target_sample_rate = RateTarget::Dsd(DsdRate::Dsd64);

        let artifacts = log_test_artifacts();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains("PCM→DSD filter preset"));
        // "DSD filter preset" without the PCM→ prefix must not appear
        assert!(!log.lines().any(|line| {
            line.contains("DSD filter preset") && !line.contains("PCM→DSD filter preset")
        }));
        assert!(!log.contains("DSD→PCM lowpass method"));
    }

    #[test]
    fn pipeline_line_prefers_planned_command_descriptions() {
        let source = log_test_source();
        let req = log_test_request();
        let artifacts = log_test_artifacts();
        let mut record = ok_record();
        record.commands[0].description = Some("Decode FLAC to PCM".to_string());
        let mut second = command_record();
        second.description = Some("Encode FLAC level 8".to_string());
        record.commands.push(second);
        let outcome = AlbumOutcome::Complete {
            tracks: vec![record],
            stages: stage_records(),
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains("Pipeline: Decode FLAC to PCM → Encode FLAC level 8"));
    }

    #[test]
    fn dsd_dst_stats_are_written_to_track_stage_and_command_log() {
        let source = log_test_source();
        let req = log_test_request();
        let artifacts = log_test_artifacts();
        let mut record = ok_record();
        record.dsd_dst_stats = Some(DsdDstPipelineStats {
            frames_read: 2,
            frames_decoded: 1,
            frames_emitted: 2,
            crc_checked: 1,
            crc_passed: 1,
            crc_missing: 1,
            dst_passthrough_frames: 1,
            dst_decoded_frames: 1,
            dst_reencoded_frames: 0,
            dst_raw_fallback_frames: 0,
            bytes_read: 8192,
            bytes_written: 4096,
            ..DsdDstPipelineStats::default()
        });
        append_dsd_dst_stats_to_command_descriptions(&mut record.commands, record.dsd_dst_stats.as_ref());
        let stage = convert_stage_record_for_tracks(&[record.clone()], StageOutcome::Ok);

        assert_eq!(stage.dsd_dst_stats.as_ref().unwrap().frames_read, 2);
        assert!(record.commands[0]
            .description
            .as_deref()
            .unwrap()
            .contains("DSD/DST stats: frames read 2, decoded 1, emitted 2"));

        let outcome = AlbumOutcome::Complete {
            tracks: vec![record],
            stages: vec![stage],
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains("DSD/DST stats: frames read 2, decoded 1, emitted 2"));
        assert!(log.contains("CRC checked 1 (passed 1, failed 0, missing 1)"));
        assert!(log.contains("DST passthrough 1, decoded 1, reencoded 0, raw 0"));
        assert!(log.contains("bytes read 8192, written 4096"));
        assert!(log.contains("Convert: Ok; DSD/DST stats"));
    }

    #[test]
    fn passthrough_copy_tracks_get_pipeline_line_without_command_records() {
        let mut source = log_test_source();
        source.tracks[0].source_ref = TrackSourceRef::StagedFile(PathBuf::from("/stage/01.flac"));
        source.tracks[0].sample_rate = Some(44_100);
        source.tracks[0].bit_depth = Some(24);

        let mut req = log_test_request();
        req.settings.target_format = PlannerAudioFormat::Flac;
        req.settings.target_sample_rate = RateTarget::Source;
        req.settings.target_bit_depth = BitDepthTarget::Source;
        req.settings.force_encode = false;

        let artifacts = log_test_artifacts();
        let mut record = ok_record();
        record.commands = Vec::new();
        record.realized_input = Some(PathBuf::from("/stage/01.flac"));
        record.output_file = Some(PathBuf::from("/encoded/01.flac"));
        let outcome = AlbumOutcome::Complete {
            tracks: vec![record],
            stages: stage_records(),
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains("Pipeline: passthrough copy"));
        assert!(log.contains("Commands: none recorded"));
    }

    #[test]
    fn empty_command_tracks_do_not_claim_passthrough_when_audio_changes() {
        let mut source = log_test_source();
        source.tracks[0].source_ref = TrackSourceRef::StagedFile(PathBuf::from("/stage/01.wav"));
        source.tracks[0].sample_rate = Some(96_000);
        source.tracks[0].bit_depth = Some(24);

        let mut req = log_test_request();
        req.settings.target_format = PlannerAudioFormat::Flac;
        req.settings.target_sample_rate = RateTarget::PcmHz(44_100);

        let artifacts = log_test_artifacts();
        let mut record = ok_record();
        record.commands = Vec::new();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![record],
            stages: stage_records(),
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(!log.contains("Pipeline: passthrough copy"));
        assert!(log.contains("Commands: none recorded"));
    }

    #[test]
    fn conversion_summary_shows_rate_depth_and_processing_changes() {
        let mut source = log_test_source();
        source.tracks[0].source_ref = TrackSourceRef::StagedFile(PathBuf::from("/stage/01.flac"));
        source.tracks[0].sample_rate = Some(96_000);
        source.tracks[0].bit_depth = Some(24);
        let mut req = log_test_request();
        req.settings.target_sample_rate = RateTarget::PcmHz(44_100);
        req.settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
        req.settings.preferred_tool = PreferredTool::Ssrc;
        req.settings.dither_type = DitherType::Tpdf;
        let artifacts = log_test_artifacts();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains(
            "Conversion: 24-bit/96kHz FLAC → 16-bit/44.1kHz FLAC (SSRC resampling, TPDF dither)"
        ));
        assert!(log.contains("Target sample rate: 44.1kHz"));
        assert!(log.contains("Target bit depth: 16-bit"));
        assert!(log.contains("Dither type: TPDF"));
    }

    #[test]
    fn dsd_source_rate_target_source_logs_planner_default_pcm_rate() {
        let mut source = log_test_source();
        source.kind = SourceKind::SacdIso;
        source.tracks[0].sample_rate = Some(DsdRate::Dsd64.hz());
        source.tracks[0].bit_depth = None;
        source.tracks[0].source_ref = TrackSourceRef::SacdTrack {
            iso: PathBuf::from("/music/source.iso"),
            track_index: 0,
            area: SacdArea::Stereo,
        };

        let mut req = log_test_request();
        req.settings.target_format = PlannerAudioFormat::Flac;
        req.settings.target_sample_rate = RateTarget::Source;

        let artifacts = log_test_artifacts();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains("Target sample rate: 88.2kHz"));
        assert!(log.contains("Preferred resampler tool"));
        assert!(log.contains("Conversion: DSD64 DSD → 88.2kHz FLAC"));
        assert!(!log.contains("Conversion: DSD64 DSD → 2822.4kHz FLAC"));
    }

    #[test]
    fn target_dsd_rates_are_logged_as_dsd_rate_labels() {
        let mut source = log_test_source();
        source.tracks[0].sample_rate = Some(96_000);
        source.tracks[0].bit_depth = Some(24);

        let mut req = log_test_request();
        req.settings.target_format = PlannerAudioFormat::Dsf;
        req.settings.target_sample_rate = RateTarget::Dsd(DsdRate::Dsd128);

        let artifacts = log_test_artifacts();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains("Target sample rate: DSD128"));
        assert!(log.contains("Conversion: 24-bit/96kHz WAV → DSD128 DSF"));
        assert!(!log.contains("Conversion: 24-bit/96kHz WAV → 5644.8kHz DSF"));
    }

    #[test]
    fn pcm_to_dsd64_target_summary_uses_dsd_rate_label_not_hz() {
        let mut source = log_test_source();
        source.tracks[0].sample_rate = Some(96_000);
        source.tracks[0].bit_depth = Some(24);
        source.tracks[0].source_ref = TrackSourceRef::StagedFile(PathBuf::from("/stage/01.wav"));

        let mut req = log_test_request();
        req.settings.target_format = PlannerAudioFormat::Dsf;
        req.settings.target_sample_rate = RateTarget::Dsd(DsdRate::Dsd64);

        let artifacts = log_test_artifacts();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains("Target sample rate: DSD64"));
        assert!(log.contains("Conversion: 24-bit/96kHz WAV → DSD64 DSF"));
        assert!(!log.contains("2822.4kHz DSF"));
    }

    #[test]
    fn provenance_artwork_metadata_and_fingerprint_are_logged_when_available() {
        let mut source = log_test_source();
        source.provenance.source_sha256 = Some("abc123".to_string());
        source
            .provenance
            .tool_versions
            .insert("ffmpeg".to_string(), "7.1".to_string());
        source.album_metadata.extra.insert(
            CUE_ARTWORK_PATH_EXTRA_KEY.to_string(),
            "/stage/cover.jpg".to_string(),
        );
        source.album_metadata.extra.insert(
            CUE_ARTWORK_MIME_EXTRA_KEY.to_string(),
            "image/jpeg".to_string(),
        );

        let req = log_test_request();
        let fingerprint = tonepoet_pipeline::fingerprint::settings_fingerprint(&req.settings);
        let mut artifacts = log_test_artifacts();
        if let AudioArtifacts::Tracks(tracks) = &mut artifacts.audio {
            tracks[0].metadata_satisfaction = PlannedMetadataSatisfaction {
                source_tags_transferred: true,
                ..PlannedMetadataSatisfaction::none()
            };
            tracks[0].metadata_required = PlannedMetadataSatisfaction {
                source_tags_transferred: true,
                authoritative_tags_applied: true,
                ..PlannedMetadataSatisfaction::none()
            };
        }
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: stage_records(),
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, Some(fingerprint));

        assert!(log.contains(&format!("Settings fingerprint: {fingerprint}")));
        assert!(log.contains("Provenance"));
        assert!(log.contains("Source SHA-256: abc123"));
        assert!(log.contains("Tool versions: ffmpeg 7.1"));
        assert!(log.contains(
            "Artwork: extracted JPEG from source image → not confirmed (metadata stage outcome unavailable)"
        ));
        assert!(!log.contains("Artwork: extracted JPEG from source image → embedded in output"));
        assert!(log.contains(
            "Metadata: planner transferred source tags; remaining required metadata: authoritative CUE tags (metadata stage outcome unavailable)"
        ));
        assert!(!log.contains("metadata stage wrote authoritative tags"));
        assert!(!log.contains("authoritative tags"));
    }

    #[test]
    fn metadata_satisfaction_log_does_not_claim_per_track_metadata_stage_writes() {
        let source = log_test_source();
        let req = log_test_request();
        let mut artifacts = log_test_artifacts();
        if let AudioArtifacts::Tracks(tracks) = &mut artifacts.audio {
            tracks[0].metadata_satisfaction = PlannedMetadataSatisfaction {
                source_tags_transferred: true,
                ..PlannedMetadataSatisfaction::none()
            };
            tracks[0].metadata_required = PlannedMetadataSatisfaction {
                source_tags_transferred: true,
                authoritative_tags_applied: true,
                ..PlannedMetadataSatisfaction::none()
            };
        }
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: vec![StageRecord {
                stage: PipelineStage::Metadata,
                outcome: StageOutcome::Ok,
                dsd_dst_stats: None,
            }],
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains(
            "Metadata: planner transferred source tags; remaining required metadata: authoritative CUE tags (metadata stage completed; per-track writes not recorded)"
        ));
        assert!(!log.contains("metadata stage wrote"));
        assert!(!log.contains("authoritative tags"));
    }

    #[test]
    fn artwork_log_uses_metadata_stage_evidence_before_claiming_post_encode_embedding() {
        let mut source = log_test_source();
        source.album_metadata.extra.insert(
            CUE_ARTWORK_PATH_EXTRA_KEY.to_string(),
            "/stage/cover.jpg".to_string(),
        );
        source.album_metadata.extra.insert(
            CUE_ARTWORK_MIME_EXTRA_KEY.to_string(),
            "image/png".to_string(),
        );
        let req = log_test_request();
        let artifacts = log_test_artifacts();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: vec![StageRecord {
                stage: PipelineStage::Metadata,
                outcome: StageOutcome::Ok,
                dsd_dst_stats: None,
            }],
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains(
            "Artwork: extracted PNG from source image → metadata stage completed post-encode artwork embedding"
        ));
        assert!(!log.contains("Artwork: extracted PNG from source image → embedded in output"));
    }

    #[test]
    fn artwork_log_prefers_planner_metadata_satisfaction_when_artwork_was_transferred() {
        let mut source = log_test_source();
        source.album_metadata.extra.insert(
            CUE_ARTWORK_PATH_EXTRA_KEY.to_string(),
            "/stage/cover.jpg".to_string(),
        );
        source.album_metadata.extra.insert(
            CUE_ARTWORK_MIME_EXTRA_KEY.to_string(),
            "image/jpeg".to_string(),
        );
        let mut artifacts = log_test_artifacts();
        if let AudioArtifacts::Tracks(tracks) = &mut artifacts.audio {
            for track in tracks {
                track.metadata_satisfaction = PlannedMetadataSatisfaction {
                    artwork_transferred: true,
                    ..PlannedMetadataSatisfaction::none()
                };
            }
        }
        let req = log_test_request();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: vec![StageRecord {
                stage: PipelineStage::Metadata,
                outcome: StageOutcome::Skipped,
                dsd_dst_stats: None,
            }],
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains(
            "Artwork: extracted JPEG from source image → planner transferred artwork into output"
        ));
    }

    #[test]
    fn artwork_log_reports_skipped_for_unsupported_target_format() {
        let mut source = log_test_source();
        source.album_metadata.extra.insert(
            CUE_ARTWORK_PATH_EXTRA_KEY.to_string(),
            "/stage/cover.jpg".to_string(),
        );
        source.album_metadata.extra.insert(
            CUE_ARTWORK_MIME_EXTRA_KEY.to_string(),
            "image/jpeg".to_string(),
        );

        let mut req = log_test_request();
        req.settings.target_format = PlannerAudioFormat::Wav;
        req.settings.metadata.preserve_artwork = true;

        let artifacts = log_test_artifacts();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: vec![StageRecord {
                stage: PipelineStage::Metadata,
                outcome: StageOutcome::Ok,
                dsd_dst_stats: None,
            }],
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains(
            "Artwork: extracted JPEG from source image → skipped (WAV container unsupported)"
        ));
        assert!(!log.contains("embedded in output"));
        assert!(!log.contains("metadata stage completed post-encode artwork embedding"));
    }

    #[test]
    fn artwork_log_reports_skipped_when_preservation_is_disabled() {
        let mut source = log_test_source();
        source.album_metadata.extra.insert(
            CUE_ARTWORK_PATH_EXTRA_KEY.to_string(),
            "/stage/cover.png".to_string(),
        );
        source.album_metadata.extra.insert(
            CUE_ARTWORK_MIME_EXTRA_KEY.to_string(),
            "image/png".to_string(),
        );

        let mut req = log_test_request();
        req.settings.target_format = PlannerAudioFormat::Flac;
        req.settings.metadata.preserve_artwork = false;

        let artifacts = log_test_artifacts();
        let outcome = AlbumOutcome::Complete {
            tracks: vec![ok_record()],
            stages: vec![StageRecord {
                stage: PipelineStage::Metadata,
                outcome: StageOutcome::Ok,
                dsd_dst_stats: None,
            }],
        };
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

        assert!(log.contains(
            "Artwork: extracted PNG from source image → skipped (artwork preservation disabled)"
        ));
        assert!(!log.contains("embedded in output"));
        assert!(!log.contains("metadata stage completed post-encode artwork embedding"));
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
        let source = log_test_source();
        let req = log_test_request();
        let artifacts = log_test_artifacts();
        let log = build_conversion_log(&outcome, &source, &req, &artifacts, None);

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
                sample_rate: Some(44_100),
                source_audio: SourceAudioDescriptor::from_scalar(
                    Some(44_100),
                    Some(24),
                    Some(SourceAudioCoding::Pcm),
                ),
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
                dvda_group_selection: DvdaGroupSelection::Default,
                dvda_group: None,
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
                sample_rate: Some(2_822_400),
                source_audio: SourceAudioDescriptor::from_scalar(
                    Some(2_822_400),
                    None,
                    Some(SourceAudioCoding::Dsd),
                ),
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
                dvda_group_selection: DvdaGroupSelection::Default,
                dvda_group: None,
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
                sample_rate: Some(44_100),
                source_audio: SourceAudioDescriptor::from_scalar(
                    Some(44_100),
                    Some(16),
                    Some(SourceAudioCoding::Pcm),
                ),
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
            dsd_dst_stats: None,
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

        let runner = BlockingToolRunner::with_behaviors([
            ToolBehavior::Succeed, // inspect existing native tags
            ToolBehavior::Succeed, // write authoritative tags
        ]);
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
        assert_eq!(transcript.len(), 2, "metadata stage first inspects existing native tags, then writes authoritative tags");
        assert_eq!(transcript[0].binary, ToolBinary::Metaflac);
        assert!(transcript[0].sanitized_args.iter().any(|arg| arg == "--export-tags-to=-"));
        assert_eq!(transcript[1].binary, ToolBinary::Metaflac);
        let args = &transcript[1].sanitized_args;
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
                description: None,
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
                description: None,
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

        assert_eq!(result.expect("validation should pass"), Some(1_000_000));
    }

    #[tokio::test]
    async fn lossless_target_with_exact_sample_drift_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        let out = temp.path().join("track.wav");
        std::fs::write(&out, b"fake-wav").expect("write");
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

        assert!(matches!(result, Err(ConvertError::TrackValidation(message)) if message.contains("post-encode sample drift")));
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

        assert_eq!(result.expect("lossy validation should skip"), Some(1_000_000));
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

        assert_eq!(result.expect("DSD validation should skip"), Some(1_000_000));
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

        assert_eq!(result.expect("missing expected samples should skip"), None);
        assert_eq!(runner.transcript().len(), 0);
    }

    #[tokio::test]
    async fn approximate_probe_allows_only_one_millisecond_drift() {
        let temp = tempfile::tempdir().expect("temp dir");
        let out = temp.path().join("track.flac");
        std::fs::write(&out, b"fake-flac").expect("write");
        // Expected 1,000,000. Duration 22.676s -> 1,000,012 samples.
        // One-millisecond fallback tolerance at 44.1 kHz is 44 samples.
        let runner = stub_with_probe(&ffprobe_approx_json(44100, 22.676));
        let cancel = CancellationToken::new();

        let result = validate_encoded_output(
            &out,
            Some(1_000_000),
            &tonepoet_pipeline::AudioFormat::Flac,
            &runner,
            &cancel,
        )
        .await
        .expect("duration-only drift inside one millisecond should pass");

        let actual = result.expect("lossless validation should return probed samples");
        assert_ne!(actual, 1_000_000, "approximate probe should return the probed value");
        assert!(actual.abs_diff(1_000_000) <= 44, "drift {} should be within one millisecond", actual.abs_diff(1_000_000));
    }

    #[tokio::test]
    async fn approximate_probe_rejects_more_than_one_millisecond_drift() {
        let temp = tempfile::tempdir().expect("temp dir");
        let out = temp.path().join("track.alac.m4a");
        std::fs::write(&out, b"fake-alac").expect("write");
        let runner = stub_with_probe(&ffprobe_approx_json(44100, 22.678));
        let cancel = CancellationToken::new();

        let result = validate_encoded_output(
            &out,
            Some(1_000_000),
            &tonepoet_pipeline::AudioFormat::Alac,
            &runner,
            &cancel,
        )
        .await;

        assert!(matches!(result, Err(ConvertError::TrackValidation(message)) if message.contains("post-encode sample drift")));
    }
}
