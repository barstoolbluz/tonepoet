//! PR 8 - CUE image materializer.
//!
//! Parses a single-image CUE layout into `ImageSegment` track refs. Cutting
//! is left to `realize_track`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::errors::{MaterializeError, SourceDetectError, ToolRunnerError};
use super::reporter::PipelineReporter;
use super::stages::Materializer;
use super::tool::{ToolBinary, ToolCommand, ToolRunner};
use super::types::*;
use crate::tui::cue_parser::{parse_cue, parse_cue_file, CueSheet};

const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "wav", "wave", "aiff", "aif", "aifc", "wv", "mp3", "m4a", "mp4", "aac", "opus", "ogg",
    "ape", "w64", "rf64",
];

#[derive(Debug, Clone)]
struct CueInput {
    image_path: PathBuf,
    sheet: CueSheet,
    raw_cue: String,
}

#[derive(Debug, Clone, Copy)]
struct AudioProbe {
    sample_rate: u32,
    total_samples: u64,
    exact_samples: bool,
    bit_depth: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CueOrigin {
    Sidecar,
    Embedded,
}

pub struct CueImageMaterializer;

#[async_trait]
impl Materializer for CueImageMaterializer {
    async fn materialize(
        &self,
        req: &PipelineRequest,
        staging: &StagingDir,
        runner: &dyn ToolRunner,
        _reporter: Option<&dyn PipelineReporter>,
        _tool_paths: &HashMap<String, PathBuf>,
        cancel: &CancellationToken,
    ) -> Result<PreparedSource, MaterializeError> {
        std::fs::create_dir_all(&staging.root)?;

        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }

        let cue_input = resolve_cue_input(req)?;
        let probe = probe_audio_image(&cue_input.image_path, runner, cancel).await?;
        let boundaries = compute_track_boundaries(
            &cue_input.sheet,
            probe.total_samples,
            probe.sample_rate,
            probe.exact_samples,
        )?;
        let cue_annotations = CueAnnotations::parse(&cue_input.raw_cue);

        let mut tracks = Vec::with_capacity(cue_input.sheet.tracks.len());
        for (idx, cue_track) in cue_input.sheet.tracks.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(MaterializeError::Cancelled);
            }

            let ordinal = (idx + 1) as u32;
            let (start_sample, samples) = boundaries[idx];
            let mut metadata = cue_track_metadata(
                cue_track,
                &cue_input.sheet,
                cue_annotations.track_pre_emphasis(cue_track.number),
            );
            cue_annotations.add_track_extras(cue_track.number, &mut metadata.extra);

            tracks.push(PreparedTrack {
                id: TrackId {
                    source_ordinal: ordinal,
                    disc_number: None,
                    track_number: cue_track.number,
                },
                source_ref: TrackSourceRef::ImageSegment {
                    image: cue_input.image_path.clone(),
                    start_sample,
                    samples,
                },
                metadata,
                expected_samples: Some(samples),
                sample_rate: probe.sample_rate,
                bit_depth: probe.bit_depth,
            });
        }

        let tracks = apply_track_selection(tracks, &req.source.track_selection)?;
        let mut album_metadata = cue_album_metadata(&cue_input.sheet, tracks.len() as u32);
        cue_annotations.add_album_extras(&mut album_metadata.extra);

        let provenance = ExtractionProvenance {
            source_kind: SourceKind::CueImage,
            source_sha256: None,
            tool_versions: BTreeMap::new(),
            extracted_at: chrono::Utc::now(),
        };

        Ok(PreparedSource {
            container: req.container.clone(),
            kind: SourceKind::CueImage,
            tracks,
            album_metadata,
            provenance,
        })
    }
}

