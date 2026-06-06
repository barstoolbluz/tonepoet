//! PR 8 - CUE image materializer.
//!
//! Parses CUE image layouts and stages each CUE track as a bounded 32-bit
//! signed PCM WAV file. The staged carrier is always `pcm_s32le`; the
//! `PreparedTrack::bit_depth` field remains the original probed source-image
//! bit depth so `target_bit_depth = Source` resolves to the source, not the
//! carrier. Downstream planning receives a typed `CueSegmentCarrier` source
//! fact and encodes the requested final target from that validated WAV instead
//! of re-encoding through an intermediate FLAC carrier.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STALE_CUE_SEGMENT_TMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::errors::{MaterializeError, SourceDetectError, ToolRunnerError};
use super::reporter::PipelineReporter;
use super::stages::Materializer;
use super::tool::{ToolBinary, ToolCommand, ToolRunner};
use super::types::*;
use crate::tui::cue_parser::{decode_cue_bytes_for_path, parse_cue, CueSheet};

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

#[derive(Debug, Clone)]
struct AudioProbe {
    sample_rate: u32,
    total_samples: u64,
    exact_samples: bool,
    bit_depth: Option<u32>,
    codec_name: Option<String>,
    format_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImageArtworkProbe {
    stream_index: u32,
    codec_name: String,
    mime_type: String,
    extension: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExtractedCueArtwork {
    path: PathBuf,
    mime_type: String,
    source_image: PathBuf,
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
        let mut image_artwork = HashMap::new();
        for image_path in &unique_images {
            if cancel.is_cancelled() {
                return Err(MaterializeError::Cancelled);
            }
            probes.insert(
                path_identity(image_path),
                probe_audio_image(image_path, runner, cancel).await?,
            );
            image_metadata.insert(path_identity(image_path), read_image_album_metadata(image_path));
            let artwork = if req.settings.metadata.preserve_artwork {
                extract_cue_image_artwork(image_path, staging, runner, cancel).await?
            } else {
                None
            };
            image_artwork.insert(path_identity(image_path), artwork);
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

        let selected_track_indices = selected_track_indices(
            cue_input.sheet.tracks.len(),
            &req.source.track_selection,
        )?;
        let mut tracks = Vec::with_capacity(selected_track_indices.len());
        for idx in selected_track_indices {
            if cancel.is_cancelled() {
                return Err(MaterializeError::Cancelled);
            }

            let cue_track = &cue_input.sheet.tracks[idx];
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
            let staged_path = staged_cue_segment_path(
                staging,
                ordinal,
                cue_track.number,
                track_number_plan[idx].output_number,
                start_sample,
                samples,
            );
            stage_cue_segment_as_s32_wav(
                image_path,
                start_sample,
                samples,
                probe.sample_rate,
                &staged_path,
                runner,
                cancel,
            )
            .await?;

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
                source_ref: TrackSourceRef::CueSegmentCarrier {
                    path: staged_path,
                    source_image: image_path.clone(),
                    start_sample,
                    samples,
                    carrier: CueSegmentCarrier::PcmS32LeWav,
                },
                metadata,
                expected_samples: Some(samples),
                sample_rate: probe.sample_rate,
                bit_depth: probe.bit_depth,
            });
        }

        let mut album_metadata = cue_album_metadata(&cue_input.sheet, &album_image_metadata, total_tracks);
        if let Some(artwork) = select_album_artwork(&track_images, &image_artwork) {
            album_metadata.extra.insert(
                CUE_ARTWORK_PATH_EXTRA_KEY.to_string(),
                artwork.path.display().to_string(),
            );
            album_metadata.extra.insert(
                CUE_ARTWORK_MIME_EXTRA_KEY.to_string(),
                artwork.mime_type.clone(),
            );
            album_metadata.extra.insert(
                CUE_ARTWORK_SOURCE_EXTRA_KEY.to_string(),
                artwork.source_image.display().to_string(),
            );
        }
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
            if let Some(cue) = try_resolve_embedded_cue(req)? {
                return Ok(cue);
            }
            match find_valid_sidecar_cue_for_image(&req.container)? {
                Some(cue_path) => read_sidecar_cue(req, cue_path),
                None => Err(MaterializeError::Parse(
                    "no embedded CUESHEET and no sidecar CUE found".to_string(),
                )),
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
    let raw_cue = read_cue_text(&cue_path)?;
    let sheet = parse_cue(&raw_cue);
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
    try_resolve_embedded_cue(req)?.ok_or_else(|| {
        MaterializeError::Parse(
            "EmbeddedOnly requested but no embedded CUESHEET tag was found".to_string(),
        )
    })
}

fn try_resolve_embedded_cue(req: &PipelineRequest) -> Result<Option<CueInput>, MaterializeError> {
    let Some(raw_cue) = read_embedded_cuesheet(&req.container)? else {
        return Ok(None);
    };
    let sheet = parse_cue(&raw_cue);
    validate_embedded_single_image_layout(&sheet)?;

    Ok(Some(CueInput {
        sheet,
        raw_cue,
        origin: CueOrigin::Embedded,
        cue_parent: None,
        fallback_image: Some(req.container.clone()),
    }))
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
                return Ok(embedded_cuesheet_is_present(&req.container));
            }
            Ok(false)
        }
        CueSidecarPolicy::PreferEmbedded => {
            if embedded_cuesheet_is_present(&req.container) {
                return Ok(true);
            }
            Ok(sidecar_cue_route_candidate(&req.container)?.is_some())
        }
        CueSidecarPolicy::EmbeddedOnly => Ok(embedded_cuesheet_is_present(&req.container)),
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
        1 => {
            if sidecar_cue_is_usable_for_image(&same_stem[0], image)? {
                return Ok(Some(same_stem[0].clone()));
            }
        }
        _ => {
            return Err(SourceDetectError::AmbiguousCue(format!(
                "multiple same-stem CUE files found beside {}",
                image.display()
            )));
        }
    }

    let mut matching = Vec::new();
    for cue_path in candidates {
        if sidecar_cue_is_usable_for_image(&cue_path, image)? {
            matching.push(cue_path);
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

fn sidecar_cue_is_usable_for_image(
    cue_path: &Path,
    image: &Path,
) -> Result<bool, SourceDetectError> {
    let raw = std::fs::read(cue_path)?;
    let content = match decode_cue_bytes_for_path(&raw, cue_path) {
        Ok(content) => content,
        Err(_) => return Ok(false),
    };
    let sheet = parse_cue(&content);
    if validate_sidecar_layout_detect(&sheet).is_err() {
        return Ok(false);
    }

    let cue_input = CueInput {
        sheet,
        raw_cue: content,
        origin: CueOrigin::Sidecar,
        cue_parent: cue_path.parent().map(Path::to_path_buf),
        fallback_image: Some(image.to_path_buf()),
    };

    let Ok(track_images) = resolve_track_image_paths(&cue_input) else {
        return Ok(false);
    };

    Ok(track_images
        .iter()
        .any(|resolved| same_existing_path(resolved, image)))
}

fn embedded_cuesheet_is_present(path: &Path) -> bool {
    matches!(read_embedded_cuesheet(path), Ok(Some(_)))
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
    let raw_cue = read_cue_text(cue_path)?;
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
    cues.sort_by_key(|path| deterministic_path_sort_key(path));
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

fn read_cue_text(path: &Path) -> Result<String, MaterializeError> {
    let raw = std::fs::read(path)?;
    decode_cue_bytes_for_path(&raw, path).map_err(|message| {
        MaterializeError::Parse(format!(
            "failed to decode CUE file {}: {}",
            path.display(),
            message
        ))
    })
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

    // When materializing an audio image that already has an associated sidecar
    // CUE, the image itself is an explicit policy choice. It is therefore safe
    // to prefer it for extension-mismatch CUE references. A bare .cue input has
    // no fallback and must resolve stem-only matches uniquely below.
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

    let wanted_name = raw_path.file_name().and_then(|value| value.to_str());
    let wanted_stem = raw_path.file_stem().and_then(|value| value.to_str());
    let fallback_search_dir = audio_reference_fallback_search_dir(base, &raw_path);

    if let Some(wanted) = wanted_name {
        let name_matches = collect_audio_reference_candidates(&fallback_search_dir, |path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case(wanted))
                .unwrap_or(false)
        })?;
        match unique_audio_reference_candidate(file_ref, name_matches)? {
            Some(path) => return Ok(path),
            None => {}
        }
    }

    if let Some(wanted) = wanted_stem {
        let stem_matches = collect_audio_reference_candidates(&fallback_search_dir, |path| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case(wanted))
                .unwrap_or(false)
        })?;
        if let Some(path) = unique_audio_reference_candidate(file_ref, stem_matches)? {
            return Ok(path);
        }
    }

    Err(MaterializeError::Parse(format!(
        "CUE image file was not found: {file_ref}"
    )))
}


fn audio_reference_fallback_search_dir(base: &Path, raw_path: &Path) -> PathBuf {
    raw_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| base.join(parent))
        .unwrap_or_else(|| base.to_path_buf())
}

fn collect_audio_reference_candidates(
    base: &Path,
    matches_reference: impl Fn(&Path) -> bool,
) -> Result<Vec<PathBuf>, MaterializeError> {
    let entries = match std::fs::read_dir(base) {
        Ok(entries) => entries,
        Err(err) if matches!(err.kind(), std::io::ErrorKind::NotFound) => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };

    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && has_audio_extension(&path) && matches_reference(&path) {
            candidates.push(path);
        }
    }
    candidates.sort_by_key(|path| deterministic_path_sort_key(path));
    candidates.dedup_by(|left, right| same_existing_path(left, right));
    Ok(candidates)
}

