use async_trait::async_trait;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tonepoet_pipeline::fingerprint::{settings_fingerprint, SettingsFingerprint};
use tonepoet_pipeline::settings::PipelineSettings;

use super::manifest::{
    file_sha256, manifest_path, metadata_mtime_secs, read_manifest, resolve_manifest_output_path,
    ConversionManifest, ManifestError, ManifestRouteIdentityV2,
    ManifestTrackExecutionIdentityV2, ValidationStatus,
};
use super::track_executor::ReferenceRerunPreflightAuthority;
use super::types::OverwritePolicy;

#[derive(Debug, Clone)]
pub enum RerunDecision {
    Proceed { reason: RerunReason },
    Skip {
        manifest: ConversionManifest,
        manifest_path: PathBuf,
    },
    Verify {
        manifest: ConversionManifest,
        manifest_path: PathBuf,
    },
    Redo {
        reason: RerunReason,
        warning: Option<String>,
        publish_overwrite: OverwritePolicy,
    },
    Fail { reason: RerunReason },
}

#[derive(Debug, Clone)]
pub enum RerunReason {
    AlbumMissing,
    DestinationExists,
    AlwaysRedo,
    ManifestMissing,
    ManifestCorrupt(String),
    ManifestFingerprintMismatch {
        expected: SettingsFingerprint,
        found: SettingsFingerprint,
    },
    ManifestPathEscapesAlbum(PathBuf),
    NativeAuthorityRequiresPreflight,
    NativeAuthorityMismatch(String),
    NativePreflightFailed(String),
    OutputMissing(PathBuf),
    OutputHashMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    OutputSizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    SourceMissing(PathBuf),
    SourceSizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    SourceMtimeMismatch {
        path: PathBuf,
        expected: i64,
        actual: i64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceStalenessPolicy {
    CheckSizeAndMtime,
    CheckSizeOnly,
    Disabled,
}

impl Default for SourceStalenessPolicy {
    fn default() -> Self {
        Self::CheckSizeAndMtime
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RerunOptions {
    pub source_staleness: SourceStalenessPolicy,
}

#[derive(Debug, Clone)]
pub struct RerunPreflight {
    pub deleted_publish_temp_dirs: Vec<PathBuf>,
}

pub fn prepare_rerun_state(album_dir: &Path) -> io::Result<RerunPreflight> {
    Ok(RerunPreflight {
        deleted_publish_temp_dirs: delete_stale_publish_temp_dirs(album_dir)?,
    })
}

pub fn delete_stale_publish_temp_dirs(album_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let final_parent = album_dir.parent().unwrap_or_else(|| Path::new("."));
    let album_name = album_dir
        .file_name()
        .and_then(|s| s.to_str())
        .map(sanitize_component)
        .unwrap_or_else(|| "album".to_string());
    let prefix = format!(".{album_name}.tmp");

    let mut deleted = Vec::new();
    if !final_parent.exists() {
        return Ok(deleted);
    }

    for entry in fs::read_dir(final_parent)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with(&prefix) {
            fs::remove_dir_all(&path)?;
            deleted.push(path);
        }
    }

    Ok(deleted)
}

pub fn decide_rerun(
    album_dir: &Path,
    settings: &PipelineSettings,
    overwrite: OverwritePolicy,
) -> RerunDecision {
    decide_rerun_with_options(album_dir, settings, overwrite, RerunOptions::default())
}

pub fn decide_rerun_with_options(
    album_dir: &Path,
    settings: &PipelineSettings,
    overwrite: OverwritePolicy,
    options: RerunOptions,
) -> RerunDecision {
    if !album_dir.exists() {
        return RerunDecision::Proceed {
            reason: RerunReason::AlbumMissing,
        };
    }

    if overwrite == OverwritePolicy::AlwaysRedo {
        return RerunDecision::Redo {
            reason: RerunReason::AlwaysRedo,
            warning: None,
            publish_overwrite: OverwritePolicy::ReplaceWithBackup,
        };
    }

    let current_fingerprint = settings_fingerprint(settings);
    let path = manifest_path(album_dir);
    let manifest = match read_manifest(album_dir) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => return missing_manifest_decision(overwrite),
        Err(err) => {
            return RerunDecision::Redo {
                reason: RerunReason::ManifestCorrupt(err.to_string()),
                warning: Some(format!(
                    "manifest {} could not be read; album will be regenerated",
                    path.display()
                )),
                publish_overwrite: OverwritePolicy::ReplaceWithBackup,
            };
        }
    };

    let Some(found_fingerprint) = manifest.legacy_settings_fingerprint() else {
        // Native-v2 skip/verify authority includes source content, semantic
        // plan, qualification, executable identities, and runtime dispatch.
        // The generic preflight does not yet possess those current facts, so
        // it must never infer equivalence from settings alone.
        return RerunDecision::Redo {
            reason: RerunReason::NativeAuthorityRequiresPreflight,
            warning: Some(
                "native Reference manifest requires exact source/toolchain preflight; album will be regenerated"
                    .to_string(),
            ),
            publish_overwrite: OverwritePolicy::ReplaceWithBackup,
        };
    };

    if found_fingerprint != current_fingerprint {
        return RerunDecision::Redo {
            reason: RerunReason::ManifestFingerprintMismatch {
                expected: current_fingerprint,
                found: found_fingerprint,
            },
            warning: None,
            publish_overwrite: OverwritePolicy::ReplaceWithBackup,
        };
    }

    if let Err(reason) = manifest_sources_match(&manifest, options.source_staleness) {
        return RerunDecision::Redo {
            reason,
            warning: Some("source facts changed since the manifest was written".to_string()),
            publish_overwrite: OverwritePolicy::ReplaceWithBackup,
        };
    }

    if let Err(reason) = manifest_outputs_match(album_dir, &manifest) {
        return RerunDecision::Redo {
            reason,
            warning: Some(
                "manifest matched settings, but output files did not match manifest facts".to_string(),
            ),
            publish_overwrite: OverwritePolicy::ReplaceWithBackup,
        };
    }

    matched_manifest_decision(manifest, path, overwrite)
}

pub(crate) fn decide_rerun_with_reference_preflight(
    album_dir: &Path,
    settings: &PipelineSettings,
    overwrite: OverwritePolicy,
    authority: &ReferenceRerunPreflightAuthority,
) -> RerunDecision {
    if !album_dir.exists() {
        return RerunDecision::Proceed {
            reason: RerunReason::AlbumMissing,
        };
    }
    if overwrite == OverwritePolicy::AlwaysRedo {
        return RerunDecision::Redo {
            reason: RerunReason::AlwaysRedo,
            warning: None,
            publish_overwrite: OverwritePolicy::ReplaceWithBackup,
        };
    }

    let path = manifest_path(album_dir);
    let manifest = match read_manifest(album_dir) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => return missing_manifest_decision(overwrite),
        Err(err) => {
            return RerunDecision::Redo {
                reason: RerunReason::ManifestCorrupt(err.to_string()),
                warning: Some(format!(
                    "manifest {} could not be read; album will be regenerated",
                    path.display()
                )),
                publish_overwrite: OverwritePolicy::ReplaceWithBackup,
            };
        }
    };

    let route_matches = matches!(
        &manifest.route_identity,
        ManifestRouteIdentityV2::DsdReferenceV2 {
            settings_snapshot_fingerprint_v2,
            resolved_output_target,
            policy,
            qualification_manifest_digest,
        } if *settings_snapshot_fingerprint_v2 == authority.settings_snapshot_fingerprint_v2
            && *resolved_output_target == authority.resolved_output_target
            && *policy == authority.policy
            && *qualification_manifest_digest == authority.qualification_manifest_digest
    );
    if !route_matches {
        return native_authority_redo(
            "route, settings, target, policy, or qualification identity changed",
        );
    }
    if &manifest.settings != settings {
        return native_authority_redo(
            "manifest audit settings differ from the current native-v2 settings",
        );
    }
    if manifest.tracks.len() != 1 {
        return native_authority_redo("P0 Reference manifest is not a singleton");
    }
    let track = &manifest.tracks[0];
    if track.track_identity != authority.track_identity
        || track.source_content_sha256 != Some(authority.source_content_sha256)
        || track.source_probe_digest != Some(authority.source_probe_digest)
        || track.original_dsd_source_kind.as_ref() != Some(&authority.original_source_kind)
        || track.dsd_front_end != Some(authority.front_end)
        || track.output_hash.is_none()
        || track.validation_status != ValidationStatus::Passed
    {
        return native_authority_redo(
            "track, source, front-end, validation, or output-hash authority changed",
        );
    }
    let execution_matches = matches!(
        &track.execution_identity,
        ManifestTrackExecutionIdentityV2::NativeDsdV2 {
            behavior_fingerprint_v1,
            execution_fingerprint_v1,
            semantic_plan_hash_v1,
            ..
        } if *behavior_fingerprint_v1 == authority.behavior_fingerprint_v1
            && *execution_fingerprint_v1 == authority.execution_fingerprint_v1
            && *semantic_plan_hash_v1 == authority.semantic_plan_hash_v1
    );
    if !execution_matches {
        return native_authority_redo(
            "behavior, semantic-plan, or execution identity changed",
        );
    }
    if let Err(reason) = manifest_outputs_match(album_dir, &manifest) {
        return RerunDecision::Redo {
            reason,
            warning: Some(
                "native Reference authority matched, but published output bytes did not"
                    .to_string(),
            ),
            publish_overwrite: OverwritePolicy::ReplaceWithBackup,
        };
    }

    if overwrite == OverwritePolicy::VerifyIfManifestMatch {
        return RerunDecision::Skip {
            manifest,
            manifest_path: path,
        };
    }
    matched_manifest_decision(manifest, path, overwrite)
}

fn native_authority_redo(detail: impl Into<String>) -> RerunDecision {
    let detail = detail.into();
    RerunDecision::Redo {
        reason: RerunReason::NativeAuthorityMismatch(detail.clone()),
        warning: Some(format!(
            "native Reference rerun authority mismatch; album will be regenerated: {detail}"
        )),
        publish_overwrite: OverwritePolicy::ReplaceWithBackup,
    }
}

fn matched_manifest_decision(
    manifest: ConversionManifest,
    path: PathBuf,
    overwrite: OverwritePolicy,
) -> RerunDecision {
    match overwrite {
        OverwritePolicy::SkipIfManifestMatch => RerunDecision::Skip {
            manifest,
            manifest_path: path,
        },
        OverwritePolicy::VerifyIfManifestMatch => RerunDecision::Verify {
            manifest,
            manifest_path: path,
        },
        OverwritePolicy::ReplaceWithBackup => RerunDecision::Redo {
            reason: RerunReason::DestinationExists,
            warning: None,
            publish_overwrite: OverwritePolicy::ReplaceWithBackup,
        },
        OverwritePolicy::FailIfExists => RerunDecision::Fail {
            reason: RerunReason::DestinationExists,
        },
        OverwritePolicy::AlwaysRedo => unreachable!("handled before manifest read"),
    }
}

fn missing_manifest_decision(overwrite: OverwritePolicy) -> RerunDecision {
    match overwrite {
        OverwritePolicy::FailIfExists => RerunDecision::Fail {
            reason: RerunReason::ManifestMissing,
        },
        OverwritePolicy::ReplaceWithBackup
        | OverwritePolicy::SkipIfManifestMatch
        | OverwritePolicy::VerifyIfManifestMatch
        | OverwritePolicy::AlwaysRedo => RerunDecision::Redo {
            reason: RerunReason::ManifestMissing,
            warning: Some("album exists without a manifest; album will be regenerated".to_string()),
            publish_overwrite: OverwritePolicy::ReplaceWithBackup,
        },
    }
}

pub fn manifest_outputs_match(
    album_dir: &Path,
    manifest: &ConversionManifest,
) -> Result<(), RerunReason> {
    for track in &manifest.tracks {
        let output = resolve_manifest_output_path(album_dir, &track.output_path).map_err(|err| match err {
            ManifestError::OutputPathEscapesAlbum { path }
            | ManifestError::OutputPathNotRelative { path } => RerunReason::ManifestPathEscapesAlbum(path),
            other => RerunReason::ManifestCorrupt(other.to_string()),
        })?;

        let metadata = fs::metadata(&output)
            .map_err(|_| RerunReason::OutputMissing(output.clone()))?;
        let actual = metadata.len();
        if actual != track.output_size {
            return Err(RerunReason::OutputSizeMismatch {
                path: output,
                expected: track.output_size,
                actual,
            });
        }
        if let Some(expected) = track.output_hash {
            let actual_hash = file_sha256(&output)
                .map_err(|err| RerunReason::ManifestCorrupt(err.to_string()))?;
            let expected_hash = expected.to_hex();
            if actual_hash != expected_hash {
                return Err(RerunReason::OutputHashMismatch {
                    path: output,
                    expected: expected_hash,
                    actual: actual_hash,
                });
            }
        }
    }

    Ok(())
}

pub fn manifest_sources_match(
    manifest: &ConversionManifest,
    policy: SourceStalenessPolicy,
) -> Result<(), RerunReason> {
    if policy == SourceStalenessPolicy::Disabled {
        return Ok(());
    }

    for track in &manifest.tracks {
        let metadata = fs::metadata(&track.source_path)
            .map_err(|_| RerunReason::SourceMissing(track.source_path.clone()))?;

        let actual_size = metadata.len();
        if actual_size != track.source_size {
            return Err(RerunReason::SourceSizeMismatch {
                path: track.source_path.clone(),
                expected: track.source_size,
                actual: actual_size,
            });
        }

        if policy == SourceStalenessPolicy::CheckSizeAndMtime {
            let actual_mtime = metadata_mtime_secs(&metadata).map_err(|err| {
                RerunReason::ManifestCorrupt(format!("could not read source mtime: {err}"))
            })?;
            if actual_mtime != track.source_mtime_secs {
                return Err(RerunReason::SourceMtimeMismatch {
                    path: track.source_path.clone(),
                    expected: track.source_mtime_secs,
                    actual: actual_mtime,
                });
            }
        }
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ExistingOutputVerificationError {
    #[error("hash mismatch for {path:?}: expected {expected}, got {actual}")]
    HashMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },

    #[error("decode verification failed for {path:?}: {reason}")]
    DecodeFailed { path: PathBuf, reason: String },

    #[error("manifest output path rejected: {0}")]
    UnsafeManifestPath(String),

    #[error("I/O error for {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[async_trait]
pub trait ExistingOutputVerifier {
    async fn verify_existing_output(
        &self,
        path: &Path,
        settings: &PipelineSettings,
    ) -> Result<(), ExistingOutputVerificationError>;
}

pub async fn verify_manifest_outputs_at_album_dir<V: ExistingOutputVerifier + Sync>(
    album_dir: &Path,
    manifest: &ConversionManifest,
    settings: &PipelineSettings,
    verifier: &V,
) -> Result<(), ExistingOutputVerificationError> {
    for track in &manifest.tracks {
        let path = resolve_manifest_output_path(album_dir, &track.output_path).map_err(|err| {
            ExistingOutputVerificationError::UnsafeManifestPath(err.to_string())
        })?;

        if let Some(expected) = &track.output_hash {
            let actual = file_sha256(&path).map_err(|err| match err {
                ManifestError::Io { path, source } => ExistingOutputVerificationError::Io { path, source },
                other => ExistingOutputVerificationError::DecodeFailed {
                    path: path.clone(),
                    reason: other.to_string(),
                },
            })?;

            let expected = expected.to_hex();
            if actual != expected {
                return Err(ExistingOutputVerificationError::HashMismatch {
                    path,
                    expected,
                    actual,
                });
            }
            continue;
        }

        verifier.verify_existing_output(&path, settings).await?;
    }

    Ok(())
}

fn sanitize_component(component: &str) -> String {
    let sanitized: String = component
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' { ch } else { '_' })
        .collect();
    if sanitized.is_empty() {
        "album".to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod chunk_2_1_3_manifest_failure_interaction_tests {
    use super::*;
    use crate::convert::pipeline::manifest::{
        manifest_path, write_manifest, ConversionManifestTrack, TrackIdentity, ValidationStatus,
    };
    use chrono::Utc;

    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent dir");
        }
        std::fs::write(path, bytes).expect("write file");
    }

    fn manifest_for_one_track(
        album_dir: &Path,
        settings: &PipelineSettings,
        source_path: &Path,
        output_rel: &Path,
        output_size: u64,
    ) -> ConversionManifest {
        let source_metadata = std::fs::metadata(source_path).expect("source metadata");
        ConversionManifest::new(
            album_dir.to_path_buf(),
            settings.clone(),
            vec![ConversionManifestTrack {
                source_path: source_path.to_path_buf(),
                source_size: source_metadata.len(),
                source_mtime_secs: metadata_mtime_secs(&source_metadata).expect("source mtime"),
                source_audio_md5: None,
                source_content_sha256: None,
                source_probe_digest: None,
                original_dsd_source_kind: None,
                dsd_front_end: None,
                canonical_materialization_sha256: None,
                track_identity: TrackIdentity {
                    source_ordinal: 1,
                    disc_number: None,
                    track_number: Some(1),
                },
                execution_identity: super::super::manifest::ManifestTrackExecutionIdentityV2::LegacyPipelineV1 {
                    settings_fingerprint_v1: settings_fingerprint(settings),
                    planner_version: "test".to_string(),
                    planned_command_hash: "plan-hash".to_string(),
                },
                output_path: output_rel.to_path_buf(),
                output_size,
                output_hash: None,
                validation_status: ValidationStatus::Passed,
                publish_timestamp: Utc::now(),
            }],
        )
    }

    fn manifest_for_merged_output(
        album_dir: &Path,
        settings: &PipelineSettings,
        source_path: &Path,
        output_rel: &Path,
        output_size: u64,
    ) -> ConversionManifest {
        let source_metadata = std::fs::metadata(source_path).expect("source metadata");
        ConversionManifest::new(
            album_dir.to_path_buf(),
            settings.clone(),
            vec![ConversionManifestTrack {
                source_path: source_path.to_path_buf(),
                source_size: source_metadata.len(),
                source_mtime_secs: metadata_mtime_secs(&source_metadata).expect("source mtime"),
                source_audio_md5: None,
                source_content_sha256: None,
                source_probe_digest: None,
                original_dsd_source_kind: None,
                dsd_front_end: None,
                canonical_materialization_sha256: None,
                track_identity: TrackIdentity::merged_output(),
                execution_identity: super::super::manifest::ManifestTrackExecutionIdentityV2::LegacyPipelineV1 {
                    settings_fingerprint_v1: settings_fingerprint(settings),
                    planner_version: "test".to_string(),
                    planned_command_hash: "merge-plan-hash".to_string(),
                },
                output_path: output_rel.to_path_buf(),
                output_size,
                output_hash: None,
                validation_status: ValidationStatus::Passed,
                publish_timestamp: Utc::now(),
            }],
        )
    }

    fn alter_manifest_fingerprint_json(value: &mut serde_json::Value) {
        fn alter_value(value: &mut serde_json::Value) -> bool {
            match value {
                serde_json::Value::String(s) => {
                    s.push_str("-changed");
                    true
                }
                serde_json::Value::Number(n) => {
                    // Flip the number to produce a different fingerprint
                    *value = serde_json::Value::Number(
                        serde_json::Number::from(n.as_u64().unwrap_or(0).wrapping_add(1)),
                    );
                    true
                }
                serde_json::Value::Object(map) => map.values_mut().any(alter_value),
                serde_json::Value::Array(values) => values.iter_mut().any(alter_value),
                _ => false,
            }
        }

        let fingerprint = value
            .get_mut("route_identity")
            .and_then(|route| route.get_mut("settings_fingerprint_v1"))
            .expect("legacy route settings fingerprint field");
        assert!(alter_value(fingerprint), "fingerprint JSON must contain a mutable component");
        // The v2 parser cross-validates the route-level v1 fingerprint against
        // every track's execution identity; mutate the tracks with the same
        // deterministic transformation so the altered manifest stays
        // well-formed and the decision reflects a fingerprint MISMATCH, not
        // manifest corruption.
        if let Some(tracks) = value.get_mut("tracks").and_then(|t| t.as_array_mut()) {
            for track in tracks {
                if let Some(track_fingerprint) = track
                    .get_mut("execution_identity")
                    .and_then(|identity| identity.get_mut("settings_fingerprint_v1"))
                {
                    assert!(alter_value(track_fingerprint));
                }
            }
        }
    }

    #[test]
    fn successful_manifest_match_skips_when_policy_requests_skip() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_dir = temp.path().join("Album");
        let source = temp.path().join("source.flac");
        let output_rel = PathBuf::from("01.flac");
        let output = album_dir.join(&output_rel);
        let settings = PipelineSettings::default();
        write_file(&source, b"source audio");
        write_file(&output, b"encoded audio");
        let manifest = manifest_for_one_track(
            &album_dir,
            &settings,
            &source,
            &output_rel,
            13,
        );
        write_manifest(&album_dir, &manifest).expect("write manifest");

        let decision = decide_rerun_with_options(
            &album_dir,
            &settings,
            OverwritePolicy::SkipIfManifestMatch,
            RerunOptions {
                source_staleness: SourceStalenessPolicy::CheckSizeAndMtime,
            },
        );

        assert!(matches!(decision, RerunDecision::Skip { .. }));
    }

    #[test]
    fn native_album_profile_change_does_not_match_legacy_album_manifest() {
        use tonepoet_pipeline::{
            DsdAutoGainScope, DsdReconstructionSelection, DsdSettings, DsdSourceGainMode,
        };

        let temp = tempfile::tempdir().expect("temp dir");
        let album_dir = temp.path().join("Album");
        let source = temp.path().join("source.dsf");
        let output_rel = PathBuf::from("01.flac");
        let output = album_dir.join(&output_rel);
        write_file(&source, b"source audio");
        write_file(&output, b"encoded audio");

        let mut reference = PipelineSettings::default();
        reference.dsd = DsdSettings::native_v2();
        reference.dsd.from_dsd.gain_mode = DsdSourceGainMode::NormalizePeak;
        reference.dsd.from_dsd.profile = DsdReconstructionSelection::Reference;
        reference.dsd.set_auto_gain_scope(DsdAutoGainScope::Album);
        reference.dsd.bind_runtime_album_gain(
            "-0.750000000".parse().unwrap(),
            Some("-0.490000000".parse().unwrap()),
            2,
        );
        let manifest = manifest_for_one_track(
            &album_dir,
            &reference,
            &source,
            &output_rel,
            13,
        );
        write_manifest(&album_dir, &manifest).expect("write manifest");

        let mut wideband = reference.clone();
        wideband.dsd.from_dsd.profile = DsdReconstructionSelection::Wideband;
        assert!(matches!(
            decide_rerun(
                &album_dir,
                &wideband,
                OverwritePolicy::SkipIfManifestMatch,
            ),
            RerunDecision::Redo {
                reason: RerunReason::ManifestFingerprintMismatch { .. },
                ..
            }
        ));
    }

    #[test]
    fn merged_manifest_match_skips_when_policy_requests_skip() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_dir = temp.path().join("Album");
        let source = temp.path().join("album.cue");
        let output_rel = PathBuf::from("merged.flac");
        let output = album_dir.join(&output_rel);
        let settings = PipelineSettings::default();
        write_file(&source, b"FILE album.wav WAVE\n");
        write_file(&output, b"merged audio");
        let manifest = manifest_for_merged_output(
            &album_dir,
            &settings,
            &source,
            &output_rel,
            12,
        );
        write_manifest(&album_dir, &manifest).expect("write manifest");

        let decision = decide_rerun(
            &album_dir,
            &settings,
            OverwritePolicy::SkipIfManifestMatch,
        );

        assert!(matches!(decision, RerunDecision::Skip { .. }));
    }

    #[test]
    fn merged_manifest_fingerprint_mismatch_redoes() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_dir = temp.path().join("Album");
        let source = temp.path().join("album.cue");
        let output_rel = PathBuf::from("merged.flac");
        let output = album_dir.join(&output_rel);
        let settings = PipelineSettings::default();
        write_file(&source, b"FILE album.wav WAVE\n");
        write_file(&output, b"merged audio");
        let manifest = manifest_for_merged_output(
            &album_dir,
            &settings,
            &source,
            &output_rel,
            12,
        );
        let mut value = serde_json::to_value(&manifest).expect("manifest value");
        alter_manifest_fingerprint_json(&mut value);
        write_file(
            &manifest_path(&album_dir),
            &serde_json::to_vec_pretty(&value).expect("manifest json"),
        );

        let decision = decide_rerun(
            &album_dir,
            &settings,
            OverwritePolicy::SkipIfManifestMatch,
        );

        assert!(matches!(
            decision,
            RerunDecision::Redo {
                reason: RerunReason::ManifestFingerprintMismatch { .. },
                publish_overwrite: OverwritePolicy::ReplaceWithBackup,
                ..
            }
        ));
    }

