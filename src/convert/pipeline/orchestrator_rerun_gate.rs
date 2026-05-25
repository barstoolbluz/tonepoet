use std::path::{Path, PathBuf};
use tonepoet_pipeline::fingerprint::{settings_fingerprint, SettingsFingerprint};
use tonepoet_pipeline::settings::PipelineSettings;

use super::rerun::{
    decide_rerun, prepare_rerun_state, verify_manifest_outputs_at_album_dir,
    ExistingOutputVerificationError, ExistingOutputVerifier, RerunDecision, RerunReason,
};
use super::types::{OverwritePolicy, PublishPolicy};

#[derive(Debug, Clone)]
pub enum AlbumRerunGateDecision {
    Continue {
        effective_publish_policy: PublishPolicy,
        settings_fingerprint: SettingsFingerprint,
        warning: Option<String>,
        reason: Option<RerunReason>,
    },
    Skip {
        settings_fingerprint: SettingsFingerprint,
        manifest_path: PathBuf,
        verified: bool,
    },
    Fail {
        reason: RerunReason,
    },
}

pub async fn evaluate_album_rerun_gate<V: ExistingOutputVerifier + Sync>(
    album_dir: &Path,
    settings: &PipelineSettings,
    publish_policy: PublishPolicy,
    verifier: &V,
) -> Result<AlbumRerunGateDecision, ExistingOutputVerificationError> {
    prepare_rerun_state(album_dir).map_err(|source| ExistingOutputVerificationError::Io {
        path: album_dir.to_path_buf(),
        source,
    })?;

    let current_fingerprint = settings_fingerprint(settings);
    match decide_rerun(album_dir, settings, publish_policy.overwrite) {
        RerunDecision::Proceed { reason } => Ok(AlbumRerunGateDecision::Continue {
            effective_publish_policy: publish_policy,
            settings_fingerprint: current_fingerprint,
            warning: None,
            reason: Some(reason),
        }),
        RerunDecision::Skip { manifest_path, .. } => Ok(AlbumRerunGateDecision::Skip {
            settings_fingerprint: current_fingerprint,
            manifest_path,
            verified: false,
        }),
        RerunDecision::Verify { manifest, manifest_path } => {
            match verify_manifest_outputs_at_album_dir(album_dir, &manifest, settings, verifier).await {
                Ok(()) => Ok(AlbumRerunGateDecision::Skip {
                    settings_fingerprint: current_fingerprint,
                    manifest_path,
                    verified: true,
                }),
                Err(err) => {
                    let mut effective_publish_policy = publish_policy;
                    effective_publish_policy.overwrite = OverwritePolicy::ReplaceWithBackup;
                    Ok(AlbumRerunGateDecision::Continue {
                        effective_publish_policy,
                        settings_fingerprint: current_fingerprint,
                        warning: Some(format!(
                            "existing output verification failed; album will be regenerated: {err}"
                        )),
                        reason: None,
                    })
                }
            }
        }
        RerunDecision::Redo { publish_overwrite, warning, reason } => {
            let mut effective_publish_policy = publish_policy;
            effective_publish_policy.overwrite = publish_overwrite;
            Ok(AlbumRerunGateDecision::Continue {
                effective_publish_policy,
                settings_fingerprint: current_fingerprint,
                warning,
                reason: Some(reason),
            })
        }
        RerunDecision::Fail { reason } => Ok(AlbumRerunGateDecision::Fail { reason }),
    }
}