fn unique_audio_reference_candidate(
    file_ref: &str,
    candidates: Vec<PathBuf>,
) -> Result<Option<PathBuf>, MaterializeError> {
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.into_iter().next()),
        _ => Err(MaterializeError::Parse(format!(
            "ambiguous CUE image file reference {file_ref:?}; candidates: {}",
            format_candidate_paths_for_error(&candidates)
        ))),
    }
}

fn deterministic_path_sort_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn format_candidate_paths_for_error(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
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
            "stream=codec_name,sample_rate,duration_ts,time_base,duration,bits_per_raw_sample,bits_per_sample"
                .into(),
            "-show_entries".into(),
            "format=format_name,duration".into(),
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


async fn extract_cue_image_artwork(
    image: &Path,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<Option<ExtractedCueArtwork>, MaterializeError> {
    let Some(probe) = probe_image_artwork(image, runner, cancel).await? else {
        return Ok(None);
    };

    let destination = staged_cue_artwork_path(staging, image, probe.stream_index, probe.extension);
    if destination.metadata().map(|meta| meta.is_file() && meta.len() > 0).unwrap_or(false) {
        return Ok(Some(ExtractedCueArtwork {
            path: destination,
            mime_type: probe.mime_type,
            source_image: image.to_path_buf(),
        }));
    }

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_destination = temporary_artwork_path(&destination)?;
    if let Some(parent) = tmp_destination.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_path_if_exists(&tmp_destination)?;

    let cmd = cue_artwork_extract_command(image, &probe, &tmp_destination);
    let run_result = runner.run(cmd, cancel).await;
    match run_result {
        Ok(_) => {
            let meta = tmp_destination.metadata().map_err(|err| {
                MaterializeError::Parse(format!(
                    "extracted CUE artwork {} is not readable: {err}",
                    tmp_destination.display()
                ))
            })?;
            if !meta.is_file() || meta.len() == 0 {
                let _ = remove_path_if_exists(&tmp_destination);
                return Err(MaterializeError::Parse(format!(
                    "extracted CUE artwork {} is empty",
                    tmp_destination.display()
                )));
            }
            sync_file_to_storage(&tmp_destination)?;
            if destination.exists() {
                let _ = remove_path_if_exists(&destination);
            }
            fs::rename(&tmp_destination, &destination)?;
            sync_parent_dir_best_effort(&destination);
            Ok(Some(ExtractedCueArtwork {
                path: destination,
                mime_type: probe.mime_type,
                source_image: image.to_path_buf(),
            }))
        }
        Err(ToolRunnerError::Cancelled { .. }) => {
            let _ = remove_path_if_exists(&tmp_destination);
            Err(MaterializeError::Cancelled)
        }
        Err(err) => {
            let _ = remove_path_if_exists(&tmp_destination);
            Err(err.into())
        }
    }
}

async fn probe_image_artwork(
    path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<Option<ImageArtworkProbe>, MaterializeError> {
    let cmd = ToolCommand {
        binary: ToolBinary::Ffprobe,
        args: vec![
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            "v".into(),
            "-show_entries".into(),
            "stream=index,codec_name:stream_disposition=attached_pic:stream_tags=mimetype".into(),
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

    parse_image_artwork_probe_json(&output.stdout_tail)
}

fn parse_image_artwork_probe_json(json_str: &str) -> Result<Option<ImageArtworkProbe>, MaterializeError> {
    let value: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|err| MaterializeError::Parse(format!("ffprobe artwork JSON parse failed: {err}")))?;
    let Some(streams) = value.get("streams").and_then(|value| value.as_array()) else {
        return Ok(None);
    };

    for stream in streams {
        let attached_pic = stream
            .pointer("/disposition/attached_pic")
            .and_then(|value| value.as_i64())
            .unwrap_or(0)
            == 1;
        let codec_name = stream
            .get("codec_name")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !attached_pic {
            continue;
        }
        let supported = artwork_codec_to_mime_ext(&codec_name);
        let Some((mime_type, extension)) = supported else {
            log::warn!(
                "CUE image contains attached artwork codec {:?}, but this path currently supports only PNG and JPEG artwork extraction",
                codec_name
            );
            return Ok(None);
        };
        let Some(stream_index) = stream.get("index").and_then(json_u32_from_value) else {
            continue;
        };
        let mime_type = stream
            .pointer("/tags/mimetype")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(mime_type)
            .to_string();
        return Ok(Some(ImageArtworkProbe {
            stream_index,
            codec_name,
            mime_type,
            extension,
        }));
    }

    Ok(None)
}

fn artwork_codec_to_mime_ext(codec_name: &str) -> Option<(&'static str, &'static str)> {
    match codec_name {
        "mjpeg" | "jpeg" | "jpg" => Some(("image/jpeg", "jpg")),
        "png" => Some(("image/png", "png")),
        _ => None,
    }
}

fn cue_artwork_extract_command(
    image: &Path,
    probe: &ImageArtworkProbe,
    destination: &Path,
) -> ToolCommand {
    ToolCommand {
        binary: ToolBinary::Ffmpeg,
        args: vec![
            "-v".into(),
            "error".into(),
            "-hide_banner".into(),
            "-nostdin".into(),
            "-y".into(),
            "-i".into(),
            image.display().to_string(),
            "-map".into(),
            format!("0:{}", probe.stream_index),
            "-an".into(),
            "-sn".into(),
            "-dn".into(),
            "-c:v".into(),
            "copy".into(),
            "-frames:v".into(),
            "1".into(),
            "-f".into(),
            "image2".into(),
            destination.display().to_string(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(2 * 60),
    }
}

fn staged_cue_artwork_path(
    staging: &StagingDir,
    image: &Path,
    stream_index: u32,
    extension: &str,
) -> PathBuf {
    let stem = image
        .file_stem()
        .and_then(|value| value.to_str())
        .map(sanitize_artwork_component)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "cue-image".to_string());
    let hash = deterministic_artwork_hash(&image.display().to_string());
    staging.root.join("cue-artwork").join(format!(
        "{stem}-{hash:016x}-stream{stream_index}.{extension}"
    ))
}

fn sanitize_artwork_component(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_ascii_lowercase()
}

fn deterministic_artwork_hash(value: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn select_album_artwork(
    track_images: &[PathBuf],
    artwork_by_image: &HashMap<PathBuf, Option<ExtractedCueArtwork>>,
) -> Option<ExtractedCueArtwork> {
    for image_path in track_images {
        let Some(Some(artwork)) = artwork_by_image.get(&path_identity(image_path)) else {
            continue;
        };
        return Some(artwork.clone());
    }
    None
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
    let codec_name = stream
        .get("codec_name")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let format_name = value
        .pointer("/format/format_name")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());

    if let Some(duration_ts_samples) = samples_from_duration_ts(stream, sample_rate) {
        if duration_ts_samples > 0 {
            return Ok(AudioProbe {
                sample_rate,
                total_samples: duration_ts_samples,
                exact_samples: true,
                bit_depth,
                codec_name,
                format_name,
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
        codec_name,
        format_name,
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

fn staged_cue_segment_path(
    staging: &StagingDir,
    source_ordinal: u32,
    cue_track_number: u32,
    output_track_number: u32,
    start_sample: u64,
    samples: u64,
) -> PathBuf {
    staging.root.join("cue-segments").join(format!(
        "{source_ordinal:03}-cue{cue_track_number:02}-track{output_track_number:02}-s{start_sample}-n{samples}.wav"
    ))
}

async fn stage_cue_segment_as_s32_wav(
    image: &Path,
    start_sample: u64,
    samples: u64,
    sample_rate: u32,
    destination: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(), MaterializeError> {
    if destination.exists() {
        match validate_staged_cue_segment(destination, sample_rate, samples, runner, cancel).await {
            Ok(()) => return Ok(()),
            Err(MaterializeError::Cancelled) => return Err(MaterializeError::Cancelled),
            Err(_) => {
                // Do not remove the published path yet. A stale or partial file from a
                // previous interrupted run is not trusted, but keeping it in place until a
                // fully validated replacement is ready avoids a publish gap for any
                // accidental concurrent observer of the private staging directory.
            }
        }
    }

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }

    let tmp_destination = temporary_segment_path(destination)?;
    if let Some(parent) = tmp_destination.parent() {
        fs::create_dir_all(parent)?;
    }
    cleanup_old_temporary_segments(destination);
    remove_path_if_exists(&tmp_destination)?;

    let cmd = cue_segment_ffmpeg_command(image, start_sample, samples, &tmp_destination)?;

    let run_result = runner.run(cmd, cancel).await;
    match run_result {
        Ok(_) => {
            if let Err(err) = validate_staged_cue_segment(
                &tmp_destination,
                sample_rate,
                samples,
                runner,
                cancel,
            )
            .await
            {
                let _ = remove_path_if_exists(&tmp_destination);
                return Err(err);
            }

            sync_file_to_storage(&tmp_destination)?;

            if destination.exists() {
                match validate_staged_cue_segment(destination, sample_rate, samples, runner, cancel)
                    .await
                {
                    Ok(()) => {
                        let _ = remove_path_if_exists(&tmp_destination);
                        return Ok(());
                    }
                    Err(MaterializeError::Cancelled) => {
                        let _ = remove_path_if_exists(&tmp_destination);
                        return Err(MaterializeError::Cancelled);
                    }
                    Err(_) => {}
                }
            }

            if let Err(err) = publish_validated_staged_segment(
                &tmp_destination,
                destination,
                sample_rate,
                samples,
                runner,
                cancel,
            )
            .await
            {
                let _ = remove_path_if_exists(&tmp_destination);
                return Err(err);
            }
            Ok(())
        }
        Err(ToolRunnerError::Cancelled { .. }) => {
            let _ = remove_path_if_exists(&tmp_destination);
            Err(MaterializeError::Cancelled)
        }
        Err(err) => {
            let _ = remove_path_if_exists(&tmp_destination);
            Err(err.into())
        }
    }
}

async fn validate_staged_cue_segment(
    path: &Path,
    expected_sample_rate: u32,
    expected_samples: u64,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(), MaterializeError> {
    let metadata = path.metadata().map_err(|err| {
        MaterializeError::Parse(format!(
            "staged CUE segment {} is not readable: {err}",
            path.display()
        ))
    })?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(MaterializeError::Parse(format!(
            "staged CUE segment {} is missing or empty",
            path.display()
        )));
    }

    let probe = probe_staged_cue_segment(path, runner, cancel).await?;
    if probe.sample_rate != expected_sample_rate {
        return Err(MaterializeError::Parse(format!(
            "staged CUE segment {} has sample_rate {}, expected {}",
            path.display(),
            probe.sample_rate,
            expected_sample_rate
        )));
    }
    if !probe.exact_samples {
        return Err(MaterializeError::Parse(format!(
            "staged CUE segment {} did not report an exact sample count",
            path.display()
        )));
    }
    if probe.total_samples != expected_samples {
        return Err(MaterializeError::Parse(format!(
            "staged CUE segment {} has {} samples, expected {}",
            path.display(),
            probe.total_samples,
            expected_samples
        )));
    }
    if probe.codec_name.as_deref() != Some("pcm_s32le") {
        return Err(MaterializeError::Parse(format!(
            "staged CUE segment {} has codec {:?}, expected pcm_s32le",
            path.display(),
            probe.codec_name
        )));
    }
    if !probe
        .format_name
        .as_deref()
        .map(format_name_contains_wav)
        .unwrap_or(false)
    {
        return Err(MaterializeError::Parse(format!(
            "staged CUE segment {} has container {:?}, expected wav",
            path.display(),
            probe.format_name
        )));
    }

    Ok(())
}

async fn probe_staged_cue_segment(
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
            "-show_entries".into(),
            "stream=codec_name,sample_rate,duration_ts,time_base,duration,bits_per_raw_sample,bits_per_sample"
                .into(),
            "-show_entries".into(),
            "format=format_name,duration".into(),
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

fn format_name_contains_wav(format_name: &str) -> bool {
    format_name
        .split(',')
        .any(|name| name.trim().eq_ignore_ascii_case("wav"))
}

fn temporary_segment_path(destination: &Path) -> Result<PathBuf, MaterializeError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let random = random_temp_suffix()?;
    let file_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("cue-segment.wav");
    let tmp_dir = destination
        .parent()
        .map(|parent| parent.join(".tmp"))
        .unwrap_or_else(|| PathBuf::from(".tmp"));
    Ok(tmp_dir.join(format!("{file_name}.tmp.{pid}.{stamp}.{random}")))
}

fn temporary_artwork_path(destination: &Path) -> Result<PathBuf, MaterializeError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let random = random_temp_suffix()?;
    let stem = destination
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("cue-artwork");
    let extension = destination
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("img");
    let tmp_dir = destination
        .parent()
        .map(|parent| parent.join(".tmp"))
        .unwrap_or_else(|| PathBuf::from(".tmp"));
    Ok(tmp_dir.join(format!(
        "{stem}.tmp.{pid}.{stamp}.{random}.{extension}"
    )))
}

fn random_temp_suffix() -> Result<String, MaterializeError> {
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|err| {
        MaterializeError::Parse(format!(
            "failed to generate random temporary CUE segment suffix: {err}"
        ))
    })?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn cleanup_old_temporary_segments(destination: &Path) {
    let tmp_dir = destination
        .parent()
        .map(|parent| parent.join(".tmp"))
        .unwrap_or_else(|| PathBuf::from(".tmp"));
    cleanup_old_temporary_segments_in_dir(&tmp_dir, SystemTime::now(), STALE_CUE_SEGMENT_TMP_MAX_AGE);
}

fn cleanup_old_temporary_segments_in_dir(tmp_dir: &Path, now: SystemTime, max_age: Duration) {
    let entries = match fs::read_dir(tmp_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = match path.file_name().and_then(|value| value.to_str()) {
            Some(name) => name,
            None => continue,
        };
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(_) => continue,
        };
        if should_remove_old_temporary_segment(name, modified, now, max_age) {
            let _ = fs::remove_file(path);
        }
    }
}

fn should_remove_old_temporary_segment(
    file_name: &str,
    modified: SystemTime,
    now: SystemTime,
    max_age: Duration,
) -> bool {
    if !is_staged_segment_temporary_file_name(file_name) {
        return false;
    }
    now.duration_since(modified)
        .map(|age| age > max_age)
        .unwrap_or(false)
}

fn is_staged_segment_temporary_file_name(file_name: &str) -> bool {
    let Some((base_name, suffix)) = file_name.rsplit_once(".tmp.") else {
        return false;
    };
    if base_name.is_empty() || !base_name.to_ascii_lowercase().ends_with(".wav") {
        return false;
    }

    let mut parts = suffix.split('.');
    let pid = parts.next();
    let timestamp = parts.next();
    let random = parts.next();
    if parts.next().is_some() {
        return false;
    }

    let Some(pid) = pid else {
        return false;
    };
    let Some(timestamp) = timestamp else {
        return false;
    };
    let Some(random) = random else {
        return false;
    };

    !pid.is_empty()
        && pid.chars().all(|ch| ch.is_ascii_digit())
        && !timestamp.is_empty()
        && timestamp.chars().all(|ch| ch.is_ascii_digit())
        && random.len() == 32
        && random.chars().all(|ch| ch.is_ascii_hexdigit())
}

async fn publish_validated_staged_segment(
    tmp_destination: &Path,
    destination: &Path,
    expected_sample_rate: u32,
    expected_samples: u64,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(), MaterializeError> {
    match fs::rename(tmp_destination, destination) {
        Ok(()) => {
            sync_parent_dir_best_effort(destination);
            validate_staged_cue_segment(
                destination,
                expected_sample_rate,
                expected_samples,
                runner,
                cancel,
            )
            .await?;
            Ok(())
        }
        Err(first_err) if destination.exists() => {
            // POSIX rename replaces an existing path atomically. Some platforms, notably
            // Windows through std::fs::rename, do not replace an existing destination. The
            // std-only fallback has an unavoidable remove+rename window, so before deleting
            // anything, re-check whether a concurrent worker has already published a valid
            // segment. If so, discard our temp and reuse the published file.
            match validate_staged_cue_segment(
                destination,
                expected_sample_rate,
                expected_samples,
                runner,
                cancel,
            )
            .await
            {
                Ok(()) => {
                    let _ = remove_path_if_exists(tmp_destination);
                    return Ok(());
                }
                Err(MaterializeError::Cancelled) => return Err(MaterializeError::Cancelled),
                Err(_) => {}
            }

            // The destination is still missing or invalid after the last-moment validation.
            // Remove only after our replacement has already been validated and fsynced.
            remove_path_if_exists(destination)?;
            match fs::rename(tmp_destination, destination) {
                Ok(()) => {}
                Err(second_err) if destination.exists() => {
                    // Another concurrent worker may have published in the tiny interval
                    // after our delete/rename attempt. Prefer a valid destination over
                    // failing or overwriting it.
                    match validate_staged_cue_segment(
                        destination,
                        expected_sample_rate,
                        expected_samples,
                        runner,
                        cancel,
                    )
                    .await
                    {
                        Ok(()) => {
                            let _ = remove_path_if_exists(tmp_destination);
                            return Ok(());
                        }
                        Err(MaterializeError::Cancelled) => {
                            return Err(MaterializeError::Cancelled);
                        }
                        Err(_) => {
                            return Err(MaterializeError::Parse(format!(
                                "failed to publish validated staged CUE segment {} over {}: first rename failed: {}; second rename failed: {}",
                                tmp_destination.display(),
                                destination.display(),
                                first_err,
                                second_err
                            )));
                        }
                    }
                }
                Err(second_err) => {
                    return Err(MaterializeError::Parse(format!(
                        "failed to publish validated staged CUE segment {} over {}: first rename failed: {}; second rename failed: {}",
                        tmp_destination.display(),
                        destination.display(),
                        first_err,
                        second_err
                    )));
                }
            }
            sync_parent_dir_best_effort(destination);
            validate_staged_cue_segment(
                destination,
                expected_sample_rate,
                expected_samples,
                runner,
                cancel,
            )
            .await?;
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

fn remove_path_if_exists(path: &Path) -> Result<(), MaterializeError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}


fn sync_file_to_storage(path: &Path) -> Result<(), MaterializeError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn sync_parent_dir_best_effort(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

fn cue_segment_ffmpeg_command(
    image: &Path,
    start_sample: u64,
    samples: u64,
    destination: &Path,
) -> Result<ToolCommand, MaterializeError> {
    let filter = cue_segment_atrim_filter(start_sample, samples)?;
    Ok(ToolCommand {
        binary: ToolBinary::Ffmpeg,
        args: vec![
            "-v".into(),
            "error".into(),
            "-hide_banner".into(),
            "-nostdin".into(),
            "-y".into(),
            "-i".into(),
            image.display().to_string(),
            "-map".into(),
            "0:a:0".into(),
            "-vn".into(),
            "-sn".into(),
            "-dn".into(),
            "-af".into(),
            filter,
            "-f".into(),
            "wav".into(),
            "-c:a".into(),
            "pcm_s32le".into(),
            destination.display().to_string(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(15 * 60),
    })
}

fn cue_segment_atrim_filter(start_sample: u64, samples: u64) -> Result<String, MaterializeError> {
    let end_sample = start_sample.checked_add(samples).ok_or_else(|| {
        MaterializeError::Parse("CUE segment sample range overflowed u64".to_string())
    })?;
    if samples == 0 {
        return Err(MaterializeError::Parse(
            "CUE segment has zero audio samples".to_string(),
        ));
    }
    Ok(format!(
        "atrim=start_sample={start_sample}:end_sample={end_sample},asetpts=N/SR/TB"
    ))
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

fn selected_track_indices(
    track_count: usize,
    selection: &TrackSelection,
) -> Result<Vec<usize>, MaterializeError> {
    match selection {
        TrackSelection::All => Ok((0..track_count).collect()),
        TrackSelection::Range { start, end } => {
            if *start == 0 || *end == 0 || start > end {
                return Err(MaterializeError::InvalidTrackSelection(format!(
                    "invalid range {start}-{end}"
                )));
            }

            let max_ordinal = track_count as u32;
            if *start > max_ordinal {
                return Err(MaterializeError::InvalidTrackSelection(format!(
                    "range start {start} exceeds track count {max_ordinal}"
                )));
            }

            let end = (*end).min(max_ordinal);
            Ok((*start..=end)
                .map(|ordinal| (ordinal - 1) as usize)
                .collect())
        }
        TrackSelection::Set(indices) => {
            if indices.is_empty() {
                return Err(MaterializeError::InvalidTrackSelection(
                    "empty track set".to_string(),
                ));
            }

            let max_ordinal = track_count as u32;
            for &idx in indices {
                if idx == 0 || idx > max_ordinal {
                    return Err(MaterializeError::InvalidTrackSelection(format!(
                        "track {idx} outside valid range 1-{max_ordinal}"
                    )));
                }
            }

            Ok(indices.iter().map(|idx| (*idx - 1) as usize).collect())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CueAnnotationScope {
    Album,
    AudioTrack(u32),
    IgnoredTrack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CueAnnotationTrackHeader {
    Audio(u32),
    NonAudioOrMalformed,
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
        let mut scope = CueAnnotationScope::Album;

        for (idx, line) in raw.lines().enumerate() {
            let line = if idx == 0 {
                line.trim_start_matches('\u{FEFF}')
            } else {
                line
            };
            let trimmed = line.trim();
            if let Some(header) = parse_annotation_track_header(trimmed) {
                scope = match header {
                    CueAnnotationTrackHeader::Audio(track_no) => {
                        CueAnnotationScope::AudioTrack(track_no)
                    }
                    CueAnnotationTrackHeader::NonAudioOrMalformed => CueAnnotationScope::IgnoredTrack,
                };
                continue;
            }
            if keyword_rest_ci(trimmed, "FLAGS").is_some()
                && trimmed
                    .split_whitespace()
                    .any(|token| token.eq_ignore_ascii_case("PRE"))
            {
                if let CueAnnotationScope::AudioTrack(track_no) = scope {
                    annotations.pre_emphasis.push(track_no);
                }
                continue;
            }
            if let Some((key, value)) = parse_rem_line(trimmed) {
                let key = format!("rem_{}", key.to_ascii_lowercase());
                if matches!(key.as_str(), "rem_date" | "rem_year" | "rem_genre") {
                    continue;
                }
                match scope {
                    CueAnnotationScope::Album => {
                        annotations.album_extra.insert(key, value);
                    }
                    CueAnnotationScope::AudioTrack(track_no) => {
                        annotations
                            .track_extra
                            .entry(track_no)
                            .or_default()
                            .insert(key, value);
                    }
                    CueAnnotationScope::IgnoredTrack => {}
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

fn parse_annotation_track_header(line: &str) -> Option<CueAnnotationTrackHeader> {
    let rest = keyword_rest_ci(line, "TRACK")?.trim_start();
    let mut parts = rest.split_whitespace();
    let number = match parts.next() {
        Some(value) => value,
        None => return Some(CueAnnotationTrackHeader::NonAudioOrMalformed),
    };
    let mode = match parts.next() {
        Some(value) => value,
        None => return Some(CueAnnotationTrackHeader::NonAudioOrMalformed),
    };
    if !mode.eq_ignore_ascii_case("AUDIO") {
        return Some(CueAnnotationTrackHeader::NonAudioOrMalformed);
    }
    Some(match number.parse() {
        Ok(track_no) => CueAnnotationTrackHeader::Audio(track_no),
        Err(_) => CueAnnotationTrackHeader::NonAudioOrMalformed,
    })
}

fn parse_rem_line(line: &str) -> Option<(String, String)> {
    let rest = keyword_rest_ci(line, "REM")?.trim_start();
    let (key, value) = rest.split_once(char::is_whitespace)?;
    let value = value.trim();
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key.to_string(), unquote(value).to_string()))
}

fn keyword_rest_ci<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    if line.len() < keyword.len() {
        return None;
    }
    let head = line.get(..keyword.len())?;
    let rest = line.get(keyword.len()..)?;
    if !head.eq_ignore_ascii_case(keyword) {
        return None;
    }
    if rest.is_empty() || rest.chars().next().map_or(false, char::is_whitespace) {
        Some(rest)
    } else {
        None
    }
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
    use async_trait::async_trait;
    use crate::convert::pipeline::tool::StubToolRunner;
    use crate::convert::pipeline::tool::ToolOutput;
    use std::collections::VecDeque;
    use std::sync::Mutex;

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

    fn ffprobe_json_staged_segment(sample_rate: u32, total_samples: u64) -> String {
        let time_base = format!("1/{sample_rate}");
        format!(
            r#"{{
  "streams": [{{
    "codec_name": "pcm_s32le",
    "sample_rate": "{sample_rate}",
    "duration_ts": {total_samples},
    "time_base": "{time_base}",
    "bits_per_raw_sample": "32"
  }}],
  "format": {{
    "format_name": "wav"
  }}
}}"#
        )
    }

    fn expected_ffprobe(
        path: impl Into<PathBuf>,
        stdout: impl Into<String>,
    ) -> ExpectedFfprobeOutput {
        ExpectedFfprobeOutput {
            target: ExpectedFfprobeTarget::Exact(path.into()),
            stdout: stdout.into(),
        }
    }

    fn expected_temp_ffprobe_for(
        final_path: impl Into<PathBuf>,
        stdout: impl Into<String>,
    ) -> ExpectedFfprobeOutput {
        ExpectedFfprobeOutput {
            target: ExpectedFfprobeTarget::TemporarySegmentFor(final_path.into()),
            stdout: stdout.into(),
        }
    }

    fn stub_runner_with_expected_probes(
        ffprobe_outputs: Vec<ExpectedFfprobeOutput>,
    ) -> SegmentWritingRunner {
        SegmentWritingRunner {
            ffprobe_stdout: Mutex::new(VecDeque::from(ffprobe_outputs)),
            ffmpeg_destinations: Mutex::new(Vec::new()),
        }
    }

    #[derive(Debug)]
    struct ExpectedFfprobeOutput {
        target: ExpectedFfprobeTarget,
        stdout: String,
    }

    impl ExpectedFfprobeOutput {
        fn assert_matches_path(&self, actual: &Path) {
            match &self.target {
                ExpectedFfprobeTarget::Exact(expected) => {
                    assert_eq!(
                        actual, expected,
                        "ffprobe path mismatch: expected {}, got {}",
                        expected.display(),
                        actual.display()
                    );
                }
                ExpectedFfprobeTarget::TemporarySegmentFor(final_path) => {
                    assert_temporary_probe_path_for_destination(actual, final_path);
                }
            }
        }
    }

    #[derive(Debug)]
    enum ExpectedFfprobeTarget {
        Exact(PathBuf),
        TemporarySegmentFor(PathBuf),
    }

    fn assert_temporary_probe_path_for_destination(actual: &Path, final_path: &Path) {
        let expected_tmp_dir = final_path
            .parent()
            .expect("staged destination has parent")
            .join(".tmp");
        assert_eq!(
            actual.parent(),
            Some(expected_tmp_dir.as_path()),
            "ffprobe temporary path should be under {} for final destination {}; got {}",
            expected_tmp_dir.display(),
            final_path.display(),
            actual.display()
        );

        let final_name = final_path
            .file_name()
            .and_then(|value| value.to_str())
            .expect("staged destination has UTF-8 file name");
        let actual_name = actual
            .file_name()
            .and_then(|value| value.to_str())
            .expect("temporary staged path has UTF-8 file name");
        let expected_prefix = format!("{final_name}.tmp.");
        assert!(
            actual_name.starts_with(&expected_prefix),
            "temporary staged probe path {} should start with {} for final destination {}",
            actual.display(),
            expected_prefix,
            final_path.display()
        );
        assert!(
            is_staged_segment_temporary_file_name(actual_name),
            "temporary staged probe path {} should match the staging temp naming pattern",
            actual.display()
        );
    }

    struct SegmentWritingRunner {
        ffprobe_stdout: Mutex<VecDeque<ExpectedFfprobeOutput>>,
        ffmpeg_destinations: Mutex<Vec<PathBuf>>,
    }

    #[async_trait]
    impl ToolRunner for SegmentWritingRunner {
        async fn run(
            &self,
            cmd: ToolCommand,
            _cancel: &CancellationToken,
        ) -> Result<ToolOutput, ToolRunnerError> {
            let is_ffmpeg = matches!(&cmd.binary, ToolBinary::Ffmpeg);
            let stdout_tail = if is_ffmpeg {
                let destination = cmd.args.last().expect("ffmpeg command has destination");
                self.ffmpeg_destinations
                    .lock()
                    .expect("ffmpeg destination log lock")
                    .push(PathBuf::from(destination));
                let bytes = if Path::new(destination)
                    .components()
                    .any(|component| component.as_os_str() == std::ffi::OsStr::new("cue-artwork"))
                {
                    b"JPEG-artwork".as_slice()
                } else {
                    b"RIFF-staged-cue-segment".as_slice()
                };
                std::fs::write(Path::new(destination), bytes);
                String::new()
            } else if cmd.args.windows(2).any(|pair| pair[0] == "-select_streams" && pair[1] == "v") {
                r#"{"streams":[]}"#.to_string()
            } else {
                let probed_path = PathBuf::from(cmd.args.last().expect("ffprobe command has path"));
                let expected = self
                    .ffprobe_stdout
                    .lock()
                    .expect("ffprobe output queue lock")
                    .pop_front()
                    .expect("queued ffprobe output");
                expected.assert_matches_path(&probed_path);
                expected.stdout
            };
            let command_binary = if is_ffmpeg {
                ToolBinary::Ffmpeg
            } else {
                ToolBinary::Ffprobe
            };

            Ok(ToolOutput {
                exit: crate::convert::pipeline::tool::ProcessExit::Code(0),
                stdout_tail,
                stderr_tail: String::new(),
                elapsed: Duration::from_millis(10),
                command: crate::convert::pipeline::tool::CommandRecord {
                    binary: command_binary,
                    sanitized_args: cmd.args.clone(),
                    cwd: None,
                    env_keys: vec![],
                    exit: Some(crate::convert::pipeline::tool::ProcessExit::Code(0)),
                    stdout_tail: String::new(),
                    stderr_tail: String::new(),
                    elapsed: Duration::from_millis(10),
                    description: None,
                },
            })
        }
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


    fn assert_cue_segment_carrier_ref(track: &PreparedTrack) -> &Path {
        let TrackSourceRef::CueSegmentCarrier { path, carrier, .. } = &track.source_ref else {
            panic!("expected typed CueSegmentCarrier so downstream planning does not infer CUE carrier semantics from a path convention");
        };
        assert_eq!(*carrier, CueSegmentCarrier::PcmS32LeWav);
        assert_eq!(path.extension().and_then(|value| value.to_str()), Some("wav"));
        path.as_path()
    }

    fn boundaries_for_cue(
        cue_content: &str,
        total_samples: u64,
        sample_rate: u32,
        exact_total: bool,
    ) -> Vec<(u64, u64)> {
        let sheet = parse_cue(cue_content);
        compute_track_boundaries(&sheet, total_samples, sample_rate, exact_total)
            .expect("boundary computation succeeds")
    }


    #[test]
    fn cue_annotations_ignore_non_audio_track_scope() {
        let annotations = CueAnnotations::parse(
            r#"REM COMMENT "album note"
TRACK 01 AUDIO
  FLAGS PRE
  REM COMMENT "audio note"
TRACK 02 MODE1/2352
  FLAGS PRE
  REM COMMENT "data note"
TRACK 03 AUDIO
  REM COMMENT "third note"
"#,
        );

        assert!(annotations.track_pre_emphasis(1));
        assert!(!annotations.track_pre_emphasis(2));
        assert!(!annotations.track_pre_emphasis(3));

        let mut album_extra = BTreeMap::new();
        annotations.add_album_extras(&mut album_extra);
        assert_eq!(
            album_extra.get("rem_comment"),
            Some(&"album note".to_string())
        );
        assert!(!album_extra.values().any(|value| value == "data note"));

        let mut track_one_extra = BTreeMap::new();
        annotations.add_track_extras(1, &mut track_one_extra);
        assert_eq!(
            track_one_extra.get("rem_comment"),
            Some(&"audio note".to_string())
        );

        let mut track_two_extra = BTreeMap::new();
        annotations.add_track_extras(2, &mut track_two_extra);
        assert!(track_two_extra.is_empty());

        let mut track_three_extra = BTreeMap::new();
        annotations.add_track_extras(3, &mut track_three_extra);
        assert_eq!(
            track_three_extra.get("rem_comment"),
            Some(&"third note".to_string())
        );
    }

    #[test]
    fn cue_annotations_malformed_track_header_clears_audio_scope() {
        let annotations = CueAnnotations::parse(
            r#"TRACK 01 AUDIO
  REM COMMENT "audio note"
TRACK XX AUDIO
  FLAGS PRE
  REM COMMENT "malformed note"
"#,
        );

        assert!(!annotations.track_pre_emphasis(1));

        let mut track_one_extra = BTreeMap::new();
        annotations.add_track_extras(1, &mut track_one_extra);
        assert_eq!(
            track_one_extra.get("rem_comment"),
            Some(&"audio note".to_string())
        );
        assert!(!track_one_extra.values().any(|value| value == "malformed note"));

        let mut album_extra = BTreeMap::new();
        annotations.add_album_extras(&mut album_extra);
        assert!(!album_extra.values().any(|value| value == "malformed note"));
    }

    struct ObservingSegmentRunner {
        ffprobe_stdout: Mutex<VecDeque<ExpectedFfprobeOutput>>,
        destination: PathBuf,
        observed_during_ffmpeg: Mutex<Option<Vec<u8>>>,
    }

    #[async_trait]
    impl ToolRunner for ObservingSegmentRunner {
        async fn run(
            &self,
            cmd: ToolCommand,
            _cancel: &CancellationToken,
        ) -> Result<ToolOutput, ToolRunnerError> {
            let is_ffmpeg = matches!(&cmd.binary, ToolBinary::Ffmpeg);
            let stdout_tail = if is_ffmpeg {
                let observed = std::fs::read(&self.destination).ok();
                *self
                    .observed_during_ffmpeg
                    .lock()
                    .expect("observed lock") = observed;
                let destination = cmd.args.last().expect("ffmpeg command has destination");
                std::fs::write(Path::new(destination), b"RIFF-staged-cue-segment");
                String::new()
            } else {
                let probed_path = PathBuf::from(cmd.args.last().expect("ffprobe command has path"));
                let expected = self
                    .ffprobe_stdout
                    .lock()
                    .expect("ffprobe output queue lock")
                    .pop_front()
                    .expect("queued ffprobe output");
                expected.assert_matches_path(&probed_path);
                expected.stdout
            };
            let command_binary = if is_ffmpeg {
                ToolBinary::Ffmpeg
            } else {
                ToolBinary::Ffprobe
            };

            Ok(ToolOutput {
                exit: crate::convert::pipeline::tool::ProcessExit::Code(0),
                stdout_tail,
                stderr_tail: String::new(),
                elapsed: Duration::from_millis(10),
                command: crate::convert::pipeline::tool::CommandRecord {
                    binary: command_binary,
                    sanitized_args: cmd.args.clone(),
                    cwd: None,
                    env_keys: vec![],
                    exit: Some(crate::convert::pipeline::tool::ProcessExit::Code(0)),
                    stdout_tail: String::new(),
                    stderr_tail: String::new(),
                    elapsed: Duration::from_millis(10),
                    description: None,
                },
            })
        }
    }

    #[test]
    fn cue_segment_command_uses_sample_exact_s32_pcm_staging() {
        let image = Path::new("album.flac");
        let destination = Path::new("staging/cue-segments/002-cue02-track02.wav");
        let cmd = cue_segment_ffmpeg_command(image, 3_969_000, 7_188_300, destination)
            .expect("build ffmpeg segment command");

        assert!(matches!(cmd.binary, ToolBinary::Ffmpeg));
        assert!(cmd.args.windows(2).any(|pair| {
            pair[0] == "-af"
                && pair[1]
                    == "atrim=start_sample=3969000:end_sample=11157300,asetpts=N/SR/TB"
        }));
        assert!(cmd.args.windows(2).any(|pair| pair[0] == "-c:a" && pair[1] == "pcm_s32le"));
        assert!(cmd.args.windows(2).any(|pair| pair[0] == "-f" && pair[1] == "wav"));
        assert!(!cmd.args.iter().any(|arg| arg == "-ss" || arg == "-t"));
    }

    #[test]
    fn cue_artwork_probe_and_extract_command_use_attached_picture_stream() {
        let probe = parse_image_artwork_probe_json(
            r#"{
              "streams": [{
                "index": 2,
                "codec_name": "mjpeg",
                "disposition": { "attached_pic": 1 },
                "tags": { "mimetype": "image/jpeg" }
              }]
            }"#,
        )
        .expect("artwork probe parses")
        .expect("attached picture stream detected");

        assert_eq!(probe.stream_index, 2);
        assert_eq!(probe.mime_type, "image/jpeg");
        assert_eq!(probe.extension, "jpg");

        let cmd = cue_artwork_extract_command(
            Path::new("album.flac"),
            &probe,
            Path::new("staging/cue-artwork/cover.jpg"),
        );
        assert!(matches!(cmd.binary, ToolBinary::Ffmpeg));
        assert!(cmd.args.windows(2).any(|pair| pair[0] == "-map" && pair[1] == "0:2"));
        assert!(cmd.args.windows(2).any(|pair| pair[0] == "-c:v" && pair[1] == "copy"));
        assert!(cmd.args.windows(2).any(|pair| pair[0] == "-frames:v" && pair[1] == "1"));
        assert!(cmd.args.windows(2).any(|pair| pair[0] == "-f" && pair[1] == "image2"));
        assert!(cmd.args.iter().any(|arg| arg == "-an"));
    }

    #[test]
    fn cue_artwork_temp_path_keeps_image_extension_for_ffmpeg_muxer_selection() {
        let destination = Path::new("staging/cue-artwork/cover.jpg");
        let temp = temporary_artwork_path(destination).expect("artwork temp path");

        assert_eq!(
            temp.parent().and_then(|path| path.file_name()).and_then(|name| name.to_str()),
            Some(".tmp")
        );
        assert_eq!(
            temp.extension().and_then(|value| value.to_str()),
            Some("jpg")
        );
        let name = temp
            .file_name()
            .and_then(|value| value.to_str())
            .expect("temp file name");
        assert!(
            name.starts_with("cover.tmp."),
            "artwork temp file should preserve the final stem before the temp suffix: {name}"
        );
        assert!(
            name.ends_with(".jpg"),
            "artwork temp file should preserve an image suffix FFmpeg can use: {name}"
        );
    }

    #[test]
    fn cue_segment_filter_rejects_zero_length_and_overflow() {
        assert!(cue_segment_atrim_filter(100, 0).is_err());
        assert!(cue_segment_atrim_filter(u64::MAX, 1).is_err());
    }

    #[test]
    fn temporary_segment_path_uses_private_tmp_dir_and_random_suffix() {
        let destination = Path::new("staging/cue-segments/001.wav");
        let first = temporary_segment_path(destination).expect("first temp path");
        let second = temporary_segment_path(destination).expect("second temp path");

        assert_eq!(
            first.parent().and_then(|path| path.file_name()).and_then(|name| name.to_str()),
            Some(".tmp")
        );
        for path in [first, second] {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .expect("temp file name");
            let random_suffix = name.rsplit('.').next().expect("random suffix");
            assert_eq!(random_suffix.len(), 32, "expected 128-bit hex random suffix");
            assert!(random_suffix.chars().all(|ch| ch.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn old_tmp_cleanup_only_targets_staging_temp_pattern_and_age() {
        let now = UNIX_EPOCH + Duration::from_secs(48 * 60 * 60);
        let old = UNIX_EPOCH;
        let recent = now - Duration::from_secs(60);
        let matching = "001-cue01-track01-s0-n44100.wav.tmp.123.456.0123456789abcdef0123456789abcdef";

        assert!(is_staged_segment_temporary_file_name(matching));
        assert!(should_remove_old_temporary_segment(
            matching,
            old,
            now,
            STALE_CUE_SEGMENT_TMP_MAX_AGE
        ));
        assert!(!should_remove_old_temporary_segment(
            matching,
            recent,
            now,
            STALE_CUE_SEGMENT_TMP_MAX_AGE
        ));
        assert!(!should_remove_old_temporary_segment(
            "001-cue01-track01.wav",
            old,
            now,
            STALE_CUE_SEGMENT_TMP_MAX_AGE
        ));
        assert!(!should_remove_old_temporary_segment(
            "001-cue01-track01-s0-n44100.wav.tmp.pid.456.0123456789abcdef0123456789abcdef",
            old,
            now,
            STALE_CUE_SEGMENT_TMP_MAX_AGE
        ));
        assert!(!should_remove_old_temporary_segment(
            "001-cue01-track01-s0-n44100.wav.tmp.123.456.not-random",
            old,
            now,
            STALE_CUE_SEGMENT_TMP_MAX_AGE
        ));
    }

    #[tokio::test]
    async fn existing_staged_segment_is_reused_only_after_probe_validation() {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("staging/cue-segments/001.wav");
        write_test_file(&destination, b"partial-wav");

        let stale_probe = ffprobe_json_staged_segment(44_100, 1);
        let valid_probe = ffprobe_json_staged_segment(44_100, 44_100);
        let runner = stub_runner_with_expected_probes(vec![
            expected_ffprobe(&destination, stale_probe.clone()),
            expected_temp_ffprobe_for(&destination, valid_probe.clone()),
            expected_ffprobe(&destination, stale_probe),
            expected_ffprobe(&destination, valid_probe),
        ]);
        let cancel = CancellationToken::new();

        stage_cue_segment_as_s32_wav(
            Path::new("album.flac"),
            0,
            44_100,
            44_100,
            &destination,
            &runner,
            &cancel,
        )
        .await
        .expect("invalid existing segment is regenerated");

        assert_eq!(
            std::fs::read(&destination).expect("read regenerated segment"),
            b"RIFF-staged-cue-segment"
        );
        let ffmpeg_destinations = runner
            .ffmpeg_destinations
            .lock()
            .expect("ffmpeg destination log lock");
        assert_eq!(ffmpeg_destinations.len(), 1);
        assert_eq!(
            ffmpeg_destinations[0]
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str()),
            Some(".tmp")
        );
        assert!(
            !ffmpeg_destinations[0].exists(),
            "temporary staging file should be unpublished after materialization"
        );
        assert!(
            runner
                .ffprobe_stdout
                .lock()
                .expect("ffprobe output queue lock")
                .is_empty(),
            "post-publish validation should consume the final destination probe"
        );
    }

    #[tokio::test]
    async fn invalid_published_segment_remains_until_replacement_is_ready() {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("staging/cue-segments/001.wav");
        write_test_file(&destination, b"old-partial-segment");

        let stale_probe = ffprobe_json_staged_segment(44_100, 1);
        let valid_probe = ffprobe_json_staged_segment(44_100, 44_100);
        let runner = ObservingSegmentRunner {
            ffprobe_stdout: Mutex::new(VecDeque::from(vec![
                expected_ffprobe(&destination, stale_probe.clone()),
                expected_temp_ffprobe_for(&destination, valid_probe.clone()),
                expected_ffprobe(&destination, stale_probe),
                expected_ffprobe(&destination, valid_probe),
            ])),
            destination: destination.clone(),
            observed_during_ffmpeg: Mutex::new(None),
        };
        let cancel = CancellationToken::new();

        stage_cue_segment_as_s32_wav(
            Path::new("album.flac"),
            0,
            44_100,
            44_100,
            &destination,
            &runner,
            &cancel,
        )
        .await
        .expect("invalid existing segment is regenerated");

        assert_eq!(
            runner
                .observed_during_ffmpeg
                .lock()
                .expect("observed lock")
                .as_deref(),
            Some(&b"old-partial-segment"[..]),
            "the old destination should not be removed before the replacement is validated"
        );
        assert_eq!(
            std::fs::read(&destination).expect("read regenerated segment"),
            b"RIFF-staged-cue-segment"
        );
        assert!(
            runner
                .ffprobe_stdout
                .lock()
                .expect("ffprobe output queue lock")
                .is_empty(),
            "post-publish validation should consume the final destination probe"
        );
    }

    #[tokio::test]
    async fn staged_segment_validation_rejects_wrong_codec() {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("staging/cue-segments/001.wav");
        write_test_file(&destination, b"RIFF-staged-cue-segment");

        let wrong_codec = ffprobe_json_staged_segment(44_100, 44_100)
            .replace("pcm_s32le", "pcm_s16le");
        let runner = stub_runner_with_expected_probes(vec![expected_ffprobe(&destination, wrong_codec)]);
        let cancel = CancellationToken::new();

        let result = validate_staged_cue_segment(
            &destination,
            44_100,
            44_100,
            &runner,
            &cancel,
        )
        .await;

        let err = result.expect_err("wrong codec is rejected").to_string();
        assert!(err.contains("pcm_s32le"), "error should mention codec: {err}");
    }

    async fn materialize_cue(
        cue_content: &str,
        probe_json: &str,
        temp: &tempfile::TempDir,
    ) -> Result<PreparedSource, MaterializeError> {
        materialize_cue_with_audio_files(cue_content, &[probe_json], &["album.flac"], temp).await
    }

    fn expected_probe_outputs_for_test(
        req: &PipelineRequest,
        staging: &StagingDir,
        probe_jsons: &[&str],
    ) -> Result<Vec<ExpectedFfprobeOutput>, String> {
        let cue_input = resolve_cue_input(req).map_err(|err| {
            format!(
                "failed to resolve test CUE input for {}: {err}",
                req.container.display()
            )
        })?;
        let track_images = resolve_track_image_paths(&cue_input).map_err(|err| {
            format!(
                "failed to resolve test CUE track image paths for {}: {err}",
                req.container.display()
            )
        })?;
        let unique_images = unique_existing_paths(&track_images);
        if unique_images.len() != probe_jsons.len() {
            return Err(format!(
                "test queued {} source ffprobe response(s), but resolved {} unique image path(s): {:?}",
                probe_jsons.len(),
                unique_images.len(),
                unique_images
            ));
        }

        let mut expected = Vec::new();
        let mut probes = HashMap::new();
        for (image_path, probe_json) in unique_images.iter().zip(probe_jsons.iter()) {
            expected.push(expected_ffprobe(image_path, (*probe_json).to_string()));
            let probe = parse_audio_probe_json(probe_json).map_err(|err| {
                format!(
                    "failed to parse queued source ffprobe JSON for {}: {err}",
                    image_path.display()
                )
            })?;
            probes.insert(path_identity(image_path), probe);
        }

        let boundaries = compute_track_boundaries_for_layout(
            &cue_input.sheet,
            &track_images,
            &probes,
        )
        .map_err(|err| {
            format!(
                "failed to compute expected staged CUE boundaries for {}: {err}",
                req.container.display()
            )
        })?;

        let track_number_plan = cue_track_number_plan(&cue_input.sheet);
        let selected_indices = selected_track_indices(
            cue_input.sheet.tracks.len(),
            &req.source.track_selection,
        )
        .map_err(|err| {
            format!(
                "failed to apply test track selection for {}: {err}",
                req.container.display()
            )
        })?;

        for idx in selected_indices {
            let cue_track = &cue_input.sheet.tracks[idx];
            let (start_sample, samples) = boundaries[idx];
            let staged_path = staged_cue_segment_path(
                staging,
                (idx + 1) as u32,
                cue_track.number,
                track_number_plan[idx].output_number,
                start_sample,
                samples,
            );
            let probe = probes
                .get(&path_identity(&track_images[idx]))
                .expect("track image probe exists");
            let staged_probe = ffprobe_json_staged_segment(probe.sample_rate, samples);
            // Each newly staged segment is probed twice: once while it is still
            // in cue-segments/.tmp/, then again after publish at the final path.
            // Queue both the expected response and the expected target path so tests
            // catch wrong-path probes as well as call-count mistakes.
            expected.push(expected_temp_ffprobe_for(&staged_path, staged_probe.clone()));
            expected.push(expected_ffprobe(&staged_path, staged_probe));
        }

        Ok(expected)
    }

    async fn materialize_cue_with_audio_files(
        cue_content: &str,
        probe_jsons: &[&str],
        audio_files: &[&str],
        temp: &tempfile::TempDir,
    ) -> Result<PreparedSource, MaterializeError> {
        let cue_path = write_cue_fixture(cue_content, audio_files, temp);
        let req = test_request(&cue_path);
        let staging = test_staging(temp);
        let expected_probes = expected_probe_outputs_for_test(&req, &staging, probe_jsons)
            .expect("test should be able to derive expected ffprobe calls from its valid CUE fixture");
        materialize_cue_with_expected_probes_for_request(req, staging, expected_probes).await
    }

    async fn materialize_cue_with_explicit_expected_probes(
        cue_content: &str,
        expected_probes: Vec<ExpectedFfprobeOutput>,
        audio_files: &[&str],
        temp: &tempfile::TempDir,
    ) -> Result<PreparedSource, MaterializeError> {
        let cue_path = write_cue_fixture(cue_content, audio_files, temp);
        let req = test_request(&cue_path);
        let staging = test_staging(temp);
        materialize_cue_with_expected_probes_for_request(req, staging, expected_probes).await
    }

    async fn materialize_cue_with_expected_probes_for_request(
        req: PipelineRequest,
        mut staging: StagingDir,
        expected_probes: Vec<ExpectedFfprobeOutput>,
    ) -> Result<PreparedSource, MaterializeError> {
        let runner = stub_runner_with_expected_probes(expected_probes);
        let cancel = CancellationToken::new();
        let result = CueImageMaterializer
            .materialize(&req, &staging, &runner, None, &HashMap::new(), &cancel)
            .await;
        staging.disarm();
        result
    }

    fn write_cue_fixture(
        cue_content: &str,
        audio_files: &[&str],
        temp: &tempfile::TempDir,
    ) -> PathBuf {
        let cue_path = temp.path().join("album.cue");
        write_test_file(&cue_path, cue_content.as_bytes());
        for audio_file in audio_files {
            write_test_file(&temp.path().join(audio_file), b"fake-audio-data");
        }
        cue_path
    }

    fn write_test_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create test fixture parent directory");
        }
        std::fs::write(path, contents).expect("write test fixture file");
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
        assert_cue_segment_carrier_ref(&source.tracks[0]);
        assert_cue_segment_carrier_ref(&source.tracks[1]);
        assert_cue_segment_carrier_ref(&source.tracks[2]);
        let boundaries = boundaries_for_cue(&cue_sheet_3_track(), total_samples, 44100, true);
        assert_eq!(boundaries[0], (0, 3_969_000));

        // Track 2: starts at 1:30:00 = 6750 frames = 6750 * 44100 / 75 = 3,969,000 samples
        assert_eq!(boundaries[1].0, 3_969_000);

        // Track 3: starts at 4:13:00 = 18975 frames = 18975 * 44100 / 75 = 11,157,300 samples
        assert_eq!(boundaries[2], (11_157_300, total_samples - 11_157_300));

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
        assert_cue_segment_carrier_ref(&source.tracks[0]);
        assert_eq!(source.tracks[0].expected_samples, Some(total_samples));
        assert_eq!(source.tracks[0].metadata.title.as_deref(), Some("Only Track"));
    }

    #[tokio::test]
    async fn track_selection_filters_before_staging_segments() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue_path = temp.path().join("album.cue");
        std::fs::write(&cue_path, cue_sheet_3_track().as_bytes());
        std::fs::write(&temp.path().join("album.flac"), b"fake-audio-data");

        let mut req = test_request(&cue_path);
        req.source.track_selection = TrackSelection::Set(std::collections::BTreeSet::from([2]));

        let total_samples: u64 = 26_460_000;
        let probe = ffprobe_json_exact(44100, total_samples, 16);
        let mut staging = test_staging(&temp);
        let expected_probes = expected_probe_outputs_for_test(&req, &staging, &[&probe])
            .expect("test should be able to derive expected ffprobe calls from selected CUE fixture");
        let runner = stub_runner_with_expected_probes(expected_probes);
        let cancel = CancellationToken::new();

        let source = CueImageMaterializer
            .materialize(&req, &staging, &runner, None, &HashMap::new(), &cancel)
            .await
            .expect("selected CUE track materializes");
        staging.disarm();

        assert_eq!(source.tracks.len(), 1);
        assert_eq!(source.tracks[0].id.source_ordinal, 2);
        assert_eq!(source.tracks[0].id.track_number, 2);
        assert_eq!(source.tracks[0].metadata.title.as_deref(), Some("Breathe"));

        let ffmpeg_destinations = runner
            .ffmpeg_destinations
            .lock()
            .expect("ffmpeg destination log lock");
        assert_eq!(
            ffmpeg_destinations.len(),
            1,
            "only the selected track should be staged"
        );
        assert!(
            ffmpeg_destinations[0]
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("002-cue02-track02-"))
                .unwrap_or(false),
            "selected track 2 should be the only staged segment: {:?}",
            ffmpeg_destinations[0]
        );
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

        // Verify contiguous, non-overlapping boundaries and staged file refs.
        let boundaries = boundaries_for_cue(&cue, total_samples, 44100, true);
        let mut prev_end: u64 = 0;
        for (track, (start_sample, samples)) in source.tracks.iter().zip(boundaries.iter()) {
            assert_cue_segment_carrier_ref(track);
            assert_eq!(*start_sample, prev_end, "track {} must start where previous ended", track.id.track_number);
            assert!(*samples > 0, "track {} must have positive length", track.id.track_number);
            assert_eq!(track.expected_samples, Some(*samples));
            prev_end = start_sample + samples;
        }
        assert_eq!(prev_end, total_samples);
    }

    // ── Category B: malformed CUE ──

    #[tokio::test]
    async fn empty_cue_sheet_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        let result = materialize_cue_with_explicit_expected_probes(
            "",
            Vec::new(),
            &["album.flac"],
            &temp,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cue_without_index_01_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue = r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "No Index"
"#;
        let result = materialize_cue_with_explicit_expected_probes(
            cue,
            Vec::new(),
            &["album.flac"],
            &temp,
        )
        .await;
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

        assert_cue_segment_carrier_ref(&source.tracks[0]);
        assert_eq!(source.tracks[0].expected_samples, Some(441_000));
        assert_eq!(source.tracks[0].metadata.title.as_deref(), Some("One"));

        assert_cue_segment_carrier_ref(&source.tracks[1]);
        assert_eq!(source.tracks[1].expected_samples, Some(882_000));
        assert_eq!(source.tracks[1].metadata.title.as_deref(), Some("Two"));
    }

    #[tokio::test]
    async fn multifile_pregap_file_switch_materializes_index01_from_new_file() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue = r#"FILE "02 - Trouble.wav" WAVE
  TRACK 02 AUDIO
    TITLE "Trouble No More"
    INDEX 01 00:00:00
  TRACK 03 AUDIO
    TITLE "Don't Keep Me Wonderin'"
    INDEX 00 03:43:37
FILE "03 - Wonderin.wav" WAVE
    INDEX 01 00:00:00
"#;
        let sheet = parse_cue(cue);
        assert_eq!(sheet.tracks[1].file.as_deref(), Some("03 - Wonderin.wav"));
        assert_eq!(sheet.tracks[1].index00_frames, Some(16_762));
        assert_eq!(sheet.tracks[1].index01_frames, Some(0));

        let trouble_probe = ffprobe_json_exact(44100, 10_000_000, 16);
        let wonderin_probe = ffprobe_json_exact(44100, 5_000_000, 16);
        let source = materialize_cue_with_audio_files(
            cue,
            &[&trouble_probe, &wonderin_probe],
            &["02 - Trouble.wav", "03 - Wonderin.wav"],
            &temp,
        )
        .await
        .expect("noncompliant multi-file pregap layout materializes");

        assert_eq!(source.tracks.len(), 2);
        assert_cue_segment_carrier_ref(&source.tracks[0]);
        assert_eq!(source.tracks[0].expected_samples, Some(10_000_000));
        assert_cue_segment_carrier_ref(&source.tracks[1]);
        assert_eq!(source.tracks[1].expected_samples, Some(5_000_000));
        assert_eq!(
            source.tracks[1]
                .metadata
                .extra
                .get("index00_frames")
                .map(String::as_str),
            Some("16762")
        );
    }

    #[test]
    fn resolve_audio_reference_errors_on_ambiguous_stem_matches() {
        let temp = tempfile::tempdir().expect("temp dir");
        let flac = temp.path().join("album.flac");
        let wav = temp.path().join("album.wav");
        std::fs::write(&flac, b"fake flac");
        std::fs::write(&wav, b"fake wav");

        let err = resolve_audio_reference(Some(temp.path()), "album.ape", None)
            .expect_err("ambiguous stem-only reference should fail");
        let msg = err.to_string();
        assert!(msg.contains("ambiguous"), "error should explain ambiguity: {msg}");
        assert!(msg.contains("album.flac"), "error should list flac candidate: {msg}");
        assert!(msg.contains("album.wav"), "error should list wav candidate: {msg}");
    }

    #[test]
    fn resolve_audio_reference_falls_back_inside_referenced_subdirectory() {
        let temp = tempfile::tempdir().expect("temp dir");
        let disc = temp.path().join("disc");
        std::fs::create_dir(&disc).expect("create disc dir");
        let image = disc.join("image.flac");
        std::fs::write(&image, b"fake flac");

        let resolved = resolve_audio_reference(Some(temp.path()), "disc/image.wav", None)
            .expect("extension mismatch should resolve inside referenced subdirectory");
        assert_eq!(resolved, image);
    }

    #[test]
    fn sidecar_discovery_matches_audio_inside_multiple_file_cue() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue_path = temp.path().join("album.cue");
        let track1 = temp.path().join("track1.flac");
        let track2 = temp.path().join("track2.flac");
        std::fs::write(
            &cue_path,
            br#"FILE "track1.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
FILE "track2.flac" WAVE
  TRACK 02 AUDIO
    INDEX 01 00:00:00
"#,
        );
        std::fs::write(&track1, b"fake-audio-data");
        std::fs::write(&track2, b"fake-audio-data");

        let discovered = find_valid_sidecar_cue_for_image(&track2)
            .expect("sidecar search succeeds")
            .expect("multi-file CUE matches referenced audio");
        assert_eq!(discovered, cue_path);
    }

    #[test]
    fn malformed_same_stem_sidecar_does_not_route_audio_to_cue_materializer() {
        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("album.flac");
        let cue_path = temp.path().join("album.cue");
        std::fs::write(&image, b"fake-audio-data");
        std::fs::write(
            &cue_path,
            br#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "No INDEX 01"
"#,
        );

        let req = test_request(&image);
        assert!(
            !is_cue_image_candidate(&req).expect("candidate detection succeeds"),
            "a malformed same-stem sidecar must not force a normal audio file onto the CUE route"
        );
    }

    #[test]
    fn same_stem_sidecar_must_reference_image_before_routing_audio_to_cue_materializer() {
        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("album.flac");
        let other = temp.path().join("other.flac");
        let cue_path = temp.path().join("album.cue");
        std::fs::write(&image, b"fake-audio-data");
        std::fs::write(&other, b"fake-audio-data");
        std::fs::write(
            &cue_path,
            br#"FILE "other.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        );

        let req = test_request(&image);
        assert!(
            !is_cue_image_candidate(&req).expect("candidate detection succeeds"),
            "a usable same-stem sidecar for a different image must not hijack this audio file"
        );
    }

    #[test]
    fn valid_same_stem_sidecar_routes_audio_to_cue_materializer() {
        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("album.flac");
        let cue_path = temp.path().join("album.cue");
        std::fs::write(&image, b"fake-audio-data");
        std::fs::write(
            &cue_path,
            br#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        );

        let req = test_request(&image);
        assert!(
            is_cue_image_candidate(&req).expect("candidate detection succeeds"),
            "a valid same-stem sidecar that references the image should select the CUE route"
        );
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
        let result = materialize_cue_with_explicit_expected_probes(
            cue,
            Vec::new(),
            &["album.flac"],
            &temp,
        )
        .await;

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
        let result = materialize_cue_with_explicit_expected_probes(
            cue,
            vec![expected_ffprobe(temp.path().join("album.flac"), probe)],
            &["album.flac"],
            &temp,
        )
        .await;
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
        assert_cue_segment_carrier_ref(&source.tracks[1]);
        let boundaries = boundaries_for_cue(cue, total_samples, 44100, true);
        assert_eq!(boundaries[1].0, 9_922_500);

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

        assert_cue_segment_carrier_ref(&source.tracks[0]);
        assert_eq!(source.tracks[0].expected_samples, Some(10_000_000));
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
        assert_cue_segment_carrier_ref(&source.tracks[1]);
        let boundaries = boundaries_for_cue(cue, total_samples, 96000, true);
        assert_eq!(boundaries[1].0, 5_760_000);
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
        assert_cue_segment_carrier_ref(&source.tracks[1]);
        let boundaries = boundaries_for_cue(cue, total_samples, 192000, true);
        assert_eq!(boundaries[1].0, 23_040_000);
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
        let result = materialize_cue_with_explicit_expected_probes(
            cue,
            vec![expected_ffprobe(temp.path().join("album.flac"), probe)],
            &["album.flac"],
            &temp,
        )
        .await;
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
        assert_cue_segment_carrier_ref(&source.tracks[1]);
        assert_eq!(source.tracks[1].expected_samples, Some(500));
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

    struct RealProcessToolRunner;

    #[async_trait]
    impl ToolRunner for RealProcessToolRunner {
        async fn run(
            &self,
            cmd: ToolCommand,
            _cancel: &CancellationToken,
        ) -> Result<ToolOutput, ToolRunnerError> {
            let binary_name = match &cmd.binary {
                ToolBinary::Ffmpeg => "ffmpeg",
                ToolBinary::Ffprobe => "ffprobe",
                _ => panic!("real CUE staging fixture only supports ffmpeg/ffprobe"),
            };

            let mut process = std::process::Command::new(binary_name);
            process.args(&cmd.args);
            if let Some(cwd) = &cmd.cwd {
                process.current_dir(cwd);
            }
            for env_var in &cmd.env {
                process.env(&env_var.key, env_var.value.expose());
            }

            let output = process.output().unwrap_or_else(|err| {
                panic!(
                    "spawn {binary_name} for real CUE staging fixture failed: {err}; args={:?}",
                    cmd.args
                )
            });
            let stdout_tail = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr_tail = String::from_utf8_lossy(&output.stderr).into_owned();
            assert!(
                output.status.success(),
                "real CUE staging fixture command failed: binary={binary_name} status={:?}\nargs={:?}\nstdout={}\nstderr={}",
                output.status.code(),
                cmd.args,
                stdout_tail,
                stderr_tail
            );

            let exit_code = output.status.code().unwrap_or(0);
            Ok(ToolOutput {
                exit: crate::convert::pipeline::tool::ProcessExit::Code(exit_code),
                stdout_tail: stdout_tail.clone(),
                stderr_tail: stderr_tail.clone(),
                elapsed: Duration::from_millis(0),
                command: crate::convert::pipeline::tool::CommandRecord {
                    binary: cmd.binary,
                    sanitized_args: cmd.args,
                    cwd: cmd.cwd,
                    env_keys: cmd.env.into_iter().map(|ev| ev.key).collect(),
                    exit: Some(crate::convert::pipeline::tool::ProcessExit::Code(exit_code)),
                    stdout_tail,
                    stderr_tail,
                    elapsed: Duration::from_millis(0),
                    description: None,
                },
            })
        }
    }

    #[tokio::test]
    async fn real_ffmpeg_staging_cuts_and_validates_exact_sample_count_when_available() {
        if !fixture_tool_available("ffmpeg") || !fixture_tool_available("ffprobe") {
            eprintln!("skipping real CUE segment staging fixture: ffmpeg or ffprobe is unavailable");
            return;
        }

        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("source.wav");
        let destination = temp
            .path()
            .join("staging/cue-segments/track01-s11025-n22050.wav");

        run_fixture_command(
            std::process::Command::new("ffmpeg")
                .arg("-y")
                .arg("-hide_banner")
                .arg("-loglevel")
                .arg("error")
                .arg("-f")
                .arg("lavfi")
                .arg("-i")
                .arg("anullsrc=r=44100:cl=mono:d=2")
                .arg("-c:a")
                .arg("pcm_s16le")
                .arg(&image),
        );

        let runner = RealProcessToolRunner;
        let cancel = CancellationToken::new();
        stage_cue_segment_as_s32_wav(
            &image,
            11_025,
            22_050,
            44_100,
            &destination,
            &runner,
            &cancel,
        )
        .await
        .expect("real ffmpeg stages the requested exact sample segment");

        validate_staged_cue_segment(&destination, 44_100, 22_050, &runner, &cancel)
            .await
            .expect("real ffprobe validates staged pcm_s32le WAV sample count");

        assert!(destination.exists(), "published staged segment should exist");
        assert!(
            destination.metadata().expect("staged metadata").len() > 0,
            "published staged segment should be non-empty"
        );
    }

    #[test]
    fn prefer_embedded_falls_back_to_sidecar_only_when_embedded_absent() {
        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("album.flac");
        let cue_path = temp.path().join("album.cue");
        std::fs::write(&image, b"not-a-real-flac-with-no-readable-tags");
        std::fs::write(
            &cue_path,
            br#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        );

        let mut req = test_request(&image);
        req.source.cue_sidecar = CueSidecarPolicy::PreferEmbedded;

        let cue_input = resolve_cue_input(&req)
            .expect("absent embedded CUESHEET falls back to sidecar");
        assert_eq!(cue_input.origin, CueOrigin::Sidecar);
        assert_eq!(cue_input.cue_parent.as_deref(), cue_path.parent());
    }

    #[test]
    fn prefer_embedded_does_not_swallow_malformed_embedded_cuesheet() {
        if !fixture_tool_available("ffmpeg") || !fixture_tool_available("metaflac") {
            eprintln!(
                "skipping malformed embedded CUESHEET fixture: ffmpeg or metaflac is unavailable"
            );
            return;
        }

        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("album.flac");
        let embedded_cue = temp.path().join("embedded-malformed.cue");
        let sidecar_cue = temp.path().join("album.cue");

        std::fs::write(
            &embedded_cue,
            br#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Malformed embedded CUE with no index"
"#,
        );
        std::fs::write(
            &sidecar_cue,
            br#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        );

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
                .arg(format!(
                    "--set-tag-from-file=CUESHEET={}",
                    embedded_cue.display()
                ))
                .arg(&image),
        );

        let mut req = test_request(&image);
        req.source.cue_sidecar = CueSidecarPolicy::PreferEmbedded;

        assert!(
            is_cue_image_candidate(&req).expect("candidate detection succeeds"),
            "malformed embedded CUESHEET must still route to the materializer so it can fail visibly"
        );
        let err = resolve_cue_input(&req)
            .expect_err("malformed embedded CUESHEET must not fall back to sidecar");
        let err = err.to_string();
        assert!(
            err.contains("INDEX 01"),
            "error should report embedded CUE validation failure, got: {err}"
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
        std::fs::write(&cue_path, cue_sheet_single_track().as_bytes());
        std::fs::write(&audio_path, b"fake-audio-data");

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
