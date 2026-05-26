use std::fs;
use std::path::PathBuf;
use tonepoet_pipeline::fingerprint::{settings_fingerprint, SettingsFingerprint};
use tonepoet_pipeline::settings::PipelineSettings;

use super::manifest::{
    file_sha256, tonepoet_pipeline_version,
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
    /// Pre-computed hash of the planned command sequence. Per-track entries
    /// receive the track plan hash; merged entries receive the merge sequence
    /// hash.
    pub planned_command_hash: String,
    pub album_relative_output_path: PathBuf,
    pub staged_output_path: PathBuf,
    pub validation_status: ValidationStatus,
    pub record_output_hash: bool,
}

impl ManifestTrackBuildInput {
    pub fn merged_output(
        source_path: PathBuf,
        planned_command_hash: String,
        album_relative_output_path: PathBuf,
        staged_output_path: PathBuf,
        validation_status: ValidationStatus,
        record_output_hash: bool,
    ) -> Self {
        Self {
            source_path,
            source_audio_md5: None,
            track_identity: TrackIdentity::merged_output(),
            planned_command_hash,
            album_relative_output_path,
            staged_output_path,
            validation_status,
            record_output_hash,
        }
    }
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
        input.planned_command_hash,
        album_relative_output_path,
        output_metadata.len(),
        output_hash,
        input.validation_status,
    )
}


#[cfg(test)]
mod manifest_merge_gap_tests {
    use super::*;
    use crate::convert::pipeline::manifest::{file_sha256, read_manifest, write_manifest};
    use std::path::Path;

    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent dir");
        }
        std::fs::write(path, bytes).expect("write file");
    }

    #[test]
    fn merged_manifest_build_write_read_round_trips_single_manifest_entry() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_dir = temp.path().join("Album");
        let source = temp.path().join("album.cue");
        let staged = temp.path().join("staging").join("merged.flac");
        let output_rel = PathBuf::from("merged.flac");
        let settings = PipelineSettings::default();
        write_file(&source, b"FILE album.wav WAVE\n");
        write_file(&staged, b"merged audio bytes");

        let manifest = build_conversion_manifest(ManifestBuildInput {
            album_dir: album_dir.clone(),
            settings,
            tracks: vec![ManifestTrackBuildInput::merged_output(
                source.clone(),
                "merge-sequence-hash".to_string(),
                output_rel.clone(),
                staged.clone(),
                ValidationStatus::Passed,
                true,
            )],
        })
        .expect("build merged manifest");
        write_manifest(&album_dir, &manifest).expect("write manifest");

        let read = read_manifest(&album_dir)
            .expect("read manifest")
            .expect("manifest present");
        let [entry] = read.tracks.as_slice() else { panic!("one merged output entry") };

        assert_eq!(read.total_tracks, 1);
        assert!(entry.track_identity.is_merged_output());
        assert_eq!(entry.source_path, source);
        assert_eq!(entry.output_path, output_rel);
        assert_eq!(entry.output_size, 18);
        assert_eq!(entry.output_hash, Some(file_sha256(&staged).expect("staged sha256")));
        assert_eq!(entry.planned_command_hash, "merge-sequence-hash");
    }
}
