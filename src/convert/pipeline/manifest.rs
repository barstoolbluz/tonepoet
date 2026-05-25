use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tonepoet_pipeline::fingerprint::{settings_fingerprint, SettingsFingerprint};
use tonepoet_pipeline::plan::ConversionPlan;
use tonepoet_pipeline::settings::PipelineSettings;

pub const MANIFEST_VERSION: u32 = 1;
pub const MANIFEST_FILE_NAME: &str = ".tonepoet-manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionManifest {
    pub manifest_version: u32,
    pub album_dir: PathBuf,
    pub total_tracks: usize,
    pub settings: PipelineSettings,
    pub settings_fingerprint: SettingsFingerprint,
    pub tracks: Vec<ConversionManifestTrack>,
}

impl ConversionManifest {
    pub fn new(album_dir: PathBuf, settings: PipelineSettings, tracks: Vec<ConversionManifestTrack>) -> Self {
        let settings_fingerprint = settings_fingerprint(&settings);
        Self {
            manifest_version: MANIFEST_VERSION,
            album_dir,
            total_tracks: tracks.len(),
            settings,
            settings_fingerprint,
            tracks,
        }
    }

    pub fn stamp_publish_timestamp(&mut self, timestamp: DateTime<Utc>) {
        for track in &mut self.tracks {
            track.publish_timestamp = timestamp;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionManifestTrack {
    pub source_path: PathBuf,
    pub source_size: u64,
    pub source_mtime_secs: i64,
    pub source_audio_md5: Option<String>,
    pub track_identity: TrackIdentity,
    pub settings_fingerprint: SettingsFingerprint,
    pub planner_version: String,
    pub planned_command_hash: String,

    /// Always album-relative. Never store staging, temp, or arbitrary absolute paths here.
    pub output_path: PathBuf,
    pub output_size: u64,
    pub output_hash: Option<String>,
    pub validation_status: ValidationStatus,
    pub publish_timestamp: DateTime<Utc>,
}

impl ConversionManifestTrack {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_path: PathBuf,
        source_metadata: &fs::Metadata,
        source_audio_md5: Option<String>,
        track_identity: TrackIdentity,
        settings_fingerprint: SettingsFingerprint,
        planner_version: String,
        planned_command_hash: String,
        album_relative_output_path: PathBuf,
        output_size: u64,
        output_hash: Option<String>,
        validation_status: ValidationStatus,
    ) -> Result<Self, ManifestError> {
        Ok(Self {
            source_path,
            source_size: source_metadata.len(),
            source_mtime_secs: metadata_mtime_secs(source_metadata)?,
            source_audio_md5,
            track_identity,
            settings_fingerprint,
            planner_version,
            planned_command_hash,
            output_path: album_relative_output_path,
            output_size,
            output_hash,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationStatus {
    Passed,
    Skipped,
    Failed { reason: String },
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

pub fn validate_manifest(album_dir: &Path, manifest: &ConversionManifest) -> Result<(), ManifestError> {
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
    let path = manifest_path(temp_album_dir);
    write_manifest_file_unchecked(&path, manifest)?;
    Ok(path)
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

    for track in &mut manifest.tracks {
        let relative = validate_album_relative_output_path(&track.output_path)?;
        let staged_output = temp_album_dir.join(&relative);
        let metadata = fs::metadata(&staged_output).map_err(|source| ManifestError::Io {
            path: staged_output.clone(),
            source,
        })?;
        track.output_path = relative;
        track.output_size = metadata.len();
        track.output_hash = if record_output_hash {
            Some(file_sha256(&staged_output)?)
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