fn resolve_cue_input(req: &PipelineRequest) -> Result<CueInput, MaterializeError> {
    match req.source.cue_sidecar {
        CueSidecarPolicy::IgnoreCue => Err(MaterializeError::Parse(
            "IgnoreCue must stay on the legacy single-file path".to_string(),
        )),
        CueSidecarPolicy::SidecarOnly => resolve_sidecar_cue(req),
        CueSidecarPolicy::PreferSidecar => {
            match find_valid_sidecar_cue_for_image(&req.container)? {
                Some(cue_path) => read_sidecar_cue(req, cue_path),
                None => resolve_embedded_cue(req),
            }
        }
        CueSidecarPolicy::EmbeddedOnly => resolve_embedded_cue(req),
    }
}

fn resolve_sidecar_cue(req: &PipelineRequest) -> Result<CueInput, MaterializeError> {
    let cue_path = find_valid_sidecar_cue_for_image(&req.container)?.ok_or_else(|| {
        MaterializeError::Parse(
            "SidecarOnly requested but no matching single-image CUE was found".to_string(),
        )
    })?;
    read_sidecar_cue(req, cue_path)
}

fn read_sidecar_cue(
    req: &PipelineRequest,
    cue_path: PathBuf,
) -> Result<CueInput, MaterializeError> {
    let raw_cue = read_text_lossy(&cue_path)?;
    let sheet = parse_cue_file(&cue_path).map_err(MaterializeError::Parse)?;
    let image_path = resolve_single_image_path(
        &sheet,
        CueOrigin::Sidecar,
        cue_path.parent(),
        Some(&req.container),
    )?;

    if !same_existing_path(&image_path, &req.container) && !has_extension(&req.container, "cue") {
        return Err(MaterializeError::Parse(format!(
            "CUE file {} does not reference input image {}",
            cue_path.display(),
            req.container.display()
        )));
    }

    Ok(CueInput {
        image_path,
        sheet,
        raw_cue,
    })
}

fn resolve_embedded_cue(req: &PipelineRequest) -> Result<CueInput, MaterializeError> {
    let raw_cue = read_embedded_cuesheet(&req.container)?.ok_or_else(|| {
        MaterializeError::Parse(
            "EmbeddedOnly requested but no embedded CUESHEET tag was found".to_string(),
        )
    })?;
    let sheet = parse_cue(&raw_cue);
    validate_single_image_layout(&sheet)?;

    Ok(CueInput {
        image_path: req.container.clone(),
        sheet,
        raw_cue,
    })
}

pub(crate) fn is_cue_image_candidate(req: &PipelineRequest) -> Result<bool, SourceDetectError> {
    if req.source.cue_sidecar == CueSidecarPolicy::IgnoreCue {
        return Ok(false);
    }

    if has_extension(&req.container, "cue") {
        // A .cue path is a CUE-image candidate even when its contents are
        // malformed. The materializer owns parse validation and will return
        // MaterializeError::Parse instead of letting the processor fall back to
        // one-file legacy conversion.
        return Ok(true);
    }

    if !has_audio_extension(&req.container) {
        return Ok(false);
    }

    match req.source.cue_sidecar {
        CueSidecarPolicy::SidecarOnly | CueSidecarPolicy::PreferSidecar => {
            if sidecar_cue_route_candidate(&req.container)?.is_some() {
                return Ok(true);
            }
            if req.source.cue_sidecar == CueSidecarPolicy::PreferSidecar {
                return Ok(embedded_cuesheet_is_single_image(&req.container));
            }
            Ok(false)
        }
        CueSidecarPolicy::EmbeddedOnly => Ok(embedded_cuesheet_is_single_image(&req.container)),
        CueSidecarPolicy::IgnoreCue => Ok(false),
    }
}

