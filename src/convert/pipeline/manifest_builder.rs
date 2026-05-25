use std::fs;
use std::path::PathBuf;
use tonepoet_pipeline::fingerprint::{settings_fingerprint, SettingsFingerprint};
use tonepoet_pipeline::settings::PipelineSettings;

use super::manifest::{
    file_sha256, planned_command_hash, tonepoet_pipeline_version,
    validate_album_relative_output_path, ConversionManifest, ConversionManifestTrack,
    ManifestError, TrackIdentity, ValidationStatus,
};

pub struct ManifestBuildInput {
    pub album_dir: PathBuf,
    pub settings: PipelineSettings,
    pub tracks: Vec<ManifestTrackBuildInput>,
}

/// The durable output path invariant is explicit here:
/// - `album_relative_output_path` is the only path stored in the manifest.
/// - `staged_output_path` is used only to stat/hash the file before publish.
/// - absolute staging paths are never serialized into the manifest.
pub struct ManifestTrackBuildInput {
    pub source_path: PathBuf,
    pub source_audio_md5: Option<String>,
    pub track_identity: TrackIdentity,
    pub conversion_plan: tonepoet_pipeline::plan::ConversionPlan,
    pub album_relative_output_path: PathBuf,
    pub staged_output_path: PathBuf,
    pub validation_status: ValidationStatus,
    pub record_output_hash: bool,
}

pub fn build_conversion_manifest(input: ManifestBuildInput) -> Result<ConversionManifest, ManifestError> {
    let fingerprint = settings_fingerprint(&input.settings);
    let mut tracks = Vec::with_capacity(input.tracks.len());

    for track in input.tracks {
        tracks.push(build_manifest_track(track, fingerprint)?);
    }

    let manifest = ConversionManifest::new(input.album_dir, input.settings, tracks);
    Ok(manifest)
}

pub fn build_manifest_track(
    input: ManifestTrackBuildInput,
    fingerprint: SettingsFingerprint,
) -> Result<ConversionManifestTrack, ManifestError> {
    let album_relative_output_path = validate_album_relative_output_path(&input.album_relative_output_path)?;

    let source_metadata = fs::metadata(&input.source_path).map_err(|source| ManifestError::Io {
        path: input.source_path.clone(),
        source,
    })?;

    let output_metadata = fs::metadata(&input.staged_output_path).map_err(|source| ManifestError::Io {
        path: input.staged_output_path.clone(),
        source,
    })?;

    let output_hash = if input.record_output_hash {
        Some(file_sha256(&input.staged_output_path)?)
    } else {
        None
    };

    ConversionManifestTrack::new(
        input.source_path,
        &source_metadata,
        input.source_audio_md5,
        input.track_identity,
        fingerprint,
        tonepoet_pipeline_version().to_string(),
        planned_command_hash(&input.conversion_plan)?,
        album_relative_output_path,
        output_metadata.len(),
        output_hash,
        input.validation_status,
    )
}
