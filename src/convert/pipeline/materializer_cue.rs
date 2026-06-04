//! PR 8 - CUE image materializer.
//!
//! Parses CUE image layouts into `ImageSegment` track refs. Cutting
//! is left to `realize_track`. Sidecar CUE sheets may reference one image
//! or multiple image files; embedded CUE sheets are constrained to their
//! owning image.

use std::collections::{BTreeMap, BTreeSet, HashMap};
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
    sheet: CueSheet,
    raw_cue: String,
    origin: CueOrigin,
    cue_parent: Option<PathBuf>,
    fallback_image: Option<PathBuf>,
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
        let track_images = resolve_track_image_paths(&cue_input)?;
        let unique_images = unique_existing_paths(&track_images);

        let mut probes = HashMap::new();
        let mut image_metadata = HashMap::new();
        for image_path in &unique_images {
            if cancel.is_cancelled() {
                return Err(MaterializeError::Cancelled);
            }
            probes.insert(
                path_identity(image_path),
                probe_audio_image(image_path, runner, cancel).await?,
            );
            image_metadata.insert(path_identity(image_path), read_image_album_metadata(image_path));
        }

        let boundaries = compute_track_boundaries_for_layout(
            &cue_input.sheet,
            &track_images,
            &probes,
        )?;
        let cue_annotations = CueAnnotations::parse(&cue_input.raw_cue);
        let track_number_plan = cue_track_number_plan(&cue_input.sheet);
        warn_if_cue_track_numbering_normalized(&track_number_plan);
        let album_image_metadata = merge_image_album_metadata(&track_images, &image_metadata);
        let total_tracks = cue_input.sheet.tracks.len() as u32;

        let mut tracks = Vec::with_capacity(cue_input.sheet.tracks.len());
        for (idx, cue_track) in cue_input.sheet.tracks.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(MaterializeError::Cancelled);
            }

            let ordinal = (idx + 1) as u32;
            let image_path = &track_images[idx];
            let image_key = path_identity(image_path);
            let probe = probes.get(&image_key).ok_or_else(|| {
                MaterializeError::Parse(format!(
                    "missing audio probe for CUE image {}",
                    image_path.display()
                ))
            })?;
            let image_metadata_for_track = image_metadata.get(&image_key).ok_or_else(|| {
                MaterializeError::Parse(format!(
                    "missing image metadata slot for CUE image {}",
                    image_path.display()
                ))
            })?;
            let (start_sample, samples) = boundaries[idx];
            let mut metadata = cue_track_metadata(
                cue_track,
                &cue_input.sheet,
                image_metadata_for_track,
                cue_annotations.track_pre_emphasis(cue_track.number),
                track_number_plan[idx],
            );
            cue_annotations.add_track_extras(cue_track.number, &mut metadata.extra);

            tracks.push(PreparedTrack {
                id: TrackId {
                    source_ordinal: ordinal,
                    disc_number: None,
                    track_number: track_number_plan[idx].output_number,
                },
                source_ref: TrackSourceRef::ImageSegment {
                    image: image_path.clone(),
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
        let mut album_metadata = cue_album_metadata(&cue_input.sheet, &album_image_metadata, total_tracks);
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
        CueSidecarPolicy::PreferEmbedded => {
            match resolve_embedded_cue(req) {
                Ok(cue) => Ok(cue),
                Err(_) => {
                    match find_valid_sidecar_cue_for_image(&req.container)? {
                        Some(cue_path) => read_sidecar_cue(req, cue_path),
                        None => Err(MaterializeError::Parse(
                            "no embedded CUESHEET and no sidecar CUE found".to_string(),
                        )),
                    }
                }
            }
        }
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
            "SidecarOnly requested but no matching CUE was found".to_string(),
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
    validate_sidecar_layout(&sheet)?;

    let cue_parent = cue_path.parent().map(Path::to_path_buf);
    let fallback_image = (!has_extension(&req.container, "cue")).then(|| req.container.clone());
    let cue_input = CueInput {
        sheet,
        raw_cue,
        origin: CueOrigin::Sidecar,
        cue_parent,
        fallback_image,
    };

    if !has_extension(&req.container, "cue") {
        let track_images = resolve_track_image_paths(&cue_input)?;
        if !track_images
            .iter()
            .any(|image_path| same_existing_path(image_path, &req.container))
        {
            return Err(MaterializeError::Parse(format!(
                "CUE file {} does not reference input image {}",
                cue_path.display(),
                req.container.display()
            )));
        }
    }

    Ok(cue_input)
}

fn resolve_embedded_cue(req: &PipelineRequest) -> Result<CueInput, MaterializeError> {
    let raw_cue = read_embedded_cuesheet(&req.container)?.ok_or_else(|| {
        MaterializeError::Parse(
            "EmbeddedOnly requested but no embedded CUESHEET tag was found".to_string(),
        )
    })?;
    let sheet = parse_cue(&raw_cue);
    validate_embedded_single_image_layout(&sheet)?;

    Ok(CueInput {
        sheet,
        raw_cue,
        origin: CueOrigin::Embedded,
        cue_parent: None,
        fallback_image: Some(req.container.clone()),
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
        CueSidecarPolicy::PreferEmbedded => {
            if embedded_cuesheet_is_single_image(&req.container) {
                return Ok(true);
            }
            Ok(sidecar_cue_route_candidate(&req.container)?.is_some())
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
        let content = String::from_utf8_lossy(&raw).into_owned();
        let sheet = parse_cue(&content);
        if validate_sidecar_layout_detect(&sheet).is_err() {
            continue;
        }
        let cue_input = CueInput {
            sheet,
            raw_cue: content,
            origin: CueOrigin::Sidecar,
            cue_parent: cue_path.parent().map(Path::to_path_buf),
            fallback_image: Some(image.to_path_buf()),
        };
        if let Ok(track_images) = resolve_track_image_paths(&cue_input) {
            if track_images
                .iter()
                .any(|resolved| same_existing_path(resolved, image))
            {
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
    validate_embedded_single_image_layout_detect(&sheet).is_ok()
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
    validate_sidecar_layout(&sheet)?;
    let cue_input = CueInput {
        sheet,
        raw_cue,
        origin: CueOrigin::Sidecar,
        cue_parent: cue_path.parent().map(Path::to_path_buf),
        fallback_image: Some(image.to_path_buf()),
    };
    let track_images = resolve_track_image_paths(&cue_input)?;
    Ok(track_images
        .iter()
        .any(|resolved| same_existing_path(resolved, image)))
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

fn validate_sidecar_layout(sheet: &CueSheet) -> Result<(), MaterializeError> {
    validate_sidecar_layout_detect(sheet).map_err(MaterializeError::Parse)
}

fn validate_sidecar_layout_detect(sheet: &CueSheet) -> Result<(), String> {
    validate_common_cue_layout(sheet)?;
    if !sheet.tracks.iter().all(|track| track.file.is_some()) {
        return Err("sidecar CUE requires a FILE reference for every track".to_string());
    }
    validate_index_order_per_file(sheet)
}

fn validate_embedded_single_image_layout(sheet: &CueSheet) -> Result<(), MaterializeError> {
    validate_embedded_single_image_layout_detect(sheet).map_err(MaterializeError::Parse)
}

fn validate_embedded_single_image_layout_detect(sheet: &CueSheet) -> Result<(), String> {
    validate_common_cue_layout(sheet)?;

    let first_file = sheet.tracks.iter().find_map(|track| track.file.as_ref());
    if let Some(file_ref) = first_file {
        let all_same = sheet
            .tracks
            .iter()
            .all(|track| track.file.as_ref().is_none() || track.file.as_ref() == Some(file_ref));
        if !all_same {
            return Err("embedded CUE cannot reference multiple audio files".to_string());
        }
    }

    validate_index_order_per_file(sheet)
}

fn validate_common_cue_layout(sheet: &CueSheet) -> Result<(), String> {
    if sheet.tracks.is_empty() {
        return Err("CUE sheet has no tracks".to_string());
    }
    validate_track_number_identity(sheet)?;
    if !sheet
        .tracks
        .iter()
        .all(|track| track.index01_frames.is_some())
    {
        return Err("CUE requires INDEX 01 for every track".to_string());
    }
    Ok(())
}

fn validate_track_number_identity(sheet: &CueSheet) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for track in &sheet.tracks {
        if track.number == 0 {
            return Err("CUE track numbers must be positive".to_string());
        }
        if !seen.insert(track.number) {
            return Err(format!("duplicate CUE track number {}", track.number));
        }
    }
    Ok(())
}

fn validate_index_order_per_file(sheet: &CueSheet) -> Result<(), String> {
    let mut previous_by_file: HashMap<String, u32> = HashMap::new();
    for track in &sheet.tracks {
        let current = track.index01_frames.expect("checked above");
        let file_key = track
            .file
            .as_deref()
            .map(normalize_cue_file_key)
            .unwrap_or_else(|| "<embedded>".to_string());
        if previous_by_file
            .get(&file_key)
            .is_some_and(|previous| current <= *previous)
        {
            return Err(format!(
                "non-increasing INDEX 01 at track {} in FILE {}",
                track.number,
                track.file.as_deref().unwrap_or("<embedded>")
            ));
        }
        previous_by_file.insert(file_key, current);
    }
    Ok(())
}

fn normalize_cue_file_key(value: &str) -> String {
    value.replace('\\', "/").to_ascii_lowercase()
}

fn resolve_track_image_paths(input: &CueInput) -> Result<Vec<PathBuf>, MaterializeError> {
    match input.origin {
        CueOrigin::Embedded => {
            let image = input.fallback_image.as_ref().ok_or_else(|| {
                MaterializeError::Parse("embedded CUE has no owning image".to_string())
            })?;
            Ok(vec![image.clone(); input.sheet.tracks.len()])
        }
        CueOrigin::Sidecar => input
            .sheet
            .tracks
            .iter()
            .map(|track| {
                let file_ref = track.file.as_ref().ok_or_else(|| {
                    MaterializeError::Parse(format!(
                        "track {} has no CUE FILE reference",
                        track.number
                    ))
                })?;
                resolve_audio_reference(
                    input.cue_parent.as_deref(),
                    file_ref,
                    input.fallback_image.as_deref(),
                )
            })
            .collect(),
    }
}

fn unique_existing_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut unique: Vec<PathBuf> = Vec::new();
    for path in paths {
        if !unique.iter().any(|existing| same_existing_path(existing, path)) {
            unique.push(path.clone());
        }
    }
    unique
}

fn path_identity(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
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
            "stream=sample_rate,duration_ts,time_base,duration,bits_per_raw_sample,bits_per_sample"
                .into(),
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

fn compute_track_boundaries_for_layout(
    sheet: &CueSheet,
    track_images: &[PathBuf],
    probes: &HashMap<PathBuf, AudioProbe>,
) -> Result<Vec<(u64, u64)>, MaterializeError> {
    if sheet.tracks.len() != track_images.len() {
        return Err(MaterializeError::Parse(format!(
            "CUE track/image cardinality mismatch: {} tracks, {} images",
            sheet.tracks.len(),
            track_images.len()
        )));
    }

    let image_keys: Vec<PathBuf> = track_images
        .iter()
        .map(|image_path| path_identity(image_path))
        .collect();
    let mut boundaries = Vec::with_capacity(sheet.tracks.len());

    for (idx, track) in sheet.tracks.iter().enumerate() {
        let image_path = &track_images[idx];
        let image_key = &image_keys[idx];
        let probe = probes.get(image_key).ok_or_else(|| {
            MaterializeError::Parse(format!(
                "missing audio probe for CUE image {}",
                image_path.display()
            ))
        })?;
        let index01 = track.index01_frames.ok_or_else(|| {
            MaterializeError::Parse(format!("track {} has no INDEX 01", track.number))
        })?;
        let start = cue_frames_to_samples(index01 as u64, probe.sample_rate);

        let next_start = ((idx + 1)..sheet.tracks.len())
            .find(|next_idx| image_keys[*next_idx] == *image_key)
            .map(|next_idx| {
                let next_frames = sheet.tracks[next_idx]
                    .index01_frames
                    .expect("validated INDEX 01");
                cue_frames_to_samples(next_frames as u64, probe.sample_rate)
            });
        let end = next_start.unwrap_or(probe.total_samples);

        if end <= start {
            return Err(MaterializeError::Parse(format!(
                "invalid CUE boundary for track {} in image {}",
                track.number,
                image_path.display()
            )));
        }
        if end > probe.total_samples {
            return Err(MaterializeError::Parse(format!(
                "track {} starts beyond image duration for {}",
                track.number,
                image_path.display()
            )));
        }
        if !probe.exact_samples
            && next_start.is_none()
            && probe.total_samples.saturating_sub(start) < probe.sample_rate as u64 / 20
        {
            return Err(MaterializeError::Parse(format!(
                "image sample count probe is too coarse for final CUE segment in {}",
                image_path.display()
            )));
        }
        boundaries.push((start, end - start));
    }

    Ok(boundaries)
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ImageAlbumMetadata {
    album: Option<String>,
    album_artist: Option<String>,
    artist: Option<String>,
    genre: Option<String>,
    date: Option<String>,
    total_discs: Option<u32>,
    disc_number: Option<u32>,
    source: Option<String>,
}

fn merge_image_album_metadata(
    track_images: &[PathBuf],
    metadata_by_image: &HashMap<PathBuf, ImageAlbumMetadata>,
) -> ImageAlbumMetadata {
    let mut merged = ImageAlbumMetadata::default();
    let mut sources = Vec::new();

    for image_path in unique_existing_paths(track_images) {
        let Some(metadata) = metadata_by_image.get(&path_identity(&image_path)) else {
            continue;
        };

        if let Some(source) = &metadata.source {
            if !sources.iter().any(|seen| seen == source) {
                sources.push(source.clone());
            }
        }
        if merged.album.is_none() {
            merged.album = metadata.album.clone();
        }
        if merged.album_artist.is_none() {
            merged.album_artist = metadata.album_artist.clone();
        }
        if merged.artist.is_none() {
            merged.artist = metadata.artist.clone();
        }
        if merged.genre.is_none() {
            merged.genre = metadata.genre.clone();
        }
        if merged.date.is_none() {
            merged.date = metadata.date.clone();
        }
        if merged.total_discs.is_none() {
            merged.total_discs = metadata.total_discs;
        }
        if merged.disc_number.is_none() {
            merged.disc_number = metadata.disc_number;
        }
    }

    if !sources.is_empty() {
        merged.source = Some(sources.join("; "));
    }

    merged
}

fn read_image_album_metadata(path: &Path) -> ImageAlbumMetadata {
    use lofty::prelude::*;

    let tagged = match lofty::read_from_path(path) {
        Ok(tagged) => tagged,
        Err(err) => {
            log::warn!(
                "unable to read album-level tags from CUE image '{}'; using CUE metadata without image tag fallback: {err}",
                path.display()
            );
            return ImageAlbumMetadata::default();
        }
    };
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return ImageAlbumMetadata::default();
    };

    let mut metadata = ImageAlbumMetadata::default();
    for item in tag.items() {
        let key = normalized_lofty_item_key(item.key());
        let Some(value) = item.value().text().map(str::trim).filter(|value| !value.is_empty()) else {
            continue;
        };
        match cue_image_tag_field(&key) {
            Some(ImageTagField::Album) => set_if_empty(&mut metadata.album, value),
            Some(ImageTagField::AlbumArtist) => set_if_empty(&mut metadata.album_artist, value),
            Some(ImageTagField::Artist) => set_if_empty(&mut metadata.artist, value),
            Some(ImageTagField::Genre) => set_if_empty(&mut metadata.genre, value),
            Some(ImageTagField::Date) => set_if_empty(&mut metadata.date, value),
            Some(ImageTagField::DiscNumber) => {
                if metadata.disc_number.is_none() {
                    metadata.disc_number = parse_tag_number(value);
                }
            }
            Some(ImageTagField::TotalDiscs) => {
                if metadata.total_discs.is_none() {
                    metadata.total_discs = parse_tag_number(value);
                }
            }
            None => {}
        }
    }

    if metadata.album.is_some()
        || metadata.album_artist.is_some()
        || metadata.artist.is_some()
        || metadata.genre.is_some()
        || metadata.date.is_some()
        || metadata.total_discs.is_some()
        || metadata.disc_number.is_some()
    {
        metadata.source = Some(path.display().to_string());
    }
    metadata
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageTagField {
    Album,
    AlbumArtist,
    Artist,
    Genre,
    Date,
    DiscNumber,
    TotalDiscs,
}

fn cue_image_tag_field(key: &str) -> Option<ImageTagField> {
    match key {
        "album" | "albumtitle" | "talb" => Some(ImageTagField::Album),
        "albumartist" | "albumartistsort" | "albumartistsortorder" | "tpe2" => {
            Some(ImageTagField::AlbumArtist)
        }
        "artist" | "trackartist" | "performer" | "tpe1" => Some(ImageTagField::Artist),
        "genre" | "tcon" => Some(ImageTagField::Genre),
        "date" | "year" | "recordingdate" | "originaldate" | "tdrc" | "tyer" => {
            Some(ImageTagField::Date)
        }
        "discnumber" | "disc" | "partofset" | "tpos" => Some(ImageTagField::DiscNumber),
        "totaldiscs" | "disctotal" => Some(ImageTagField::TotalDiscs),
        _ => None,
    }
}

fn normalized_lofty_item_key(key: &lofty::tag::ItemKey) -> String {
    let raw = match key {
        lofty::tag::ItemKey::Unknown(value) => value.as_str().to_string(),
        _ => format!("{key:?}"),
    };
    normalize_tag_key(&raw)
}

fn normalize_tag_key(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn set_if_empty(slot: &mut Option<String>, value: &str) {
    if slot.is_none() {
        *slot = Some(value.to_string());
    }
}

fn parse_tag_number(value: &str) -> Option<u32> {
    value
        .split('/')
        .next()
        .unwrap_or(value)
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CueTrackNumberPlan {
    output_number: u32,
    cue_number: u32,
}

fn cue_track_number_plan(sheet: &CueSheet) -> Vec<CueTrackNumberPlan> {
    sheet
        .tracks
        .iter()
        .enumerate()
        .map(|(idx, track)| CueTrackNumberPlan {
            output_number: (idx + 1) as u32,
            cue_number: track.number,
        })
        .collect()
}

fn warn_if_cue_track_numbering_normalized(plan: &[CueTrackNumberPlan]) {
    if plan
        .iter()
        .all(|entry| entry.output_number == entry.cue_number)
    {
        return;
    }

    let mapping = plan
        .iter()
        .map(|entry| format!("{}->{}", entry.cue_number, entry.output_number))
        .collect::<Vec<_>>()
        .join(", ");
    log::warn!(
        "CUE track numbers are not sequential from 1 in playback order; writing normalized TRACKNUMBER values and preserving CUE numbers in TRACK_CUE_TRACK_NUMBER tags: {mapping}"
    );
}

fn cue_track_metadata(
    cue_track: &crate::tui::cue_parser::CueTrack,
    sheet: &CueSheet,
    image: &ImageAlbumMetadata,
    pre_emphasis: bool,
    numbering: CueTrackNumberPlan,
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
    if numbering.cue_number != numbering.output_number {
        extra.insert(
            "cue_track_number".to_string(),
            numbering.cue_number.to_string(),
        );
    }

    let performer = cue_track
        .performer
        .clone()
        .or_else(|| sheet.performer.clone())
        .or_else(|| image.artist.clone())
        .or_else(|| image.album_artist.clone());
    let album_artist = sheet
        .performer
        .clone()
        .or_else(|| image.album_artist.clone())
        .or_else(|| image.artist.clone());

    TrackMetadata {
        title: cue_track.title.clone(),
        artist: performer.clone(),
        album_artist,
        composer: None,
        performer,
        genre: sheet.genre.clone().or_else(|| image.genre.clone()),
        date: sheet.date.clone().or_else(|| image.date.clone()),
        track_number: Some(numbering.output_number),
        disc_number: None,
        isrc: cue_track.isrc.clone(),
        publisher: None,
        copyright: None,
        comment: None,
        pre_emphasis,
        extra,
    }
}

fn cue_album_metadata(
    sheet: &CueSheet,
    image: &ImageAlbumMetadata,
    total_tracks: u32,
) -> AlbumMetadata {
    let mut extra = BTreeMap::new();
    if let Some(catalog) = &sheet.catalog {
        extra.insert("catalog".to_string(), catalog.clone());
    }
    if let Some(source) = &image.source {
        extra.insert("image_metadata_source".to_string(), source.clone());
    }

    AlbumMetadata {
        album: sheet.title.clone().or_else(|| image.album.clone()),
        album_artist: sheet
            .performer
            .clone()
            .or_else(|| image.album_artist.clone())
            .or_else(|| image.artist.clone()),
        genre: sheet.genre.clone().or_else(|| image.genre.clone()),
        date: sheet.date.clone().or_else(|| image.date.clone()),
        total_tracks,
        total_discs: image.total_discs,
        disc_number: image.disc_number,
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
        assert_eq!(
            json_u32_from_value(&serde_json::json!("not-a-number")),
            None
        );
    }
}

#[cfg(test)]
mod materializer_cue_tests {
    use super::*;
    use crate::convert::pipeline::tool::StubToolRunner;
    use crate::convert::pipeline::tool::ToolOutput;

    // ── helpers ──

    fn ffprobe_json_exact(sample_rate: u32, total_samples: u64, bit_depth: u32) -> String {
        let time_base = format!("1/{sample_rate}");
        format!(
            r#"{{
  "streams": [{{
    "sample_rate": "{sample_rate}",
    "duration_ts": {total_samples},
    "time_base": "{time_base}",
    "bits_per_raw_sample": "{bit_depth}"
  }}],
  "format": {{}}
}}"#
        )
    }

    fn ffprobe_json_approx(sample_rate: u32, duration_secs: f64) -> String {
        format!(
            r#"{{
  "streams": [{{
    "sample_rate": "{sample_rate}",
    "duration": "{duration_secs}"
  }}],
  "format": {{
    "duration": "{duration_secs}"
  }}
}}"#
        )
    }

    fn stub_runner_with_probe(json: &str) -> StubToolRunner {
        stub_runner_with_probes(&[json])
    }

    fn stub_runner_with_probes(json_outputs: &[&str]) -> StubToolRunner {
        let runner = StubToolRunner::new();
        for json in json_outputs {
            runner.push_output(ToolOutput {
                exit: crate::convert::pipeline::tool::ProcessExit::Code(0),
                stdout_tail: (*json).to_string(),
                stderr_tail: String::new(),
                elapsed: Duration::from_millis(10),
                command: crate::convert::pipeline::tool::CommandRecord {
                    binary: ToolBinary::Ffprobe,
                    sanitized_args: vec![],
                    cwd: None,
                    env_keys: vec![],
                    exit: Some(crate::convert::pipeline::tool::ProcessExit::Code(0)),
                    stdout_tail: String::new(),
                    stderr_tail: String::new(),
                    elapsed: Duration::from_millis(10),
                },
            });
        }
        runner
    }

    fn write_file(path: &std::path::Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(path, contents).expect("write file");
    }

    fn cue_sheet_3_track() -> String {
        r#"REM GENRE Rock
REM DATE 1973
PERFORMER "Pink Floyd"
TITLE "The Dark Side of the Moon"
FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Speak to Me"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Breathe"
    INDEX 01 01:30:00
  TRACK 03 AUDIO
    TITLE "On the Run"
    INDEX 01 04:13:00
"#
        .to_string()
    }

    fn cue_sheet_single_track() -> String {
        r#"PERFORMER "Artist"
TITLE "Album"
FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Only Track"
    INDEX 01 00:00:00
"#
        .to_string()
    }

    fn test_request(container: &Path) -> PipelineRequest {
        PipelineRequest {
            job_id: "test-job".to_string(),
            item_id: "test-item".to_string(),
            container: container.to_path_buf(),
            source: SourceOptions {
                archive_password: None,
                sacd_area: None,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection: TrackSelection::All,
            },
            settings: tonepoet_pipeline::PipelineSettings::default(),
            worker_count: None,
            merge: false,
            output_root: container.parent().unwrap_or(Path::new(".")).to_path_buf(),
            naming: NamingPolicy {
                template: "%NN% - %TITLE%".to_string(),
                folder_template: None,
                per_album_subdir: true,
                collision_policy: NamingCollisionPolicy::Fail,
            },
            publish: PublishPolicy {
                overwrite: OverwritePolicy::FailIfExists,
                same_filesystem_required: false,
                write_manifest: false,
            },
            log: LogPolicy {
                root: container.parent().unwrap_or(Path::new(".")).to_path_buf(),
                write_for_blocked: false,
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

    fn test_staging(temp: &tempfile::TempDir) -> StagingDir {
        let root = temp.path().join("staging");
        std::fs::create_dir_all(&root).expect("create staging dir");
        StagingDir::new(root, "test-staging".to_string())
    }

    async fn materialize_cue(
        cue_content: &str,
        probe_json: &str,
        temp: &tempfile::TempDir,
    ) -> Result<PreparedSource, MaterializeError> {
        materialize_cue_with_audio_files(cue_content, &[probe_json], &["album.flac"], temp).await
    }

    async fn materialize_cue_with_audio_files(
        cue_content: &str,
        probe_jsons: &[&str],
        audio_files: &[&str],
        temp: &tempfile::TempDir,
    ) -> Result<PreparedSource, MaterializeError> {
        let cue_path = temp.path().join("album.cue");
        write_file(&cue_path, cue_content.as_bytes());
        for audio_file in audio_files {
            write_file(&temp.path().join(audio_file), b"fake-audio-data");
        }

        let runner = stub_runner_with_probes(probe_jsons);
        let mut staging = test_staging(temp);
        let cancel = CancellationToken::new();
        let req = test_request(&cue_path);
        let result = CueImageMaterializer
            .materialize(&req, &staging, &runner, None, &HashMap::new(), &cancel)
            .await;
        staging.disarm();
        result
    }

    // ── Category A: happy path ──

    #[tokio::test]
    async fn three_track_cd_produces_correct_boundaries_and_metadata() {
        let temp = tempfile::tempdir().expect("temp dir");
        // 44100 Hz, 3 tracks: 0:00, 1:30, 4:13. Total ~10 minutes = 26,460,000 samples
        let total_samples: u64 = 26_460_000;
        let probe = ffprobe_json_exact(44100, total_samples, 16);
        let source = materialize_cue(&cue_sheet_3_track(), &probe, &temp)
            .await
            .expect("materialize succeeds");

        assert_eq!(source.kind, SourceKind::CueImage);
        assert_eq!(source.tracks.len(), 3);

        // Track 1: starts at 0:00:00 = frame 0 = sample 0
        assert_eq!(source.tracks[0].id.track_number, 1);
        assert_eq!(source.tracks[0].id.source_ordinal, 1);
        let TrackSourceRef::ImageSegment { start_sample, samples, .. } = &source.tracks[0].source_ref else {
            panic!("expected ImageSegment");
        };
        assert_eq!(*start_sample, 0);

        // Track 2: starts at 1:30:00 = 6750 frames = 6750 * 44100 / 75 = 3,969,000 samples
        let TrackSourceRef::ImageSegment { start_sample: s2, .. } = &source.tracks[1].source_ref else {
            panic!("expected ImageSegment");
        };
        assert_eq!(*s2, 3_969_000);
        assert_eq!(*samples, 3_969_000); // track 1 length = track 2 start - track 1 start

        // Track 3: starts at 4:13:00 = 18975 frames = 18975 * 44100 / 75 = 11,157,300 samples
        let TrackSourceRef::ImageSegment { start_sample: s3, samples: s3_len, .. } = &source.tracks[2].source_ref else {
            panic!("expected ImageSegment");
        };
        assert_eq!(*s3, 11_157_300);
        assert_eq!(*s3_len, total_samples - 11_157_300);

        // Metadata
        assert_eq!(source.tracks[0].metadata.title.as_deref(), Some("Speak to Me"));
        assert_eq!(source.tracks[1].metadata.title.as_deref(), Some("Breathe"));
        assert_eq!(source.tracks[2].metadata.title.as_deref(), Some("On the Run"));
        assert_eq!(source.tracks[0].metadata.artist.as_deref(), Some("Pink Floyd"));

        // Album metadata
        assert_eq!(source.album_metadata.album.as_deref(), Some("The Dark Side of the Moon"));
        assert_eq!(source.album_metadata.album_artist.as_deref(), Some("Pink Floyd"));
        assert_eq!(source.album_metadata.genre.as_deref(), Some("Rock"));
        assert_eq!(source.album_metadata.date.as_deref(), Some("1973"));

        // Sample rate and bit depth propagated
        assert_eq!(source.tracks[0].sample_rate, 44100);
        assert_eq!(source.tracks[0].bit_depth, Some(16));
        assert_eq!(source.tracks[0].expected_samples, Some(3_969_000));
    }

    #[tokio::test]
    async fn single_track_cue_spans_entire_image() {
        let temp = tempfile::tempdir().expect("temp dir");
        let total_samples: u64 = 10_000_000;
        let probe = ffprobe_json_exact(44100, total_samples, 16);
        let source = materialize_cue(&cue_sheet_single_track(), &probe, &temp)
            .await
            .expect("materialize succeeds");

        assert_eq!(source.tracks.len(), 1);
        let TrackSourceRef::ImageSegment { start_sample, samples, .. } = &source.tracks[0].source_ref else {
            panic!("expected ImageSegment");
        };
        assert_eq!(*start_sample, 0);
        assert_eq!(*samples, total_samples);
        assert_eq!(source.tracks[0].metadata.title.as_deref(), Some("Only Track"));
    }

    #[tokio::test]
    async fn twelve_track_album_has_contiguous_non_overlapping_boundaries() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut cue = String::from("FILE \"album.flac\" WAVE\n");
        for i in 1..=12 {
            let mm = (i - 1) * 4;
            cue.push_str(&format!(
                "  TRACK {:02} AUDIO\n    TITLE \"Track {}\"\n    INDEX 01 {:02}:00:00\n",
                i, i, mm
            ));
        }
        // 12 tracks at 4 min each = 48 min. Total = 50 min = 132,300,000 samples at 44100
        let total_samples: u64 = 132_300_000;
        let probe = ffprobe_json_exact(44100, total_samples, 16);
        let source = materialize_cue(&cue, &probe, &temp)
            .await
            .expect("materialize succeeds");

        assert_eq!(source.tracks.len(), 12);

        // Verify contiguous, non-overlapping
        let mut prev_end: u64 = 0;
        for track in &source.tracks {
            let TrackSourceRef::ImageSegment { start_sample, samples, .. } = &track.source_ref else {
                panic!("expected ImageSegment");
            };
            assert_eq!(*start_sample, prev_end, "track {} must start where previous ended", track.id.track_number);
            assert!(*samples > 0, "track {} must have positive length", track.id.track_number);
            prev_end = start_sample + samples;
        }
        assert_eq!(prev_end, total_samples);
    }

    // ── Category B: malformed CUE ──

    #[tokio::test]
    async fn empty_cue_sheet_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        let probe = ffprobe_json_exact(44100, 10_000_000, 16);
        let result = materialize_cue("", &probe, &temp).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cue_without_index_01_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue = r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "No Index"
"#;
        let probe = ffprobe_json_exact(44100, 10_000_000, 16);
        let result = materialize_cue(cue, &probe, &temp).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("INDEX 01"), "error should mention INDEX 01: {err}");
    }

    #[tokio::test]
    async fn track_per_file_cue_materializes_each_referenced_file() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue = r#"FILE "track1.flac" WAVE
  TRACK 01 AUDIO
    TITLE "One"
    INDEX 01 00:00:00
FILE "track2.flac" WAVE
  TRACK 02 AUDIO
    TITLE "Two"
    INDEX 01 00:00:00
"#;
        let probe1 = ffprobe_json_exact(44100, 441_000, 16);
        let probe2 = ffprobe_json_exact(44100, 882_000, 16);
        let source = materialize_cue_with_audio_files(
            cue,
            &[&probe1, &probe2],
            &["track1.flac", "track2.flac"],
            &temp,
        )
        .await
        .expect("track-per-file CUE materializes");

        assert_eq!(source.tracks.len(), 2);
        assert_eq!(source.album_metadata.total_tracks, 2);

        let TrackSourceRef::ImageSegment { image, start_sample, samples } = &source.tracks[0].source_ref else {
            panic!("expected ImageSegment");
        };
        assert_eq!(image.file_name().and_then(|value| value.to_str()), Some("track1.flac"));
        assert_eq!(*start_sample, 0);
        assert_eq!(*samples, 441_000);
        assert_eq!(source.tracks[0].metadata.title.as_deref(), Some("One"));

        let TrackSourceRef::ImageSegment { image, start_sample, samples } = &source.tracks[1].source_ref else {
            panic!("expected ImageSegment");
        };
        assert_eq!(image.file_name().and_then(|value| value.to_str()), Some("track2.flac"));
        assert_eq!(*start_sample, 0);
        assert_eq!(*samples, 882_000);
        assert_eq!(source.tracks[1].metadata.title.as_deref(), Some("Two"));
    }

    #[test]
    fn sidecar_discovery_matches_audio_inside_multiple_file_cue() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue_path = temp.path().join("album.cue");
        let track1 = temp.path().join("track1.flac");
        let track2 = temp.path().join("track2.flac");
        write_file(
            &cue_path,
            br#"FILE "track1.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
FILE "track2.flac" WAVE
  TRACK 02 AUDIO
    INDEX 01 00:00:00
"#,
        );
        write_file(&track1, b"fake-audio-data");
        write_file(&track2, b"fake-audio-data");

        let discovered = find_valid_sidecar_cue_for_image(&track2)
            .expect("sidecar search succeeds")
            .expect("multi-file CUE matches referenced audio");
        assert_eq!(discovered, cue_path);
    }

    #[tokio::test]
    async fn non_sequential_cue_track_numbers_are_normalized_and_preserved() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue = r#"FILE "album.flac" WAVE
  TRACK 05 AUDIO
    TITLE "Hidden Lead-In"
    INDEX 01 00:00:00
  TRACK 07 AUDIO
    TITLE "Second Audible Track"
    INDEX 01 01:00:00
"#;
        let probe = ffprobe_json_exact(44100, 10_000_000, 16);
        let source = materialize_cue(cue, &probe, &temp)
            .await
            .expect("non-sequential CUE numbering materializes");

        assert_eq!(source.tracks.len(), 2);
        assert_eq!(source.album_metadata.total_tracks, 2);

        assert_eq!(source.tracks[0].id.track_number, 1);
        assert_eq!(source.tracks[0].metadata.track_number, Some(1));
        assert_eq!(
            source.tracks[0].metadata.extra.get("cue_track_number").map(String::as_str),
            Some("5")
        );

        assert_eq!(source.tracks[1].id.track_number, 2);
        assert_eq!(source.tracks[1].metadata.track_number, Some(2));
        assert_eq!(
            source.tracks[1].metadata.extra.get("cue_track_number").map(String::as_str),
            Some("7")
        );
    }

    #[tokio::test]
    async fn duplicate_cue_track_numbers_fail_before_metadata_mapping() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue = r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "First"
    INDEX 01 00:00:00
  TRACK 01 AUDIO
    TITLE "Duplicate"
    INDEX 01 01:00:00
"#;
        let probe = ffprobe_json_exact(44100, 10_000_000, 16);
        let result = materialize_cue(cue, &probe, &temp).await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("duplicate CUE track number 1"),
            "error should identify duplicate CUE track number: {err}"
        );
    }

    #[tokio::test]
    async fn index_beyond_image_duration_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        // Image is only 1,000,000 samples (~22.7 sec at 44100), but track 2 starts at 5:00
        let cue = r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 05:00:00
"#;
        let probe = ffprobe_json_exact(44100, 1_000_000, 16);
        let result = materialize_cue(cue, &probe, &temp).await;
        assert!(result.is_err());
    }

    // ── Category C: pregap/INDEX edge cases ──

    #[tokio::test]
    async fn pregap_index00_preserved_in_metadata_boundary_uses_index01() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue = r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Track One"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Track Two"
    INDEX 00 03:43:37
    INDEX 01 03:45:00
"#;
        let total_samples: u64 = 20_000_000;
        let probe = ffprobe_json_exact(44100, total_samples, 16);
        let source = materialize_cue(cue, &probe, &temp)
            .await
            .expect("materialize succeeds");

        assert_eq!(source.tracks.len(), 2);

        // Track 2 boundary should use INDEX 01 (03:45:00), not INDEX 00 (03:43:37)
        // INDEX 01 at 03:45:00 = 16875 frames = 16875 * 44100 / 75 = 9,922,500 samples
        let TrackSourceRef::ImageSegment { start_sample, .. } = &source.tracks[1].source_ref else {
            panic!("expected ImageSegment");
        };
        assert_eq!(*start_sample, 9_922_500);

        // INDEX 00 should be preserved in extras
        let extras = &source.tracks[1].metadata.extra;
        assert!(
            extras.contains_key("index00_frames"),
            "INDEX 00 should be preserved in metadata extras"
        );
    }

    #[tokio::test]
    async fn track_1_starting_at_zero_has_start_sample_zero() {
        let temp = tempfile::tempdir().expect("temp dir");
        let probe = ffprobe_json_exact(44100, 10_000_000, 16);
        let source = materialize_cue(&cue_sheet_single_track(), &probe, &temp)
            .await
            .expect("materialize succeeds");

        let TrackSourceRef::ImageSegment { start_sample, .. } = &source.tracks[0].source_ref else {
            panic!("expected ImageSegment");
        };
        assert_eq!(*start_sample, 0);
    }

    // ── Category D: Unicode/encoding ──

    #[tokio::test]
    async fn utf8_bom_stripped_metadata_parsed() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue = format!(
            "\u{FEFF}PERFORMER \"Artist\"\nTITLE \"Album\"\nFILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n"
        );
        let probe = ffprobe_json_exact(44100, 10_000_000, 16);
        let source = materialize_cue(&cue, &probe, &temp)
            .await
            .expect("materialize succeeds with BOM");

        assert_eq!(source.album_metadata.album_artist.as_deref(), Some("Artist"));
        assert_eq!(source.album_metadata.album.as_deref(), Some("Album"));
    }

    #[tokio::test]
    async fn non_ascii_metadata_preserved() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue = "PERFORMER \"Björk\"\nTITLE \"Début\"\nFILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Human Behaviour\"\n    PERFORMER \"Björk\"\n    INDEX 01 00:00:00\n";
        let probe = ffprobe_json_exact(44100, 10_000_000, 16);
        let source = materialize_cue(cue, &probe, &temp)
            .await
            .expect("materialize succeeds with non-ASCII");

        assert_eq!(source.album_metadata.album_artist.as_deref(), Some("Björk"));
        assert_eq!(source.album_metadata.album.as_deref(), Some("Début"));
        assert_eq!(source.tracks[0].metadata.artist.as_deref(), Some("Björk"));
    }

    // ── Category E: high sample rates ──

    #[tokio::test]
    async fn ninety_six_khz_boundary_computation() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue = r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 01:00:00
