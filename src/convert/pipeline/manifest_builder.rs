use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use tonepoet_pipeline::fingerprint::{
    conversion_behavior_fingerprint_v1, execution_fingerprint_v1,
    settings_fingerprint, settings_snapshot_fingerprint_v2,
    SemanticPlanHashV1, SettingsFingerprint,
};
use tonepoet_pipeline::settings::PipelineSettings;
use tonepoet_pipeline::{DsdInputFrontEnd, Sha256Digest};

use super::manifest::{
    file_sha256, tonepoet_pipeline_version, validate_album_relative_output_path,
    ConversionManifest, ConversionManifestTrack, ManifestError, ManifestRouteIdentityV2,
    ManifestTrackExecutionIdentityV2, TrackIdentity, ValidationStatus,
};
use super::track_executor::{
    reference_execution_identity_input, reference_materialization_identity_digest,
    ReferenceExecutionEvidence,
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
    /// Frozen legacy command identity. Native Reference ignores this value and
    /// derives authority from the executed semantic plan and toolchain evidence.
    pub planned_command_hash: String,
    pub album_relative_output_path: PathBuf,
    pub staged_output_path: PathBuf,
    pub validation_status: ValidationStatus,
    pub record_output_hash: bool,
    /// Native-v2 Reference evidence emitted only after qualified execution.
    pub reference_evidence: Option<ReferenceExecutionEvidence>,
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
            reference_evidence: None,
        }
    }
}

pub fn build_conversion_manifest(
    input: ManifestBuildInput,
) -> Result<ConversionManifest, ManifestError> {
    let native_count = input
        .tracks
        .iter()
        .filter(|track| track.reference_evidence.is_some())
        .count();

    if native_count == 0 {
        return build_legacy_manifest(input);
    }
    if native_count != input.tracks.len() {
        return Err(ManifestError::InvalidAuthority(
            "manifest cannot mix legacy and native Reference track authority".to_string(),
        ));
    }
    if input.tracks.len() != 1 || input.tracks[0].track_identity.is_merged_output() {
        return Err(ManifestError::InvalidAuthority(
            "P0 Reference manifests require exactly one singleton track".to_string(),
        ));
    }

    build_reference_manifest(input)
}

fn build_legacy_manifest(input: ManifestBuildInput) -> Result<ConversionManifest, ManifestError> {
    let fingerprint = settings_fingerprint(&input.settings);
    let mut tracks = Vec::with_capacity(input.tracks.len());
    for track in input.tracks {
        tracks.push(build_legacy_manifest_track(track, fingerprint)?);
    }
    Ok(ConversionManifest::new_legacy(
        input.album_dir,
        input.settings,
        tracks,
    ))
}

fn build_reference_manifest(
    mut input: ManifestBuildInput,
) -> Result<ConversionManifest, ManifestError> {
    let track = input.tracks.pop().ok_or_else(|| {
        ManifestError::InvalidAuthority("Reference manifest has no track".to_string())
    })?;
    let evidence = track.reference_evidence.as_ref().ok_or_else(|| {
        ManifestError::InvalidAuthority("Reference manifest has no execution evidence".to_string())
    })?;

    if evidence.plan.policy != input.settings.dsd.from_dsd.reference_policy {
        return Err(ManifestError::InvalidAuthority(
            "executed Reference policy does not match persisted settings".to_string(),
        ));
    }

    let route_identity = ManifestRouteIdentityV2::DsdReferenceV2 {
        settings_snapshot_fingerprint_v2: settings_snapshot_fingerprint_v2(&input.settings),
        resolved_output_target: evidence.plan.target,
        policy: evidence.plan.policy,
        qualification_manifest_digest: evidence.plan.qualification_manifest_digest,
    };
    let manifest_track = build_reference_manifest_track(track)?;
    ConversionManifest::new_reference(
        input.album_dir,
        input.settings,
        route_identity,
        vec![manifest_track],
    )
}