    #[test]
    fn missing_manifest_after_failed_conversion_redoes_for_manifest_match_policy() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_dir = temp.path().join("Album");
        let settings = PipelineSettings::default();
        std::fs::create_dir_all(&album_dir).expect("album dir");
        write_file(&album_dir.join("01.flac"), b"possibly stale audio");

        let decision = decide_rerun(
            &album_dir,
            &settings,
            OverwritePolicy::SkipIfManifestMatch,
        );

        assert!(matches!(
            decision,
            RerunDecision::Redo {
                reason: RerunReason::ManifestMissing,
                publish_overwrite: OverwritePolicy::ReplaceWithBackup,
                ..
            }
        ));
    }

    #[test]
    fn partial_manifest_with_track_count_mismatch_redoes_as_corrupt_manifest() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_dir = temp.path().join("Album");
        let source = temp.path().join("source.flac");
        let output_rel = PathBuf::from("01.flac");
        let output = album_dir.join(&output_rel);
        let settings = PipelineSettings::default();
        write_file(&source, b"source audio");
        write_file(&output, b"encoded audio");
        let mut manifest = manifest_for_one_track(
            &album_dir,
            &settings,
            &source,
            &output_rel,
            13,
        );
        manifest.total_tracks = 5;
        write_file(
            &manifest_path(&album_dir),
            &serde_json::to_vec_pretty(&manifest).expect("manifest json"),
        );

        let decision = decide_rerun(
            &album_dir,
            &settings,
            OverwritePolicy::SkipIfManifestMatch,
        );

        assert!(matches!(
            decision,
            RerunDecision::Redo {
                reason: RerunReason::ManifestCorrupt(_),
                publish_overwrite: OverwritePolicy::ReplaceWithBackup,
                ..
            }
        ));
    }

    #[test]
    fn output_missing_after_partial_publish_redoes_normally() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_dir = temp.path().join("Album");
        let source = temp.path().join("source.flac");
        let output_rel = PathBuf::from("01.flac");
        let settings = PipelineSettings::default();
        write_file(&source, b"source audio");
        std::fs::create_dir_all(&album_dir).expect("album dir");
        let manifest = manifest_for_one_track(
            &album_dir,
            &settings,
            &source,
            &output_rel,
            13,
        );
        write_manifest(&album_dir, &manifest).expect("write manifest");

        let decision = decide_rerun(
            &album_dir,
            &settings,
            OverwritePolicy::SkipIfManifestMatch,
        );

        assert!(matches!(
            decision,
            RerunDecision::Redo {
                reason: RerunReason::OutputMissing(_),
                publish_overwrite: OverwritePolicy::ReplaceWithBackup,
                ..
            }
        ));
    }

    #[test]
    fn cancelled_conversion_publish_temp_is_deleted_before_rerun() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_dir = temp.path().join("Album");
        let stale_temp = temp.path().join(".Album.tmp-abandoned");
        write_file(&stale_temp.join("01.flac"), b"partial");

        let preflight = prepare_rerun_state(&album_dir).expect("prepare rerun state");

        assert_eq!(preflight.deleted_publish_temp_dirs, vec![stale_temp.clone()]);
        assert!(!stale_temp.exists());
        assert!(matches!(
            decide_rerun(
                &album_dir,
                &PipelineSettings::default(),
                OverwritePolicy::SkipIfManifestMatch,
            ),
            RerunDecision::Proceed {
                reason: RerunReason::AlbumMissing,
            }
        ));
    }
}
