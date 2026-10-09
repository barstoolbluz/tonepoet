use std::fs;
use std::path::{Path, PathBuf};

use tonepoet::convert::pipeline::manifest::{
    file_sha256, read_manifest, refresh_manifest_output_facts_for_publish,
    validate_album_relative_output_path, write_manifest, write_manifest_for_publish,
    ConversionManifest, ConversionManifestTrack, ManifestError, TrackIdentity, ValidationStatus,
};
use tonepoet_pipeline::fingerprint::settings_fingerprint;
use tonepoet_pipeline::settings::PipelineSettings;

fn settings() -> PipelineSettings {
    PipelineSettings::default()
}

fn make_track(album_dir: &Path, relative_output: &str, output_bytes: &[u8]) -> ConversionManifestTrack {
    let source_path = album_dir.join("source.wav");
    fs::write(&source_path, b"source").unwrap();
    let output_path = album_dir.join(relative_output);
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&output_path, output_bytes).unwrap();
    let source_metadata = fs::metadata(&source_path).unwrap();
    let st = settings();
    let fp = settings_fingerprint(&st);
    ConversionManifestTrack::new(
        source_path,
        &source_metadata,
        None,
        TrackIdentity {
            source_ordinal: 0,
            disc_number: Some(1),
            track_number: Some(1),
        },
        fp,
        "tonepoet-pipeline-test".to_string(),
        "plan-hash".to_string(),
        PathBuf::from(relative_output),
        output_bytes.len() as u64,
        None,
        ValidationStatus::Passed,
    )
    .unwrap()
}

fn make_manifest(album_dir: &Path) -> ConversionManifest {
    ConversionManifest::new(album_dir.to_path_buf(), settings(), vec![make_track(album_dir, "01.flac", b"audio")])
}

#[test]
fn manifest_round_trip_keeps_album_relative_output_paths() {
    let temp = tempfile::tempdir().unwrap();
    let album_dir = temp.path().join("Album");
    fs::create_dir_all(&album_dir).unwrap();
    let manifest = make_manifest(&album_dir);

    let manifest_path = write_manifest(&album_dir, &manifest).unwrap();
    assert_eq!(manifest_path, album_dir.join(".tonepoet-manifest.json"));

    let reread = read_manifest(&album_dir).unwrap().unwrap();
    assert_eq!(reread.tracks[0].output_path, PathBuf::from("01.flac"));
    assert!(!reread.tracks[0].output_path.is_absolute());
}

#[test]
fn absolute_or_parent_output_paths_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let album_dir = temp.path().join("Album");
    fs::create_dir_all(&album_dir).unwrap();

    assert!(matches!(
        validate_album_relative_output_path(Path::new("../escape.flac")),
        Err(ManifestError::OutputPathEscapesAlbum { .. })
    ));
    assert!(matches!(
        validate_album_relative_output_path(&album_dir.join("01.flac")),
        Err(ManifestError::OutputPathNotRelative { .. })
    ));
}

#[test]
fn read_manifest_rejects_mismatched_album_dir() {
    let temp = tempfile::tempdir().unwrap();
    let album_dir = temp.path().join("Album");
    let other_dir = temp.path().join("Other");
    fs::create_dir_all(&album_dir).unwrap();
    fs::create_dir_all(&other_dir).unwrap();
    let manifest = make_manifest(&album_dir);

    // Write raw JSON into another album path to simulate a copied or edited manifest.
    fs::write(
        other_dir.join(".tonepoet-manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    assert!(matches!(
        read_manifest(&other_dir),
        Err(ManifestError::AlbumDirMismatch { .. })
    ));
}

#[test]
fn refresh_manifest_for_publish_honors_hashing_policy() {
    let temp = tempfile::tempdir().unwrap();
    let final_album_dir = temp.path().join("Album");
    let temp_album_dir = temp.path().join(".Album.tmp-1");
    fs::create_dir_all(&final_album_dir).unwrap();
    fs::create_dir_all(&temp_album_dir).unwrap();

    let mut manifest = make_manifest(&final_album_dir);
    fs::write(temp_album_dir.join("01.flac"), b"published audio").unwrap();

    refresh_manifest_output_facts_for_publish(&mut manifest, &temp_album_dir, &final_album_dir, false).unwrap();
    assert_eq!(manifest.tracks[0].output_hash, None);
    assert_eq!(manifest.tracks[0].output_size, b"published audio".len() as u64);

    refresh_manifest_output_facts_for_publish(&mut manifest, &temp_album_dir, &final_album_dir, true).unwrap();
    assert_eq!(
        manifest.tracks[0].output_hash.map(tonepoet_pipeline::Sha256Digest::to_hex),
        Some(file_sha256(&temp_album_dir.join("01.flac")).unwrap())
    );
}