fn sidecar_cue_route_candidate(image: &Path) -> Result<Option<PathBuf>, SourceDetectError> {
    if has_extension(image, "cue") {
        return Ok(Some(image.to_path_buf()));
    }

    let candidates = sidecar_cue_candidates(image)?;
    let same_stem = same_stem_sidecars(image, &candidates);
    match same_stem.len() {
        0 => {}
        1 => return Ok(Some(same_stem[0].clone())),
        _ => {
            return Err(SourceDetectError::AmbiguousCue(format!(
                "multiple same-stem CUE files found beside {}",
                image.display()
            )));
        }
    }

    let mut matching = Vec::new();
    for cue_path in candidates {
        let raw = std::fs::read(&cue_path)?;
        let content = String::from_utf8_lossy(&raw);
        let sheet = parse_cue(&content);
        if validate_single_image_layout_detect(&sheet).is_err() {
            continue;
        }
        if let Some(resolved) = resolve_single_image_path_detect(
            &sheet,
            CueOrigin::Sidecar,
            cue_path.parent(),
            Some(image),
        ) {
            if same_existing_path(&resolved, image) {
                matching.push(cue_path);
            }
        }
    }

    match matching.len() {
        0 => Ok(None),
        1 => Ok(matching.into_iter().next()),
        _ => Err(SourceDetectError::AmbiguousCue(format!(
            "multiple matching CUE files found beside {}",
            image.display()
        ))),
    }
}

fn embedded_cuesheet_is_single_image(path: &Path) -> bool {
    let Ok(Some(raw)) = read_embedded_cuesheet(path) else {
        return false;
    };
    let sheet = parse_cue(&raw);
    validate_single_image_layout_detect(&sheet).is_ok()
}

fn find_valid_sidecar_cue_for_image(image: &Path) -> Result<Option<PathBuf>, MaterializeError> {
    if has_extension(image, "cue") {
        return Ok(Some(image.to_path_buf()));
    }

    let candidates = sidecar_cue_candidates(image).map_err(source_detect_to_materialize)?;
    let same_stem = same_stem_sidecars(image, &candidates);
    match same_stem.len() {
        0 => {}
        1 => {
            validate_sidecar_cue_matches_image(&same_stem[0], image)?;
            return Ok(Some(same_stem[0].clone()));
        }
        _ => {
            return Err(MaterializeError::Parse(format!(
                "multiple same-stem CUE files found beside {}",
                image.display()
            )));
        }
    }

    let mut matching = Vec::new();
    for cue_path in candidates {
        match sidecar_cue_matches_image(&cue_path, image) {
            Ok(true) => matching.push(cue_path),
            Ok(false) => {}
            Err(_) => {
                // A non-stem CUE may belong to another image in the directory.
                // Only same-stem sidecars are treated as authoritative enough to
                // convert parse/layout errors into MaterializeError::Parse.
            }
        }
    }

    match matching.len() {
        0 => Ok(None),
        1 => Ok(matching.into_iter().next()),
        _ => Err(MaterializeError::Parse(format!(
            "multiple matching CUE files found beside {}",
            image.display()
        ))),
    }
}

fn validate_sidecar_cue_matches_image(
    cue_path: &Path,
    image: &Path,
) -> Result<(), MaterializeError> {
    if sidecar_cue_matches_image(cue_path, image)? {
        Ok(())
    } else {
        Err(MaterializeError::Parse(format!(
            "CUE file {} does not reference input image {}",
            cue_path.display(),
            image.display()
        )))
    }
}

fn sidecar_cue_matches_image(cue_path: &Path, image: &Path) -> Result<bool, MaterializeError> {
    let raw_cue = read_text_lossy(cue_path)?;
    let sheet = parse_cue(&raw_cue);
    validate_single_image_layout(&sheet)?;
    let resolved =
        resolve_single_image_path(&sheet, CueOrigin::Sidecar, cue_path.parent(), Some(image))?;
    Ok(same_existing_path(&resolved, image))
}

fn source_detect_to_materialize(err: SourceDetectError) -> MaterializeError {
    match err {
        SourceDetectError::Io(io) => MaterializeError::Io(io),
        SourceDetectError::AmbiguousCue(message) => MaterializeError::Parse(message),
        SourceDetectError::UnknownSource => {
            MaterializeError::Parse("unknown CUE source".to_string())
        }
    }
}

fn sidecar_cue_candidates(path: &Path) -> Result<Vec<PathBuf>, SourceDetectError> {
    if has_extension(path, "cue") {
        return Ok(vec![path.to_path_buf()]);
    }

    let Some(parent) = path.parent() else {
        return Ok(Vec::new());
    };

    let mut cues = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let candidate = entry.path();
        if candidate.is_file() && has_extension(&candidate, "cue") {
            cues.push(candidate);
        }
    }
    cues.sort();
    Ok(cues)
}

