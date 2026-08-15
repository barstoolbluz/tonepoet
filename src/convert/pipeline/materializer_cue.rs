//! PR 8 - CUE image materializer.
//!
//! Parses CUE image layouts and stages each CUE track as a bounded PCM WAV
//! carrier. Integer sources normalize to `pcm_s32le`; Float32/Float64 sources
//! retain their sample class as `pcm_f32le`/`pcm_f64le`. The
//! `PreparedTrack::bit_depth` field remains the original probed source-image
//! representation so `target_bit_depth = Source` resolves to the source, not
//! merely the carrier width. Downstream planning receives a typed
//! `CueSegmentCarrier` fact and encodes the requested final target from that
//! validated WAV instead of re-encoding through an intermediate FLAC carrier.

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
use crate::convert::classify::is_cue_sheet_path;
use crate::tui::cue_parser::{decode_cue_bytes_for_path, parse_cue, CueSheet};

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
    /// Original source sample representation using the shared source-depth
    /// convention: integer widths are literal bits; 320/640 are Float32/64.
    bit_depth: Option<u32>,
    coding: SourceAudioCoding,
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
        let mut track_count_by_image = HashMap::new();
        for image_path in &track_images {
            *track_count_by_image
                .entry(path_identity(image_path))
                .or_insert(0usize) += 1;
        }

        let mut probes = HashMap::new();
        let mut decode_paths = HashMap::new();
        let mut image_metadata = HashMap::new();
        let mut image_artwork = HashMap::new();
        for image_path in &unique_images {
            if cancel.is_cancelled() {
                return Err(MaterializeError::Cancelled);
            }
            let image_key = path_identity(image_path);
            let (probe, decode_path, used_wvunpack_fallback) =
                probe_cue_image_with_wavpack_fallback(
                    image_path,
                    staging,
                    runner,
                    cancel,
                )
                .await?;
            probes.insert(image_key.clone(), probe);
            decode_paths.insert(image_key.clone(), decode_path);
            image_metadata.insert(image_key.clone(), read_image_album_metadata(image_path));
            let artwork = if req.settings.metadata.preserve_artwork {
                match extract_cue_image_artwork(image_path, staging, runner, cancel).await {
                    Ok(artwork) => artwork,
                    Err(MaterializeError::Cancelled) => return Err(MaterializeError::Cancelled),
                    Err(_) if used_wvunpack_fallback => None,
                    Err(err) => return Err(err),
                }
            } else {
                None
            };
            image_artwork.insert(image_key, artwork);
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
            let SegmentBounds {
                start_sample,
                samples,
                is_image_tail,
            } = boundaries[idx];
            let staged_path = staged_cue_segment_path(
                staging,
                ordinal,
                cue_track.number,
                track_number_plan[idx].output_number,
                start_sample,
                samples,
            );
            let decode_path = decode_paths.get(&image_key).ok_or_else(|| {
                MaterializeError::Parse(format!(
                    "missing decode source for CUE image {}",
                    image_path.display()
                ))
            })?;
            let carrier = CueSegmentCarrier::for_source_depth_descriptor(probe.bit_depth);
            // A lossy image's header length is an estimate; its tail segment
            // is staged open-ended and the DECODED length becomes the fact
            // (backfilled below). All other segments stay exact.
            let policy = if probe.coding == SourceAudioCoding::Lossy && is_image_tail {
                SegmentLengthPolicy::LossyTail
            } else {
                SegmentLengthPolicy::Exact
            };
            let staged = stage_cue_segment_as_wav(
                decode_path,
                start_sample,
                samples,
                probe.sample_rate,
                carrier,
                &staged_path,
                policy,
                runner,
                cancel,
            )
            .await?;
            let staged_path = staged.path;
            let samples = staged.samples;

            let mut metadata = cue_track_metadata(
                cue_track,
                &cue_input.sheet,
                image_metadata_for_track,
                track_count_by_image.get(&image_key) == Some(&1),
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
                    carrier,
                },
                metadata,
                expected_samples: Some(samples),
                sample_rate: Some(probe.sample_rate),
                bit_depth: probe.bit_depth,
                source_audio: SourceAudioDescriptor::from_scalar(
                    Some(probe.sample_rate),
                    probe.bit_depth,
                    Some(probe.coding),
                ),
                warnings: Vec::new(),
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

        let tool_versions = cue_tool_versions(runner);
        let provenance = ExtractionProvenance {
            source_kind: SourceKind::CueImage,
            source_sha256: None,
            tool_versions,
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


fn cue_tool_versions(runner: &dyn ToolRunner) -> BTreeMap<String, String> {
    let mut tool_versions = BTreeMap::new();
    if let Some(version) = runner.tool_version(ToolBinary::Ffmpeg) {
        tool_versions.insert("ffmpeg".to_string(), version);
    }
    tool_versions
}

#[cfg(test)]
mod provenance_tests {
    use super::*;

    struct VersionOnlyRunner(HashMap<ToolBinary, String>);

    #[async_trait::async_trait]
    impl ToolRunner for VersionOnlyRunner {
        async fn run(
            &self,
            _cmd: ToolCommand,
            _cancel: &CancellationToken,
        ) -> Result<super::super::tool::ToolOutput, ToolRunnerError> {
            panic!("VersionOnlyRunner must not execute commands")
        }

        fn tool_version(&self, binary: ToolBinary) -> Option<String> {
            self.0.get(&binary).cloned()
        }
    }

    #[test]
    fn cue_materializer_provenance_records_detected_ffmpeg_version() {
        let runner = VersionOnlyRunner(HashMap::from([
            (ToolBinary::Ffmpeg, "7.1.3".to_string()),
        ]));
        let versions = cue_tool_versions(&runner);

        assert_eq!(versions.get("ffmpeg").map(String::as_str), Some("7.1.3"));
    }

    #[test]
    fn cue_materializer_provenance_omits_missing_external_version() {
        let runner = VersionOnlyRunner(HashMap::new());
        let versions = cue_tool_versions(&runner);

        assert!(versions.is_empty(), "missing external versions must not be mislabeled as in-process");
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
    let req_container_is_visible_cue = is_cue_sheet_path(&req.container);
    let fallback_image = (!req_container_is_visible_cue).then(|| req.container.clone());
    let cue_input = CueInput {
        sheet,
        raw_cue,
        origin: CueOrigin::Sidecar,
        cue_parent,
        fallback_image,
    };

    if !req_container_is_visible_cue {
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

    // Queued sidecar CUE sources: the metadata editor writes corrections to
    // the referenced image (flat tags plus a regenerated embedded CUESHEET)
    // and, when sidecar write-back is eligible, to the associated `.cue` file.
    // The sidecar remains authoritative for structure and image resolution.
    // When the referenced image carries an embedded sheet that structurally
    // matches, prefer that sheet for metadata so conversion observes saved
    // editor corrections even if sidecar write-back was skipped or failed.
    if req_container_is_visible_cue {
        if let Some(upgraded) = try_upgrade_sidecar_to_embedded_image_cue(&cue_input) {
            return Ok(upgraded);
        }
    }

    Ok(cue_input)
}

/// Effective metadata sheet for a sidecar CUE at dispatch time: the same
/// freshness precedence the materializer applies. The dispatcher's batch
/// identity probes must see the corrected embedded metadata when conversion
/// will, or a corrected multi-disc set resolves its album folder name from
/// stale sidecar text while its tracks carry the corrections.
pub(crate) fn dispatch_metadata_sheet_for_sidecar_cue(cue_path: &Path) -> Option<CueSheet> {
    let raw_cue = read_cue_text(cue_path).ok()?;
    let sheet = parse_cue(&raw_cue);
    let sidecar = CueInput {
        sheet,
        raw_cue,
        origin: CueOrigin::Sidecar,
        cue_parent: cue_path.parent().map(Path::to_path_buf),
        fallback_image: None,
    };
    match try_upgrade_sidecar_to_embedded_image_cue(&sidecar) {
        Some(upgraded) => Some(upgraded.sheet),
        None => Some(sidecar.sheet),
    }
}

/// Best-effort freshness upgrade for a sidecar-resolved single-image CUE: use
/// the referenced image's embedded CUESHEET when it exists, is a valid
/// single-image sheet, and has the same track count as the sidecar. Any
/// failure or structural disagreement keeps the sidecar (logged) — conversion
/// must never fail because an embedded sheet is absent or malformed.
fn try_upgrade_sidecar_to_embedded_image_cue(sidecar: &CueInput) -> Option<CueInput> {
    let track_images = match resolve_track_image_paths(sidecar) {
        Ok(images) => images,
        Err(_) => return None,
    };
    let mut unique = track_images.clone();
    unique.sort();
    unique.dedup();
    if unique.len() != 1 {
        // Track-per-file or multi-image layouts keep sidecar authority; the
        // editor's embedded round-trip only exists for single-image rips.
        return None;
    }
    let image = &unique[0];

    let raw_cue = match read_embedded_cuesheet(image) {
        Ok(Some(raw)) => raw,
        Ok(None) => return None,
        Err(err) => {
            log::warn!(
                "sidecar CUE kept: embedded CUESHEET on {} was unreadable: {err}",
                image.display()
            );
            return None;
        }
    };
    let sheet = parse_cue(&raw_cue);
    if let Err(err) = validate_embedded_single_image_layout(&sheet) {
        log::warn!(
            "sidecar CUE kept: embedded CUESHEET on {} failed validation: {err}",
            image.display()
        );
        return None;
    }
    if sheet.tracks.len() != sidecar.sheet.tracks.len() {
        log::warn!(
            "sidecar CUE kept: embedded CUESHEET on {} has {} tracks but the sidecar has {}",
            image.display(),
            sheet.tracks.len(),
            sidecar.sheet.tracks.len()
        );
        return None;
    }
    // Structure authority means split points, not just track count: segment
    // boundaries are INDEX 01 driven, so the embedded sheet is only a safe
    // wholesale substitute when every INDEX 01 matches the sidecar's. The
    // editor's regenerated sheets round-trip boundaries exactly; third-party
    // embedded sheets that disagree keep the sidecar.
    let boundaries_match = sheet
        .tracks
        .iter()
        .zip(sidecar.sheet.tracks.iter())
        .all(|(embedded, side)| embedded.index01_frames == side.index01_frames);
    if !boundaries_match {
        log::warn!(
            "sidecar CUE kept: embedded CUESHEET on {} has matching track count but different INDEX 01 boundaries",
            image.display()
        );
        return None;
    }

    log::info!(
        "using embedded CUESHEET metadata from {} (sidecar structure verified, {} tracks)",
        image.display(),
        sheet.tracks.len()
    );
    Some(CueInput {
        sheet,
        raw_cue,
        origin: CueOrigin::Embedded,
        cue_parent: None,
        fallback_image: Some(image.clone()),
    })
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

    if is_cue_sheet_path(&req.container) {
        // A user-visible .cue path is a CUE-image candidate even when its
        // contents are malformed. Hidden dot-cues are sidecar artifacts and
        // must be ignored consistently with Browse and queue expansion.
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
    if is_cue_sheet_path(image) {
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
        if sidecar_cue_subdivides_image(&cue_path, image)? {
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

/// Number of the CUE's tracks whose resolved FILE reference is `image`
/// (0 when the CUE cannot be decoded, parsed, or fully resolved).
fn sidecar_cue_track_count_for_image(
    cue_path: &Path,
    image: &Path,
) -> Result<usize, SourceDetectError> {
    let raw = std::fs::read(cue_path)?;
    let content = match decode_cue_bytes_for_path(&raw, cue_path) {
        Ok(content) => content,
        Err(_) => return Ok(0),
    };
    let sheet = parse_cue(&content);
    if validate_sidecar_layout_detect(&sheet).is_err() {
        return Ok(0);
    }

    let cue_input = CueInput {
        sheet,
        raw_cue: content,
        origin: CueOrigin::Sidecar,
        cue_parent: cue_path.parent().map(Path::to_path_buf),
        fallback_image: Some(image.to_path_buf()),
    };

    let Ok(track_images) = resolve_track_image_paths(&cue_input) else {
        return Ok(0);
    };

    Ok(track_images
        .iter()
        .filter(|resolved| same_existing_path(resolved, image))
        .count())
}

/// Explicit-pairing check for SAME-STEM sidecars: the CUE merely has to
/// reference the image. A same-stem name is a deliberate association made by
/// whoever produced the rip, so subdivision evidence is not required.
fn sidecar_cue_is_usable_for_image(
    cue_path: &Path,
    image: &Path,
) -> Result<bool, SourceDetectError> {
    Ok(sidecar_cue_track_count_for_image(cue_path, image)? >= 1)
}

/// Inference check for DIRECTORY-SCAN association: a foreign-stem CUE
/// qualifies `image` as a decomposable CUE image only when it SUBDIVIDES it
/// (maps two or more tracks to it). A CUE that maps exactly one track to the
/// file is an album-level split-track listing (one FILE per track); the file
/// is a split track and must stay on the legacy single-file path — otherwise
/// every track of such an album decomposes into the whole album, duplicating
/// the conversion once per queued track.
fn sidecar_cue_subdivides_image(
    cue_path: &Path,
    image: &Path,
) -> Result<bool, SourceDetectError> {
    Ok(sidecar_cue_track_count_for_image(cue_path, image)? >= 2)
}

pub(crate) fn embedded_cuesheet_is_present(path: &Path) -> bool {
    matches!(read_embedded_cuesheet(path), Ok(Some(_)))
}

fn find_valid_sidecar_cue_for_image(image: &Path) -> Result<Option<PathBuf>, MaterializeError> {
    if is_cue_sheet_path(image) {
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
        match sidecar_cue_subdivides_image_materialize(&cue_path, image) {
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

fn sidecar_cue_track_count_for_image_materialize(
    cue_path: &Path,
    image: &Path,
) -> Result<usize, MaterializeError> {
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
        .filter(|resolved| same_existing_path(resolved, image))
        .count())
}

/// Explicit same-stem pairing: reference alone suffices (see the detect-time
/// twin `sidecar_cue_is_usable_for_image` for the rationale).
fn sidecar_cue_matches_image(cue_path: &Path, image: &Path) -> Result<bool, MaterializeError> {
    Ok(sidecar_cue_track_count_for_image_materialize(cue_path, image)? >= 1)
}

/// Directory-scan inference: requires subdivision (two or more tracks mapped
/// to the image) — one-track-per-file album CUEs list split tracks and must
/// not turn them into whole-album CUE images (see the detect-time twin
/// `sidecar_cue_subdivides_image`).
fn sidecar_cue_subdivides_image_materialize(
    cue_path: &Path,
    image: &Path,
) -> Result<bool, MaterializeError> {
    Ok(sidecar_cue_track_count_for_image_materialize(cue_path, image)? >= 2)
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
    if is_cue_sheet_path(path) {
        return Ok(vec![path.to_path_buf()]);
    }

    let Some(parent) = path.parent() else {
        return Ok(Vec::new());
    };

    let mut cues = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let candidate = entry.path();
        if candidate.is_file() && is_cue_sheet_path(&candidate) {
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
        Err(error) if crate::metadata_persistence::native_ape_error_is_eligible(&error) => {
            let outcome = match crate::metadata_persistence::read_native_ape_fallback(path) {
                Ok(outcome) => outcome,
                Err(native_error) => {
                    log::warn!(
                        "embedded CUESHEET metadata unavailable for '{}': {error}; native APEv2 fallback refused: {native_error}",
                        path.display()
                    );
                    return Ok(None);
                }
            };
            if let Some(warning) = outcome.warning {
                log::warn!("{}", warning.message());
            }
            return Ok(outcome
                .rows
                .into_iter()
                .find(|row| {
                    !row.is_binary
                        && (row.raw_key.eq_ignore_ascii_case("CUESHEET")
                            || row.canonical_key.eq_ignore_ascii_case("CUESHEET"))
                })
                .map(|row| row.value));
        }
        Err(error) => {
            log::warn!(
                "embedded CUESHEET metadata unavailable for '{}': {error}",
                path.display()
            );
            return Ok(None);
        }
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
        let Some(current) = track.index01_frames else {
            return Err(format!(
                "CUE requires INDEX 01 for track {}",
                track.number
            ));
        };
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

fn is_wavpack_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("wv"))
        .unwrap_or(false)
}

fn staged_wavpack_decode_path(staging: &StagingDir, input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("wavpack-image")
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' { ch } else { '_' })
        .collect::<String>();
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in path_identity(input).to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    staging
        .root
        .join("cue-decoded-images")
        .join(format!("{stem}-{hash:016x}.wav"))
}


fn temporary_wavpack_decode_path(destination: &Path) -> Result<PathBuf, MaterializeError> {
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
        .unwrap_or("wavpack-image");
    let tmp_dir = destination
        .parent()
        .map(|parent| parent.join(".tmp"))
        .unwrap_or_else(|| PathBuf::from(".tmp"));
    // Keep a terminal .wav extension: wvunpack uses the requested output
    // filename directly, and downstream ffprobe must see an unambiguous WAV
    // carrier even while the file is private and unpublished.
    Ok(tmp_dir.join(format!("{stem}.tmp.{pid}.{stamp}.{random}.wav")))
}

async fn decode_wavpack_image_for_cue(
    input: &Path,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<PathBuf, MaterializeError> {
    let destination = staged_wavpack_decode_path(staging, input);
    if destination.exists() {
        match probe_audio_image(&destination, runner, cancel).await {
            Ok(_) => return Ok(destination),
            Err(MaterializeError::Cancelled) => return Err(MaterializeError::Cancelled),
            Err(_) => {}
        }
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = temporary_wavpack_decode_path(&destination)?;
    if let Some(parent) = temporary.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_path_if_exists(&temporary)?;
    let command = ToolCommand {
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        binary: ToolBinary::Wvunpack,
        args: vec![
            "-q".into(),
            "-o".into(),
            temporary.display().to_string(),
            input.display().to_string(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(15 * 60),
    };
    match runner.run(command, cancel).await {
        Ok(_) => {}
        Err(ToolRunnerError::Cancelled { .. }) => {
            let _ = remove_path_if_exists(&temporary);
            return Err(MaterializeError::Cancelled);
        }
        Err(err) => {
            let _ = remove_path_if_exists(&temporary);
            return Err(MaterializeError::Parse(format!(
                "ffmpeg could not read WavPack CUE image {}; wvunpack fallback failed: {err}",
                input.display()
            )));
        }
    }
    if let Err(err) = probe_audio_image(&temporary, runner, cancel).await {
        let _ = remove_path_if_exists(&temporary);
        return Err(MaterializeError::Parse(format!(
            "wvunpack produced an unusable decode for CUE image {}: {err}",
            input.display()
        )));
    }
    sync_file_to_storage(&temporary)?;
    if destination.exists() {
        match probe_audio_image(&destination, runner, cancel).await {
            Ok(_) => {
                let _ = remove_path_if_exists(&temporary);
                return Ok(destination);
            }
            Err(MaterializeError::Cancelled) => {
                let _ = remove_path_if_exists(&temporary);
                return Err(MaterializeError::Cancelled);
            }
            Err(_) => {
                remove_path_if_exists(&destination)?;
            }
        }
    }
    fs::rename(&temporary, &destination)?;
    sync_parent_dir_best_effort(&destination);
    Ok(destination)
}

async fn probe_cue_image_with_wavpack_fallback(
    path: &Path,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(AudioProbe, PathBuf, bool), MaterializeError> {
    match probe_audio_image(path, runner, cancel).await {
        Ok(probe) => Ok((probe, path.to_path_buf(), false)),
        Err(MaterializeError::Cancelled) => Err(MaterializeError::Cancelled),
        Err(primary) if is_wavpack_path(path) => {
            let decoded = decode_wavpack_image_for_cue(path, staging, runner, cancel).await?;
            let probe = probe_audio_image(&decoded, runner, cancel).await.map_err(|fallback| {
                MaterializeError::Parse(format!(
                    "ffmpeg probe failed for WavPack CUE image {} ({primary}); decoded fallback {} also failed: {fallback}",
                    path.display(),
                    decoded.display()
                ))
            })?;
            Ok((probe, decoded, true))
        }
        Err(err) => Err(err),
    }
}

async fn probe_audio_image(
    path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<AudioProbe, MaterializeError> {
    let cmd = ToolCommand {
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        binary: ToolBinary::Ffprobe,
        args: vec![
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            "a:0".into(),
            "-count_frames".into(),
            "-show_entries".into(),
            "stream=codec_name,sample_fmt,sample_rate,duration_ts,time_base,duration,bits_per_raw_sample,bits_per_sample"
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
        // `-count_frames` decodes the ENTIRE image to count samples, so this
        // probe scales with image length and codec decode speed. 30s (the
        // header-read probes' budget) is fine for FLAC/WAV images but a full
        // Monkey's Audio (APE) CD image legitimately takes minutes.
        timeout: Duration::from_secs(600),
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
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
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
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
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
    let sample_fmt = stream
        .get("sample_fmt")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty() && *value != "N/A")
        .map(str::to_string);
    let integer_bit_depth = stream
        .get("bits_per_raw_sample")
        .and_then(json_u32_from_value)
        .or_else(|| {
            stream
                .get("bits_per_sample")
                .and_then(json_u32_from_value)
        });
    let codec_name = stream
        .get("codec_name")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty() && *value != "N/A")
        .map(str::to_string);
    let (coding, bit_depth) = classify_source_audio_probe(
        codec_name.as_deref(),
        sample_fmt.as_deref(),
        integer_bit_depth,
    );
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
                coding,
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
        coding,
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

/// Per-track segment bounds. `is_image_tail` marks the last track of its
/// image file — the only segment whose end is the (header-derived) image
/// total rather than a CUE INDEX position.
#[derive(Debug, Clone, Copy)]
struct SegmentBounds {
    start_sample: u64,
    samples: u64,
    is_image_tail: bool,
}

fn compute_track_boundaries_for_layout(
    sheet: &CueSheet,
    track_images: &[PathBuf],
    probes: &HashMap<PathBuf, AudioProbe>,
) -> Result<Vec<SegmentBounds>, MaterializeError> {
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

        let mut next_start = None;
        for next_idx in (idx + 1)..sheet.tracks.len() {
            if image_keys[next_idx] != *image_key {
                continue;
            }
            let next_frames = sheet.tracks[next_idx].index01_frames.ok_or_else(|| {
                MaterializeError::Parse(format!(
                    "track {} has no INDEX 01",
                    sheet.tracks[next_idx].number
                ))
            })?;
            next_start = Some(cue_frames_to_samples(next_frames as u64, probe.sample_rate));
            break;
        }
        let is_lossy_tail = probe.coding == SourceAudioCoding::Lossy && next_start.is_none();
        // Lossy headers are estimates. In particular, Xing-less VBR headers
        // may understate the decoded duration enough that the final INDEX 01
        // appears to start beyond `total_samples`. Admit that final tail and
        // let the open-ended decode establish the authoritative length.
        let end = if is_lossy_tail {
            next_start.unwrap_or_else(|| probe.total_samples.max(start.saturating_add(1)))
        } else {
            next_start.unwrap_or(probe.total_samples)
        };

        if !is_lossy_tail && start >= probe.total_samples {
            return Err(MaterializeError::Parse(format!(
                "track {} starts beyond image duration for {}",
                track.number,
                image_path.display()
            )));
        }
        if end <= start {
            return Err(MaterializeError::Parse(format!(
                "invalid CUE boundary for track {} in image {}",
                track.number,
                image_path.display()
            )));
        }
        if !is_lossy_tail && end > probe.total_samples {
            return Err(MaterializeError::Parse(format!(
                "track {} ends beyond image duration for {}",
                track.number,
                image_path.display()
            )));
        }
        if !is_lossy_tail
            && !probe.exact_samples
            && next_start.is_none()
            && probe.total_samples.saturating_sub(start) < probe.sample_rate as u64 / 20
        {
            return Err(MaterializeError::Parse(format!(
                "image sample count probe is too coarse for final CUE segment in {}",
                image_path.display()
            )));
        }
        boundaries.push(SegmentBounds {
            start_sample: start,
            samples: end - start,
            is_image_tail: next_start.is_none(),
        });
    }

    Ok(boundaries)
}

#[cfg(test)]
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

#[cfg(test)] // test-only shim over the typed-carrier path
async fn stage_cue_segment_as_s32_wav(
    image: &Path,
    start_sample: u64,
    samples: u64,
    sample_rate: u32,
    destination: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(), MaterializeError> {
    stage_cue_segment_as_wav(
        image,
        start_sample,
        samples,
        sample_rate,
        CueSegmentCarrier::PcmS32LeWav,
        destination,
        SegmentLengthPolicy::Exact,
        runner,
        cancel,
    )
    .await
    .map(|_| ())
}

/// How a staged segment's length is validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentLengthPolicy {
    /// The segment length is fully determined by CUE INDEX positions (or a
    /// lossless image's exact total): the staged WAV must match exactly.
    Exact,
    /// The segment is the tail of a LOSSY image: the header-derived length is
    /// an estimate (encoder delay/padding, VBR duration estimates), so the
    /// segment is staged open-ended and the DECODED length becomes the fact.
    /// A bounded shortfall guards against genuinely truncated sources.
    LossyTail,
}

/// Maximum acceptable shortfall of a lossy image-tail decode versus its
/// header-derived length (~120 ms). Codec delay/padding classes top out
/// around 4.7k samples (AAC); anything beyond this indicates a truncated
/// source and fails closed.
fn lossy_tail_shortfall_limit(sample_rate: u32, header_samples: u64) -> u64 {
    // Permit at most ~120 ms of codec delay/padding, but never let that fixed
    // allowance consume an entire short tail. The relative cap keeps the
    // truncation guard meaningful even when the header-derived tail is small.
    let codec_delay_cap = (u64::from(sample_rate).saturating_mul(120) / 1_000).max(1);
    let relative_cap = (header_samples / 4).max(1);
    codec_delay_cap.min(relative_cap)
}

/// Rebuild a staged-segment destination for a corrected sample count. The
/// provisional name embeds the header-derived `-n{samples}.wav` suffix
/// (see `staged_cue_segment_path`); test shims may pass arbitrary names, in
/// which case the provisional path is kept as-is.
fn segment_destination_with_samples(
    provisional: &Path,
    header_samples: u64,
    measured_samples: u64,
) -> PathBuf {
    if measured_samples == header_samples {
        return provisional.to_path_buf();
    }
    let Some(name) = provisional.file_name().and_then(|value| value.to_str()) else {
        return provisional.to_path_buf();
    };
    let expected_suffix = format!("-n{header_samples}.wav");
    let Some(stem) = name.strip_suffix(expected_suffix.as_str()) else {
        return provisional.to_path_buf();
    };
    provisional.with_file_name(format!("{stem}-n{measured_samples}.wav"))
}

/// A published, validated staged segment: the final path plus the measured
/// sample count (equal to the requested count under `Exact`).
#[derive(Debug, Clone)]
struct StagedCueSegment {
    path: PathBuf,
    samples: u64,
}

async fn stage_cue_segment_as_wav(
    image: &Path,
    start_sample: u64,
    samples: u64,
    sample_rate: u32,
    carrier: CueSegmentCarrier,
    destination: &Path,
    policy: SegmentLengthPolicy,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<StagedCueSegment, MaterializeError> {
    // Pre-ffmpeg reuse short-circuit. For LossyTail the published file lives
    // under the MEASURED name, which is unknowable before decoding, so an
    // interrupted-run retry pays one extra decode (correctness unaffected —
    // the publish path still reuses a valid measured-name file).
    if policy == SegmentLengthPolicy::Exact && destination.exists() {
        match validate_staged_cue_segment_as(
            destination,
            sample_rate,
            samples,
            carrier,
            runner,
            cancel,
        )
        .await
        {
            Ok(()) => {
                return Ok(StagedCueSegment {
                    path: destination.to_path_buf(),
                    samples,
                })
            }
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

    // LossyTail stages OPEN-ENDED: the header-derived end is an estimate in
    // both directions (delay/padding overstates; Xing-less VBR understates —
    // a capped extraction would silently truncate real audio AND validate
    // clean). CUE tail semantics are "play to end of audio".
    let capped_samples = match policy {
        SegmentLengthPolicy::Exact => Some(samples),
        SegmentLengthPolicy::LossyTail => None,
    };
    let cmd = cue_segment_ffmpeg_command_for_carrier(
        image,
        start_sample,
        capped_samples,
        carrier,
        &tmp_destination,
    )?;

    let run_result = runner.run(cmd, cancel).await;
    match run_result {
        Ok(_) => {
            // Measure once on the tmp file; every later validation is Exact
            // against the measured count.
            let measured = match measure_staged_cue_segment_as(
                &tmp_destination,
                sample_rate,
                carrier,
                runner,
                cancel,
            )
            .await
            {
                Ok(measured) => measured,
                Err(err) => {
                    let _ = remove_path_if_exists(&tmp_destination);
                    return Err(err);
                }
            };
            match policy {
                SegmentLengthPolicy::Exact => {
                    if measured != samples {
                        let _ = remove_path_if_exists(&tmp_destination);
                        return Err(MaterializeError::Parse(format!(
                            "CUE image {} decoded {} samples for the requested segment, expected {}; temporary staging file was {}",
                            image.display(),
                            measured,
                            samples,
                            tmp_destination.display()
                        )));
                    }
                }
                SegmentLengthPolicy::LossyTail => {
                    let limit = lossy_tail_shortfall_limit(sample_rate, samples);
                    if measured == 0 {
                        let _ = remove_path_if_exists(&tmp_destination);
                        return Err(MaterializeError::Parse(format!(
                            "lossy CUE image {} decoded no samples for its tail segment",
                            image.display()
                        )));
                    }
                    if measured.saturating_add(limit) < samples {
                        let _ = remove_path_if_exists(&tmp_destination);
                        return Err(MaterializeError::Parse(format!(
                            "lossy CUE image {} decoded {} samples short of its header length for the tail segment (measured {}, expected {}, limit {}); the source appears truncated",
                            image.display(),
                            samples - measured,
                            measured,
                            samples,
                            limit
                        )));
                    }
                    if measured > samples.saturating_add(u64::from(sample_rate)) {
                        log::warn!(
                            "lossy CUE image {} decoded {} samples beyond its header length for the tail segment (measured {}, header {}); keeping the full decode",
                            image.display(),
                            measured - samples,
                            measured,
                            samples
                        );
                    }
                }
            }
            let final_destination =
                segment_destination_with_samples(destination, samples, measured);

            sync_file_to_storage(&tmp_destination)?;

            if final_destination.exists() {
                match validate_staged_cue_segment_as(
                    &final_destination,
                    sample_rate,
                    measured,
                    carrier,
                    runner,
                    cancel,
                )
                .await
                {
                    Ok(()) => {
                        let _ = remove_path_if_exists(&tmp_destination);
                        return Ok(StagedCueSegment {
                            path: final_destination,
                            samples: measured,
                        });
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
                &final_destination,
                sample_rate,
                measured,
                carrier,
                runner,
                cancel,
            )
            .await
            {
                let _ = remove_path_if_exists(&tmp_destination);
                return Err(err);
            }
            Ok(StagedCueSegment {
                path: final_destination,
                samples: measured,
            })
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

#[cfg(test)] // test-only shim over the typed-carrier path
async fn validate_staged_cue_segment(
    path: &Path,
    expected_sample_rate: u32,
    expected_samples: u64,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(), MaterializeError> {
    validate_staged_cue_segment_as(
        path,
        expected_sample_rate,
        expected_samples,
        CueSegmentCarrier::PcmS32LeWav,
        runner,
        cancel,
    )
    .await
}

async fn validate_staged_cue_segment_as(
    path: &Path,
    expected_sample_rate: u32,
    expected_samples: u64,
    carrier: CueSegmentCarrier,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(), MaterializeError> {
    let measured = measure_staged_cue_segment_as(
        path,
        expected_sample_rate,
        carrier,
        runner,
        cancel,
    )
    .await?;
    if measured != expected_samples {
        return Err(MaterializeError::Parse(format!(
            "staged CUE segment {} has {} samples, expected {}",
            path.display(),
            measured,
            expected_samples
        )));
    }
    Ok(())
}

/// All staged-segment integrity checks EXCEPT the sample-count comparison:
/// readable, non-empty, expected sample rate, exact probe, expected carrier
/// codec, WAV container. Returns the measured sample count so callers can
/// apply their own length policy.
async fn measure_staged_cue_segment_as(
    path: &Path,
    expected_sample_rate: u32,
    carrier: CueSegmentCarrier,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<u64, MaterializeError> {
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
    if probe.codec_name.as_deref() != Some(carrier.codec_name()) {
        return Err(MaterializeError::Parse(format!(
            "staged CUE segment {} has codec {:?}, expected {}",
            path.display(),
            probe.codec_name,
            carrier.codec_name()
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

    Ok(probe.total_samples)
}

async fn probe_staged_cue_segment(
    path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<AudioProbe, MaterializeError> {
    let cmd = ToolCommand {
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        binary: ToolBinary::Ffprobe,
        args: vec![
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            "a:0".into(),
            "-show_entries".into(),
            "stream=codec_name,sample_fmt,sample_rate,duration_ts,time_base,duration,bits_per_raw_sample,bits_per_sample"
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
    carrier: CueSegmentCarrier,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(), MaterializeError> {
    match fs::rename(tmp_destination, destination) {
        Ok(()) => {
            sync_parent_dir_best_effort(destination);
            validate_staged_cue_segment_as(
                destination,
                expected_sample_rate,
                expected_samples,
                carrier,
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
            match validate_staged_cue_segment_as(
                destination,
                expected_sample_rate,
                expected_samples,
                carrier,
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
                    match validate_staged_cue_segment_as(
                        destination,
                        expected_sample_rate,
                        expected_samples,
                        carrier,
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
            validate_staged_cue_segment_as(
                destination,
                expected_sample_rate,
                expected_samples,
                carrier,
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

#[cfg(test)] // test-only shim over the typed-carrier path
fn cue_segment_ffmpeg_command(
    image: &Path,
    start_sample: u64,
    samples: u64,
    destination: &Path,
) -> Result<ToolCommand, MaterializeError> {
    cue_segment_ffmpeg_command_for_carrier(
        image,
        start_sample,
        Some(samples),
        CueSegmentCarrier::PcmS32LeWav,
        destination,
    )
}

fn cue_segment_ffmpeg_command_for_carrier(
    image: &Path,
    start_sample: u64,
    samples: Option<u64>,
    carrier: CueSegmentCarrier,
    destination: &Path,
) -> Result<ToolCommand, MaterializeError> {
    let filter = cue_segment_atrim_filter(start_sample, samples)?;
    Ok(ToolCommand {
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
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
            carrier.codec_name().into(),
            destination.display().to_string(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(15 * 60),
    })
}

fn cue_segment_atrim_filter(
    start_sample: u64,
    samples: Option<u64>,
) -> Result<String, MaterializeError> {
    let Some(samples) = samples else {
        // Open-ended: lossy image-tail segments run to decode EOF because the
        // header-derived end is an estimate in both directions.
        return Ok(format!(
            "atrim=start_sample={start_sample},asetpts=N/SR/TB"
        ));
    };
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
    album_artist: MetadataValueList,
    artist: MetadataValueList,
    genre: MetadataValueList,
    date: Option<String>,
    composer: MetadataValueList,
    performer: MetadataValueList,
    arranger: MetadataValueList,
    isrc: Option<String>,
    publisher: Option<String>,
    copyright: Option<String>,
    comment: Option<String>,
    total_discs: Option<u32>,
    disc_number: Option<u32>,
    extra: BTreeMap<String, String>,
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
        if merged.composer.is_none() {
            merged.composer = metadata.composer.clone();
        }
        if merged.performer.is_none() {
            merged.performer = metadata.performer.clone();
        }
        if merged.arranger.is_none() {
            merged.arranger = metadata.arranger.clone();
        }
        if merged.isrc.is_none() {
            merged.isrc = metadata.isrc.clone();
        }
        if merged.publisher.is_none() {
            merged.publisher = metadata.publisher.clone();
        }
        if merged.copyright.is_none() {
            merged.copyright = metadata.copyright.clone();
        }
        if merged.comment.is_none() {
            merged.comment = metadata.comment.clone();
        }
        if merged.total_discs.is_none() {
            merged.total_discs = metadata.total_discs;
        }
        if merged.disc_number.is_none() {
            merged.disc_number = metadata.disc_number;
        }
        merge_image_album_metadata_extra(&mut merged.extra, &metadata.extra, &image_path);
    }

    if !sources.is_empty() {
        merged.source = Some(sources.join("; "));
    }

    merged
}

fn merge_image_album_metadata_extra(
    merged: &mut BTreeMap<String, String>,
    candidate: &BTreeMap<String, String>,
    image_path: &Path,
) {
    for (key, value) in candidate {
        if value.trim().is_empty() {
            continue;
        }
        match merged.get(key) {
            None => {
                merged.insert(key.clone(), value.clone());
            }
            Some(existing) if existing == value => {}
            Some(existing) => {
                log::warn!(
                    "conflicting album-level image tag {key} on CUE member '{}'; keeping first value {:?}, ignoring {:?}",
                    image_path.display(),
                    existing,
                    value
                );
            }
        }
    }
}

fn set_extra_if_empty(extra: &mut BTreeMap<String, String>, key: &str, value: &str) {
    if !value.trim().is_empty() && !extra.contains_key(key) {
        extra.insert(key.to_string(), value.trim().to_string());
    }
}

fn cue_image_extra_key(key: &str) -> Option<&'static str> {
    match key {
        "catalog" | "catalognumber" | "discogscatalog" => Some("catalognumber"),
        "releasecountry" | "country" => Some("releasecountry"),
        "originalyear" => Some("originalyear"),
        "originaldate" | "originalreleasedate" | "tdor" => Some("originaldate"),
        "musicbrainzalbumid" | "musicbrainzreleaseid" => Some("musicbrainz_albumid"),
        "musicbrainzalbumartistid" | "musicbrainzreleaseartistid" => Some("musicbrainz_albumartistid"),
        "musicbrainzreleasegroupid" => Some("musicbrainz_releasegroupid"),
        _ => None,
    }
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
    let tag_type = tag.tag_type();
    let (mut set_values, set_value_warnings) =
        crate::tui::probe::read_pipeline_set_valued_text_fields(path, tag);
    for warning in set_value_warnings {
        log::warn!("{warning}");
    }
    metadata.album_artist =
        MetadataValueList::from_values(set_values.remove("ALBUMARTIST").unwrap_or_default());
    metadata.artist = MetadataValueList::from_values(set_values.remove("ARTIST").unwrap_or_default());
    metadata.genre = MetadataValueList::from_values(set_values.remove("GENRE").unwrap_or_default());
    metadata.composer =
        MetadataValueList::from_values(set_values.remove("COMPOSER").unwrap_or_default());
    metadata.performer =
        MetadataValueList::from_values(set_values.remove("PERFORMER").unwrap_or_default());
    metadata.arranger =
        MetadataValueList::from_values(set_values.remove("ARRANGER").unwrap_or_default());

    for item in tag.items() {
        let key = normalized_lofty_item_key(item.key());
        let Some(value) = item.value().text().map(str::trim).filter(|value| !value.is_empty()) else {
            continue;
        };

        // Keep the source text-tag provenance contract used by the single-file
        // materializer. Structural/per-track CUE keys are intentionally not
        // promoted album-wide when an image is split into output tracks.
        if !cue_image_tag_is_structural_or_track_scoped(&key) {
            let source_key = super::materializer_single::item_key_to_extra_key(item.key(), tag_type);
            insert_source_text_tag(&mut metadata.extra, &source_key, value);
        }

        match cue_image_tag_field(&key) {
            Some(ImageTagField::Album) => set_if_empty(&mut metadata.album, value),
            Some(ImageTagField::AlbumArtist) => {
                // The shared format-aware reader is authoritative for the six
                // ordered-list fields. Preserve the old first-value fallback
                // only for legacy aliases that its canonical mapping does not
                // classify (for example an album-artist-sort carrier).
                if metadata.album_artist.is_empty() {
                    metadata.album_artist = MetadataValueList::from_scalar(value);
                }
            }
            Some(ImageTagField::Artist) => {
                if metadata.artist.is_empty() {
                    metadata.artist = MetadataValueList::from_scalar(value);
                }
            }
            Some(ImageTagField::Genre) => {
                if metadata.genre.is_empty() {
                    metadata.genre = MetadataValueList::from_scalar(value);
                }
            }
            Some(ImageTagField::Composer) => {
                if metadata.composer.is_empty() {
                    metadata.composer = MetadataValueList::from_scalar(value);
                }
            }
            Some(ImageTagField::Performer) => {
                if metadata.performer.is_empty() {
                    metadata.performer = MetadataValueList::from_scalar(value);
                }
            }
            Some(ImageTagField::Arranger) => {
                if metadata.arranger.is_empty() {
                    metadata.arranger = MetadataValueList::from_scalar(value);
                }
            }
            Some(ImageTagField::Date) => set_if_empty(&mut metadata.date, value),
            Some(ImageTagField::Isrc) => set_if_empty(&mut metadata.isrc, value),
            Some(ImageTagField::Publisher) => set_if_empty(&mut metadata.publisher, value),
            Some(ImageTagField::Copyright) => set_if_empty(&mut metadata.copyright, value),
            Some(ImageTagField::Comment) => set_if_empty(&mut metadata.comment, value),
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
            None => {
                if let Some(extra_key) = cue_image_extra_key(&key) {
                    set_extra_if_empty(&mut metadata.extra, extra_key, value);
                }
            }
        }
    }

    if metadata.artist.is_empty() && !metadata.performer.is_empty() {
        // Preserve the historical image PERFORMER -> ARTIST fallback, but copy
        // the complete ordered list rather than only its first physical value.
        metadata.artist = metadata.performer.clone();
    }

    if metadata.album.is_some()
        || metadata.album_artist.is_some()
        || metadata.artist.is_some()
        || metadata.genre.is_some()
        || metadata.date.is_some()
        || metadata.composer.is_some()
        || metadata.performer.is_some()
        || metadata.arranger.is_some()
        || metadata.isrc.is_some()
        || metadata.publisher.is_some()
        || metadata.copyright.is_some()
        || metadata.comment.is_some()
        || metadata.total_discs.is_some()
        || metadata.disc_number.is_some()
        || !metadata.extra.is_empty()
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
    Composer,
    Performer,
    Arranger,
    Isrc,
    Publisher,
    Copyright,
    Comment,
    DiscNumber,
    TotalDiscs,
}

fn cue_image_tag_field(key: &str) -> Option<ImageTagField> {
    match key {
        "album" | "albumtitle" | "talb" => Some(ImageTagField::Album),
        "albumartist" | "albumartistsort" | "albumartistsortorder" | "tpe2" => {
            Some(ImageTagField::AlbumArtist)
        }
        "artist" | "trackartist" | "tpe1" => Some(ImageTagField::Artist),
        "genre" | "tcon" => Some(ImageTagField::Genre),
        "date" | "year" | "recordingdate" | "tdrc" | "tyer" => Some(ImageTagField::Date),
        "composer" | "tcom" => Some(ImageTagField::Composer),
        "performer" => Some(ImageTagField::Performer),
        "arranger" => Some(ImageTagField::Arranger),
        "isrc" | "tsrc" => Some(ImageTagField::Isrc),
        "publisher" | "label" | "tpub" => Some(ImageTagField::Publisher),
        "copyright" | "copyrightmessage" | "tcop" => Some(ImageTagField::Copyright),
        "comment" | "description" | "comm" => Some(ImageTagField::Comment),
        "discnumber" | "disc" | "partofset" | "tpos" => Some(ImageTagField::DiscNumber),
        "totaldiscs" | "disctotal" => Some(ImageTagField::TotalDiscs),
        _ => None,
    }
}

fn cue_image_tag_is_structural_or_track_scoped(key: &str) -> bool {
    matches!(
        key,
        "cuesheet"
            | "title"
            | "tracktitle"
            | "tracknumber"
            | "track"
            | "tracktotal"
            | "totaltracks"
            | "musicbrainztrackid"
            | "musicbrainzrecordingid"
            | "musicbrainzreleasetrackid"
    ) || key.starts_with("replaygaintrack")
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
    image_is_track_unique: bool,
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

    let performer = if let Some(value) = cue_track
        .performer
        .clone()
        .or_else(|| sheet.performer.clone())
    {
        MetadataValueList::from_scalar(value)
    } else if !image.performer.is_empty() {
        image.performer.clone()
    } else if !image.artist.is_empty() {
        image.artist.clone()
    } else {
        image.album_artist.clone()
    };
    let album_artist = if let Some(value) = sheet.performer.clone() {
        MetadataValueList::from_scalar(value)
    } else if !image.album_artist.is_empty() {
        image.album_artist.clone()
    } else {
        image.artist.clone()
    };
    let genre = if let Some(value) = sheet.genre.clone() {
        MetadataValueList::from_scalar(value)
    } else {
        image.genre.clone()
    };

    TrackMetadata {
        title: cue_track.title.clone(),
        artist: performer.clone(),
        album_artist,
        composer: image.composer.clone(),
        performer,
        arranger: image.arranger.clone(),
        genre,
        date: sheet.date.clone().or_else(|| image.date.clone()),
        track_number: Some(numbering.output_number),
        disc_number: None,
        isrc: cue_track
            .isrc
            .clone()
            .or_else(|| image_is_track_unique.then(|| image.isrc.clone()).flatten()),
        publisher: image.publisher.clone(),
        copyright: image.copyright.clone(),
        comment: image.comment.clone(),
        pre_emphasis,
        extra,
    }
}

/// Canonical CUE-sheet-to-track-metadata mapping for conversion planning and
/// already-split sidecar transfer. Keeping this wrapper beside the CueImage
/// materializer prevents queue/planning code from maintaining a second field
/// mapping. `pre_emphasis` comes from the raw-CUE annotation pass when that
/// text is available.
pub(crate) fn cue_sheet_track_metadata_for_conversion(
    sheet: &CueSheet,
    track_index: usize,
    pre_emphasis: bool,
) -> Option<TrackMetadata> {
    let cue_track = sheet.tracks.get(track_index)?;
    let numbering = cue_track_number_plan(sheet).get(track_index).copied()?;
    Some(cue_track_metadata(
        cue_track,
        sheet,
        &ImageAlbumMetadata::default(),
        false,
        pre_emphasis,
        numbering,
    ))
}

fn cue_album_metadata(
    sheet: &CueSheet,
    image: &ImageAlbumMetadata,
    total_tracks: u32,
) -> AlbumMetadata {
    let mut extra = BTreeMap::new();
    for (key, value) in &image.extra {
        extra.entry(key.clone()).or_insert_with(|| value.clone());
    }
    if let Some(catalog) = &sheet.catalog {
        extra.insert("catalog".to_string(), catalog.clone());
        extra.insert("catalognumber".to_string(), catalog.clone());
    }
    if let Some(source) = &image.source {
        extra.insert("image_metadata_source".to_string(), source.clone());
    }

    let album_artist = if let Some(value) = sheet.performer.clone() {
        MetadataValueList::from_scalar(value)
    } else if !image.album_artist.is_empty() {
        image.album_artist.clone()
    } else {
        image.artist.clone()
    };
    let genre = if let Some(value) = sheet.genre.clone() {
        MetadataValueList::from_scalar(value)
    } else {
        image.genre.clone()
    };

    AlbumMetadata {
        album: sheet.title.clone().or_else(|| image.album.clone()),
        album_artist,
        genre,
        date: sheet.date.clone().or_else(|| image.date.clone()),
        total_tracks,
        total_discs: image.total_discs,
        disc_number: image.disc_number,
        extra,
    }
}

/// Read metadata for one already-split carrier from the exact sidecar-CUE
/// mapping transferred by queue expansion. This deliberately reuses the same
/// CUE -> `TrackMetadata` / `AlbumMetadata` mapping as the CueImage
/// materializer. It does not resolve the FILE token to choose a carrier again;
/// queue admission already made that association. Instead, the captured track
/// number and FILE token are change detectors so a materially edited sidecar
/// fails closed rather than silently applying metadata to the wrong file.
pub(crate) fn metadata_for_transferred_sidecar_cue_track(
    source: &SidecarCueTrackMetadataSource,
) -> Result<(TrackMetadata, AlbumMetadata), MaterializeError> {
    let raw_cue = read_cue_text(&source.cue_path)?;
    let sheet = parse_cue(&raw_cue);
    let cue_track = sheet.tracks.get(source.track_index).ok_or_else(|| {
        MaterializeError::Parse(format!(
            "sidecar CUE '{}' changed after queue admission: track position {} no longer exists",
            source.cue_path.display(),
            source.track_index + 1,
        ))
    })?;
    if cue_track.number != source.cue_track_number
        || cue_track.file.as_ref() != source.cue_file_reference.as_ref()
    {
        return Err(MaterializeError::Parse(format!(
            "sidecar CUE '{}' changed after queue admission at track position {} (expected TRACK {} FILE {:?}, found TRACK {} FILE {:?})",
            source.cue_path.display(),
            source.track_index + 1,
            source.cue_track_number,
            source.cue_file_reference,
            cue_track.number,
            cue_track.file,
        )));
    }

    let annotations = CueAnnotations::parse(&raw_cue);
    let mut track_metadata = cue_sheet_track_metadata_for_conversion(
        &sheet,
        source.track_index,
        annotations.track_pre_emphasis(cue_track.number),
    )
    .ok_or_else(|| MaterializeError::Parse("CUE metadata mapping lost admitted track".to_string()))?;
    annotations.add_track_extras(cue_track.number, &mut track_metadata.extra);

    let image_metadata = ImageAlbumMetadata::default();
    let mut album_metadata = cue_album_metadata(&sheet, &image_metadata, sheet.tracks.len() as u32);
    annotations.add_album_extras(&mut album_metadata.extra);
    Ok((track_metadata, album_metadata))
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
    // Source-admission gate: CUE image resolution must follow the same
    // classifier as Browse and queue expansion. Do not maintain an
    // independent extension table here.
    crate::convert::classify::is_audio_file_path(path)
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

    #[allow(dead_code)]
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
    "codec_name": "flac",
    "sample_rate": "{sample_rate}",
    "duration_ts": {total_samples},
    "time_base": "{time_base}",
    "bits_per_raw_sample": "{bit_depth}"
  }}],
  "format": {{}}
}}"#
        )
    }

    #[test]
    fn cue_probe_preserves_float32_sample_class_for_source_resolution() {
        let probe = parse_audio_probe_json(
            r#"{
  "streams": [{
    "codec_name": "pcm_f32le",
    "sample_fmt": "flt",
    "sample_rate": "96000",
    "duration_ts": 96000,
    "time_base": "1/96000",
    "bits_per_raw_sample": "32"
  }],
  "format": { "format_name": "wav" }
}"#,
        )
        .expect("parse float32 CUE image probe");

        assert_eq!(probe.bit_depth, Some(320));
        assert_eq!(
            super::super::plan_bridge::pcm_bit_depth_from_source_bits(
                probe.bit_depth.expect("source depth descriptor")
            ),
            Some(tonepoet_pipeline::PcmBitDepth::Float32)
        );
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
                let _ = std::fs::write(Path::new(destination), bytes);
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
                    environment_policy: cmd.environment_policy,
                    environment: cmd.sanitized_environment(),
                    binary: command_binary,
                    sanitized_args: cmd.args.clone(),
                    cwd: cmd.cwd.clone(),
                    env_keys: cmd.env_keys(),
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

    pub(super) fn test_request(container: &Path) -> PipelineRequest {
        PipelineRequest {
            job_id: "test-job".to_string(),
            actions: crate::convert::pipeline::ActionPipeline::default(),
            item_id: "test-item".to_string(),
            container: container.to_path_buf(),
            source: SourceOptions {
                archive_password: None,
                sacd_area: None,
                dvda_group: None,
                dvda_group_selection: DvdaGroupSelection::Default,
                dvda_assume_decrypted: false,
                dvda_downmix_policy: DvdaDownmixPolicy::Auto,
                dvdv_vts: None,
                dvdv_title: None,
                dvdv_audio_stream: None,
                dvdv_angle: None,
                bluray_playlist: None,
                bluray_audio_pid: None,
                bluray_audio_stream: None,
                bluray_angle: None,
                sidecar_cue_track_metadata: None,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection: TrackSelection::All,
            },
            settings: tonepoet_pipeline::PipelineSettings::default(),
            worker_count: None,
            scratch_staging: None,
            merge: false,
            output_root: container.parent().unwrap_or(Path::new(".")).to_path_buf(),
            naming: NamingPolicy {
                template: "%NN% - %TITLE%".to_string(),
                folder_template: None,
                per_album_subdir: true,
                collision_policy: NamingCollisionPolicy::Fail,
            windows_portable: false,
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
                write_conversion_log: true,
            },
            stages: StagePolicy {
                metadata: StageRequirement::Enabled,
                replaygain: StageRequirement::Disabled,
                features: StageRequirement::Disabled,
                generate_cue: false,
            },
            failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
            album_batch: None,
            album_batch_track: None,
            suppress_incremental_conversion_log_append: false,
            companion: Default::default(),
            pre_extracted_staging: None,
            archive_metadata_overrides: Vec::new(),
            metadata_overrides: Default::default(),
            batch_resolved_identity: None,
            expected_album_track_count: None,
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
                let _ = std::fs::write(Path::new(destination), b"RIFF-staged-cue-segment");
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
                    environment_policy: cmd.environment_policy,
                    environment: cmd.sanitized_environment(),
                    binary: command_binary,
                    sanitized_args: cmd.args.clone(),
                    cwd: cmd.cwd.clone(),
                    env_keys: cmd.env_keys(),
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
        assert!(cue_segment_atrim_filter(100, Some(0)).is_err());
        assert!(cue_segment_atrim_filter(u64::MAX, Some(1)).is_err());
        // Open-ended (lossy tail) has no end_sample and no zero-length guard.
        assert_eq!(
            cue_segment_atrim_filter(100, None).unwrap(),
            "atrim=start_sample=100,asetpts=N/SR/TB"
        );
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

    #[test]
    fn segment_destination_with_samples_rewrites_only_the_expected_suffix() {
        let provisional = Path::new("staging/cue-segments/001-cue01-track01-s0-n17280.wav");
        assert_eq!(
            segment_destination_with_samples(provisional, 17_280, 15_435),
            Path::new("staging/cue-segments/001-cue01-track01-s0-n15435.wav")
        );
        // Equal counts keep the provisional path (exact segments: no-op).
        assert_eq!(
            segment_destination_with_samples(provisional, 17_280, 17_280),
            provisional
        );
        // Arbitrary names (test shims) are kept as-is.
        let arbitrary = Path::new("staging/cue-segments/001.wav");
        assert_eq!(
            segment_destination_with_samples(arbitrary, 17_280, 15_435),
            arbitrary
        );
    }

    #[test]
    fn lossy_tail_shortfall_limit_stays_meaningful_for_short_tails() {
        assert_eq!(lossy_tail_shortfall_limit(44_100, 4_000), 1_000);
        assert_eq!(lossy_tail_shortfall_limit(44_100, 17_280), 4_320);
        assert_eq!(lossy_tail_shortfall_limit(96_000, 100_000), 11_520);
    }

    #[test]
    fn lossy_final_track_is_admitted_when_header_duration_understates_index() {
        let image = PathBuf::from("album.mp3");
        let sheet = parse_cue(
            "FILE \"album.mp3\" MP3\n  TRACK 01 AUDIO\n    INDEX 01 00:01:00\n",
        );
        let mut probes = HashMap::new();
        probes.insert(
            path_identity(&image),
            AudioProbe {
                sample_rate: 44_100,
                total_samples: 30_000,
                exact_samples: false,
                bit_depth: None,
                coding: SourceAudioCoding::Lossy,
                codec_name: Some("mp3".to_string()),
                format_name: Some("mp3".to_string()),
            },
        );

        let bounds = compute_track_boundaries_for_layout(&sheet, &[image.clone()], &probes)
            .expect("lossy final track is admitted for open-ended decode");
        assert_eq!(bounds.len(), 1);
        assert_eq!(bounds[0].start_sample, 44_100);
        assert_eq!(bounds[0].samples, 1);
        assert!(bounds[0].is_image_tail);

        probes.get_mut(&path_identity(&image)).unwrap().coding = SourceAudioCoding::Pcm;
        let error = compute_track_boundaries_for_layout(&sheet, &[image], &probes)
            .expect_err("lossless duration remains authoritative");
        assert!(error.to_string().contains("starts beyond image duration"));
    }

    #[tokio::test]
    async fn lossy_tail_shortfall_within_bound_backfills_measured_facts() {
        let temp = tempfile::tempdir().expect("temp dir");
        let provisional = temp
            .path()
            .join("staging/cue-segments/001-cue01-track01-s0-n17280.wav");
        let measured_name = temp
            .path()
            .join("staging/cue-segments/001-cue01-track01-s0-n15435.wav");
        // MP3 delay/padding class: header 17280, decode 15435 (< 100ms short).
        let measured_probe = ffprobe_json_staged_segment(44_100, 15_435);
        let runner = stub_runner_with_expected_probes(vec![
            expected_temp_ffprobe_for(&provisional, measured_probe.clone()),
            expected_ffprobe(&measured_name, measured_probe),
        ]);
        let cancel = CancellationToken::new();

        let staged = stage_cue_segment_as_wav(
            Path::new("album.mp3"),
            0,
            17_280,
            44_100,
            CueSegmentCarrier::PcmS32LeWav,
            &provisional,
            SegmentLengthPolicy::LossyTail,
            &runner,
            &cancel,
        )
        .await
        .expect("lossy tail shortfall within the bound is accepted");

        assert_eq!(staged.path, measured_name);
        assert_eq!(staged.samples, 15_435);
        assert!(measured_name.exists(), "published under the measured name");
        assert!(!provisional.exists(), "no file under the header-derived name");
    }

    #[tokio::test]
    async fn lossy_tail_shortfall_beyond_bound_fails_closed() {
        let temp = tempfile::tempdir().expect("temp dir");
        let provisional = temp
            .path()
            .join("staging/cue-segments/001-cue01-track01-s0-n200000.wav");
        // Way past the ~100ms/8192-sample limit: a genuinely truncated source.
        let runner = stub_runner_with_expected_probes(vec![expected_temp_ffprobe_for(
            &provisional,
            ffprobe_json_staged_segment(44_100, 100_000),
        )]);
        let cancel = CancellationToken::new();

        let err = stage_cue_segment_as_wav(
            Path::new("album.mp3"),
            0,
            200_000,
            44_100,
            CueSegmentCarrier::PcmS32LeWav,
            &provisional,
            SegmentLengthPolicy::LossyTail,
            &runner,
            &cancel,
        )
        .await
        .expect_err("shortfall beyond the bound must fail closed");
        let message = err.to_string();
        assert!(message.contains("appears truncated"), "{message}");
    }

    #[tokio::test]
    async fn short_lossy_tail_cannot_hide_major_truncation_under_fixed_codec_allowance() {
        let temp = tempfile::tempdir().expect("temp dir");
        let provisional = temp
            .path()
            .join("staging/cue-segments/001-cue01-track01-s0-n4000.wav");
        let runner = stub_runner_with_expected_probes(vec![expected_temp_ffprobe_for(
            &provisional,
            ffprobe_json_staged_segment(44_100, 2_500),
        )]);
        let cancel = CancellationToken::new();

        let error = stage_cue_segment_as_wav(
            Path::new("short-tail.mp3"),
            0,
            4_000,
            44_100,
            CueSegmentCarrier::PcmS32LeWav,
            &provisional,
            SegmentLengthPolicy::LossyTail,
            &runner,
            &cancel,
        )
        .await
        .expect_err("a 37.5% shortfall in a short tail must fail closed");
        let message = error.to_string();
        assert!(message.contains("short-tail.mp3"), "{message}");
        assert!(message.contains("limit 1000"), "{message}");
    }

    #[tokio::test]
    async fn lossy_tail_overage_is_kept_and_backfilled() {
        let temp = tempfile::tempdir().expect("temp dir");
        let provisional = temp
            .path()
            .join("staging/cue-segments/001-cue01-track01-s0-n17280.wav");
        let measured_name = temp
            .path()
            .join("staging/cue-segments/001-cue01-track01-s0-n80000.wav");
        // Xing-less VBR: header understates; the full decode is the fact.
        let measured_probe = ffprobe_json_staged_segment(44_100, 80_000);
        let runner = stub_runner_with_expected_probes(vec![
            expected_temp_ffprobe_for(&provisional, measured_probe.clone()),
            expected_ffprobe(&measured_name, measured_probe),
        ]);
        let cancel = CancellationToken::new();

        let staged = stage_cue_segment_as_wav(
            Path::new("album.mp3"),
            0,
            17_280,
            44_100,
            CueSegmentCarrier::PcmS32LeWav,
            &provisional,
            SegmentLengthPolicy::LossyTail,
            &runner,
            &cancel,
        )
        .await
        .expect("header-understating decode keeps the full audio");
        assert_eq!(staged.samples, 80_000);
        assert_eq!(staged.path, measured_name);
    }

    #[tokio::test]
    async fn exact_segments_keep_the_strict_count_failure() {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp
            .path()
            .join("staging/cue-segments/001-cue01-track01-s0-n17280.wav");
        let runner = stub_runner_with_expected_probes(vec![expected_temp_ffprobe_for(
            &destination,
            ffprobe_json_staged_segment(44_100, 15_435),
        )]);
        let cancel = CancellationToken::new();

        let err = stage_cue_segment_as_wav(
            Path::new("album.flac"),
            0,
            17_280,
            44_100,
            CueSegmentCarrier::PcmS32LeWav,
            &destination,
            SegmentLengthPolicy::Exact,
            &runner,
            &cancel,
        )
        .await
        .expect_err("exact segments must not tolerate any count mismatch");
        let message = err.to_string();
        assert!(message.contains("album.flac"), "{message}");
        assert!(message.contains("decoded 15435 samples"), "{message}");
        assert!(message.contains("expected 17280"), "{message}");
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
            let SegmentBounds {
                start_sample,
                samples,
                ..
            } = boundaries[idx];
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
        assert_eq!(source.tracks[0].sample_rate, Some(44100));
        assert_eq!(source.tracks[0].bit_depth, Some(16));
        assert_eq!(source.tracks[0].expected_samples, Some(3_969_000));
    }

    #[tokio::test]
    async fn synthetic_multifile_cue_materializes_as_one_prepared_album_source() {
        let temp = tempfile::tempdir().expect("temp dir");
        let samples_per_image: u64 = 4_410_000;
        let probe_a = ffprobe_json_exact(44100, samples_per_image, 16);
        let probe_b = ffprobe_json_exact(44100, samples_per_image, 16);
        let cue = r#"PERFORMER "Artist"
TITLE "Album"
FILE "side_a.flac" WAVE
  TRACK 01 AUDIO
    TITLE "A1"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "A2"
    INDEX 01 00:30:00
FILE "side_b.flac" WAVE
  TRACK 03 AUDIO
    TITLE "B1"
    INDEX 01 00:00:00
  TRACK 04 AUDIO
    TITLE "B2"
    INDEX 01 00:30:00
"#;

        let source = materialize_cue_with_audio_files(
            cue,
            &[probe_a.as_str(), probe_b.as_str()],
            &["side_a.flac", "side_b.flac"],
            &temp,
        )
        .await
        .expect("synthetic multi-FILE CUE materializes");

        assert_eq!(source.kind, SourceKind::CueImage);
        assert_eq!(source.tracks.len(), 4, "one synthetic CUE must produce one album source with every track");
        assert_eq!(source.album_metadata.album.as_deref(), Some("Album"));
        assert_eq!(source.album_metadata.album_artist.as_deref(), Some("Artist"));
        assert_eq!(source.album_metadata.total_tracks, 4);
        assert_eq!(
            source
                .tracks
                .iter()
                .map(|track| track.id.track_number)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4],
        );
        assert_eq!(source.tracks[0].metadata.title.as_deref(), Some("A1"));
        assert_eq!(source.tracks[3].metadata.title.as_deref(), Some("B2"));
        assert!(
            source
                .tracks
                .iter()
                .all(|track| track.metadata.extra.get("album").map(String::as_str) == Some("Album")),
            "downstream naming/log/companion code must see one reconciled album, not side titles"
        );

        let album_values: BTreeSet<String> = source
            .tracks
            .iter()
            .filter_map(|track| track.metadata.extra.get("album").cloned())
            .collect();
        assert_eq!(album_values.into_iter().collect::<Vec<_>>(), vec!["Album".to_string()]);
        assert!(
            source
                .tracks
                .iter()
                .all(|track| !track.metadata.extra.values().any(|value| value.contains("Side A") || value.contains("Side B"))),
            "the materialized source handed to downstream output planning must not carry side album identity"
        );
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
        let _ = std::fs::write(&cue_path, cue_sheet_3_track().as_bytes());
        let _ = std::fs::write(&temp.path().join("album.flac"), b"fake-audio-data");

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
        let _ = std::fs::write(&flac, b"fake flac");
        let _ = std::fs::write(&wav, b"fake wav");

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
        let _ = std::fs::write(&image, b"fake flac");

        let resolved = resolve_audio_reference(Some(temp.path()), "disc/image.wav", None)
            .expect("extension mismatch should resolve inside referenced subdirectory");
        assert_eq!(resolved, image);
    }

    #[test]
    fn sidecar_discovery_ignores_one_track_per_file_album_cue_for_listed_tracks() {
        // A one-track-per-FILE album CUE lists split tracks; discovering it
        // for a listed track would decompose the whole album once per queued
        // track (the Kansas incident). Scan-based discovery requires the CUE
        // to SUBDIVIDE the queried file (map two or more tracks to it).
        let temp = tempfile::tempdir().expect("temp dir");
        let cue_path = temp.path().join("album.cue");
        let track1 = temp.path().join("track1.flac");
        let track2 = temp.path().join("track2.flac");
        let _ = std::fs::write(
            &cue_path,
            br#"FILE "track1.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
FILE "track2.flac" WAVE
  TRACK 02 AUDIO
    INDEX 01 00:00:00
"#,
        );
        let _ = std::fs::write(&track1, b"fake-audio-data");
        let _ = std::fs::write(&track2, b"fake-audio-data");

        let discovered =
            find_valid_sidecar_cue_for_image(&track2).expect("sidecar search succeeds");
        assert_eq!(
            discovered, None,
            "a split track listed once in an album CUE must not adopt that CUE"
        );
    }

    #[test]
    fn sidecar_discovery_ignores_hidden_dot_cue_when_visible_matching_cue_exists() {
        // Hidden dot-cues are filesystem sidecars (for example AppleDouble
        // ._album.cue) and must not participate in CUE route detection. A
        // valid hidden CUE that also subdivides the image must not make the
        // visible sidecar ambiguous or win discovery.
        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("album.flac");
        let visible_cue = temp.path().join("visible.cue");
        let hidden_cue = temp.path().join("._album.cue");
        let _ = std::fs::write(&image, b"fake-audio-data");
        let cue_text = br#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:00:00
"#;
        let _ = std::fs::write(&visible_cue, cue_text);
        let _ = std::fs::write(&hidden_cue, cue_text);

        let route = sidecar_cue_route_candidate(&image)
            .expect("hidden dot-cue must not make route detection ambiguous")
            .expect("visible subdividing CUE should be discovered");
        assert_eq!(route, visible_cue);

        let materializer_cue = find_valid_sidecar_cue_for_image(&image)
            .expect("hidden dot-cue must not make materializer discovery ambiguous")
            .expect("visible subdividing CUE should be discovered");
        assert_eq!(materializer_cue, visible_cue);

        let req = test_request(&image);
        assert!(
            is_cue_image_candidate(&req).expect("candidate detection succeeds"),
            "visible CUE should still select the CUE materializer route"
        );
    }

    #[test]
    fn sidecar_discovery_matches_side_image_subdivided_by_multi_file_cue() {
        // The legitimate multi-FILE case: per-side images where the CUE maps
        // several tracks to each file. Scan-based discovery still applies.
        let temp = tempfile::tempdir().expect("temp dir");
        let cue_path = temp.path().join("album.cue");
        let side_a = temp.path().join("side-a.flac");
        let side_b = temp.path().join("side-b.flac");
        let _ = std::fs::write(
            &cue_path,
            br#"FILE "side-a.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:00:00
FILE "side-b.flac" WAVE
  TRACK 03 AUDIO
    INDEX 01 00:00:00
  TRACK 04 AUDIO
    INDEX 01 04:00:00
"#,
        );
        let _ = std::fs::write(&side_a, b"fake-audio-data");
        let _ = std::fs::write(&side_b, b"fake-audio-data");

        let discovered = find_valid_sidecar_cue_for_image(&side_a)
            .expect("sidecar search succeeds")
            .expect("a CUE subdividing the queried image is discovered");
        assert_eq!(discovered, cue_path);
    }

    #[test]
    fn malformed_same_stem_sidecar_does_not_route_audio_to_cue_materializer() {
        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("album.flac");
        let cue_path = temp.path().join("album.cue");
        let _ = std::fs::write(&image, b"fake-audio-data");
        let _ = std::fs::write(
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
        let _ = std::fs::write(&image, b"fake-audio-data");
        let _ = std::fs::write(&other, b"fake-audio-data");
        let _ = std::fs::write(
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
        let _ = std::fs::write(&image, b"fake-audio-data");
        let _ = std::fs::write(
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
        assert_eq!(source.tracks[0].sample_rate, Some(96000));
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
        assert_eq!(source.tracks[0].sample_rate, Some(192000));
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
        image.album_artist = MetadataValueList::from_scalar("Image Album Artist");
        image.artist = MetadataValueList::from_scalar("Image Artist");
        image.genre = MetadataValueList::from_scalar("Image Genre");
        image.date = Some("1984".to_string());
        image.disc_number = Some(1);
        image.total_discs = Some(2);

        let sheet = parse_cue(
            "PERFORMER \"Cue Artist\"\nTITLE \"Cue Album\"\nFILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n",
        );

        let numbering = cue_track_number_plan(&sheet);
        let track = cue_track_metadata(&sheet.tracks[0], &sheet, &image, true, false, numbering[0]);
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


    pub(super) fn fixture_tool_available(tool: &str) -> bool {
        std::process::Command::new(tool)
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    pub(super) fn run_fixture_command(command: &mut std::process::Command) {
        let output = command.output().expect("spawn fixture command");
        assert!(
            output.status.success(),
            "fixture command failed: status={:?}\nstdout={}\nstderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    pub(super) fn write_lofty_tags(path: &Path, tags: &[(&str, &str)]) {
        use lofty::config::WriteOptions;
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::tag::{ItemKey, ItemValue, TagItem};

        let mut tagged = lofty::read_from_path(path)
            .unwrap_or_else(|err| panic!("failed to read {} with lofty: {err}", path.display()));
        if tagged.primary_tag().is_none() {
            let tag_type = tagged.primary_tag_type();
            tagged.insert_tag(lofty::tag::Tag::new(tag_type));
        }
        let tag = tagged
            .primary_tag_mut()
            .unwrap_or_else(|| panic!("failed to create primary tag for {}", path.display()));

        for (key, value) in tags {
            let item_key = ItemKey::Unknown((*key).to_string());
            tag.remove_key(&item_key);
            tag.insert_unchecked(TagItem::new(
                item_key,
                ItemValue::Text((*value).to_string()),
            ));
        }

        tagged
            .save_to_path(path, WriteOptions::default())
            .unwrap_or_else(|err| panic!("failed to save {} with lofty: {err}", path.display()));
    }

    pub(super) fn write_lofty_cuesheet(path: &Path, cue: &str) {
        write_lofty_tags(path, &[("CUESHEET", cue)]);
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

            let binary_path = crate::convert::pipeline::tool::resolve_command_launch_path(
                PathBuf::from(binary_name),
                cmd.environment_policy,
            );
            let mut process = std::process::Command::new(&binary_path);
            process.args(&cmd.args);
            if let Some(cwd) = &cmd.cwd {
                process.current_dir(cwd);
            }
            if cmd.environment_policy
                == tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
            {
                process.env_clear();
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
            let environment_policy = cmd.environment_policy;
            let environment = cmd.sanitized_environment();
            let env_keys = cmd.env_keys();
            Ok(ToolOutput {
                exit: crate::convert::pipeline::tool::ProcessExit::Code(exit_code),
                stdout_tail: stdout_tail.clone(),
                stderr_tail: stderr_tail.clone(),
                elapsed: Duration::from_millis(0),
                command: crate::convert::pipeline::tool::CommandRecord {
                    environment_policy,
                    environment,
                    binary: cmd.binary,
                    sanitized_args: cmd.args,
                    cwd: cmd.cwd,
                    env_keys,
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

    #[tokio::test]
    async fn real_ffmpeg_staging_preserves_float32_carrier_class_when_available() {
        if !fixture_tool_available("ffmpeg") || !fixture_tool_available("ffprobe") {
            eprintln!(
                "skipping real float CUE segment staging fixture: ffmpeg or ffprobe is unavailable"
            );
            return;
        }

        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("source-f32.wav");
        let destination = temp
            .path()
            .join("staging/cue-segments/track01-f32-s0-n22050.wav");

        run_fixture_command(
            std::process::Command::new("ffmpeg")
                .arg("-y")
                .arg("-hide_banner")
                .arg("-loglevel")
                .arg("error")
                .arg("-f")
                .arg("lavfi")
                .arg("-i")
                .arg("anullsrc=r=44100:cl=mono:d=1")
                .arg("-c:a")
                .arg("pcm_f32le")
                .arg(&image),
        );

        let runner = RealProcessToolRunner;
        let cancel = CancellationToken::new();
        let carrier = CueSegmentCarrier::PcmF32LeWav;
        stage_cue_segment_as_wav(
            &image,
            0,
            22_050,
            44_100,
            carrier,
            &destination,
            SegmentLengthPolicy::Exact,
            &runner,
            &cancel,
        )
        .await
        .expect("real ffmpeg stages a Float32 CUE segment without integer quantization");

        validate_staged_cue_segment_as(
            &destination,
            44_100,
            22_050,
            carrier,
            &runner,
            &cancel,
        )
        .await
        .expect("real ffprobe validates staged pcm_f32le WAV sample count and class");

        let probe = probe_staged_cue_segment(&destination, &runner, &cancel)
            .await
            .expect("probe staged Float32 carrier");
        assert_eq!(probe.codec_name.as_deref(), Some("pcm_f32le"));
        assert_eq!(probe.bit_depth, Some(320));
    }

    fn split_track_album_cue_fixture(dir: &Path, ref_ext: &str) -> Vec<std::path::PathBuf> {
        // Kansas-shape layout: individual track files plus ONE album-level
        // noncompliant CUE that lists every track as its own FILE entry.
        let titles = ["01-Alpha", "02-Beta", "03-Gamma"];
        let mut cue = String::new();
        let mut files = Vec::new();
        for (idx, title) in titles.iter().enumerate() {
            let audio = dir.join(format!("{title}.flac"));
            std::fs::write(&audio, b"not-a-real-flac").expect("track fixture");
            cue.push_str(&format!(
                "FILE \"{title}.{ref_ext}\" WAVE\n  TRACK {:02} AUDIO\n  TITLE \"{title}\"\n  INDEX 01 00:00:00\n",
                idx + 1
            ));
            files.push(audio);
        }
        std::fs::write(dir.join("album.cue"), cue.as_bytes()).expect("album cue");
        files
    }

    #[test]
    fn split_track_listed_in_album_cue_is_not_a_cue_image_candidate() {
        // Regression: a noncompliant multi-FILE album CUE (one track per FILE)
        // references every split track; each queued track must stay on the
        // legacy single-file path, NOT decompose the whole album per item.
        let temp = tempfile::tempdir().expect("temp dir");
        let files = split_track_album_cue_fixture(temp.path(), "flac");

        for audio in &files {
            let mut req = test_request(audio);
            req.source.cue_sidecar = CueSidecarPolicy::PreferEmbedded;
            let candidate = is_cue_image_candidate(&req).expect("candidacy check");
            assert!(
                !candidate,
                "{} is a split track listed in an album CUE and must not route as a CUE image",
                audio.display()
            );
        }
    }

    #[test]
    fn split_track_listed_in_wav_referencing_album_cue_is_not_a_cue_image_candidate() {
        // Same layout but the CUE references .wav names while files are .flac
        // (the common EAC noncompliant-cue artifact); stem resolution must not
        // turn split tracks into whole-album CUE images either.
        let temp = tempfile::tempdir().expect("temp dir");
        let files = split_track_album_cue_fixture(temp.path(), "wav");

        for audio in &files {
            let mut req = test_request(audio);
            req.source.cue_sidecar = CueSidecarPolicy::PreferEmbedded;
            let candidate = is_cue_image_candidate(&req).expect("candidacy check");
            assert!(
                !candidate,
                "{} is a split track listed in a wav-referencing album CUE and must not route as a CUE image",
                audio.display()
            );
        }
    }

    #[test]
    fn genuine_single_image_cue_remains_a_cue_image_candidate() {
        // Control: one image subdivided into multiple tracks stays on the CUE
        // pipeline under the same policy.
        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("album-image.flac");
        std::fs::write(&image, b"not-a-real-flac").expect("image fixture");
        std::fs::write(
            temp.path().join("album-image.cue"),
            br#"FILE "album-image.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:00:00
"#,
        )
        .expect("image cue");

        let mut req = test_request(&image);
        req.source.cue_sidecar = CueSidecarPolicy::PreferEmbedded;
        assert!(is_cue_image_candidate(&req).expect("candidacy check"));
    }

    #[test]
    fn prefer_embedded_falls_back_to_sidecar_only_when_embedded_absent() {
        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("album.flac");
        let cue_path = temp.path().join("album.cue");
        let _ = std::fs::write(&image, b"not-a-real-flac-with-no-readable-tags");
        let _ = std::fs::write(
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
        if !fixture_tool_available("ffmpeg") {
            eprintln!("skipping malformed embedded CUESHEET fixture: ffmpeg unavailable");
            return;
        }

        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("album.flac");
        let embedded_cue = temp.path().join("embedded-malformed.cue");
        let sidecar_cue = temp.path().join("album.cue");

        let _ = std::fs::write(
            &embedded_cue,
            br#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Malformed embedded CUE with no index"
"#,
        );
        let _ = std::fs::write(
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
        let embedded = std::fs::read_to_string(&embedded_cue).expect("embedded cue text");
        write_lofty_cuesheet(&image, &embedded);

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
        if !fixture_tool_available("ffmpeg") {
            eprintln!("skipping real FLAC/lofty fixture: ffmpeg unavailable");
            return;
        }

        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("lofty-image.flac");
        let cue = r#"PERFORMER "Embedded Cue Artist"
TITLE "Embedded Cue Album"
FILE "lofty-image.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Embedded Cue Track One"
    INDEX 01 00:00:00
"#;

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
        write_lofty_tags(
            &image,
            &[
                ("ALBUM", "Lofty Image Album"),
                ("ARTIST", "Lofty Image Artist"),
                ("ALBUMARTIST", "Lofty Image Album Artist"),
                ("DATE", "2026"),
                ("GENRE", "Lofty Fixture"),
                ("DISCNUMBER", "1"),
                ("TOTALDISCS", "2"),
                ("CATALOGNUMBER", "IMG-001"),
                ("RELEASECOUNTRY", "JP"),
                ("ORIGINALYEAR", "1973"),
                ("MUSICBRAINZ_ALBUMID", "mb-album"),
                ("MUSICBRAINZ_ALBUMARTISTID", "mb-album-artist"),
                ("MUSICBRAINZ_RELEASEGROUPID", "mb-release-group"),
                ("COMPOSER", "Rick Davies"),
                ("PERFORMER", "Supertramp"),
                ("ISRC", "USRC17607839"),
                ("PUBLISHER", "A&M Records"),
                ("COPYRIGHT", "1974 A&M Records"),
                ("COMMENT", "Japan first-press LP"),
                ("LINEAGE", "LP > ADC > WavPack"),
                ("DISCOGS_URL", "https://www.discogs.com/release/123"),
                ("MUSICBRAINZ_TRACKID", "mb-track-must-not-copy"),
                ("CUESHEET", cue),
            ],
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
        assert_eq!(image_metadata.extra.get("catalognumber").map(String::as_str), Some("IMG-001"));
        assert_eq!(image_metadata.extra.get("releasecountry").map(String::as_str), Some("JP"));
        assert_eq!(
            image_metadata
                .extra
                .get("originalyear")
                .or_else(|| image_metadata.extra.get("originaldate"))
                .map(String::as_str),
            Some("1973"),
            "lofty canonicalizes Vorbis ORIGINALYEAR to OriginalReleaseDate on read"
        );
        assert_eq!(image_metadata.extra.get("musicbrainz_albumid").map(String::as_str), Some("mb-album"));
        assert_eq!(image_metadata.extra.get("musicbrainz_albumartistid").map(String::as_str), Some("mb-album-artist"));
        assert_eq!(image_metadata.extra.get("musicbrainz_releasegroupid").map(String::as_str), Some("mb-release-group"));
        assert_eq!(image_metadata.composer.as_deref(), Some("Rick Davies"));
        assert_eq!(image_metadata.performer.as_deref(), Some("Supertramp"));
        assert_eq!(image_metadata.isrc.as_deref(), Some("USRC17607839"));
        assert_eq!(image_metadata.publisher.as_deref(), Some("A&M Records"));
        assert_eq!(image_metadata.copyright.as_deref(), Some("1974 A&M Records"));
        assert_eq!(image_metadata.comment.as_deref(), Some("Japan first-press LP"));
        assert_eq!(
            image_metadata
                .extra
                .get(&format!("{SOURCE_TEXT_TAG_EXTRA_PREFIX}lineage"))
                .map(String::as_str),
            Some("LP > ADC > WavPack"),
        );
        assert_eq!(
            image_metadata
                .extra
                .get(&format!("{SOURCE_TEXT_TAG_EXTRA_PREFIX}discogs_url"))
                .map(String::as_str),
            Some("https://www.discogs.com/release/123"),
        );
        assert!(!image_metadata.extra.contains_key("musicbrainz_trackid"), "per-track MusicBrainz IDs must not be copied as album metadata");

        let mut sheet = crate::tui::cue_parser::parse_cue(cue);
        sheet.catalog = Some("SHEET-CATALOG".to_string());
        let album = cue_album_metadata(&sheet, &image_metadata, 1);
        assert_eq!(album.extra.get("catalog").map(String::as_str), Some("SHEET-CATALOG"));
        assert_eq!(album.extra.get("catalognumber").map(String::as_str), Some("SHEET-CATALOG"), "CUE CATALOG is the authoritative catalog number when both sheet and image tags provide catalog data");
        assert_eq!(album.extra.get("releasecountry").map(String::as_str), Some("JP"));
        assert_eq!(
            album.extra.get("originalyear").or_else(|| album.extra.get("originaldate")).map(String::as_str),
            Some("1973"),
            "lofty canonicalizes Vorbis ORIGINALYEAR to OriginalReleaseDate on read"
        );
        assert_eq!(album.extra.get("musicbrainz_albumid").map(String::as_str), Some("mb-album"));

        let track = cue_track_metadata(
            &sheet.tracks[0],
            &sheet,
            &image_metadata,
            true,
            false,
            CueTrackNumberPlan {
                output_number: 1,
                cue_number: 1,
            },
        );
        let tags = crate::convert::pipeline::stages::authoritative_metadata_tags(&track, &album);
        let tag_value = |key: &str| {
            tags.iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(tag_value("COMMENT"), Some("Japan first-press LP"));
        assert_eq!(tag_value("COMPOSER"), Some("Rick Davies"));
        // CUE-sheet PERFORMER is authoritative (like CATALOG/TITLE); a distinct
        // image-tag PERFORMER does not override the sheet at track level.
        assert_eq!(tag_value("PERFORMER"), Some("Embedded Cue Artist"));
        assert_eq!(tag_value("ISRC"), Some("USRC17607839"));
        let shared_image_track = cue_track_metadata(
            &sheet.tracks[0],
            &sheet,
            &image_metadata,
            false,
            false,
            CueTrackNumberPlan {
                output_number: 1,
                cue_number: 1,
            },
        );
        assert_eq!(
            shared_image_track.isrc, None,
            "one image-level ISRC must not be broadcast across multiple CUE tracks",
        );
        assert_eq!(tag_value("PUBLISHER"), Some("A&M Records"));
        assert_eq!(tag_value("COPYRIGHT"), Some("1974 A&M Records"));
        assert_eq!(tag_value("LINEAGE"), Some("LP > ADC > WavPack"));
        assert_eq!(
            tag_value("DISCOGS_URL"),
            Some("https://www.discogs.com/release/123"),
        );
        assert_eq!(tag_value("CUESHEET"), None);
        assert_eq!(tag_value("MUSICBRAINZ_TRACKID"), None);

        let embedded = read_embedded_cuesheet(&image)
            .expect("lofty read should succeed")
            .expect("embedded CUESHEET should exist");
        assert!(embedded.contains("Embedded Cue Album"));
        assert!(embedded.contains("Embedded Cue Track One"));
    }


    #[tokio::test]
    async fn cue_image_materializer_preserves_complete_repeated_flac_lists_when_tools_are_available() {
        let metaflac_available = std::process::Command::new("metaflac")
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !fixture_tool_available("ffmpeg")
            || !fixture_tool_available("ffprobe")
            || !metaflac_available
        {
            eprintln!(
                "skipping repeated CUE-image FLAC metadata fixture: ffmpeg/ffprobe/metaflac are required"
            );
            return;
        }

        let temp = tempfile::tempdir().expect("temp dir");
        let image = temp.path().join("album.flac");
        let cue_path = temp.path().join("album.cue");
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
        std::fs::write(
            &cue_path,
            br#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Track"
    INDEX 01 00:00:00
"#,
        )
        .expect("write minimal CUE sidecar");

        let mut metaflac = std::process::Command::new("metaflac");
        for key in ["ARTIST", "ALBUMARTIST", "COMPOSER", "PERFORMER", "ARRANGER", "GENRE"] {
            metaflac.arg(format!("--remove-tag={key}"));
        }
        for (key, value) in [
            ("ARTIST", "A"),
            ("ARTIST", "B"),
            ("ARTIST", "A"),
            ("ALBUMARTIST", "AA1"),
            ("ALBUMARTIST", "AA2"),
            ("ALBUMARTIST", "AA1"),
            ("COMPOSER", "C1"),
            ("COMPOSER", "C2"),
            ("COMPOSER", "C1"),
            ("PERFORMER", "P1"),
            ("PERFORMER", "P2"),
            ("PERFORMER", "P1"),
            ("ARRANGER", "R1"),
            ("ARRANGER", "R2"),
            ("ARRANGER", "R1"),
            ("GENRE", "G1"),
            ("GENRE", "G2"),
            ("GENRE", "G1"),
        ] {
            metaflac.arg(format!("--set-tag={key}={value}"));
        }
        metaflac.arg(&image);
        run_fixture_command(&mut metaflac);

        let image_metadata = read_image_album_metadata(&image);
        assert_eq!(image_metadata.artist.values(), &["A".to_string(), "B".to_string(), "A".to_string()]);
        assert_eq!(image_metadata.album_artist.values(), &["AA1".to_string(), "AA2".to_string(), "AA1".to_string()]);
        assert_eq!(image_metadata.composer.values(), &["C1".to_string(), "C2".to_string(), "C1".to_string()]);
        assert_eq!(image_metadata.performer.values(), &["P1".to_string(), "P2".to_string(), "P1".to_string()]);
        assert_eq!(image_metadata.arranger.values(), &["R1".to_string(), "R2".to_string(), "R1".to_string()]);
        assert_eq!(image_metadata.genre.values(), &["G1".to_string(), "G2".to_string(), "G1".to_string()]);

        let mut req = test_request(&image);
        req.settings.metadata.preserve_artwork = false;
        let mut staging = test_staging(&temp);
        let runner = RealProcessToolRunner;
        let prepared = CueImageMaterializer
            .materialize(
                &req,
                &staging,
                &runner,
                None,
                &HashMap::new(),
                &CancellationToken::new(),
            )
            .await
            .expect("real CUE image materialization with repeated metadata");
        staging.disarm();

        assert_eq!(prepared.tracks.len(), 1);
        let track = &prepared.tracks[0].metadata;
        // Existing CUE-image precedence treats image PERFORMER as the first
        // fallback for both track ARTIST and PERFORMER; preserve that rule but
        // retain the complete list.
        assert_eq!(track.artist.values(), &["P1".to_string(), "P2".to_string(), "P1".to_string()]);
        assert_eq!(track.performer.values(), &["P1".to_string(), "P2".to_string(), "P1".to_string()]);
        assert_eq!(track.arranger.values(), &["R1".to_string(), "R2".to_string(), "R1".to_string()]);
        assert_eq!(track.album_artist.values(), &["AA1".to_string(), "AA2".to_string(), "AA1".to_string()]);
        assert_eq!(track.composer.values(), &["C1".to_string(), "C2".to_string(), "C1".to_string()]);
        assert_eq!(track.genre.values(), &["G1".to_string(), "G2".to_string(), "G1".to_string()]);
        assert_eq!(prepared.album_metadata.album_artist.values(), &["AA1".to_string(), "AA2".to_string(), "AA1".to_string()]);
        assert_eq!(prepared.album_metadata.genre.values(), &["G1".to_string(), "G2".to_string(), "G1".to_string()]);
    }

    #[test]
    fn cue_performer_and_genre_scalar_overrides_replace_image_lists() {
        let image = ImageAlbumMetadata {
            album_artist: vec!["AA1".to_string(), "AA2".to_string()].into(),
            artist: vec!["A1".to_string(), "A2".to_string()].into(),
            genre: vec!["G1".to_string(), "G2".to_string()].into(),
            composer: vec!["C1".to_string(), "C2".to_string()].into(),
            performer: vec!["P1".to_string(), "P2".to_string()].into(),
            ..ImageAlbumMetadata::default()
        };
        let sheet = parse_cue(
            "PERFORMER \"Cue Performer\"\nREM GENRE \"Cue Genre\"\nFILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n",
        );
        let numbering = cue_track_number_plan(&sheet);
        let track = cue_track_metadata(&sheet.tracks[0], &sheet, &image, true, false, numbering[0]);
        let album = cue_album_metadata(&sheet, &image, 1);

        assert_eq!(track.artist.values(), &["Cue Performer".to_string()]);
        assert_eq!(track.performer.values(), &["Cue Performer".to_string()]);
        assert_eq!(track.album_artist.values(), &["Cue Performer".to_string()]);
        assert_eq!(track.genre.values(), &["Cue Genre".to_string()]);
        assert_eq!(track.composer.values(), &["C1".to_string(), "C2".to_string()]);
        assert_eq!(album.album_artist.values(), &["Cue Performer".to_string()]);
        assert_eq!(album.genre.values(), &["Cue Genre".to_string()]);
    }

    #[test]
    fn image_album_metadata_merge_keeps_complete_first_non_empty_lists() {
        let first = PathBuf::from("/album/side_a.flac");
        let second = PathBuf::from("/album/side_b.flac");
        let first_metadata = ImageAlbumMetadata {
            artist: vec!["A", "B", "A"].into_iter().map(str::to_string).collect::<Vec<_>>().into(),
            composer: vec!["C1", "C2", "C1"].into_iter().map(str::to_string).collect::<Vec<_>>().into(),
            ..ImageAlbumMetadata::default()
        };
        let second_metadata = ImageAlbumMetadata {
            artist: vec!["X".to_string(), "Y".to_string()].into(),
            composer: vec!["Z".to_string()].into(),
            genre: vec!["Second only".to_string(), "Second duplicate".to_string()].into(),
            ..ImageAlbumMetadata::default()
        };
        let mut by_image = HashMap::new();
        by_image.insert(path_identity(&first), first_metadata);
        by_image.insert(path_identity(&second), second_metadata);

        let merged = merge_image_album_metadata(&[first, second], &by_image);
        assert_eq!(merged.artist.values(), &["A".to_string(), "B".to_string(), "A".to_string()]);
        assert_eq!(merged.composer.values(), &["C1".to_string(), "C2".to_string(), "C1".to_string()]);
        assert_eq!(
            merged.genre.values(),
            &["Second only".to_string(), "Second duplicate".to_string()],
            "a later image may fill an empty field but must never combine lists across images",
        );
    }

    #[test]
    fn image_album_metadata_extra_merge_is_first_non_empty_and_conflict_safe() {
        let first = PathBuf::from("/album/side_a.flac");
        let second = PathBuf::from("/album/side_b.flac");
        let mut first_metadata = ImageAlbumMetadata::default();
        first_metadata.extra.insert("releasecountry".to_string(), "JP".to_string());
        first_metadata.extra.insert("originalyear".to_string(), "1973".to_string());
        let mut second_metadata = ImageAlbumMetadata::default();
        second_metadata.extra.insert("releasecountry".to_string(), "US".to_string());
        second_metadata.extra.insert("musicbrainz_albumid".to_string(), "mb-album".to_string());

        let mut by_image = HashMap::new();
        by_image.insert(path_identity(&first), first_metadata);
        by_image.insert(path_identity(&second), second_metadata);

        let merged = merge_image_album_metadata(&[first, second], &by_image);
        assert_eq!(merged.extra.get("releasecountry").map(String::as_str), Some("JP"));
        assert_eq!(merged.extra.get("originalyear").map(String::as_str), Some("1973"));
        assert_eq!(merged.extra.get("musicbrainz_albumid").map(String::as_str), Some("mb-album"));
    }

    #[test]
    fn album_extra_aliases_and_structural_exclusions_are_explicit() {
        assert_eq!(cue_image_extra_key(&normalize_tag_key("CATALOGNUMBER")), Some("catalognumber"));
        assert_eq!(cue_image_extra_key(&normalize_tag_key("RELEASECOUNTRY")), Some("releasecountry"));
        assert_eq!(cue_image_extra_key(&normalize_tag_key("MUSICBRAINZ_ALBUMID")), Some("musicbrainz_albumid"));
        assert!(!cue_image_tag_is_structural_or_track_scoped(&normalize_tag_key("COMMENT")));
        assert!(!cue_image_tag_is_structural_or_track_scoped(&normalize_tag_key("LINEAGE")));
        assert!(cue_image_tag_is_structural_or_track_scoped(&normalize_tag_key("CUESHEET")));
        assert!(cue_image_tag_is_structural_or_track_scoped(&normalize_tag_key("MUSICBRAINZ_TRACKID")));
        assert!(cue_image_tag_is_structural_or_track_scoped(&normalize_tag_key("MUSICBRAINZ_RELEASETRACKID")));
    }

    #[test]
    fn transferred_sidecar_cue_reuses_canonical_track_and_album_mapping() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue_path = temp.path().join("Thriller.cue");
        std::fs::write(
            &cue_path,
            r#"REM DATE 1984
REM GENRE "Pop"
CATALOG 4988005123999
PERFORMER "Michael Jackson"
TITLE "Thriller"
FILE "01 - Wanna Be Startin' Somethin'.dff" WAVE
  TRACK 01 AUDIO
    TITLE "Wanna Be Startin' Somethin'"
    PERFORMER "Michael Jackson"
    ISRC JPES08400001
    FLAGS PRE
    INDEX 01 00:00:00
"#,
        )
        .expect("cue fixture");
        let source = SidecarCueTrackMetadataSource {
            cue_path: cue_path.clone(),
            track_index: 0,
            cue_track_number: 1,
            cue_file_reference: Some("01 - Wanna Be Startin' Somethin'.dff".to_string()),
        };

        let (track, album) =
            metadata_for_transferred_sidecar_cue_track(&source).expect("transferred metadata");

        assert_eq!(track.title.as_deref(), Some("Wanna Be Startin' Somethin'"));
        assert_eq!(track.artist.as_deref(), Some("Michael Jackson"));
        assert_eq!(track.performer.as_deref(), Some("Michael Jackson"));
        assert_eq!(track.album_artist.as_deref(), Some("Michael Jackson"));
        assert_eq!(track.date.as_deref(), Some("1984"));
        assert_eq!(track.genre.as_deref(), Some("Pop"));
        assert_eq!(track.track_number, Some(1));
        assert_eq!(track.isrc.as_deref(), Some("JPES08400001"));
        assert!(track.pre_emphasis, "transferred FLAGS PRE must promote to pre_emphasis");
        assert_eq!(track.extra.get("album").map(String::as_str), Some("Thriller"));
        assert_eq!(track.extra.get("catalog").map(String::as_str), Some("4988005123999"));

        assert_eq!(album.album.as_deref(), Some("Thriller"));
        assert_eq!(album.album_artist.as_deref(), Some("Michael Jackson"));
        assert_eq!(album.date.as_deref(), Some("1984"));
        assert_eq!(album.genre.as_deref(), Some("Pop"));
        assert_eq!(album.total_tracks, 1);
        assert_eq!(album.extra.get("catalog").map(String::as_str), Some("4988005123999"));
        assert_eq!(album.extra.get("catalognumber").map(String::as_str), Some("4988005123999"));
    }

    #[test]
    fn transferred_headerless_cue_keeps_album_empty_without_invention() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue_path = temp.path().join("headerless.cue");
        std::fs::write(
            &cue_path,
            r#"FILE "track.dts" WAVE
  TRACK 01 AUDIO
    TITLE "Track From Cue"
    PERFORMER "Cue Artist"
    ISRC USAAA2600001
    INDEX 01 00:00:00
"#,
        )
        .expect("headerless cue fixture");
        let source = SidecarCueTrackMetadataSource {
            cue_path,
            track_index: 0,
            cue_track_number: 1,
            cue_file_reference: Some("track.dts".to_string()),
        };

        let (track, album) =
            metadata_for_transferred_sidecar_cue_track(&source).expect("transferred metadata");
        assert_eq!(track.title.as_deref(), Some("Track From Cue"));
        assert_eq!(track.artist.as_deref(), Some("Cue Artist"));
        assert_eq!(track.isrc.as_deref(), Some("USAAA2600001"));
        assert!(!track.extra.contains_key("album"));
        assert!(album.album.is_none());
        assert!(album.album_artist.is_none());
        assert!(album.date.is_none());
        assert!(album.genre.is_none());
    }

    #[test]
    fn transferred_sidecar_cue_fails_closed_if_admitted_mapping_changes() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue_path = temp.path().join("album.cue");
        std::fs::write(
            &cue_path,
            "FILE \"track.dff\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n",
        )
        .expect("initial cue");
        let source = SidecarCueTrackMetadataSource {
            cue_path: cue_path.clone(),
            track_index: 0,
            cue_track_number: 1,
            cue_file_reference: Some("track.dff".to_string()),
        };
        metadata_for_transferred_sidecar_cue_track(&source).expect("initial mapping valid");

        std::fs::write(
            &cue_path,
            "FILE \"different.dff\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n",
        )
        .expect("changed cue");
        let err = metadata_for_transferred_sidecar_cue_track(&source)
            .expect_err("changed FILE mapping must fail closed");
        assert!(err.to_string().contains("changed after queue admission"));
    }

    // ── Category G: probe failure ──

    #[tokio::test]
    async fn ffprobe_failure_propagates_as_error() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cue_path = temp.path().join("album.cue");
        let audio_path = temp.path().join("album.flac");
        let _ = std::fs::write(&cue_path, cue_sheet_single_track().as_bytes());
        let _ = std::fs::write(&audio_path, b"fake-audio-data");

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

#[cfg(test)]
mod sidecar_embedded_upgrade_tests {
    use super::*;
    use super::materializer_cue_tests::{
        fixture_tool_available, run_fixture_command, test_request, write_lofty_cuesheet,
    };

    fn write_image_with_embedded(dir: &Path, image_name: &str, embedded_cue: Option<&str>) -> PathBuf {
        let image = dir.join(image_name);
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
        if let Some(cue) = embedded_cue {
            write_lofty_cuesheet(&image, cue);
        }
        image
    }

    fn sidecar_cue_text(image_name: &str) -> String {
        format!(
            "PERFORMER \"Stale Artist\"\nTITLE \"Stale Album\"\nFILE \"{image_name}\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Stale One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Stale Two\"\n    INDEX 01 00:30:00\n"
        )
    }

    /// If sidecar write-back was skipped or failed, the metadata editor's
    /// corrections can still live only in the image's embedded CUESHEET; a
    /// structurally matching embedded sheet must then drive conversion metadata
    /// instead of stale sidecar text.
    #[test]
    fn sidecar_resolution_prefers_matching_embedded_sheet_metadata() {
        if !fixture_tool_available("ffmpeg") {
            eprintln!("skipping: ffmpeg unavailable");
            return;
        }
        let temp = tempfile::tempdir().expect("temp dir");
        let embedded = "PERFORMER \"Corrected Artist\"\nTITLE \"Corrected Album\"\nFILE \"image.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Corrected One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Corrected Two\"\n    INDEX 01 00:30:00\n";
        write_image_with_embedded(temp.path(), "image.flac", Some(embedded));
        let cue_path = temp.path().join("album.cue");
        std::fs::write(&cue_path, sidecar_cue_text("image.flac")).expect("sidecar cue");

        let req = test_request(&cue_path);
        let input = read_sidecar_cue(&req, cue_path.clone()).expect("sidecar resolution");

        assert_eq!(input.origin, CueOrigin::Embedded);
        assert_eq!(input.sheet.title.as_deref(), Some("Corrected Album"));
        assert_eq!(input.sheet.tracks[0].title.as_deref(), Some("Corrected One"));
    }

    /// Structural disagreement (different track count) keeps the sidecar —
    /// the upgrade is metadata freshness, never a structure override.
    #[test]
    fn sidecar_resolution_keeps_sidecar_when_embedded_track_count_differs() {
        if !fixture_tool_available("ffmpeg") {
            eprintln!("skipping: ffmpeg unavailable");
            return;
        }
        let temp = tempfile::tempdir().expect("temp dir");
        let embedded = "TITLE \"Corrected Album\"\nFILE \"image.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Only One\"\n    INDEX 01 00:00:00\n";
        write_image_with_embedded(temp.path(), "image.flac", Some(embedded));
        let cue_path = temp.path().join("album.cue");
        std::fs::write(&cue_path, sidecar_cue_text("image.flac")).expect("sidecar cue");

        let req = test_request(&cue_path);
        let input = read_sidecar_cue(&req, cue_path.clone()).expect("sidecar resolution");

        assert_eq!(input.origin, CueOrigin::Sidecar);
        assert_eq!(input.sheet.title.as_deref(), Some("Stale Album"));
    }

    /// Same track count but different INDEX 01 boundaries: the sidecar keeps
    /// structure authority — a wholesale embedded swap would move split
    /// points.
    #[test]
    fn sidecar_resolution_keeps_sidecar_when_embedded_boundaries_differ() {
        if !fixture_tool_available("ffmpeg") {
            eprintln!("skipping: ffmpeg unavailable");
            return;
        }
        let temp = tempfile::tempdir().expect("temp dir");
        let embedded = "TITLE \"Corrected Album\"\nFILE \"image.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Corrected One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Corrected Two\"\n    INDEX 01 00:45:00\n";
        write_image_with_embedded(temp.path(), "image.flac", Some(embedded));
        let cue_path = temp.path().join("album.cue");
        std::fs::write(&cue_path, sidecar_cue_text("image.flac")).expect("sidecar cue");

        let req = test_request(&cue_path);
        let input = read_sidecar_cue(&req, cue_path.clone()).expect("sidecar resolution");

        assert_eq!(input.origin, CueOrigin::Sidecar);
        assert_eq!(input.sheet.title.as_deref(), Some("Stale Album"));
    }

    /// Dispatch-time identity probing and materialization must share one
    /// metadata precedence: the dispatch sheet for a corrected single-image
    /// album carries the embedded (corrected) metadata, so batch identity
    /// cannot name album folders from stale sidecar text.
    #[test]
    fn dispatch_metadata_sheet_matches_materialization_precedence() {
        if !fixture_tool_available("ffmpeg") {
            eprintln!("skipping: ffmpeg unavailable");
            return;
        }
        let temp = tempfile::tempdir().expect("temp dir");
        let embedded = "PERFORMER \"Corrected Artist\"\nTITLE \"Corrected Album\"\nFILE \"image.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Corrected One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Corrected Two\"\n    INDEX 01 00:30:00\n";
        write_image_with_embedded(temp.path(), "image.flac", Some(embedded));
        let cue_path = temp.path().join("album.cue");
        std::fs::write(&cue_path, sidecar_cue_text("image.flac")).expect("sidecar cue");

        let sheet = dispatch_metadata_sheet_for_sidecar_cue(&cue_path)
            .expect("dispatch sheet resolves");
        assert_eq!(sheet.title.as_deref(), Some("Corrected Album"));
        assert_eq!(sheet.performer.as_deref(), Some("Corrected Artist"));

        // And without an embedded sheet, dispatch sees the sidecar unchanged.
        let plain = temp.path().join("plain");
        std::fs::create_dir_all(&plain).expect("plain dir");
        write_image_with_embedded(&plain, "image.flac", None);
        let plain_cue = plain.join("album.cue");
        std::fs::write(&plain_cue, sidecar_cue_text("image.flac")).expect("plain cue");
        let sheet = dispatch_metadata_sheet_for_sidecar_cue(&plain_cue)
            .expect("dispatch sheet resolves");
        assert_eq!(sheet.title.as_deref(), Some("Stale Album"));
    }

    /// No embedded sheet: byte-identical to today's behavior.
    #[test]
    fn sidecar_resolution_unchanged_without_embedded_sheet() {
        if !fixture_tool_available("ffmpeg") {
            eprintln!("skipping: ffmpeg unavailable");
            return;
        }
        let temp = tempfile::tempdir().expect("temp dir");
        write_image_with_embedded(temp.path(), "image.flac", None);
        let cue_path = temp.path().join("album.cue");
        std::fs::write(&cue_path, sidecar_cue_text("image.flac")).expect("sidecar cue");

        let req = test_request(&cue_path);
        let input = read_sidecar_cue(&req, cue_path.clone()).expect("sidecar resolution");

        assert_eq!(input.origin, CueOrigin::Sidecar);
        assert_eq!(input.sheet.tracks[0].title.as_deref(), Some("Stale One"));
    }
}
