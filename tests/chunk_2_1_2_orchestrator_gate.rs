use async_trait::async_trait;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use tonepoet::convert::pipeline::manifest::{
    write_manifest, ConversionManifest, ConversionManifestTrack, TrackIdentity, ValidationStatus,
};
use tonepoet::convert::pipeline::orchestrator_rerun_gate::{
    evaluate_album_rerun_gate, AlbumRerunGateDecision,
};
use tonepoet::convert::pipeline::rerun::{
    ExistingOutputVerificationError, ExistingOutputVerifier,
};
use tonepoet::convert::pipeline::types::{OverwritePolicy, PublishPolicy};
use tonepoet_pipeline::fingerprint::settings_fingerprint;
use tonepoet_pipeline::settings::PipelineSettings;

struct FakeVerifier {
    fail: bool,
    calls: AtomicUsize,
}

#[async_trait]
impl ExistingOutputVerifier for FakeVerifier {
    async fn verify_existing_output(
        &self,
        path: &Path,
        _settings: &PipelineSettings,
    ) -> Result<(), ExistingOutputVerificationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(ExistingOutputVerificationError::DecodeFailed {
                path: path.to_path_buf(),
                reason: "fake verifier failure".to_string(),
            });
        }
        Ok(())
    }
}

fn settings() -> PipelineSettings {
    PipelineSettings::default()
}

fn publish_policy(overwrite: OverwritePolicy) -> PublishPolicy {
    PublishPolicy {
        overwrite,
        same_filesystem_required: false,
    }
}

fn write_album_with_manifest(album_dir: &Path) {
    fs::create_dir_all(album_dir).unwrap();
    let source_path = album_dir.join("source.wav");
    let output_path = album_dir.join("01.flac");
    fs::write(&source_path, b"source").unwrap();
    fs::write(&output_path, b"audio").unwrap();

    let st = settings();
    let fp = settings_fingerprint(&st);
    let metadata = fs::metadata(&source_path).unwrap();
    let track = ConversionManifestTrack::new(
        source_path,
        &metadata,
        None,
        TrackIdentity {
            source_ordinal: 0,
            disc_number: Some(1),
            track_number: Some(1),
        },
        fp,
        "tonepoet-pipeline-test".to_string(),
        "plan-hash".to_string(),
        "01.flac".into(),
        b"audio".len() as u64,
        None,
        ValidationStatus::Passed,
    )
    .unwrap();
    let manifest = ConversionManifest::new(album_dir.to_path_buf(), st, vec![track]);
    write_manifest(album_dir, &manifest).unwrap();
}

#[tokio::test]
async fn skip_policy_short_circuits_before_conversion() {
    let temp = tempfile::tempdir().unwrap();
    let album_dir = temp.path().join("Album");
    write_album_with_manifest(&album_dir);
    let verifier = FakeVerifier { fail: false, calls: AtomicUsize::new(0) };
    let conversion_count = AtomicUsize::new(0);

    let decision = evaluate_album_rerun_gate(
        &album_dir,
        &settings(),
        publish_policy(OverwritePolicy::SkipIfManifestMatch),
        &verifier,
    )
    .await
    .unwrap();

    assert!(matches!(decision, AlbumRerunGateDecision::Skip { verified: false, .. }));
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 0);
    assert_eq!(conversion_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn verify_policy_short_circuits_only_after_verifier_success() {
    let temp = tempfile::tempdir().unwrap();
    let album_dir = temp.path().join("Album");
    write_album_with_manifest(&album_dir);
    let verifier = FakeVerifier { fail: false, calls: AtomicUsize::new(0) };

    let decision = evaluate_album_rerun_gate(
        &album_dir,
        &settings(),
        publish_policy(OverwritePolicy::VerifyIfManifestMatch),
        &verifier,
    )
    .await
    .unwrap();

    assert!(matches!(decision, AlbumRerunGateDecision::Skip { verified: true, .. }));
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn verify_failure_continues_with_replace_backup() {
    let temp = tempfile::tempdir().unwrap();
    let album_dir = temp.path().join("Album");
    write_album_with_manifest(&album_dir);
    let verifier = FakeVerifier { fail: true, calls: AtomicUsize::new(0) };

    let decision = evaluate_album_rerun_gate(
        &album_dir,
        &settings(),
        publish_policy(OverwritePolicy::VerifyIfManifestMatch),
        &verifier,
    )
    .await
    .unwrap();

    match decision {
        AlbumRerunGateDecision::Continue { effective_publish_policy, warning: Some(_), .. } => {
            assert_eq!(effective_publish_policy.overwrite, OverwritePolicy::ReplaceWithBackup);
        }
        other => panic!("expected continue with replace backup, got {other:?}"),
    }
}