fn same_stem_sidecars(image: &Path, candidates: &[PathBuf]) -> Vec<PathBuf> {
    let Some(image_stem) = image.file_stem().and_then(|stem| stem.to_str()) else {
        return Vec::new();
    };
    candidates
        .iter()
        .filter(|candidate| {
            candidate
                .file_stem()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case(image_stem))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

fn read_text_lossy(path: &Path) -> Result<String, MaterializeError> {
    let raw = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

fn read_embedded_cuesheet(path: &Path) -> Result<Option<String>, MaterializeError> {
    use lofty::prelude::*;

    let tagged = match lofty::read_from_path(path) {
        Ok(tagged) => tagged,
        Err(_) => return Ok(None),
    };
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Ok(None);
    };

    for item in tag.items() {
        if let lofty::tag::ItemKey::Unknown(key) = item.key() {
            if key.eq_ignore_ascii_case("CUESHEET") {
                if let Some(text) = item.value().text() {
                    return Ok(Some(text.to_string()));
                }
            }
        }
    }

    Ok(tag
        .get_string(&lofty::tag::ItemKey::Unknown("CUESHEET".to_string()))
        .map(|value| value.to_string()))
}

fn validate_single_image_layout(sheet: &CueSheet) -> Result<(), MaterializeError> {
    validate_single_image_layout_detect(sheet).map_err(MaterializeError::Parse)
}

fn validate_single_image_layout_detect(sheet: &CueSheet) -> Result<(), String> {
    if sheet.tracks.is_empty() {
        return Err("CUE sheet has no tracks".to_string());
    }
    if !sheet
        .tracks
        .iter()
        .all(|track| track.index01_frames.is_some())
    {
        return Err("single-image CUE requires INDEX 01 for every track".to_string());
    }

    let first_file = sheet.tracks.iter().find_map(|track| track.file.as_ref());
    if let Some(file_ref) = first_file {
        let all_same = sheet
            .tracks
            .iter()
            .all(|track| track.file.as_ref() == Some(file_ref));
        if !all_same {
            return Err(
                "track-per-file CUE layouts are not handled by CueImageMaterializer".to_string(),
            );
        }
    }

    let mut previous = None;
    for track in &sheet.tracks {
        let current = track.index01_frames.expect("checked above");
        if previous.is_some_and(|prev| current <= prev) {
            return Err(format!("non-increasing INDEX 01 at track {}", track.number));
        }
        previous = Some(current);
    }

    Ok(())
}

fn resolve_single_image_path(
    sheet: &CueSheet,
    origin: CueOrigin,
    cue_parent: Option<&Path>,
    fallback_image: Option<&Path>,
) -> Result<PathBuf, MaterializeError> {
    validate_single_image_layout(sheet)?;
    if origin == CueOrigin::Embedded {
        return fallback_image.map(Path::to_path_buf).ok_or_else(|| {
            MaterializeError::Parse("embedded CUE has no owning image".to_string())
        });
    }

    let file_ref = sheet
        .tracks
        .iter()
        .find_map(|track| track.file.as_ref())
        .ok_or_else(|| {
            MaterializeError::Parse("sidecar CUE sheet has no FILE reference".to_string())
        })?;
    resolve_audio_reference(cue_parent, file_ref, fallback_image)
}

fn resolve_single_image_path_detect(
    sheet: &CueSheet,
    origin: CueOrigin,
    cue_parent: Option<&Path>,
    fallback_image: Option<&Path>,
) -> Option<PathBuf> {
    validate_single_image_layout_detect(sheet).ok()?;
    if origin == CueOrigin::Embedded {
        return fallback_image.map(Path::to_path_buf);
    }
    let file_ref = sheet.tracks.iter().find_map(|track| track.file.as_ref())?;
    resolve_audio_reference(cue_parent, file_ref, fallback_image).ok()
}

fn resolve_audio_reference(
    cue_parent: Option<&Path>,
    file_ref: &str,
    fallback_image: Option<&Path>,
) -> Result<PathBuf, MaterializeError> {
    let normalized_ref = file_ref.replace('\\', &std::path::MAIN_SEPARATOR.to_string());
    let raw_path = PathBuf::from(&normalized_ref);

    if raw_path.is_absolute() && raw_path.is_file() {
        return Ok(raw_path);
    }

    let base = cue_parent.unwrap_or_else(|| Path::new("."));
    let direct = base.join(&raw_path);
    if direct.is_file() {
        return Ok(direct);
    }

    if let Some(fallback) = fallback_image {
        let ref_name = raw_path.file_name().and_then(|value| value.to_str());
        let ref_stem = raw_path.file_stem().and_then(|value| value.to_str());
        let fallback_name = fallback.file_name().and_then(|value| value.to_str());
        let fallback_stem = fallback.file_stem().and_then(|value| value.to_str());
        if ref_name
            .zip(fallback_name)
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
            || ref_stem
                .zip(fallback_stem)
                .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
        {
            return Ok(fallback.to_path_buf());
        }
    }

    let wanted_name = raw_path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    let wanted_stem = raw_path
        .file_stem()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());

    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || !has_audio_extension(&path) {
            continue;
        }

        let file_name_match = wanted_name.as_ref().is_some_and(|wanted| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case(wanted))
                .unwrap_or(false)
        });
        let stem_match = wanted_stem.as_ref().is_some_and(|wanted| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case(wanted))
                .unwrap_or(false)
        });
        if file_name_match || stem_match {
            return Ok(path);
        }
    }

    Err(MaterializeError::Parse(format!(
        "CUE image file was not found: {file_ref}"
    )))
}