#[test]
fn manifest_survives_temp_dir_atomic_publish_rename() {
    let temp = tempfile::tempdir().unwrap();
    let final_album_dir = temp.path().join("Album");
    let temp_album_dir = temp.path().join(".Album.tmp-1");
    fs::create_dir_all(&final_album_dir).unwrap();
    fs::create_dir_all(&temp_album_dir).unwrap();
    let mut manifest = make_manifest(&final_album_dir);
    fs::write(temp_album_dir.join("01.flac"), b"published audio").unwrap();
    refresh_manifest_output_facts_for_publish(&mut manifest, &temp_album_dir, &final_album_dir, false).unwrap();

    write_manifest_for_publish(&temp_album_dir, &final_album_dir, &manifest).unwrap();
    fs::remove_dir_all(&final_album_dir).unwrap();
    fs::rename(&temp_album_dir, &final_album_dir).unwrap();

    let reread = read_manifest(&final_album_dir).unwrap().unwrap();
    assert_eq!(reread.album_dir, final_album_dir);
    assert_eq!(reread.tracks[0].output_path, PathBuf::from("01.flac"));
}

// Issue #53: legacy manifests remain readable provenance but no longer
// authorize skipping a conversion or overriding an explicit collision policy.
// These tests exercise the actual publisher's destination behavior; the old
// orchestrator reuse gate and its tests are intentionally absent.
use tonepoet::convert::pipeline::{
    publish_album_output, OverwritePolicy, PublishEntry, PublishError, PublishPlan,
    PublishPolicy, PublishRole, StagingDir,
};

fn stage_fresh_audio(root: &Path, label: &str) -> (StagingDir, PublishPlan) {
    let staging = StagingDir::new(root.join(format!("stage-{label}")), label.to_string());
    fs::create_dir_all(&staging.root).expect("create encoded output staging");
    let staged = staging.root.join("01.flac");
    fs::write(&staged, b"fresh converted audio").expect("fresh encoded output");
    let album_dir = root.join("Album");
    let plan = PublishPlan {
        album_dir: album_dir.clone(),
        entries: vec![PublishEntry {
            staged_path: staged,
            final_path: album_dir.join("01.flac"),
            role: PublishRole::Audio,
        }],
        source_audio_track_count: 1,
        expected_album_track_count: 1,
        suppress_incremental_conversion_log_append: false,
        album_batch_completion_order: false,
        write_conversion_log: false,
    };
    (staging, plan)
}

#[test]
fn matching_prior_manifest_cannot_override_fail_if_exists() {
    let root = tempfile::tempdir().unwrap();
    let album_dir = root.path().join("Album");
    fs::create_dir_all(&album_dir).unwrap();
    let old = make_manifest(&album_dir);
    write_manifest(&album_dir, &old).unwrap();
    assert_eq!(old.legacy_settings_fingerprint(), Some(settings_fingerprint(&settings())));

    let (staging, plan) = stage_fresh_audio(root.path(), "collision");
    let failure = publish_album_output(staging, &plan, PublishPolicy {
        overwrite: OverwritePolicy::FailIfExists,
        same_filesystem_required: false,
        write_manifest: false,
    }, None).expect_err("existing album must not be silently overwritten or skipped");
    assert!(matches!(&failure, PublishError::DestinationExists(_)), "{failure:?}");
    assert_eq!(fs::read(album_dir.join("01.flac")).unwrap(), b"audio".to_vec());
    assert!(album_dir.join(".tonepoet-manifest.json").exists());
}

#[test]
fn matching_prior_manifest_does_not_suppress_incremental_overwrite() {
    let root = tempfile::tempdir().unwrap();
    let album_dir = root.path().join("Album");
    fs::create_dir_all(&album_dir).unwrap();
    let old = make_manifest(&album_dir);
    let legacy_path = write_manifest(&album_dir, &old).unwrap();
    let legacy_bytes = fs::read(&legacy_path).unwrap();
    assert_eq!(old.legacy_settings_fingerprint(), Some(settings_fingerprint(&settings())));

    let (staging, plan) = stage_fresh_audio(root.path(), "replace");
    let published = publish_album_output(staging, &plan, PublishPolicy {
        overwrite: OverwritePolicy::ReplaceWithBackup,
        same_filesystem_required: false,
        write_manifest: false,
    }, None).expect("explicit replacement must publish, even with matching old manifest");
    assert_eq!(fs::read(album_dir.join("01.flac")).unwrap(), b"fresh converted audio".to_vec());
    assert!(published.manifest_path.is_none(), "no newly published manifest without opt-in");
    assert_eq!(fs::read(&legacy_path).unwrap(), legacy_bytes,
        "incremental overwrite must leave the pre-existing manifest untouched");
    let backups = fs::read_dir(root.path()).unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir() && path != &album_dir &&
            path.file_name().unwrap().to_string_lossy().starts_with(".tonepoet-backup-"))
        .collect::<Vec<_>>();
    assert!(backups.is_empty(), "incremental overwrite must not create an album backup: {backups:?}");
}