"#;
        // 96kHz, 10 min = 57,600,000 samples
        let total_samples: u64 = 57_600_000;
        let probe = ffprobe_json_exact(96000, total_samples, 24);
        let source = materialize_cue(cue, &probe, &temp)
            .await
            .expect("materialize succeeds at 96kHz");

        // Track 2 at 01:00:00 = 4500 frames = 4500 * 96000 / 75 = 5,760,000 samples
        let TrackSourceRef::ImageSegment { start_sample, .. } = &source.tracks[1].source_ref else {
            panic!("expected ImageSegment");
        };
        assert_eq!(*start_sample, 5_760_000);
        assert_eq!(source.tracks[0].sample_rate, 96000);
        assert_eq!(source.tracks[0].bit_depth, Some(24));
    }

    #[tokio::test]
    async fn one_ninety_two_khz_boundary_computation() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue = r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 02:00:00
"#;
        // 192kHz, 10 min = 115,200,000 samples
        let total_samples: u64 = 115_200_000;
        let probe = ffprobe_json_exact(192000, total_samples, 24);
        let source = materialize_cue(cue, &probe, &temp)
            .await
            .expect("materialize succeeds at 192kHz");

        // Track 2 at 02:00:00 = 9000 frames = 9000 * 192000 / 75 = 23,040,000 samples
        let TrackSourceRef::ImageSegment { start_sample, .. } = &source.tracks[1].source_ref else {
            panic!("expected ImageSegment");
        };
        assert_eq!(*start_sample, 23_040_000);
        assert_eq!(source.tracks[0].sample_rate, 192000);
    }

    // ── Category F: sample-count validation ──

    #[tokio::test]
    async fn approximate_duration_with_tiny_final_track_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        // 2 tracks, second starts at 4:00:00 = 18000 frames = 10,584,000 samples at 44100
        // Set total duration so final track is < sample_rate/20 = 2205 samples
        // 10,584,000 + 2000 = 10,586,000. Duration = 10586000/44100 = ~240.045 sec
        let cue = r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 04:00:00