async fn probe_audio_image(
    path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<AudioProbe, MaterializeError> {
    let cmd = ToolCommand {
        binary: ToolBinary::Ffprobe,
        args: vec![
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            "a:0".into(),
            "-count_frames".into(),
            "-show_entries".into(),
            "stream=sample_rate,duration_ts,time_base,duration,bits_per_raw_sample,bits_per_sample".into(),
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
        Ok(output) => output,
        Err(ToolRunnerError::Cancelled { .. }) => return Err(MaterializeError::Cancelled),
        Err(err) => return Err(err.into()),
    };

    parse_audio_probe_json(&output.stdout_tail)
}

fn parse_audio_probe_json(json_str: &str) -> Result<AudioProbe, MaterializeError> {
    let value: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|err| MaterializeError::Parse(format!("ffprobe JSON parse failed: {err}")))?;

    let stream = value
        .pointer("/streams/0")
        .ok_or_else(|| MaterializeError::Parse("ffprobe returned no audio stream".to_string()))?;
    let sample_rate = stream
        .get("sample_rate")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    if sample_rate == 0 {
        return Err(MaterializeError::Parse(
            "ffprobe returned no valid sample_rate".to_string(),
        ));
    }
    let bit_depth = stream
        .get("bits_per_raw_sample")
        .or_else(|| stream.get("bits_per_sample"))
        .and_then(json_u32_from_value);

    if let Some(duration_ts_samples) = samples_from_duration_ts(stream, sample_rate) {
        if duration_ts_samples > 0 {
            return Ok(AudioProbe {
                sample_rate,
                total_samples: duration_ts_samples,
                exact_samples: true,
                bit_depth,
            });
        }
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
        })
        .ok_or_else(|| MaterializeError::Parse("ffprobe returned no duration".to_string()))?;

    let total_samples = (duration_secs * f64::from(sample_rate)).round() as u64;
    if total_samples == 0 {
        return Err(MaterializeError::Parse(
            "ffprobe returned zero audio samples".to_string(),
        ));
    }

    Ok(AudioProbe {
        sample_rate,
        total_samples,
        exact_samples: false,
        bit_depth,
    })
}

