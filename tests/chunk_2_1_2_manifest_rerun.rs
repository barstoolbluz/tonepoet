use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tonepoet::convert::pipeline::manifest::{
    file_sha256, read_manifest, refresh_manifest_output_facts_for_publish,
    validate_album_relative_output_path, write_manifest, write_manifest_for_publish,
    ConversionManifest, ConversionManifestTrack, ManifestError, TrackIdentity, ValidationStatus,
};
use tonepoet::convert::pipeline::rerun::{
    decide_rerun, delete_stale_publish_temp_dirs, verify_manifest_outputs_at_album_dir,
    ExistingOutputVerificationError, ExistingOutputVerifier, RerunDecision,
};
use tonepoet::convert::pipeline::types::OverwritePolicy;
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
    assert_eq!(manifest.tracks[0].output_hash, Some(file_sha256(&temp_album_dir.join("01.flac")).unwrap()));
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

#[test]
fn matching_manifest_skips_without_conversion() {
    let temp = tempfile::tempdir().unwrap();
    let album_dir = temp.path().join("Album");
    fs::create_dir_all(&album_dir).unwrap();
    let manifest = make_manifest(&album_dir);
    write_manifest(&album_dir, &manifest).unwrap();

    let conversion_count = AtomicUsize::new(0);
    match decide_rerun(&album_dir, &settings(), OverwritePolicy::SkipIfManifestMatch) {
        RerunDecision::Skip { .. } => {}
        other => panic!("expected skip, got {other:?}"),
    }
    assert_eq!(conversion_count.load(Ordering::SeqCst), 0);
}

#[test]
fn changed_source_forces_redo() {
    let temp = tempfile::tempdir().unwrap();
    let album_dir = temp.path().join("Album");
    fs::create_dir_all(&album_dir).unwrap();
    let manifest = make_manifest(&album_dir);
    write_manifest(&album_dir, &manifest).unwrap();

    fs::write(album_dir.join("source.wav"), b"source changed length").unwrap();
    match decide_rerun(&album_dir, &settings(), OverwritePolicy::SkipIfManifestMatch) {
        RerunDecision::Redo { .. } => {}
        other => panic!("expected redo, got {other:?}"),
    }
}

#[test]
fn stale_publish_temp_cleanup_matches_real_tmp_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let album_dir = temp.path().join("Album");
    let real_publish_tmp = temp.path().join(".Album.tmp-123");
    let unrelated_partial = temp.path().join(".Album.partial-123");
    fs::create_dir_all(&real_publish_tmp).unwrap();
    fs::create_dir_all(&unrelated_partial).unwrap();

    let deleted = delete_stale_publish_temp_dirs(&album_dir).unwrap();
    assert_eq!(deleted, vec![real_publish_tmp.clone()]);
    assert!(!real_publish_tmp.exists());
    assert!(unrelated_partial.exists());
}

struct CountingVerifier {
    calls: AtomicUsize,
}

#[async_trait]
impl ExistingOutputVerifier for CountingVerifier {
    async fn verify_existing_output(
        &self,
        _path: &Path,
        _settings: &PipelineSettings,
    ) -> Result<(), ExistingOutputVerificationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn verify_uses_actual_album_dir_not_manifest_album_dir() {
    let temp = tempfile::tempdir().unwrap();
    let album_dir = temp.path().join("Album");
    fs::create_dir_all(&album_dir).unwrap();
    let manifest = make_manifest(&album_dir);
    let verifier = CountingVerifier { calls: AtomicUsize::new(0) };

    verify_manifest_outputs_at_album_dir(&album_dir, &manifest, &settings(), &verifier)
        .await
        .unwrap();
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 1);
}