"#;
        let probe = ffprobe_json_approx(44100, 240.045);
        let result = materialize_cue(cue, &probe, &temp).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("coarse"), "error should mention coarse probe: {err}");
    }

    #[tokio::test]
    async fn exact_sample_count_allows_small_final_track() {
        let temp = tempfile::tempdir().expect("temp dir");
        // Same setup as above but with exact sample count — should succeed
        let cue = r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 04:00:00
"#;
        // 10,584,000 + 500 = 10,584,500 samples (tiny final track, but exact)
        let probe = ffprobe_json_exact(44100, 10_584_500, 16);
        let source = materialize_cue(cue, &probe, &temp)
            .await
            .expect("exact sample count allows small final track");

        assert_eq!(source.tracks.len(), 2);
        let TrackSourceRef::ImageSegment { samples, .. } = &source.tracks[1].source_ref else {
            panic!("expected ImageSegment");
        };
        assert_eq!(*samples, 500);
    }

    #[test]
    fn cue_metadata_uses_image_tags_only_for_cue_gaps() {
        let mut image = ImageAlbumMetadata::default();
        image.album = Some("Image Album".to_string());
        image.album_artist = Some("Image Album Artist".to_string());
        image.artist = Some("Image Artist".to_string());
        image.genre = Some("Image Genre".to_string());
        image.date = Some("1984".to_string());
        image.disc_number = Some(1);
        image.total_discs = Some(2);

        let sheet = parse_cue(
            "PERFORMER \"Cue Artist\"\nTITLE \"Cue Album\"\nFILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n",
        );

        let numbering = cue_track_number_plan(&sheet);
        let track = cue_track_metadata(&sheet.tracks[0], &sheet, &image, false, numbering[0]);
        let album = cue_album_metadata(&sheet, &image, sheet.tracks.len() as u32);

        assert_eq!(track.artist.as_deref(), Some("Cue Artist"));
        assert_eq!(track.album_artist.as_deref(), Some("Cue Artist"));
        assert_eq!(track.genre.as_deref(), Some("Image Genre"));
        assert_eq!(track.date.as_deref(), Some("1984"));
        assert_eq!(album.album.as_deref(), Some("Cue Album"));
        assert_eq!(album.album_artist.as_deref(), Some("Cue Artist"));
        assert_eq!(album.total_tracks, 1);
        assert_eq!(album.disc_number, Some(1));
        assert_eq!(album.total_discs, Some(2));
    }


    fn fixture_tool_available(tool: &str) -> bool {
        std::process::Command::new(tool)
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn run_fixture_command(command: &mut std::process::Command) {
        let output = command.output().expect("spawn fixture command");
        assert!(
            output.status.success(),
            "fixture command failed: status={:?}\nstdout={}\nstderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn lofty_reads_real_flac_tags_and_embedded_cuesheet_fixture() {
        if !fixture_tool_available("ffmpeg") || !fixture_tool_available("metaflac") {
            eprintln!("skipping real FLAC/lofty fixture: ffmpeg or metaflac is unavailable");
            return;
        }

        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("lofty-image.flac");
        let cue_path = temp.path().join("embedded.cue");
        let cue = r#"PERFORMER "Embedded Cue Artist"
TITLE "Embedded Cue Album"
FILE "lofty-image.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Embedded Cue Track One"
    INDEX 01 00:00:00
"#;
        std::fs::write(&cue_path, cue).expect("write cue text");

        run_fixture_command(
            std::process::Command::new("ffmpeg")
                .arg("-y")
                .arg("-hide_banner")
                .arg("-loglevel")
                .arg("error")
                .arg("-f")
                .arg("lavfi")
                .arg("-i")
                .arg("sine=frequency=440:sample_rate=44100:duration=1")
                .arg("-c:a")
                .arg("flac")
                .arg(&image),
        );
        run_fixture_command(
            std::process::Command::new("metaflac")
                .arg("--set-tag=ALBUM=Lofty Image Album")
                .arg("--set-tag=ARTIST=Lofty Image Artist")
                .arg("--set-tag=ALBUMARTIST=Lofty Image Album Artist")
                .arg("--set-tag=DATE=2026")
                .arg("--set-tag=GENRE=Lofty Fixture")
                .arg("--set-tag=DISCNUMBER=1")
                .arg("--set-tag=TOTALDISCS=2")
                .arg(format!("--set-tag-from-file=CUESHEET={}", cue_path.display()))
                .arg(&image),
        );

        let image_metadata = read_image_album_metadata(&image);
        assert_eq!(image_metadata.album.as_deref(), Some("Lofty Image Album"));
        assert_eq!(image_metadata.artist.as_deref(), Some("Lofty Image Artist"));
        assert_eq!(
            image_metadata.album_artist.as_deref(),
            Some("Lofty Image Album Artist")
        );
        assert_eq!(image_metadata.date.as_deref(), Some("2026"));
        assert_eq!(image_metadata.genre.as_deref(), Some("Lofty Fixture"));
        assert_eq!(image_metadata.disc_number, Some(1));
        assert_eq!(image_metadata.total_discs, Some(2));

        let embedded = read_embedded_cuesheet(&image)
            .expect("lofty read should succeed")
            .expect("embedded CUESHEET should exist");
        assert!(embedded.contains("Embedded Cue Album"));
        assert!(embedded.contains("Embedded Cue Track One"));
    }

    // ── Category G: probe failure ──

    #[tokio::test]
    async fn ffprobe_failure_propagates_as_error() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue_path = temp.path().join("album.cue");
        let audio_path = temp.path().join("album.flac");
        write_file(&cue_path, cue_sheet_single_track().as_bytes());
        write_file(&audio_path, b"fake-audio-data");

        let runner = StubToolRunner::new();
        runner.push_failure("ffprobe: error reading input");

        let mut staging = test_staging(&temp);
        let cancel = CancellationToken::new();
        let req = test_request(&cue_path);
        let result = CueImageMaterializer
            .materialize(&req, &staging, &runner, None, &HashMap::new(), &cancel)
            .await;
        staging.disarm();

        assert!(result.is_err());
    }
}
