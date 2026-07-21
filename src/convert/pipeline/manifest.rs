use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::num::NonZeroUsize;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tonepoet_pipeline::fingerprint::{
    legacy_settings_fingerprint_v1, settings_snapshot_fingerprint_v2, BehaviorFingerprintV1, ExecutionFingerprintV1,
    LegacySettingsFingerprintV1, SemanticPlanHashV1, SettingsSnapshotFingerprintV2,
};
use tonepoet_pipeline::plan::ConversionPlan;
use tonepoet_pipeline::settings::PipelineSettings;
use tonepoet_pipeline::{
    DsdInputFrontEnd, DsdReferencePolicyVersion, DsdSourceKind, ResolvedOutputTarget,
    Sha256Digest,
};

pub const MANIFEST_VERSION: u32 = 2;
pub const MANIFEST_FILE_NAME: &str = ".tonepoet-manifest.json";

fn zero_sha256_digest() -> Sha256Digest {
    Sha256Digest([0; 32])
}

fn is_zero_sha256_digest(value: &Sha256Digest) -> bool {
    *value == Sha256Digest([0; 32])
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversionManifest {
    pub manifest_version: u32,
    pub album_dir: PathBuf,
    pub total_tracks: usize,
    pub settings: PipelineSettings,
    pub route_identity: ManifestRouteIdentityV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub programme_identity: Option<ProgrammeManifestIdentityV1>,
    pub tracks: Vec<ConversionManifestTrack>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "route", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManifestRouteIdentityV2 {
    LegacyPipelineV1 {
        settings_fingerprint_v1: LegacySettingsFingerprintV1,
    },
    DsdReferenceV2 {
        settings_snapshot_fingerprint_v2: SettingsSnapshotFingerprintV2,
        resolved_output_target: ResolvedOutputTarget,
        policy: DsdReferencePolicyVersion,
        qualification_manifest_digest: Sha256Digest,
    },
    DsdManualV2 {
        settings_snapshot_fingerprint_v2: SettingsSnapshotFingerprintV2,
        resolved_output_target: ResolvedOutputTarget,
        workflow_execution_digest: Sha256Digest,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManifestTrackExecutionIdentityV2 {
    LegacyPipelineV1 {
        settings_fingerprint_v1: LegacySettingsFingerprintV1,
        planner_version: String,
        planned_command_hash: String,
    },
    NativeDsdV2 {
        behavior_fingerprint_v1: BehaviorFingerprintV1,
        execution_fingerprint_v1: ExecutionFingerprintV1,
        semantic_plan_hash_v1: SemanticPlanHashV1,
        /// Digest of the executed measurements, fully resolved argv, carrier probes,
        /// and pre/post-metadata decoded-sample verification. Missing on an
        /// early native-v2 candidate manifest deserializes as zero and is
        /// rejected as insufficient authority rather than reported as corrupt JSON.
        #[serde(default = "zero_sha256_digest")]
        executed_evidence_digest_v1: Sha256Digest,
        /// V3-and-later executed authority. This preserves the frozen v1 digest
        /// while additionally binding original source kind, admitted source
        /// content, and canonical materialization identity.
        #[serde(default = "zero_sha256_digest")]
        executed_evidence_digest_v2: Sha256Digest,
        /// V7-and-later executed authority. This preserves the frozen v1/v2
        /// digests while additionally binding the ordered post-metadata
        /// verification pipeline, including each command's explicit
        /// environment policy and sanitized environment.
        #[serde(
            default = "zero_sha256_digest",
            skip_serializing_if = "is_zero_sha256_digest"
        )]
        executed_evidence_digest_v3: Sha256Digest,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceProgrammeGrouping {
    ResolvedAlbumRelease,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProgrammeManifestIdentityV1 {
    pub policy: DsdReferencePolicyVersion,
    pub grouping_rule: ReferenceProgrammeGrouping,
    pub semantic_programme_id: Sha256Digest,
    pub programme_digest: Sha256Digest,
    pub expected_members: NonZeroUsize,
    pub ordered_member_content_ids: Vec<Sha256Digest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversionManifestTrack {
    pub source_path: PathBuf,
    pub source_size: u64,
    pub source_mtime_secs: i64,
    pub source_audio_md5: Option<String>,
    pub source_content_sha256: Option<Sha256Digest>,
    pub source_probe_digest: Option<Sha256Digest>,
    pub original_dsd_source_kind: Option<DsdSourceKind>,
    pub dsd_front_end: Option<DsdInputFrontEnd>,
    pub canonical_materialization_sha256: Option<Sha256Digest>,
    pub track_identity: TrackIdentity,
    pub execution_identity: ManifestTrackExecutionIdentityV2,
    /// Always album-relative. Never store staging, temp, or arbitrary absolute paths here.
    pub output_path: PathBuf,
    pub output_size: u64,
    pub output_hash: Option<Sha256Digest>,
    pub validation_status: ValidationStatus,
    pub publish_timestamp: DateTime<Utc>,
}

impl ConversionManifest {
    pub fn new(
        album_dir: PathBuf,
        settings: PipelineSettings,
        tracks: Vec<ConversionManifestTrack>,
    ) -> Self {
        Self::new_legacy(album_dir, settings, tracks)
    }

    pub fn new_legacy(
        album_dir: PathBuf,
        settings: PipelineSettings,
        tracks: Vec<ConversionManifestTrack>,
    ) -> Self {
        let settings_fingerprint_v1 = legacy_settings_fingerprint_v1(&settings);
        Self {
            manifest_version: MANIFEST_VERSION,
            album_dir,
            total_tracks: tracks.len(),
            settings,
            route_identity: ManifestRouteIdentityV2::LegacyPipelineV1 {
                settings_fingerprint_v1,
            },
            programme_identity: None,
            tracks,
        }
    }

    pub fn new_reference(
        album_dir: PathBuf,
        settings: PipelineSettings,
        route_identity: ManifestRouteIdentityV2,
        tracks: Vec<ConversionManifestTrack>,
    ) -> Result<Self, ManifestError> {
        if !matches!(&route_identity, ManifestRouteIdentityV2::DsdReferenceV2 { .. }) {
            return Err(ManifestError::InvalidAuthority(
                "native Reference manifest requires a DsdReferenceV2 route".to_string(),
            ));
        }
        let manifest = Self {
            manifest_version: MANIFEST_VERSION,
            album_dir,
            total_tracks: tracks.len(),
            settings,
            route_identity,
            programme_identity: None,
            tracks,
        };
        validate_manifest_authority(&manifest)?;
        Ok(manifest)
    }

    pub fn legacy_settings_fingerprint(&self) -> Option<LegacySettingsFingerprintV1> {
        match &self.route_identity {
            ManifestRouteIdentityV2::LegacyPipelineV1 {
                settings_fingerprint_v1,
            } => Some(*settings_fingerprint_v1),
            _ => None,
        }
    }

    pub fn stamp_publish_timestamp(&mut self, timestamp: DateTime<Utc>) {
        for track in &mut self.tracks {
            track.publish_timestamp = timestamp;
        }
    }
}

impl ConversionManifestTrack {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_path: PathBuf,
        source_metadata: &fs::Metadata,
        source_audio_md5: Option<String>,
        track_identity: TrackIdentity,
        settings_fingerprint: LegacySettingsFingerprintV1,
        planner_version: String,
        planned_command_hash: String,
        album_relative_output_path: PathBuf,
        output_size: u64,
        output_hash: Option<String>,
        validation_status: ValidationStatus,
    ) -> Result<Self, ManifestError> {
        let output_hash = output_hash
            .map(|value| Sha256Digest::from_hex(&value))
            .transpose()
            .map_err(ManifestError::InvalidAuthority)?;
        Ok(Self {
            source_path,
            source_size: source_metadata.len(),
            source_mtime_secs: metadata_mtime_secs(source_metadata)?,
            source_audio_md5,
            source_content_sha256: None,
            source_probe_digest: None,
            original_dsd_source_kind: None,
            dsd_front_end: None,
            canonical_materialization_sha256: None,
            track_identity,
            execution_identity: ManifestTrackExecutionIdentityV2::LegacyPipelineV1 {
                settings_fingerprint_v1: settings_fingerprint,
                planner_version,
                planned_command_hash,
            },
            output_path: album_relative_output_path,
            output_size,
            output_hash,
            validation_status,
            publish_timestamp: Utc::now(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_reference(
        source_path: PathBuf,
        source_metadata: &fs::Metadata,
        source_audio_md5: Option<String>,
        source_content_sha256: Sha256Digest,
        source_probe_digest: Sha256Digest,
        original_dsd_source_kind: DsdSourceKind,
        dsd_front_end: DsdInputFrontEnd,
        canonical_materialization_sha256: Option<Sha256Digest>,
        track_identity: TrackIdentity,
        execution_identity: ManifestTrackExecutionIdentityV2,
        album_relative_output_path: PathBuf,
        output_size: u64,
        output_hash: Sha256Digest,
        validation_status: ValidationStatus,
    ) -> Result<Self, ManifestError> {
        if !matches!(&execution_identity, ManifestTrackExecutionIdentityV2::NativeDsdV2 { .. }) {
            return Err(ManifestError::InvalidAuthority(
                "native Reference track requires NativeDsdV2 execution identity".to_string(),
            ));
        }
        Ok(Self {
            source_path,
            source_size: source_metadata.len(),
            source_mtime_secs: metadata_mtime_secs(source_metadata)?,
            source_audio_md5,
            source_content_sha256: Some(source_content_sha256),
            source_probe_digest: Some(source_probe_digest),
            original_dsd_source_kind: Some(original_dsd_source_kind),
            dsd_front_end: Some(dsd_front_end),
            canonical_materialization_sha256,
            track_identity,
            execution_identity,
            output_path: album_relative_output_path,
            output_size,
            output_hash: Some(output_hash),
            validation_status,
            publish_timestamp: Utc::now(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackIdentity {
    pub source_ordinal: usize,
    pub disc_number: Option<u32>,
    pub track_number: Option<u32>,
}

impl TrackIdentity {
    pub fn merged_output() -> Self {
        Self {
            source_ordinal: 0,
            disc_number: None,
            track_number: None,
        }
    }

    pub fn is_merged_output(&self) -> bool {
        self.source_ordinal == 0 && self.disc_number.is_none() && self.track_number.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationStatus {
    Passed,
    Skipped,
    Failed { reason: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversionManifestV2Wire {
    manifest_version: u32,
    album_dir: PathBuf,
    total_tracks: usize,
    settings: PipelineSettings,
    route_identity: ManifestRouteIdentityV2,
    #[serde(default)]
    programme_identity: Option<ProgrammeManifestIdentityV1>,
    tracks: Vec<ConversionManifestTrack>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversionManifestV1Wire {
    manifest_version: u32,
    album_dir: PathBuf,
    total_tracks: usize,
    settings: PipelineSettings,
    settings_fingerprint: LegacySettingsFingerprintV1,
    tracks: Vec<ConversionManifestTrackV1Wire>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversionManifestTrackV1Wire {
    source_path: PathBuf,
    source_size: u64,
    source_mtime_secs: i64,
    source_audio_md5: Option<String>,
    track_identity: TrackIdentity,
    settings_fingerprint: LegacySettingsFingerprintV1,
    planner_version: String,
    planned_command_hash: String,
    output_path: PathBuf,
    output_size: u64,
    output_hash: Option<String>,
    validation_status: ValidationStatus,
    publish_timestamp: DateTime<Utc>,
}

impl<'de> Deserialize<'de> for ConversionManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let version = value
            .get("manifest_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| serde::de::Error::missing_field("manifest_version"))?;
        match version {
            2 => {
                let wire: ConversionManifestV2Wire =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                if wire.manifest_version != 2 {
                    return Err(serde::de::Error::custom("manifest_version must be exactly 2"));
                }
                Ok(Self {
                    manifest_version: 2,
                    album_dir: wire.album_dir,
                    total_tracks: wire.total_tracks,
                    settings: wire.settings,
                    route_identity: wire.route_identity,
                    programme_identity: wire.programme_identity,
                    tracks: wire.tracks,
                })
            }
            1 => {
                let wire: ConversionManifestV1Wire =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                if wire.manifest_version != 1 {
                    return Err(serde::de::Error::custom("manifest_version must be exactly 1"));
                }
                let tracks = wire
                    .tracks
                    .into_iter()
                    .map(|track| {
                        let output_hash = track
                            .output_hash
                            .map(|value| Sha256Digest::from_hex(&value))
                            .transpose()
                            .map_err(serde::de::Error::custom)?;
                        Ok(ConversionManifestTrack {
                            source_path: track.source_path,
                            source_size: track.source_size,
                            source_mtime_secs: track.source_mtime_secs,
                            source_audio_md5: track.source_audio_md5,
                            source_content_sha256: None,
                            source_probe_digest: None,
                            original_dsd_source_kind: None,
                            dsd_front_end: None,
                            canonical_materialization_sha256: None,
                            track_identity: track.track_identity,
                            execution_identity: ManifestTrackExecutionIdentityV2::LegacyPipelineV1 {
                                settings_fingerprint_v1: track.settings_fingerprint,
                                planner_version: track.planner_version,
                                planned_command_hash: track.planned_command_hash,
                            },
                            output_path: track.output_path,
                            output_size: track.output_size,
                            output_hash,
                            validation_status: track.validation_status,
                            publish_timestamp: track.publish_timestamp,
                        })
                    })
                    .collect::<Result<Vec<_>, D::Error>>()?;
                Ok(Self {
                    manifest_version: 2,
                    album_dir: wire.album_dir,
                    total_tracks: wire.total_tracks,
                    settings: wire.settings,
                    route_identity: ManifestRouteIdentityV2::LegacyPipelineV1 {
                        settings_fingerprint_v1: wire.settings_fingerprint,
                    },
                    programme_identity: None,
                    tracks,
                })
            }
            other => Err(serde::de::Error::custom(format!(
                "unsupported manifest version {other}; expected 1 or 2"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFileFacts {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_secs: i64,
}

impl SourceFileFacts {
    pub fn read(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let path = path.as_ref();
        let metadata = fs::metadata(path).map_err(|source| ManifestError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            size: metadata.len(),
            mtime_secs: metadata_mtime_secs(&metadata)?,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest I/O error at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("manifest JSON error at {path:?}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("unsupported manifest version {found}; expected {expected}")]
    UnsupportedVersion { found: u32, expected: u32 },

    #[error("manifest track count mismatch: total_tracks={total_tracks}, tracks.len()={tracks_len}")]
    TrackCountMismatch { total_tracks: usize, tracks_len: usize },

    #[error("invalid manifest authority: {0}")]
    InvalidAuthority(String),

    #[error("manifest album_dir mismatch: manifest={manifest_album_dir:?}, actual={actual_album_dir:?}")]
    AlbumDirMismatch {
        manifest_album_dir: PathBuf,
        actual_album_dir: PathBuf,
    },

    #[error("manifest output path escapes album dir: {path:?}")]
    OutputPathEscapesAlbum { path: PathBuf },

    #[error("manifest output path is empty")]
    EmptyOutputPath,

    #[error("manifest output path must be album-relative, got {path:?}")]
    OutputPathNotRelative { path: PathBuf },

    #[error("system time before Unix epoch for {path:?}: {source}")]
    InvalidMtime {
        path: PathBuf,
        #[source]
        source: std::time::SystemTimeError,
    },
}

pub fn manifest_path(album_dir: &Path) -> PathBuf {
    album_dir.join(MANIFEST_FILE_NAME)
}

pub fn tonepoet_pipeline_version() -> &'static str {
    option_env!("TONEPOET_PIPELINE_VERSION").unwrap_or("unknown")
}

pub fn read_manifest(album_dir: &Path) -> Result<Option<ConversionManifest>, ManifestError> {
    let path = manifest_path(album_dir);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ManifestError::Io { path, source }),
    };
    let mut manifest: ConversionManifest = serde_json::from_slice(&bytes).map_err(|source| {
        ManifestError::Json {
            path: path.clone(),
            source,
        }
    })?;
    validate_manifest_for_album(album_dir, &manifest, true)?;
    manifest.album_dir = album_dir.to_path_buf();
    Ok(Some(manifest))
}

pub fn validate_manifest(
    album_dir: &Path,
    manifest: &ConversionManifest,
) -> Result<(), ManifestError> {
    validate_manifest_for_album(album_dir, manifest, true)
}

pub fn validate_manifest_for_publish(
    final_album_dir: &Path,
    manifest: &ConversionManifest,
) -> Result<(), ManifestError> {
    validate_manifest_for_album(final_album_dir, manifest, true)
}

fn validate_manifest_for_album(
    actual_album_dir: &Path,
    manifest: &ConversionManifest,
    check_album_dir: bool,
) -> Result<(), ManifestError> {
    if manifest.manifest_version != MANIFEST_VERSION {
        return Err(ManifestError::UnsupportedVersion {
            found: manifest.manifest_version,
            expected: MANIFEST_VERSION,
        });
    }
    if manifest.total_tracks != manifest.tracks.len() {
        return Err(ManifestError::TrackCountMismatch {
            total_tracks: manifest.total_tracks,
            tracks_len: manifest.tracks.len(),
        });
    }
    if check_album_dir && !same_path_lexical(&manifest.album_dir, actual_album_dir) {
        return Err(ManifestError::AlbumDirMismatch {
            manifest_album_dir: manifest.album_dir.clone(),
            actual_album_dir: actual_album_dir.to_path_buf(),
        });
    }
    for track in &manifest.tracks {
        validate_album_relative_output_path(&track.output_path)?;
    }
    validate_manifest_authority(manifest)
}

fn validate_manifest_authority(manifest: &ConversionManifest) -> Result<(), ManifestError> {
    if manifest.programme_identity.is_some() {
        return Err(ManifestError::InvalidAuthority(
            "programme identity is reserved and unavailable in P0".to_string(),
        ));
    }
    match &manifest.route_identity {
        ManifestRouteIdentityV2::LegacyPipelineV1 { settings_fingerprint_v1 } => {
            for track in &manifest.tracks {
                match &track.execution_identity {
                    ManifestTrackExecutionIdentityV2::LegacyPipelineV1 {
                        settings_fingerprint_v1: track_fingerprint,
                        ..
                    } if track_fingerprint == settings_fingerprint_v1 => {}
                    _ => {
                        return Err(ManifestError::InvalidAuthority(
                            "legacy route contains a nonmatching track identity".to_string(),
                        ));
                    }
                }
                if track.source_content_sha256.is_some()
                    || track.source_probe_digest.is_some()
                    || track.original_dsd_source_kind.is_some()
                    || track.dsd_front_end.is_some()
                    || track.canonical_materialization_sha256.is_some()
                {
                    return Err(ManifestError::InvalidAuthority(
                        "legacy route contains native-only source authority".to_string(),
                    ));
                }
            }
        }
        ManifestRouteIdentityV2::DsdReferenceV2 {
            settings_snapshot_fingerprint_v2: route_settings_snapshot,
            resolved_output_target,
            policy,
            qualification_manifest_digest,
        } => {
            if manifest.total_tracks != 1 {
                return Err(ManifestError::InvalidAuthority(
                    "P0 Reference manifest must contain exactly one track".to_string(),
                ));
            }
            if *route_settings_snapshot != settings_snapshot_fingerprint_v2(&manifest.settings) {
                return Err(ManifestError::InvalidAuthority(
                    "Reference route settings snapshot does not match manifest settings".to_string(),
                ));
            }
            if !manifest.settings.dsd.is_native_v2()
                || manifest.settings.dsd.from_dsd.pathway
                    != tonepoet_pipeline::DsdSourcePathway::Reference
                || manifest.settings.dsd.from_dsd.reference_policy != *policy
            {
                return Err(ManifestError::InvalidAuthority(
                    "Reference route settings do not match native-v2 policy".to_string(),
                ));
            }
            if !resolved_output_target.is_p0_reference_lossless() {
                return Err(ManifestError::InvalidAuthority(
                    "Reference route target is not a P0 lossless target".to_string(),
                ));
            }
            for track in &manifest.tracks {
                if track.source_content_sha256.is_none()
                    || track.source_probe_digest.is_none()
                    || track.original_dsd_source_kind.is_none()
                    || track.dsd_front_end.is_none()
                    || track.output_hash.is_none()
                {
                    return Err(ManifestError::InvalidAuthority(
                        "Reference track is missing required native authority".to_string(),
                    ));
                }
                match track.dsd_front_end {
                    Some(DsdInputFrontEnd::NativeUncompressed) => {
                        if track.canonical_materialization_sha256.is_some() {
                            return Err(ManifestError::InvalidAuthority(
                                "native Reference front-end must not claim a decoded materialization"
                                    .to_string(),
                            ));
                        }
                    }
                    Some(
                        DsdInputFrontEnd::DsdiffDst { .. }
                        | DsdInputFrontEnd::SacdDsd { .. }
                        | DsdInputFrontEnd::SacdDst { .. },
                    ) => {
                        if track.canonical_materialization_sha256.is_none() {
                            return Err(ManifestError::InvalidAuthority(
                                "decoded Reference front-end is missing canonical materialization authority"
                                    .to_string(),
                            ));
                        }
                    }
                    None => unreachable!("required above"),
                }
                match &track.execution_identity {
                    ManifestTrackExecutionIdentityV2::NativeDsdV2 {
                        executed_evidence_digest_v1,
                        executed_evidence_digest_v2,
                        executed_evidence_digest_v3,
                        ..
                    } if *executed_evidence_digest_v1 != Sha256Digest([0; 32])
                        && (!matches!(
                            policy,
                            DsdReferencePolicyVersion::SoxNg14801V3
                                | DsdReferencePolicyVersion::SoxNg14801V4
                                | DsdReferencePolicyVersion::SoxNg14801V5
                                | DsdReferencePolicyVersion::SoxNg14801V6
                                | DsdReferencePolicyVersion::SoxNg14801V7
                                | DsdReferencePolicyVersion::SoxNg14801V8
                        ) || *executed_evidence_digest_v2 != Sha256Digest([0; 32]))
                        && (!matches!(
                            policy,
                            DsdReferencePolicyVersion::SoxNg14801V7
                                | DsdReferencePolicyVersion::SoxNg14801V8
                        )
                            || *executed_evidence_digest_v3 != Sha256Digest([0; 32])) => {}
                    ManifestTrackExecutionIdentityV2::NativeDsdV2 { .. } => {
                        return Err(ManifestError::InvalidAuthority(
                            if matches!(
                                policy,
                                DsdReferencePolicyVersion::SoxNg14801V3
                                    | DsdReferencePolicyVersion::SoxNg14801V4
                                    | DsdReferencePolicyVersion::SoxNg14801V5
                                    | DsdReferencePolicyVersion::SoxNg14801V6
                                    | DsdReferencePolicyVersion::SoxNg14801V7
                                    | DsdReferencePolicyVersion::SoxNg14801V8
                            ) {
                                if matches!(
                                    policy,
                                    DsdReferencePolicyVersion::SoxNg14801V7
                                        | DsdReferencePolicyVersion::SoxNg14801V8
                                ) {
                                    "Reference v7+ track is missing v1, v2, or v3 executed verification authority"
                                } else {
                                    "Reference v3+ track is missing v1 or v2 executed verification authority"
                                }
                            } else {
                                "Reference track is missing executed verification authority"
                            }
                            .to_string(),
                        ));
                    }
                    ManifestTrackExecutionIdentityV2::LegacyPipelineV1 { .. } => {
                        return Err(ManifestError::InvalidAuthority(
                            "Reference route contains a legacy track identity".to_string(),
                        ));
                    }
                }
                if let Some(DsdSourceKind::SacdTrack { selection, .. }) =
                    track.original_dsd_source_kind.as_ref()
                {
                    if selection.frame_count == 0 || selection.toc_digest == Sha256Digest([0; 32]) {
                        return Err(ManifestError::InvalidAuthority(
                            "SACD Reference identity has an incomplete track selection".to_string(),
                        ));
                    }
                }
            }
            if *qualification_manifest_digest == Sha256Digest([0; 32]) {
                return Err(ManifestError::InvalidAuthority(
                    "Reference qualification digest is empty".to_string(),
                ));
            }
        }
        ManifestRouteIdentityV2::DsdManualV2 { .. } => {
            return Err(ManifestError::InvalidAuthority(
                "Manual DSD manifest authority is reserved and unavailable in P0".to_string(),
            ));
        }
    }
    Ok(())
}

pub fn write_manifest(album_dir: &Path, manifest: &ConversionManifest) -> Result<PathBuf, ManifestError> {
    validate_manifest(album_dir, manifest)?;
    let path = manifest_path(album_dir);
    write_manifest_file_unchecked(&path, manifest)?;
    Ok(path)
}

pub fn write_manifest_for_publish(
    temp_album_dir: &Path,
    final_album_dir: &Path,
    manifest: &ConversionManifest,
) -> Result<PathBuf, ManifestError> {
    validate_manifest_for_publish(final_album_dir, manifest)?;

    let staged_path = manifest_path(temp_album_dir);
    write_manifest_file_unchecked(&staged_path, manifest)?;

    // The file is written under the temporary album directory so it moves with
    // the atomic album rename, but callers need the durable post-publish path.
    // Returning the final path prevents PipelineReport from exposing a stale
    // temp-directory manifest path after publish succeeds.
    Ok(manifest_path(final_album_dir))
}

fn write_manifest_file_unchecked(path: &Path, manifest: &ConversionManifest) -> Result<(), ManifestError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| ManifestError::Io {
        path: parent.to_path_buf(),
        source,
    })?;

    let tmp_path = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or(MANIFEST_FILE_NAME),
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));

    let bytes = serde_json::to_vec_pretty(manifest).map_err(|source| ManifestError::Json {
        path: path.to_path_buf(),
        source,
    })?;

    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)
            .map_err(|source| ManifestError::Io {
                path: tmp_path.clone(),
                source,
            })?;
        file.write_all(&bytes).map_err(|source| ManifestError::Io {
            path: tmp_path.clone(),
            source,
        })?;
        file.sync_all().map_err(|source| ManifestError::Io {
            path: tmp_path.clone(),
            source,
        })?;
    }

    fs::rename(&tmp_path, path).map_err(|source| {
        let _ = fs::remove_file(&tmp_path);
        ManifestError::Io {
            path: path.to_path_buf(),
            source,
        }
    })?;

    sync_parent_dir_best_effort(path);
    Ok(())
}

pub fn refresh_manifest_output_facts_for_publish(
    manifest: &mut ConversionManifest,
    temp_album_dir: &Path,
    final_album_dir: &Path,
    record_output_hash: bool,
) -> Result<(), ManifestError> {
    manifest.album_dir = final_album_dir.to_path_buf();
    manifest.total_tracks = manifest.tracks.len();
    manifest.stamp_publish_timestamp(Utc::now());
    let native_reference_requires_hash = matches!(
        &manifest.route_identity,
        ManifestRouteIdentityV2::DsdReferenceV2 { .. }
    );

    for track in &mut manifest.tracks {
        let relative = validate_album_relative_output_path(&track.output_path)?;
        let staged_output = temp_album_dir.join(&relative);
        let metadata = fs::metadata(&staged_output).map_err(|source| ManifestError::Io {
            path: staged_output.clone(),
            source,
        })?;
        track.output_path = relative;
        track.output_size = metadata.len();
        track.output_hash = if record_output_hash || native_reference_requires_hash {
            Some(file_sha256_digest(&staged_output)?)
        } else {
            None
        };
    }

    validate_manifest_for_publish(final_album_dir, manifest)?;
    Ok(())
}

pub fn validate_album_relative_output_path(output_path: &Path) -> Result<PathBuf, ManifestError> {
    if output_path.is_absolute() {
        return Err(ManifestError::OutputPathNotRelative {
            path: output_path.to_path_buf(),
        });
    }

    let mut clean = PathBuf::new();
    for component in output_path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ManifestError::OutputPathEscapesAlbum {
                    path: output_path.to_path_buf(),
                });
            }
        }
    }

    if clean.as_os_str().is_empty() {
        return Err(ManifestError::EmptyOutputPath);
    }

    Ok(clean)
}

pub fn resolve_manifest_output_path(album_dir: &Path, album_relative_output_path: &Path) -> Result<PathBuf, ManifestError> {
    Ok(album_dir.join(validate_album_relative_output_path(album_relative_output_path)?))
}

pub fn album_relative_output_path(
    album_dir: &Path,
    final_output_path: &Path,
) -> Result<PathBuf, ManifestError> {
    let rel = final_output_path
        .strip_prefix(album_dir)
        .map_err(|_| ManifestError::OutputPathEscapesAlbum {
            path: final_output_path.to_path_buf(),
        })?;
    validate_album_relative_output_path(rel)
}

pub fn planned_command_hash(plan: &ConversionPlan) -> Result<String, ManifestError> {
    let bytes = serde_json::to_vec(plan).map_err(|source| ManifestError::Json {
        path: PathBuf::from("<conversion-plan>"),
        source,
    })?;
    Ok(sha256_hex(&bytes))
}

pub fn file_sha256(path: &Path) -> Result<String, ManifestError> {
    let file = File::open(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let n = reader.read(&mut buffer).map_err(|source| ManifestError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(hex::encode(hasher.finalize()))
}

pub fn file_sha256_digest(path: &Path) -> Result<Sha256Digest, ManifestError> {
    let value = file_sha256(path)?;
    Sha256Digest::from_hex(&value).map_err(ManifestError::InvalidAuthority)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

pub fn metadata_mtime_secs(metadata: &fs::Metadata) -> Result<i64, ManifestError> {
    system_time_secs(metadata.modified().map_err(|source| ManifestError::Io {
        path: PathBuf::from("<metadata.modified>"),
        source,
    })?)
}

pub fn system_time_secs(time: SystemTime) -> Result<i64, ManifestError> {
    let duration = time.duration_since(UNIX_EPOCH).map_err(|source| ManifestError::InvalidMtime {
        path: PathBuf::from("<mtime>"),
        source,
    })?;
    Ok(duration.as_secs() as i64)
}

fn same_path_lexical(left: &Path, right: &Path) -> bool {
    normalize_lexical(left) == normalize_lexical(right)
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn sync_parent_dir_best_effort(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
}


#[cfg(test)]
mod manifest_merge_gap_tests {
    use super::*;
    use tonepoet_pipeline::fingerprint::{
        settings_fingerprint, settings_snapshot_fingerprint_v2, BehaviorFingerprintV1,
        ExecutionFingerprintV1, SemanticPlanHashV1,
    };

    fn native_identity(
        v1: Sha256Digest,
        v2: Sha256Digest,
        v3: Sha256Digest,
    ) -> ManifestTrackExecutionIdentityV2 {
        ManifestTrackExecutionIdentityV2::NativeDsdV2 {
            behavior_fingerprint_v1: BehaviorFingerprintV1(Sha256Digest([1; 32])),
            execution_fingerprint_v1: ExecutionFingerprintV1(Sha256Digest([2; 32])),
            semantic_plan_hash_v1: SemanticPlanHashV1(Sha256Digest([3; 32])),
            executed_evidence_digest_v1: v1,
            executed_evidence_digest_v2: v2,
            executed_evidence_digest_v3: v3,
        }
    }

    fn reference_manifest_with_evidence(
        policy: DsdReferencePolicyVersion,
        v1: Sha256Digest,
        v2: Sha256Digest,
        v3: Sha256Digest,
    ) -> Result<ConversionManifest, ManifestError> {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_dir = temp.path().join("Album");
        let source = temp.path().join("source.dsf");
        fs::write(&source, b"reference source").expect("write source");
        let metadata = fs::metadata(&source).expect("source metadata");
        let mut settings = PipelineSettings::default();
        settings.dsd = tonepoet_pipeline::DsdSettings::native_v2();
        settings.dsd.from_dsd.reference_policy = policy;
        let track = ConversionManifestTrack::new_reference(
            source,
            &metadata,
            None,
            Sha256Digest([4; 32]),
            Sha256Digest([5; 32]),
            DsdSourceKind::DsfUncompressed,
            DsdInputFrontEnd::NativeUncompressed,
            None,
            TrackIdentity {
                source_ordinal: 1,
                disc_number: None,
                track_number: Some(1),
            },
            native_identity(v1, v2, v3),
            PathBuf::from("01.w64"),
            16,
            Sha256Digest([6; 32]),
            ValidationStatus::Passed,
        )?;
        let route = ManifestRouteIdentityV2::DsdReferenceV2 {
            settings_snapshot_fingerprint_v2: settings_snapshot_fingerprint_v2(&settings),
            resolved_output_target: ResolvedOutputTarget::WavW64,
            policy,
            qualification_manifest_digest: Sha256Digest([7; 32]),
        };
        ConversionManifest::new_reference(album_dir, settings, route, vec![track])
    }

    #[test]
    fn pre_promotion_default_manifest_uses_exact_legacy_route_and_flat_dsd_wire() {
        let manifest = ConversionManifest::new(
            PathBuf::from("Album"),
            PipelineSettings::default(),
            Vec::new(),
        );

        assert!(matches!(
            &manifest.route_identity,
            ManifestRouteIdentityV2::LegacyPipelineV1 { .. }
        ));
        let encoded = serde_json::to_value(manifest).expect("serialize default manifest");
        assert_eq!(encoded["route_identity"]["route"], "legacy_pipeline_v1");
        let dsd = encoded["settings"]["dsd"]
            .as_object()
            .expect("flat legacy DSD settings");
        assert!(!dsd.contains_key("schema_version"));
        assert!(!dsd.contains_key("from_dsd"));
    }

    #[test]
    fn native_identity_missing_v2_digest_deserializes_as_zero_for_historical_compatibility() {
        let identity = native_identity(
            Sha256Digest([8; 32]),
            Sha256Digest([9; 32]),
            Sha256Digest([10; 32]),
        );
        let mut value = serde_json::to_value(identity).expect("serialize native identity");
        value
            .as_object_mut()
            .expect("identity object")
            .remove("executed_evidence_digest_v2");
        let parsed: ManifestTrackExecutionIdentityV2 =
            serde_json::from_value(value).expect("deserialize historical native identity");
        assert!(matches!(
            parsed,
            ManifestTrackExecutionIdentityV2::NativeDsdV2 {
                executed_evidence_digest_v2,
                ..
            } if executed_evidence_digest_v2 == Sha256Digest([0; 32])
        ));
    }

    #[test]
    fn historical_policies_preserve_v1_v2_authority_and_v7_plus_require_v3() {
        let zero = Sha256Digest([0; 32]);
        let v1 = Sha256Digest([8; 32]);
        let v2 = Sha256Digest([9; 32]);
        let v3 = Sha256Digest([10; 32]);

        assert!(reference_manifest_with_evidence(
            DsdReferencePolicyVersion::SoxNg14801V2,
            v1,
            zero,
            zero,
        )
        .is_ok());

        for policy in [
            DsdReferencePolicyVersion::SoxNg14801V3,
            DsdReferencePolicyVersion::SoxNg14801V4,
            DsdReferencePolicyVersion::SoxNg14801V5,
            DsdReferencePolicyVersion::SoxNg14801V6,
        ] {
            let error = reference_manifest_with_evidence(policy, v1, zero, zero)
                .expect_err("v3-v6 must bind source/materialization evidence");
            assert!(error
                .to_string()
                .contains("missing v1 or v2 executed verification authority"));
            assert!(reference_manifest_with_evidence(policy, v1, v2, zero).is_ok());
        }

        let error = reference_manifest_with_evidence(
            DsdReferencePolicyVersion::SoxNg14801V7,
            v1,
            zero,
            zero,
        )
        .expect_err("v7 must retain v2 source/materialization authority");
        assert!(error
            .to_string()
            .contains("missing v1, v2, or v3 executed verification authority"));

        let error = reference_manifest_with_evidence(
            DsdReferencePolicyVersion::SoxNg14801V7,
            v1,
            v2,
            zero,
        )
        .expect_err("v7 must bind the ordered verification command pipeline");
        assert!(error
            .to_string()
            .contains("missing v1, v2, or v3 executed verification authority"));

        assert!(reference_manifest_with_evidence(
            DsdReferencePolicyVersion::SoxNg14801V7,
            v1,
            v2,
            v3,
        )
        .is_ok());

        let error = reference_manifest_with_evidence(
            DsdReferencePolicyVersion::SoxNg14801V8,
            v1,
            v2,
            zero,
        )
        .expect_err("v8 must bind the ordered verification command pipeline");
        assert!(error
            .to_string()
            .contains("missing v1, v2, or v3 executed verification authority"));
        assert!(reference_manifest_with_evidence(
            DsdReferencePolicyVersion::SoxNg14801V8,
            v1,
            v2,
            v3,
        )
        .is_ok());
    }

    #[test]
    fn native_identity_missing_v3_digest_deserializes_as_zero_for_historical_compatibility() {
        let identity = native_identity(
            Sha256Digest([8; 32]),
            Sha256Digest([9; 32]),
            Sha256Digest([10; 32]),
        );
        let mut value = serde_json::to_value(identity).expect("serialize native identity");
        value
            .as_object_mut()
            .expect("identity object")
            .remove("executed_evidence_digest_v3");
        let parsed: ManifestTrackExecutionIdentityV2 =
            serde_json::from_value(value).expect("deserialize historical native identity");
        assert!(matches!(
            parsed,
            ManifestTrackExecutionIdentityV2::NativeDsdV2 {
                executed_evidence_digest_v3,
                ..
            } if executed_evidence_digest_v3 == Sha256Digest([0; 32])
        ));
    }

    #[test]
    fn manifest_wire_tags_are_frozen() {
        let route = ManifestRouteIdentityV2::DsdReferenceV2 {
            settings_snapshot_fingerprint_v2: SettingsSnapshotFingerprintV2(Sha256Digest([1; 32])),
            resolved_output_target: ResolvedOutputTarget::WavW64,
            policy: DsdReferencePolicyVersion::SoxNg14801V6,
            qualification_manifest_digest: Sha256Digest([2; 32]),
        };
        let route_json = serde_json::to_value(route).expect("serialize route identity");
        assert_eq!(route_json["route"], "dsd_reference_v2");
        assert_eq!(route_json["policy"], "sox_ng_14_8_0_1_v6");
        assert_eq!(route_json["resolved_output_target"], "wav_w64");

        let execution = native_identity(
            Sha256Digest([3; 32]),
            Sha256Digest([4; 32]),
            Sha256Digest([0; 32]),
        );
        let execution_json = serde_json::to_value(execution).expect("serialize execution identity");
        assert_eq!(execution_json["kind"], "native_dsd_v2");
        assert!(execution_json.get("executed_evidence_digest_v3").is_none());

        let v7_execution = native_identity(
            Sha256Digest([3; 32]),
            Sha256Digest([4; 32]),
            Sha256Digest([5; 32]),
        );
        let v7_json = serde_json::to_value(v7_execution).expect("serialize v7 execution identity");
        assert_eq!(
            v7_json["executed_evidence_digest_v3"],
            serde_json::to_value(Sha256Digest([5; 32])).expect("serialize digest")
        );
    }

    #[test]
    fn merged_identity_marks_one_manifest_entry_for_merged_output() {
        let identity = TrackIdentity::merged_output();

        assert!(identity.is_merged_output());
        assert_eq!(identity.source_ordinal, 0);
        assert_eq!(identity.disc_number, None);
        assert_eq!(identity.track_number, None);
    }

    #[test]
    fn album_relative_output_path_rejects_paths_outside_album_dir() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_dir = temp.path().join("Album");
        let outside_output = temp.path().join("OtherAlbum").join("01.flac");

        let err = album_relative_output_path(&album_dir, &outside_output)
            .expect_err("outside output path must be rejected");

        assert!(matches!(err, ManifestError::OutputPathEscapesAlbum { .. }));
    }

    #[test]
    fn album_relative_output_path_returns_valid_relative_path_for_album_child() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_dir = temp.path().join("Album");
        let output = album_dir.join("disc1").join("01.flac");

        let relative = album_relative_output_path(&album_dir, &output)
            .expect("inside-album output path");

        assert_eq!(relative, PathBuf::from("disc1").join("01.flac"));
    }

    #[test]
    fn write_manifest_for_publish_writes_temp_manifest_but_returns_final_path() {
        let temp = tempfile::tempdir().expect("temp dir");
        let temp_album_dir = temp.path().join("Album.tmp");
        let final_album_dir = temp.path().join("Album");
        fs::create_dir_all(&temp_album_dir).expect("create temp album dir");

        let source = temp.path().join("source.flac");
        fs::write(&source, b"source-audio").expect("write source");
        let source_metadata = fs::metadata(&source).expect("source metadata");

        let settings = PipelineSettings::default();
        let fingerprint = settings_fingerprint(&settings);
        let track = ConversionManifestTrack::new(
            source,
            &source_metadata,
            None,
            TrackIdentity {
                source_ordinal: 1,
                disc_number: None,
                track_number: Some(1),
            },
            fingerprint,
            "test-planner".to_string(),
            "test-command-hash".to_string(),
            PathBuf::from("01.flac"),
            12,
            None,
            ValidationStatus::Passed,
        )
        .expect("manifest track");
        let manifest = ConversionManifest::new(final_album_dir.clone(), settings, vec![track]);

        let returned_path = write_manifest_for_publish(&temp_album_dir, &final_album_dir, &manifest)
            .expect("write manifest for publish");

        assert_eq!(returned_path, manifest_path(&final_album_dir));
        assert!(manifest_path(&temp_album_dir).exists());
        assert!(!returned_path.exists());
    }

    #[test]
    fn pre_change_v1_per_track_fixture_deserializes_without_new_manifest_fields() {
        let temp = tempfile::tempdir().expect("temp dir");
        let album_dir = temp.path().join("Album");
        let source = temp.path().join("source.flac");
        let settings = PipelineSettings::default();
        let settings_fingerprint = settings_fingerprint(&settings);

        // This fixture intentionally models the pre-change v1 manifest envelope
        // directly rather than serializing `ConversionManifest` with current code.
        // It includes only fields that existed in the v1 per-track schema.
        let fixture = format!(
            r#"{{
  "manifest_version": 1,
  "album_dir": {album_dir_json},
  "total_tracks": 1,
  "settings": {settings_json},
  "settings_fingerprint": {settings_fingerprint_json},
  "tracks": [
    {{
      "source_path": {source_path_json},
      "source_size": 12,
      "source_mtime_secs": 1710000000,
      "source_audio_md5": null,
      "track_identity": {{
        "source_ordinal": 1,
        "disc_number": null,
        "track_number": 1
      }},
      "settings_fingerprint": {settings_fingerprint_json},
      "planner_version": "pre-change-test-planner",
      "planned_command_hash": "pre-change-planned-command-hash",
      "output_path": "01.flac",
      "output_size": 13,
      "output_hash": null,
      "validation_status": "Passed",
      "publish_timestamp": "2026-05-26T00:00:00Z"
    }}
  ]
}}"#,
            album_dir_json = serde_json::to_string(&album_dir).expect("album dir json"),
            source_path_json = serde_json::to_string(&source).expect("source path json"),
            settings_json = serde_json::to_string(&settings).expect("settings json"),
            settings_fingerprint_json = serde_json::to_string(&settings_fingerprint).expect("fingerprint json"),
        );

        let fixture_value: serde_json::Value = serde_json::from_str(&fixture).expect("fixture JSON");
        assert!(fixture_value.get("merged_output").is_none());
        assert!(fixture_value["tracks"][0].get("merged_output").is_none());

        let parsed: ConversionManifest = serde_json::from_str(&fixture).expect("deserialize pre-change v1 manifest");

        validate_manifest(&album_dir, &parsed).expect("validate manifest");
        assert_eq!(parsed.manifest_version, MANIFEST_VERSION);
        assert_eq!(parsed.total_tracks, 1);
        assert_eq!(parsed.tracks.len(), 1);
        assert_eq!(parsed.legacy_settings_fingerprint(), Some(settings_fingerprint));
        assert_eq!(parsed.tracks[0].track_identity.source_ordinal, 1);
        assert_eq!(parsed.tracks[0].track_identity.disc_number, None);
        assert_eq!(parsed.tracks[0].track_identity.track_number, Some(1));
        assert!(!parsed.tracks[0].track_identity.is_merged_output());
        assert_eq!(parsed.tracks[0].output_path, PathBuf::from("01.flac"));
        assert!(matches!(
            &parsed.tracks[0].execution_identity,
            ManifestTrackExecutionIdentityV2::LegacyPipelineV1 { planned_command_hash, .. }
                if planned_command_hash == "pre-change-planned-command-hash"
        ));
    }
}