pub fn build_legacy_manifest_track(
    input: ManifestTrackBuildInput,
    fingerprint: SettingsFingerprint,
) -> Result<ConversionManifestTrack, ManifestError> {
    if input.reference_evidence.is_some() {
        return Err(ManifestError::InvalidAuthority(
            "legacy manifest track contains native Reference evidence".to_string(),
        ));
    }
    let album_relative_output_path =
        validate_album_relative_output_path(&input.album_relative_output_path)?;
    let source_metadata = fs::metadata(&input.source_path).map_err(|source| ManifestError::Io {
        path: input.source_path.clone(),
        source,
    })?;
    let output_metadata =
        fs::metadata(&input.staged_output_path).map_err(|source| ManifestError::Io {
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

fn build_reference_manifest_track(
    input: ManifestTrackBuildInput,
) -> Result<ConversionManifestTrack, ManifestError> {
    let evidence = input.reference_evidence.ok_or_else(|| {
        ManifestError::InvalidAuthority("Reference track has no execution evidence".to_string())
    })?;
    let album_relative_output_path =
        validate_album_relative_output_path(&input.album_relative_output_path)?;
    let source_metadata = fs::metadata(&input.source_path).map_err(|source| ManifestError::Io {
        path: input.source_path.clone(),
        source,
    })?;
    let output_metadata =
        fs::metadata(&input.staged_output_path).map_err(|source| ManifestError::Io {
            path: input.staged_output_path.clone(),
            source,
        })?;
    let output_hash = digest_file(&input.staged_output_path)?;

    let source_probe_digest = evidence.source_probe_digest;
    let behavior = conversion_behavior_fingerprint_v1(&evidence.plan, &evidence.original_source_kind);
    let semantic_plan = SemanticPlanHashV1(evidence.plan.semantic_plan_hash_v1);
    let execution = execution_fingerprint_v1(
        behavior,
        semantic_plan,
        evidence.plan.qualification_manifest_digest,
        &reference_execution_identity_input(&evidence.toolchain),
    );
    let executed_evidence_digest_v1 = reference_executed_evidence_digest_v1(&evidence)?;
    let executed_evidence_digest_v2 = reference_executed_evidence_digest_v2(&evidence)?;
    let execution_identity = ManifestTrackExecutionIdentityV2::NativeDsdV2 {
        behavior_fingerprint_v1: behavior,
        execution_fingerprint_v1: execution,
        semantic_plan_hash_v1: semantic_plan,
        executed_evidence_digest_v1,
        executed_evidence_digest_v2,
    };
    let canonical_materialization_sha256 = match evidence.plan.front_end {
        DsdInputFrontEnd::NativeUncompressed => None,
        _ => Some(evidence.canonical_materialization_sha256),
    };

    ConversionManifestTrack::new_reference(
        input.source_path,
        &source_metadata,
        input.source_audio_md5,
        evidence.source_content_sha256,
        source_probe_digest,
        evidence.original_source_kind,
        evidence.plan.front_end,
        canonical_materialization_sha256,
        input.track_identity,
        execution_identity,
        album_relative_output_path,
        output_metadata.len(),
        output_hash,
        input.validation_status,
    )
}

fn reference_executed_evidence_digest_v1(
    evidence: &ReferenceExecutionEvidence,
) -> Result<Sha256Digest, ManifestError> {
    let verification = &evidence.pcm_verification;
    let post_metadata = verification.post_metadata_sample_sha256.ok_or_else(|| {
        ManifestError::InvalidAuthority(
            "Reference manifest requires post-metadata decoded-sample verification".to_string(),
        )
    })?;
    if verification.qpcm_sample_sha256 != verification.packaged_sample_sha256
        || verification.qpcm_sample_sha256 != post_metadata
    {
        return Err(ManifestError::InvalidAuthority(
            "Reference decoded-sample verification identities do not match".to_string(),
        ));
    }
    let command = verification
        .post_metadata_verification_command
        .as_ref()
        .ok_or_else(|| {
            ManifestError::InvalidAuthority(
                "Reference manifest requires the post-metadata verification transcript".to_string(),
            )
        })?;

    let mut hasher = Sha256::new();
    hasher.update(b"tonepoet-reference-executed-evidence/v1\0");
    hasher.update(evidence.resolved_command_hash.as_bytes());
    hasher.update([0]);
    for (id, measurement) in &evidence.measurements {
        hasher.update(id.0.to_be_bytes());
        let encoded = serde_json::to_vec(measurement).map_err(|error| {
            ManifestError::InvalidAuthority(format!(
                "cannot serialize Reference measurement evidence: {error}"
            ))
        })?;
        hasher.update((encoded.len() as u64).to_be_bytes());
        hasher.update(encoded);
    }
    for digest in [
        verification.r64_contract_digest,
        verification.qpcm_contract_digest,
        verification.qpcm_sample_sha256,
        verification.packaged_sample_sha256,
        post_metadata,
    ] {
        hasher.update(digest.0);
    }
    let command_identity = serde_json::to_vec(&(
        command.binary,
        &command.sanitized_args,
        &command.cwd,
        &command.env_keys,
        &command.exit,
    ))
    .map_err(|error| {
        ManifestError::InvalidAuthority(format!(
            "cannot serialize Reference verification command identity: {error}"
        ))
    })?;
    hasher.update((command_identity.len() as u64).to_be_bytes());
    hasher.update(command_identity);
    Ok(Sha256Digest(hasher.finalize().into()))
}

fn reference_executed_evidence_digest_v2(
    evidence: &ReferenceExecutionEvidence,
) -> Result<Sha256Digest, ManifestError> {
    let v1 = reference_executed_evidence_digest_v1(evidence)?;
    let materialization = reference_materialization_identity_digest(
        &evidence.original_source_kind,
        evidence.source_content_sha256,
        evidence.canonical_materialization_sha256,
    );
    let mut hasher = Sha256::new();
    hasher.update(b"tonepoet-reference-executed-evidence/v2\0");
    hasher.update(v1.0);
    hasher.update(materialization.0);
    Ok(Sha256Digest(hasher.finalize().into()))
}

fn digest_file(path: &std::path::Path) -> Result<Sha256Digest, ManifestError> {
    let hex = file_sha256(path)?;
    Sha256Digest::from_hex(&hex).map_err(ManifestError::InvalidAuthority)
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
        std::fs::create_dir_all(&album_dir).expect("album dir");
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
        assert_eq!(
            entry.output_hash.map(Sha256Digest::to_hex),
            Some(file_sha256(&staged).expect("staged sha256"))
        );
        assert!(matches!(
            &entry.execution_identity,
            ManifestTrackExecutionIdentityV2::LegacyPipelineV1 {
                planned_command_hash,
                ..
            } if planned_command_hash == "merge-sequence-hash"
        ));
    }
}