fn samples_from_duration_ts(stream: &serde_json::Value, sample_rate: u32) -> Option<u64> {
    let duration_ts = stream.get("duration_ts").and_then(json_u64_from_value)?;
    let time_base = stream.get("time_base")?.as_str()?;
    let (num, den) = parse_ratio(time_base)?;
    if den == 0 {
        return None;
    }
    let samples = (duration_ts as u128)
        .checked_mul(num as u128)?
        .checked_mul(sample_rate as u128)?
        .checked_div(den as u128)?;
    u64::try_from(samples).ok()
}

fn parse_ratio(value: &str) -> Option<(u64, u64)> {
    let (left, right) = value.split_once('/')?;
    Some((left.parse().ok()?, right.parse().ok()?))
}

fn json_u64_from_value(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
}

fn json_u32_from_value(value: &serde_json::Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .or_else(|| value.as_str().and_then(|text| text.parse::<u32>().ok()))
}

fn compute_track_boundaries(
    sheet: &CueSheet,
    total_samples: u64,
    sample_rate: u32,
    exact_total: bool,
) -> Result<Vec<(u64, u64)>, MaterializeError> {
    let mut starts = Vec::with_capacity(sheet.tracks.len());
    for track in &sheet.tracks {
        let frames = track.index01_frames.ok_or_else(|| {
            MaterializeError::Parse(format!("track {} has no INDEX 01", track.number))
        })?;
        let start = cue_frames_to_samples(frames as u64, sample_rate);
        starts.push(start);
    }

    let mut boundaries = Vec::with_capacity(starts.len());
    for idx in 0..starts.len() {
        let start = starts[idx];
        let end = starts.get(idx + 1).copied().unwrap_or(total_samples);
        if end <= start {
            return Err(MaterializeError::Parse(format!(
                "invalid CUE boundary for track {}",
                sheet.tracks[idx].number
            )));
        }
        if end > total_samples {
            return Err(MaterializeError::Parse(format!(
                "track {} starts beyond image duration",
                sheet.tracks[idx].number
            )));
        }
        if !exact_total
            && idx == starts.len() - 1
            && total_samples.saturating_sub(start) < sample_rate as u64 / 20
        {
            return Err(MaterializeError::Parse(
                "image sample count probe is too coarse for the final CUE segment".to_string(),
            ));
        }
        boundaries.push((start, end - start));
    }

    Ok(boundaries)
}

fn cue_frames_to_samples(frames: u64, sample_rate: u32) -> u64 {
    ((frames as u128 * sample_rate as u128) / 75_u128) as u64
}

fn cue_track_metadata(
    cue_track: &crate::tui::cue_parser::CueTrack,
    sheet: &CueSheet,
    pre_emphasis: bool,
) -> TrackMetadata {
    let mut extra = BTreeMap::new();
    if let Some(album) = &sheet.title {
        extra.insert("album".to_string(), album.clone());
    }
    if let Some(catalog) = &sheet.catalog {
        extra.insert("catalog".to_string(), catalog.clone());
    }
    if let Some(index00) = cue_track.index00_frames {
        extra.insert("index00_frames".to_string(), index00.to_string());
    }
    if let Some(index01) = cue_track.index01_frames {
        extra.insert("index01_frames".to_string(), index01.to_string());
    }

    TrackMetadata {
        title: cue_track.title.clone(),
        artist: cue_track
            .performer
            .clone()
            .or_else(|| sheet.performer.clone()),
        album_artist: sheet.performer.clone(),
        composer: None,
        performer: cue_track
            .performer
            .clone()
            .or_else(|| sheet.performer.clone()),
        genre: sheet.genre.clone(),
        date: sheet.date.clone(),
        track_number: Some(cue_track.number),
        disc_number: None,
        isrc: cue_track.isrc.clone(),
        publisher: None,
        copyright: None,
        comment: None,
        pre_emphasis,
        extra,
    }
}

fn cue_album_metadata(sheet: &CueSheet, total_tracks: u32) -> AlbumMetadata {
    let mut extra = BTreeMap::new();
    if let Some(catalog) = &sheet.catalog {
        extra.insert("catalog".to_string(), catalog.clone());
    }

    AlbumMetadata {
        album: sheet.title.clone(),
        album_artist: sheet.performer.clone(),
        genre: sheet.genre.clone(),
        date: sheet.date.clone(),
        total_tracks,
        total_discs: None,
        disc_number: None,
        extra,
    }
}

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
                .filter(|track| {
                    track.id.source_ordinal >= *start && track.id.source_ordinal <= *end
                })
                .collect())
        }
        TrackSelection::Set(indices) => {
            if indices.is_empty() {
                return Err(MaterializeError::InvalidTrackSelection(
                    "empty track set".to_string(),
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
                .filter(|track| indices.contains(&track.id.source_ordinal))
                .collect())
        }
    }
}

#[derive(Debug, Default)]
struct CueAnnotations {
    album_extra: BTreeMap<String, String>,
    track_extra: BTreeMap<u32, BTreeMap<String, String>>,
    pre_emphasis: Vec<u32>,
}

impl CueAnnotations {
    fn parse(raw: &str) -> Self {
        let mut annotations = Self::default();
        let mut current_track: Option<u32> = None;

        for (idx, line) in raw.lines().enumerate() {
            let line = if idx == 0 {
                line.trim_start_matches('\u{FEFF}')
            } else {
                line
            };
            let trimmed = line.trim();
            if let Some(track_no) = parse_track_number(trimmed) {
                current_track = Some(track_no);
                continue;
            }
            if trimmed.starts_with("FLAGS")
                && trimmed
                    .split_whitespace()
                    .any(|token| token.eq_ignore_ascii_case("PRE"))
            {
                if let Some(track_no) = current_track {
                    annotations.pre_emphasis.push(track_no);
                }
                continue;
            }
            if let Some((key, value)) = parse_rem_line(trimmed) {
                let key = format!("rem_{}", key.to_ascii_lowercase());
                if matches!(key.as_str(), "rem_date" | "rem_year" | "rem_genre") {
                    continue;
                }
                if let Some(track_no) = current_track {
                    annotations
                        .track_extra
                        .entry(track_no)
                        .or_default()
                        .insert(key, value);
                } else {
                    annotations.album_extra.insert(key, value);
                }
            }
        }

        annotations
    }

    fn track_pre_emphasis(&self, track_no: u32) -> bool {
        self.pre_emphasis.contains(&track_no)
    }

    fn add_track_extras(&self, track_no: u32, extra: &mut BTreeMap<String, String>) {
        if let Some(values) = self.track_extra.get(&track_no) {
            extra.extend(values.clone());
        }
    }

    fn add_album_extras(&self, extra: &mut BTreeMap<String, String>) {
        extra.extend(self.album_extra.clone());
    }
}

fn parse_track_number(line: &str) -> Option<u32> {
    let rest = line.strip_prefix("TRACK")?.trim_start();
    let end = rest.find(|ch: char| !ch.is_ascii_digit())?;
    rest[..end].parse().ok()
}

fn parse_rem_line(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("REM")?.trim_start();
    let (key, value) = rest.split_once(char::is_whitespace)?;
    let value = value.trim();
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key.to_string(), unquote(value).to_string()))
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(value)
}

fn has_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| AUDIO_EXTENSIONS.contains(&value.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(ext))
        .unwrap_or(false)
}

fn same_existing_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn parse_probe_for_test(json: &str) -> Result<(u32, u64, bool), MaterializeError> {
        let probe = parse_audio_probe_json(json)?;
        Ok((probe.sample_rate, probe.total_samples, probe.exact_samples))
    }
}


#[cfg(test)]
mod naming_template_bit_depth_tests {
    use super::*;

    #[test]
    fn cue_json_u32_from_value_reads_string_and_number_bit_depths() {
        assert_eq!(json_u32_from_value(&serde_json::json!("24")), Some(24));
        assert_eq!(json_u32_from_value(&serde_json::json!(16)), Some(16));
        assert_eq!(json_u32_from_value(&serde_json::json!("not-a-number")), None);
    }
}
