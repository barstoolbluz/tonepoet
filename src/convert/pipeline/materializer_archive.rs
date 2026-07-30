//! PR 3 — `ArchiveMaterializer` implementation.
//!
//! Extracts generic archives via the 7z/7zz `ToolRunner` backend, discovers audio files,
//! probes them with ffprobe, reads metadata with lofty, and returns
//! a `PreparedSource` with `TrackSourceRef::StagedFile` entries.
//!
//! Does not convert, tag, merge, run ReplayGain, generate feature
//! files, publish, write durable logs, or emit terminal events.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use super::errors::{MaterializeError, ToolRunnerError};
use super::progress::{heartbeat, OperationProgressTracker};
use super::reporter::PipelineReporter;
use super::tool::{ToolBinary, ToolCommand, ToolRunner};
use super::types::*;

// =========================================================================
// Audio file extensions accepted from extracted archives
// =========================================================================

const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "wav", "aiff", "aif", "wv", "mp3", "m4a", "aac", "opus", "ogg", "ape", "dsf", "dff",
    "w64", "rf64",
];

fn is_audio_extension(ext: &str) -> bool {
    AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str())
}

// =========================================================================
// ArchiveMaterializer
// =========================================================================

pub struct ArchiveMaterializer;

#[async_trait]
impl super::stages::Materializer for ArchiveMaterializer {
    async fn materialize(
        &self,
        req: &PipelineRequest,
        staging: &StagingDir,
        runner: &dyn ToolRunner,
        reporter: Option<&dyn PipelineReporter>,
        tool_paths: &HashMap<String, PathBuf>,
        cancel: &CancellationToken,
    ) -> Result<PreparedSource, MaterializeError> {
        // Ensure the materializer-owned staging directory exists.
        std::fs::create_dir_all(&staging.root)?;

        // 1. Reuse a queue-time archive preview extraction when possible.
        // If the preview directory was removed or is empty, fall back to the
        // ordinary extraction path so persisted queue entries remain recoverable.
        let extraction_root = reusable_pre_extracted_staging(req, &staging.root)
            .transpose()?
            .unwrap_or_else(|| staging.root.clone());

        if extraction_root == staging.root {
            extract_archive(req, staging, runner, reporter, tool_paths, cancel).await?;
        }

        // Check cancellation between major steps.
        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }

        // 2. Discover audio files in the extraction tree.
        let audio_files = discover_audio_files(&extraction_root)?;
        if audio_files.is_empty() {
            return Err(MaterializeError::Extraction(
                "no audio files found in archive".into(),
            ));
        }

        // 3. Probe each audio file and read metadata.
        let mut tracks = Vec::with_capacity(audio_files.len());
        for (idx, path) in audio_files.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(MaterializeError::Cancelled);
            }

            let mut probe = probe_audio_file(path, runner, cancel).await?;
            if probe.coding == Some(SourceAudioCoding::Dsd) {
                // ffprobe reports DSF/DFF byte rates and block-padded
                // durations; the container header carries the EXACT bit rate
                // and per-channel sample count. Prefer the header facts so
                // post-encode sample validation checks against reality, not
                // an estimate. Mirrors materializer_single.
                if let Ok(Some(dsd)) =
                    crate::convert::pipeline::plan_bridge::dsd_source_metadata_from_path(path)
                {
                    let header_is_authoritative = !matches!(
                        dsd.validation,
                        crate::convert::pipeline::plan_bridge::DsdPlannerValidationStatus::Errors { .. }
                    ) && dsd.sample_rate_hz > 0
                        && dsd.sample_count_per_channel.is_some_and(|count| count > 0);
                    if header_is_authoritative {
                        probe.sample_rate = dsd.sample_rate_hz;
                        probe.expected_samples = dsd.sample_count_per_channel;
                    }
                }
            }
            let ordinal = (idx + 1) as u32;
            let (mut metadata, metadata_warnings) = read_track_metadata_with_warnings(path)?;
            super::materializer_single::report_metadata_warnings(
                reporter,
                &req.item_id,
                path,
                &metadata_warnings,
                idx as f32 / audio_files.len().max(1) as f32,
            )
            .await;
            if let Some(override_set) =
                archive_metadata_override_for_track(req, ordinal, path, &extraction_root)
            {
                apply_archive_metadata_override(&mut metadata, override_set);
            }

            tracks.push(PreparedTrack {
                id: TrackId {
                    source_ordinal: ordinal,
                    disc_number: metadata.disc_number,
                    track_number: metadata.track_number.unwrap_or(ordinal),
                },
                source_ref: TrackSourceRef::StagedFile(path.clone()),
                metadata,
                expected_samples: probe.expected_samples,
                sample_rate: Some(probe.sample_rate),
                source_audio: SourceAudioDescriptor::from_scalar(
                    Some(probe.sample_rate),
                    probe.bit_depth,
                    probe.coding,
                ),
                bit_depth: probe.bit_depth,
                warnings: metadata_warnings,
            });
        }

        // 4. Apply track selection filter.
        let tracks = apply_track_selection(tracks, &req.source.track_selection)?;

        // 5. Derive album-level metadata from the tracks.
        let album_metadata = derive_album_metadata(&tracks);

        // 6. Build provenance.
        let tool_versions = archive_tool_versions(runner);
        let provenance = ExtractionProvenance {
            source_kind: SourceKind::Archive,
            source_sha256: None,
            tool_versions,
            extracted_at: chrono::Utc::now(),
        };

        Ok(PreparedSource {
            container: req.container.clone(),
            kind: SourceKind::Archive,
            tracks,
            album_metadata,
            provenance,
        })
    }
}


fn reusable_pre_extracted_staging(
    req: &PipelineRequest,
    materializer_root: &Path,
) -> Option<Result<PathBuf, MaterializeError>> {
    let staging = req.pre_extracted_staging.as_ref()?;
    let metadata = match fs::metadata(staging) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => return Some(Err(MaterializeError::Io(err))),
    };
    if !metadata.is_dir() {
        return None;
    }

    match discover_audio_files(staging) {
        Ok(files) if files.is_empty() => None,
        Ok(_) => Some(adopt_pre_extracted_staging(staging, materializer_root)),
        Err(err) => Some(Err(err)),
    }
}

fn adopt_pre_extracted_staging(
    staging: &Path,
    materializer_root: &Path,
) -> Result<PathBuf, MaterializeError> {
    let source = fs::canonicalize(staging).map_err(MaterializeError::Io)?;
    if let Ok(root) = fs::canonicalize(materializer_root) {
        if root == source {
            return Ok(root);
        }
    }

    if let Some(parent) = materializer_root.parent() {
        fs::create_dir_all(parent).map_err(MaterializeError::Io)?;
    }
    match fs::remove_dir_all(materializer_root) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(MaterializeError::Io(err)),
    }

    match fs::rename(&source, materializer_root) {
        Ok(()) => fs::canonicalize(materializer_root).map_err(MaterializeError::Io),
        Err(_) => {
            copy_dir_recursive(&source, materializer_root)?;
            match fs::remove_dir_all(&source) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(_) => {}
            }
            fs::canonicalize(materializer_root).map_err(MaterializeError::Io)
        }
    }
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), MaterializeError> {
    fs::create_dir_all(destination).map_err(MaterializeError::Io)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(source).map_err(MaterializeError::Io)? {
        entries.push(entry.map_err(MaterializeError::Io)?);
    }
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from).map_err(MaterializeError::Io)?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            continue;
        } else if file_type.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if file_type.is_file() {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(MaterializeError::Io)?;
            }
            fs::copy(&from, &to).map_err(MaterializeError::Io)?;
        }
    }
    Ok(())
}

fn archive_tool_versions(runner: &dyn ToolRunner) -> BTreeMap<String, String> {
    let mut tool_versions = BTreeMap::new();
    if let Some(version) = runner.tool_version(ToolBinary::SevenZip) {
        tool_versions.insert("7z".to_string(), version);
    }
    tool_versions
}

fn archive_metadata_override_for_track<'a>(
    req: &'a PipelineRequest,
    source_ordinal: u32,
    path: &Path,
    extraction_root: &Path,
) -> Option<&'a ArchiveTrackMetadataOverride> {
    if req.archive_metadata_overrides.is_empty() {
        return None;
    }

    if let Some(relative_path) = archive_relative_path(path, extraction_root) {
        return req.archive_metadata_overrides.iter().find(|override_set| {
            override_set.source_ordinal == source_ordinal
                && override_set.relative_path.as_path() == relative_path.as_path()
        });
    }

    req.archive_metadata_overrides
        .iter()
        .find(|override_set| override_set.source_ordinal == source_ordinal)
}

fn archive_relative_path(path: &Path, extraction_root: &Path) -> Option<PathBuf> {
    if let Ok(relative) = path.strip_prefix(extraction_root) {
        return Some(relative.to_path_buf());
    }

    let canonical_root = fs::canonicalize(extraction_root).ok()?;
    let canonical_path = fs::canonicalize(path).ok()?;
    canonical_path
        .strip_prefix(canonical_root)
        .ok()
        .map(Path::to_path_buf)
}

fn apply_archive_metadata_override(
    metadata: &mut TrackMetadata,
    override_set: &ArchiveTrackMetadataOverride,
) {
    override_set.title.apply_to(&mut metadata.title);
    override_set.artist.apply_to(&mut metadata.artist);
    override_set.genre.apply_to(&mut metadata.genre);
    override_set.date.apply_to(&mut metadata.date);
    override_set
        .album
        .apply_to_extra_key(&mut metadata.extra, "album");
}

// =========================================================================
// Archive repackaging
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepackageArchiveFormat {
    SevenZip,
    Zip,
    Tar,
    TarGz,
    Rar,
}

/// Non-fatal details from a successful archive repackage. The most important
/// case is backup cleanup: once the temp archive has been renamed into place,
/// failure to delete the old backup is not a failed edit. Surface it as a
/// warning so users know what happened without misreporting success as failure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveRepackageReport {
    pub backup_cleanup_warning: Option<String>,
    pub install_metadata_warning: Option<String>,
}

impl ArchiveRepackageReport {
    pub fn has_warnings(&self) -> bool {
        self.backup_cleanup_warning.is_some() || self.install_metadata_warning.is_some()
    }

    pub fn warning_summary(&self) -> Option<String> {
        let mut warnings = Vec::new();
        if let Some(warning) = self.install_metadata_warning.as_deref() {
            warnings.push(warning);
        }
        if let Some(warning) = self.backup_cleanup_warning.as_deref() {
            warnings.push(warning);
        }
        if warnings.is_empty() {
            None
        } else {
            Some(warnings.join("; "))
        }
    }
}

pub const ARCHIVE_REPACKAGE_CANCELLED: &str = "archive repackage cancelled";

pub fn is_archive_repackage_cancelled(error: &str) -> bool {
    error == ARCHIVE_REPACKAGE_CANCELLED
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveRepackageStage {
    Validating,
    Compressing,
    Verifying,
    PreservingMetadata,
    Installing,
    Completed,
}

impl ArchiveRepackageStage {
    pub fn status_label(self) -> &'static str {
        match self {
            Self::Validating => "Validating staged files...",
            Self::Compressing => "Compressing archive...",
            Self::Verifying => "Verifying archive...",
            Self::PreservingMetadata => "Preserving archive metadata...",
            Self::Installing => "Installing archive...",
            Self::Completed => "Completed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRepackageProgressSnapshot {
    pub stage: ArchiveRepackageStage,
    pub status: String,
    pub current_item: Option<String>,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    pub items_done: u64,
    pub items_total: Option<u64>,
    pub rate_bytes_per_sec: Option<u64>,
}

impl ArchiveRepackageProgressSnapshot {
    fn new(stage: ArchiveRepackageStage, status: impl Into<String>) -> Self {
        Self {
            stage,
            status: status.into(),
            current_item: None,
            bytes_done: 0,
            bytes_total: None,
            items_done: 0,
            items_total: Some(1),
            rate_bytes_per_sec: None,
        }
    }

    fn with_archive_bytes(
        stage: ArchiveRepackageStage,
        status: impl Into<String>,
        archive_label: &str,
        bytes_done: u64,
        bytes_total: Option<u64>,
        rate_bytes_per_sec: Option<u64>,
    ) -> Self {
        Self {
            stage,
            status: status.into(),
            current_item: Some(archive_label.to_string()),
            bytes_done,
            bytes_total,
            items_done: 0,
            items_total: Some(1),
            rate_bytes_per_sec,
        }
    }

    fn completed(archive_label: &str, bytes_total: u64) -> Self {
        Self {
            stage: ArchiveRepackageStage::Completed,
            status: "Completed".to_string(),
            current_item: Some(archive_label.to_string()),
            bytes_done: bytes_total,
            bytes_total: Some(bytes_total),
            items_done: 1,
            items_total: Some(1),
            rate_bytes_per_sec: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct RepackageStagingStats {
    regular_files: u64,
    bytes_total: u64,
}

#[derive(Debug, Clone)]
struct ArchiveInstallMetadata {
    permissions: fs::Permissions,
    accessed: Option<SystemTime>,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
}

/// Verify that this archive format can be mutated before the caller performs
/// expensive extraction and metadata reads. This is deliberately conservative:
/// callers still validate the staging tree and verify the repackaged archive at
/// commit time, but unsupported write formats and missing creator tools are
/// reported before the UI starts an edit session.
pub fn preflight_archive_repackage_capability(
    original_archive: &Path,
    tool_paths: &HashMap<String, PathBuf>,
) -> Result<(), String> {
    let format = repackage_archive_format(original_archive)?;
    match format {
        RepackageArchiveFormat::SevenZip | RepackageArchiveFormat::Zip => {
            require_repackage_tool_available(
                tool_paths,
                &["7zz", "7z"],
                "archive creation requires `7zz` or `7z`",
            )
        }
        RepackageArchiveFormat::Tar | RepackageArchiveFormat::TarGz => {
            require_repackage_tool_available(
                tool_paths,
                &["tar"],
                "tar archive creation requires the `tar` executable",
            )
        }
        RepackageArchiveFormat::Rar => require_repackage_tool_available(
            tool_paths,
            &["rar"],
            "RAR archive creation requires the `rar` executable; install rar or convert the archive to 7z before editing metadata",
        ),
    }
}

/// Re-create an archive from an extracted staging tree and atomically replace
/// the original archive only after the new container is successfully created
/// and verified.
pub async fn repackage_archive(
    staging_dir: &Path,
    original_archive: &Path,
    tool_paths: &HashMap<String, PathBuf>,
) -> Result<(), String> {
    let cancel = CancellationToken::new();
    repackage_archive_with_progress_and_cancel(
        staging_dir,
        original_archive,
        tool_paths,
        &cancel,
        |_| {},
    )
    .await
    .map(|_| ())
}

/// Repackage an archive while reporting typed progress snapshots to the caller.
///
/// The callback is intentionally synchronous so TUI callers can use non-blocking
/// channel sends and conversion callers can pass a no-op. Use
/// [`repackage_archive_with_progress_and_cancel`] when the host has a real
/// cancellation channel.
pub async fn repackage_archive_with_progress<F>(
    staging_dir: &Path,
    original_archive: &Path,
    tool_paths: &HashMap<String, PathBuf>,
    progress: F,
) -> Result<ArchiveRepackageReport, String>
where
    F: FnMut(ArchiveRepackageProgressSnapshot) + Send,
{
    let cancel = CancellationToken::new();
    repackage_archive_with_progress_and_cancel(
        staging_dir,
        original_archive,
        tool_paths,
        &cancel,
        progress,
    )
    .await
}

/// Repackage an archive with typed progress and cooperative cancellation.
///
/// Cancellation is checked before every destructive phase and is also wired into
/// child-process execution. If cancellation wins before the archive has been
/// installed, any newly-created temporary archive is removed and staged edits
/// remain untouched for retry/discard by the caller. Once installation begins,
/// replacement is allowed to finish because interrupting an atomic replace path
/// would be more dangerous than completing the already-verified save.
pub async fn repackage_archive_with_progress_and_cancel<F>(
    staging_dir: &Path,
    original_archive: &Path,
    tool_paths: &HashMap<String, PathBuf>,
    cancel: &CancellationToken,
    mut progress: F,
) -> Result<ArchiveRepackageReport, String>
where
    F: FnMut(ArchiveRepackageProgressSnapshot) + Send,
{
    let archive_label = original_archive
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| original_archive.display().to_string());

    progress(ArchiveRepackageProgressSnapshot::new(
        ArchiveRepackageStage::Validating,
        ArchiveRepackageStage::Validating.status_label(),
    ));
    check_repackage_cancelled(cancel)?;
    let staging_stats = validate_repackage_staging_tree(staging_dir, cancel)?;
    let planned_bytes = staging_stats.bytes_total.max(1);
    progress(ArchiveRepackageProgressSnapshot::with_archive_bytes(
        ArchiveRepackageStage::Validating,
        format!(
            "Validated {} staged file(s) ({})",
            staging_stats.regular_files,
            human_bytes(staging_stats.bytes_total)
        ),
        &archive_label,
        0,
        Some(planned_bytes),
        None,
    ));

    let format = repackage_archive_format(original_archive)?;
    let parent = original_archive
        .parent()
        .ok_or_else(|| format!("archive has no parent directory: {}", original_archive.display()))?;
    if !parent.is_dir() {
        return Err(format!("archive parent is not a directory: {}", parent.display()));
    }

    let file_name = original_archive
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("archive name is not valid Unicode: {}", original_archive.display()))?;
    let install_metadata = capture_archive_install_metadata(original_archive)?;
    let nonce = uuid::Uuid::new_v4();
    let temp_archive = parent.join(format!(
        ".{file_name}.tonepoet-repack-{nonce}{}",
        repackage_format_suffix(format)
    ));
    let backup_archive = parent.join(format!(".{file_name}.tonepoet-backup-{nonce}"));

    let result = async {
        check_repackage_cancelled(cancel)?;
        progress(ArchiveRepackageProgressSnapshot::with_archive_bytes(
            ArchiveRepackageStage::Compressing,
            ArchiveRepackageStage::Compressing.status_label(),
            &archive_label,
            0,
            Some(planned_bytes),
            None,
        ));
        create_repackaged_archive(
            format,
            staging_dir,
            &temp_archive,
            tool_paths,
            cancel,
            &archive_label,
            planned_bytes,
            &mut progress,
        )
        .await?;

        check_repackage_cancelled(cancel)?;
        let created_size = fs::metadata(&temp_archive)
            .map_err(|err| format!("repackaged archive metadata failed: {err}"))?
            .len();
        progress(ArchiveRepackageProgressSnapshot::with_archive_bytes(
            ArchiveRepackageStage::Verifying,
            ArchiveRepackageStage::Verifying.status_label(),
            &archive_label,
            0,
            Some(created_size.max(1)),
            None,
        ));
        verify_repackaged_archive(
            format,
            &temp_archive,
            tool_paths,
            cancel,
            &archive_label,
            created_size.max(1),
            &mut progress,
        )
        .await?;
        if created_size == 0 {
            return Err("repackaged archive is empty".to_string());
        }

        check_repackage_cancelled(cancel)?;
        progress(ArchiveRepackageProgressSnapshot::with_archive_bytes(
            ArchiveRepackageStage::PreservingMetadata,
            ArchiveRepackageStage::PreservingMetadata.status_label(),
            &archive_label,
            created_size,
            Some(created_size.max(1)),
            None,
        ));
        let install_metadata_warning =
            apply_archive_install_metadata(&temp_archive, &install_metadata);

        // Installation is deliberately not cancelled once entered: the new
        // archive has already been created and verified, and completing the
        // atomic replacement is safer than interrupting the handoff.
        progress(ArchiveRepackageProgressSnapshot::with_archive_bytes(
            ArchiveRepackageStage::Installing,
            ArchiveRepackageStage::Installing.status_label(),
            &archive_label,
            created_size,
            Some(created_size.max(1)),
            None,
        ));
        let report = replace_archive_atomically(
            original_archive,
            &temp_archive,
            &backup_archive,
            install_metadata_warning,
        )?;
        if report.has_warnings() {
            progress(ArchiveRepackageProgressSnapshot::with_archive_bytes(
                ArchiveRepackageStage::Installing,
                "Archive installed; preservation or backup cleanup needs attention.",
                &archive_label,
                created_size,
                Some(created_size.max(1)),
                None,
            ));
        }
        progress(ArchiveRepackageProgressSnapshot::completed(
            &archive_label,
            created_size.max(1),
        ));
        Ok(report)
    }
    .await;

    if result.is_err() {
        let _ = fs::remove_file(&temp_archive);
        if backup_archive.exists() && !original_archive.exists() {
            let _ = fs::rename(&backup_archive, original_archive);
        }
    }

    result
}

fn check_repackage_cancelled(cancel: &CancellationToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err(ARCHIVE_REPACKAGE_CANCELLED.to_string())
    } else {
        Ok(())
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}


fn repackage_archive_format(path: &Path) -> Result<RepackageArchiveFormat, String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
        return Ok(RepackageArchiveFormat::TarGz);
    }
    if file_name.ends_with(".tar") {
        return Ok(RepackageArchiveFormat::Tar);
    }
    match path.extension().and_then(|ext| ext.to_str()).map(|ext| ext.to_ascii_lowercase()) {
        Some(ext) if ext == "7z" => Ok(RepackageArchiveFormat::SevenZip),
        Some(ext) if ext == "zip" => Ok(RepackageArchiveFormat::Zip),
        Some(ext) if ext == "rar" => Ok(RepackageArchiveFormat::Rar),
        _ => Err(format!(
            "unsupported archive repackage format: {}",
            path.display()
        )),
    }
}

fn repackage_format_suffix(format: RepackageArchiveFormat) -> &'static str {
    match format {
        RepackageArchiveFormat::SevenZip => ".7z",
        RepackageArchiveFormat::Zip => ".zip",
        RepackageArchiveFormat::Tar => ".tar",
        RepackageArchiveFormat::TarGz => ".tar.gz",
        RepackageArchiveFormat::Rar => ".rar",
    }
}

async fn create_repackaged_archive<F>(
    format: RepackageArchiveFormat,
    staging_dir: &Path,
    temp_archive: &Path,
    tool_paths: &HashMap<String, PathBuf>,
    cancel: &CancellationToken,
    archive_label: &str,
    planned_bytes: u64,
    progress: &mut F,
) -> Result<(), String>
where
    F: FnMut(ArchiveRepackageProgressSnapshot) + Send,
{
    let monitor = RepackageCommandMonitor {
        stage: ArchiveRepackageStage::Compressing,
        status: ArchiveRepackageStage::Compressing.status_label(),
        archive_label,
        observed_path: Some(temp_archive),
        bytes_total: Some(planned_bytes),
        complete_bytes_done: Some(planned_bytes),
    };
    match format {
        RepackageArchiveFormat::SevenZip => {
            let seven_zip = repackage_tool_path(tool_paths, &["7zz", "7z"]);
            let mut command = tokio::process::Command::new(seven_zip);
            command
                .arg("a")
                .arg("-t7z")
                .arg(temp_archive)
                .arg("-mmt=on")
                .arg(".")
                .current_dir(staging_dir);
            run_repackage_command(command, "create 7z archive", cancel, monitor, progress).await
        }
        RepackageArchiveFormat::Zip => {
            let seven_zip = repackage_tool_path(tool_paths, &["7zz", "7z"]);
            let mut command = tokio::process::Command::new(seven_zip);
            command
                .arg("a")
                .arg("-tzip")
                .arg(temp_archive)
                .arg(".")
                .current_dir(staging_dir);
            run_repackage_command(command, "create zip archive", cancel, monitor, progress).await
        }
        RepackageArchiveFormat::Tar => {
            let tar = repackage_tool_path(tool_paths, &["tar"]);
            let mut command = tokio::process::Command::new(tar);
            command
                .arg("cf")
                .arg(temp_archive)
                .arg("-C")
                .arg(staging_dir)
                .arg(".");
            run_repackage_command(command, "create tar archive", cancel, monitor, progress).await
        }
        RepackageArchiveFormat::TarGz => {
            let tar = repackage_tool_path(tool_paths, &["tar"]);
            let mut command = tokio::process::Command::new(tar);
            command
                .arg("czf")
                .arg(temp_archive)
                .arg("-C")
                .arg(staging_dir)
                .arg(".");
            run_repackage_command(command, "create tar.gz archive", cancel, monitor, progress).await
        }
        RepackageArchiveFormat::Rar => {
            let rar = repackage_tool_path(tool_paths, &["rar"]);
            let mut command = tokio::process::Command::new(rar);
            command
                .arg("a")
                .arg("-r")
                .arg(temp_archive)
                .arg(".")
                .current_dir(staging_dir);
            run_repackage_command(command, "create rar archive", cancel, monitor, progress)
                .await
                .map_err(|err| {
                    if err.contains("not found") || err.contains("No such file") {
                        "RAR archive creation requires the `rar` executable; install rar or convert the archive to 7z before editing metadata".to_string()
                    } else {
                        err
                    }
                })
        }
    }
}

async fn verify_repackaged_archive<F>(
    format: RepackageArchiveFormat,
    temp_archive: &Path,
    tool_paths: &HashMap<String, PathBuf>,
    cancel: &CancellationToken,
    archive_label: &str,
    archive_bytes: u64,
    progress: &mut F,
) -> Result<(), String>
where
    F: FnMut(ArchiveRepackageProgressSnapshot) + Send,
{
    let monitor = RepackageCommandMonitor {
        stage: ArchiveRepackageStage::Verifying,
        status: ArchiveRepackageStage::Verifying.status_label(),
        archive_label,
        observed_path: Some(temp_archive),
        bytes_total: Some(archive_bytes),
        complete_bytes_done: Some(archive_bytes),
    };
    match format {
        RepackageArchiveFormat::SevenZip | RepackageArchiveFormat::Zip => {
            let seven_zip = repackage_tool_path(tool_paths, &["7zz", "7z"]);
            let mut command = tokio::process::Command::new(seven_zip);
            command.arg("t").arg(temp_archive);
            run_repackage_command(command, "verify repackaged archive", cancel, monitor, progress).await
        }
        RepackageArchiveFormat::Tar | RepackageArchiveFormat::TarGz => {
            let tar = repackage_tool_path(tool_paths, &["tar"]);
            let mut command = tokio::process::Command::new(tar);
            command.arg("tf").arg(temp_archive);
            run_repackage_command(command, "verify repackaged tar archive", cancel, monitor, progress).await
        }
        RepackageArchiveFormat::Rar => {
            let rar = repackage_tool_path(tool_paths, &["rar"]);
            let mut command = tokio::process::Command::new(rar);
            command.arg("t").arg(temp_archive);
            run_repackage_command(command, "verify repackaged rar archive", cancel, monitor, progress).await
        }
    }
}


fn require_repackage_tool_available(
    tool_paths: &HashMap<String, PathBuf>,
    names: &[&str],
    error_message: &str,
) -> Result<(), String> {
    let tool = repackage_tool_path(tool_paths, names);
    command_path_available(&tool)
        .then_some(())
        .ok_or_else(|| error_message.to_string())
}

fn command_path_available(command: &Path) -> bool {
    if command.as_os_str().is_empty() {
        return false;
    }
    if command.is_absolute()
        || command
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
    {
        return command.is_file();
    }

    let Some(command_name) = command.to_str() else {
        return false;
    };
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    let exe_suffix = std::env::consts::EXE_SUFFIX;
    for dir in std::env::split_paths(&path_var) {
        let direct = dir.join(command_name);
        if direct.is_file() {
            return true;
        }
        if !exe_suffix.is_empty() && !command_name.ends_with(exe_suffix) {
            let with_suffix = dir.join(format!("{command_name}{exe_suffix}"));
            if with_suffix.is_file() {
                return true;
            }
        }
    }
    false
}

fn repackage_tool_path(tool_paths: &HashMap<String, PathBuf>, names: &[&str]) -> PathBuf {
    for name in names {
        if let Some(path) = tool_paths.get(*name) {
            return path.clone();
        }
        if let Some((_, path)) = tool_paths
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
        {
            return path.clone();
        }
    }
    PathBuf::from(names.first().copied().unwrap_or_default())
}

struct RepackageCommandMonitor<'a> {
    stage: ArchiveRepackageStage,
    status: &'static str,
    archive_label: &'a str,
    observed_path: Option<&'a Path>,
    bytes_total: Option<u64>,
    complete_bytes_done: Option<u64>,
}

async fn run_repackage_command<F>(
    mut command: tokio::process::Command,
    label: &str,
    cancel: &CancellationToken,
    monitor: RepackageCommandMonitor<'_>,
    progress: &mut F,
) -> Result<(), String>
where
    F: FnMut(ArchiveRepackageProgressSnapshot) + Send,
{
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|err| format!("{label}: command not found or failed to start: {err}"))?;

    let mut stdout_task = child.stdout.take().map(|mut stdout| {
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = stdout.read_to_end(&mut buf).await;
            buf
        })
    });
    let mut stderr_task = child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf).await;
            buf
        })
    });

    let started = Instant::now();
    let mut last_emit = Instant::now();
    let mut last_bytes = 0u64;
    loop {
        if cancel.is_cancelled() {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(ARCHIVE_REPACKAGE_CANCELLED.to_string());
        }
        match child
            .try_wait()
            .map_err(|err| format!("{label}: failed to poll child process: {err}"))?
        {
            Some(status) => {
                let stdout = match stdout_task.take() {
                    Some(task) => task.await.unwrap_or_default(),
                    None => Vec::new(),
                };
                let stderr = match stderr_task.take() {
                    Some(task) => task.await.unwrap_or_default(),
                    None => Vec::new(),
                };
                if status.success() {
                    let done = monitor
                        .complete_bytes_done
                        .or_else(|| observed_file_len(monitor.observed_path))
                        .unwrap_or(last_bytes);
                    progress(ArchiveRepackageProgressSnapshot::with_archive_bytes(
                        monitor.stage,
                        monitor.status,
                        monitor.archive_label,
                        done,
                        monitor.bytes_total.or(Some(done.max(1))),
                        None,
                    ));
                    return Ok(());
                }
                let stderr = String::from_utf8_lossy(&stderr);
                let stdout = String::from_utf8_lossy(&stdout);
                let detail = if !stderr.trim().is_empty() {
                    stderr.trim().to_string()
                } else {
                    stdout.trim().to_string()
                };
                return if detail.is_empty() {
                    Err(format!("{label}: command exited with status {status}"))
                } else {
                    Err(format!("{label}: {detail}"))
                };
            }
            None => {
                let now = Instant::now();
                if now.saturating_duration_since(last_emit) >= Duration::from_millis(250) {
                    let bytes_done = observed_file_len(monitor.observed_path).unwrap_or(last_bytes);
                    let elapsed = now.saturating_duration_since(started).as_secs_f64();
                    let rate = if elapsed > 0.0 && bytes_done >= last_bytes {
                        Some((bytes_done as f64 / elapsed).round() as u64).filter(|rate| *rate > 0)
                    } else {
                        None
                    };
                    last_bytes = bytes_done;
                    last_emit = now;
                    progress(ArchiveRepackageProgressSnapshot::with_archive_bytes(
                        monitor.stage,
                        monitor.status,
                        monitor.archive_label,
                        monitor
                            .bytes_total
                            .map(|total| bytes_done.min(total))
                            .unwrap_or(bytes_done),
                        monitor.bytes_total,
                        rate,
                    ));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

fn observed_file_len(path: Option<&Path>) -> Option<u64> {
    path.and_then(|path| fs::metadata(path).ok().map(|metadata| metadata.len()))
}


fn capture_archive_install_metadata(path: &Path) -> Result<ArchiveInstallMetadata, String> {
    let metadata = fs::metadata(path).map_err(|err| {
        format!(
            "failed to inspect original archive metadata '{}': {err}",
            path.display()
        )
    })?;
    Ok(ArchiveInstallMetadata {
        permissions: metadata.permissions(),
        accessed: metadata.accessed().ok(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        uid: metadata.uid(),
        #[cfg(unix)]
        gid: metadata.gid(),
    })
}

fn apply_archive_install_metadata(
    temp_archive: &Path,
    original: &ArchiveInstallMetadata,
) -> Option<String> {
    let mut warnings = Vec::new();

    #[cfg(unix)]
    if let Some(warning) = apply_archive_owner(temp_archive, original) {
        warnings.push(warning);
    }

    if let Err(err) = fs::set_permissions(temp_archive, original.permissions.clone()) {
        warnings.push(format!(
            "could not preserve archive permissions on '{}': {err}",
            temp_archive.display()
        ));
    }

    if let Err(err) = apply_archive_file_times(temp_archive, original) {
        warnings.push(format!(
            "could not preserve archive timestamps on '{}': {err}",
            temp_archive.display()
        ));
    }

    if warnings.is_empty() {
        None
    } else {
        Some(warnings.join("; "))
    }
}

fn apply_archive_file_times(
    temp_archive: &Path,
    original: &ArchiveInstallMetadata,
) -> Result<(), io::Error> {
    if original.accessed.is_none() && original.modified.is_none() {
        return Ok(());
    }

    let file = fs::OpenOptions::new().read(true).open(temp_archive)?;
    let mut times = fs::FileTimes::new();
    if let Some(accessed) = original.accessed {
        times = times.set_accessed(accessed);
    }
    if let Some(modified) = original.modified {
        times = times.set_modified(modified);
    }
    file.set_times(times)
}

#[cfg(unix)]
fn apply_archive_owner(temp_archive: &Path, original: &ArchiveInstallMetadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    let current = match fs::metadata(temp_archive) {
        Ok(metadata) => metadata,
        Err(err) => {
            return Some(format!(
                "could not inspect repackaged archive ownership '{}': {err}",
                temp_archive.display()
            ));
        }
    };
    if current.uid() == original.uid && current.gid() == original.gid {
        return None;
    }

    match std::os::unix::fs::chown(temp_archive, Some(original.uid), Some(original.gid)) {
        Ok(()) => None,
        Err(err) => Some(format!(
            "could not preserve archive owner/group on '{}': {err}",
            temp_archive.display()
        )),
    }
}

fn validate_repackage_staging_tree(
    staging_dir: &Path,
    cancel: &CancellationToken,
) -> Result<RepackageStagingStats, String> {
    let root = fs::canonicalize(staging_dir)
        .map_err(|err| format!("repackage staging directory is unavailable: {err}"))?;
    if !root.is_dir() {
        return Err(format!("repackage staging path is not a directory: {}", root.display()));
    }

    let mut stack = vec![root.clone()];
    let mut stats = RepackageStagingStats::default();
    while let Some(dir) = stack.pop() {
        check_repackage_cancelled(cancel)?;
        let mut entries = fs::read_dir(&dir)
            .map_err(|err| format!("failed to read staging directory '{}': {err}", dir.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| format!("failed to read staging entry: {err}"))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            check_repackage_cancelled(cancel)?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|err| format!("failed to inspect staging entry '{}': {err}", path.display()))?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                return Err(format!(
                    "refusing to repackage archive staging tree with symlink entry: {}",
                    path.display()
                ));
            }
            let canonical = fs::canonicalize(&path)
                .map_err(|err| format!("failed to canonicalize staging entry '{}': {err}", path.display()))?;
            if !canonical.starts_with(&root) {
                return Err(format!(
                    "refusing to repackage entry outside staging root: {}",
                    path.display()
                ));
            }
            if file_type.is_dir() {
                stack.push(canonical);
            } else if file_type.is_file() {
                stats.regular_files = stats.regular_files.saturating_add(1);
                stats.bytes_total = stats.bytes_total.saturating_add(metadata.len());
            }
        }
    }

    if stats.regular_files == 0 {
        return Err("cannot repackage archive: staging tree contains no regular files".to_string());
    }
    Ok(stats)
}

fn replace_archive_atomically(
    original_archive: &Path,
    temp_archive: &Path,
    backup_archive: &Path,
    install_metadata_warning: Option<String>,
) -> Result<ArchiveRepackageReport, String> {
    fs::rename(original_archive, backup_archive).map_err(|err| {
        format!(
            "failed to move original archive '{}' to backup '{}': {err}",
            original_archive.display(),
            backup_archive.display()
        )
    })?;

    if let Err(err) = fs::rename(temp_archive, original_archive) {
        let restore = fs::rename(backup_archive, original_archive);
        return Err(match restore {
            Ok(()) => format!(
                "failed to install repackaged archive; restored original: {err}"
            ),
            Err(restore_err) => format!(
                "failed to install repackaged archive ({err}) and failed to restore original from backup '{}' ({restore_err})",
                backup_archive.display()
            ),
        });
    }

    let backup_cleanup_warning = match fs::remove_file(backup_archive) {
        Ok(()) => None,
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => Some(format!(
            "repackaged archive installed, but backup cleanup failed for '{}': {err}",
            backup_archive.display()
        )),
    };

    Ok(ArchiveRepackageReport {
        backup_cleanup_warning,
        install_metadata_warning,
    })
}

// =========================================================================
// Archive extraction
// =========================================================================

/// Extract the source archive with 7z/7zz and, when needed, expand the
/// intermediate TAR payload produced by compressed TAR containers.
async fn extract_archive(
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    reporter: Option<&dyn PipelineReporter>,
    _tool_paths: &HashMap<String, PathBuf>,
    cancel: &CancellationToken,
) -> Result<(), MaterializeError> {
    extract_archive_to_staging(
        &req.container,
        &staging.root,
        req.item_id.as_str(),
        req.source.archive_password.as_ref().map(|pw| pw.expose()),
        runner,
        reporter,
        cancel,
    )
    .await
}

/// Shared archive extraction helper used by both queue-time preview and
/// conversion-time materialization. It performs the same compressed-TAR second
/// pass as materialization so previews and conversions see the same file tree.
pub(crate) async fn extract_archive_to_staging(
    archive_path: &Path,
    staging_root: &Path,
    item_id: &str,
    archive_password: Option<&str>,
    runner: &dyn ToolRunner,
    reporter: Option<&dyn PipelineReporter>,
    cancel: &CancellationToken,
) -> Result<(), MaterializeError> {
    run_archive_extract_command(
        item_id,
        archive_path,
        staging_root,
        archive_password,
        runner,
        reporter,
        cancel,
        "archive-extraction",
        "Extracting archive...",
    )
    .await?;

    if cancel.is_cancelled() {
        return Err(MaterializeError::Cancelled);
    }

    extract_compressed_tar_payloads(
        archive_path,
        staging_root,
        item_id,
        archive_password,
        runner,
        reporter,
        cancel,
    )
    .await
}

async fn extract_compressed_tar_payloads(
    archive_path: &Path,
    staging_root: &Path,
    item_id: &str,
    archive_password: Option<&str>,
    runner: &dyn ToolRunner,
    reporter: Option<&dyn PipelineReporter>,
    cancel: &CancellationToken,
) -> Result<(), MaterializeError> {
    let tar_files = intermediate_tar_files_for_compressed_tar(archive_path, staging_root)?;
    if tar_files.is_empty() {
        return Ok(());
    }

    for tar_path in tar_files {
        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }

        run_archive_extract_command(
            item_id,
            &tar_path,
            staging_root,
            archive_password,
            runner,
            reporter,
            cancel,
            "archive-tar-expansion",
            "Expanding compressed tar payload...",
        )
        .await?;

        match fs::remove_file(&tar_path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(MaterializeError::Io(err)),
        }
    }

    Ok(())
}

/// Build and run one 7z extraction command through `ToolRunner`.
async fn run_archive_extract_command(
    item_id: &str,
    source_path: &Path,
    output_root: &Path,
    archive_password: Option<&str>,
    runner: &dyn ToolRunner,
    reporter: Option<&dyn PipelineReporter>,
    cancel: &CancellationToken,
    heartbeat_key: &'static str,
    heartbeat_message: &'static str,
) -> Result<(), MaterializeError> {
    let mut args = vec![
        "x".to_string(),
        source_path.display().to_string(),
        "-mmt=on".to_string(),
    ];
    let mut secret_args = Vec::new();

    // Passwords are exposed only at the process-argument boundary and marked
    // for transcript redaction.
    if let Some(pw) = archive_password {
        let pw_arg = format!("-p{}", pw);
        secret_args.push(args.len());
        args.push(pw_arg);
    }

    args.push(format!("-o{}", output_root.display()));
    args.push("-y".to_string());

    let cmd = ToolCommand {
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        binary: ToolBinary::SevenZip,
        args,
        secret_args,
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(3600),
    };

    let result = match reporter {
        Some(rpt) => {
            let mut tracker = OperationProgressTracker::new(
                item_id.to_string(),
                PipelineStage::Materialize,
                Some(rpt),
            );
            heartbeat::run_with_heartbeat(
                runner.run(cmd, cancel),
                &mut tracker,
                heartbeat_key,
                heartbeat_message,
                Duration::from_secs(5),
            )
            .await
        }
        None => runner.run(cmd, cancel).await,
    };

    match result {
        Ok(_output) => Ok(()),
        Err(ToolRunnerError::Cancelled { .. }) => Err(MaterializeError::Cancelled),
        Err(ToolRunnerError::NonZeroExit { stderr_tail, .. }) => {
            let lower = stderr_tail.to_lowercase();
            if lower.contains("wrong password")
                || lower.contains("encrypted")
                || lower.contains("can not open encrypted")
            {
                Err(MaterializeError::Encrypted)
            } else {
                Err(MaterializeError::Extraction(stderr_tail))
            }
        }
        Err(e) => Err(e.into()),
    }
}

fn intermediate_tar_files_for_compressed_tar(
    container: &Path,
    staging_root: &Path,
) -> Result<Vec<PathBuf>, MaterializeError> {
    if !is_compressed_tar_source(container) {
        return Ok(Vec::new());
    }

    let canonical_staging_root = fs::canonicalize(staging_root).map_err(MaterializeError::Io)?;
    let mut tar_files = Vec::new();
    for path in compressed_tar_intermediate_candidates(container, staging_root) {
        if is_regular_file_entry_within_staging(&path, &canonical_staging_root)? {
            tar_files.push(path);
        }
    }
    tar_files.sort();
    tar_files.dedup();

    if !tar_files.is_empty() {
        return Ok(tar_files);
    }

    let mut top_level_tars = Vec::new();
    for entry in fs::read_dir(&canonical_staging_root).map_err(MaterializeError::Io)? {
        let path = entry.map_err(MaterializeError::Io)?.path();
        if is_regular_file_entry_within_staging(&path, &canonical_staging_root)?
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("tar"))
        {
            top_level_tars.push(path);
        }
    }
    top_level_tars.sort();

    if top_level_tars.len() == 1 {
        Ok(top_level_tars)
    } else {
        Ok(Vec::new())
    }
}

fn is_compressed_tar_source(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".tar.gz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".tar.xz")
        || lower.ends_with(".tar.zst")
        || lower.ends_with(".tar.lz")
        || lower.ends_with(".tar.lzma")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tbz2")
        || lower.ends_with(".txz")
}

fn compressed_tar_intermediate_candidates(container: &Path, staging_root: &Path) -> Vec<PathBuf> {
    let Some(file_name) = container.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };
    let lower = file_name.to_ascii_lowercase();

    for suffix in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.lz", ".tar.lzma"] {
        if lower.ends_with(suffix) {
            let base = &file_name[..file_name.len() - suffix.len()];
            return vec![staging_root.join(format!("{base}.tar"))];
        }
    }

    for suffix in [".tgz", ".tbz2", ".txz"] {
        if lower.ends_with(suffix) {
            let base = &file_name[..file_name.len() - suffix.len()];
            return vec![staging_root.join(format!("{base}.tar"))];
        }
    }

    Vec::new()
}

// =========================================================================
// Audio file discovery
// =========================================================================

/// Recursively walk `dir`, collect regular audio files, and return them
/// sorted by path for deterministic ordering. The walker never follows
/// symlinked directories or files. Symlinks that resolve outside the staging
/// root are rejected so archive extraction cannot smuggle traversal through a
/// later decode step.
pub(crate) fn discover_archive_audio_files(dir: &Path) -> Result<Vec<PathBuf>, MaterializeError> {
    discover_audio_files(dir)
}

fn discover_audio_files(dir: &Path) -> Result<Vec<PathBuf>, MaterializeError> {
    let staging_root = fs::canonicalize(dir).map_err(MaterializeError::Io)?;
    let mut files = Vec::new();
    walk_audio_files(&staging_root, &staging_root, &mut files)?;
    files.sort();
    Ok(files)
}

fn walk_audio_files(
    dir: &Path,
    staging_root: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), MaterializeError> {
    let raw_entries = fs::read_dir(dir).map_err(MaterializeError::Io)?;
    // Collect and sort directory entries for deterministic traversal. Propagate
    // read errors instead of silently losing archive entries.
    let mut entries = Vec::new();
    for entry in raw_entries {
        entries.push(entry.map_err(MaterializeError::Io)?);
    }
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(MaterializeError::Io)?;
        let file_type = metadata.file_type();

        if file_type.is_symlink() {
            reject_external_symlink_target(&path, staging_root)?;
            continue;
        }

        let canonical = canonical_entry_within_staging(&path, staging_root)?;
        if file_type.is_dir() {
            walk_audio_files(&canonical, staging_root, out)?;
        } else if file_type.is_file() {
            if let Some(ext) = canonical.extension().and_then(|e| e.to_str()) {
                if is_audio_extension(ext) {
                    out.push(canonical);
                }
            }
        }
    }
    Ok(())
}

fn canonical_entry_within_staging(
    path: &Path,
    staging_root: &Path,
) -> Result<PathBuf, MaterializeError> {
    let canonical = fs::canonicalize(path).map_err(MaterializeError::Io)?;
    if canonical.starts_with(staging_root) {
        Ok(canonical)
    } else {
        Err(MaterializeError::Extraction(format!(
            "archive entry resolves outside staging root: {}",
            path.display()
        )))
    }
}

fn reject_external_symlink_target(
    path: &Path,
    staging_root: &Path,
) -> Result<(), MaterializeError> {
    match fs::canonicalize(path) {
        Ok(canonical) if canonical.starts_with(staging_root) => Ok(()),
        Ok(_) => Err(MaterializeError::Extraction(format!(
            "archive symlink resolves outside staging root: {}",
            path.display()
        ))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(MaterializeError::Io(err)),
    }
}

fn is_regular_file_entry_within_staging(
    path: &Path,
    staging_root: &Path,
) -> Result<bool, MaterializeError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(MaterializeError::Io(err)),
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        reject_external_symlink_target(path, staging_root)?;
        return Ok(false);
    }
    if file_type.is_file() {
        canonical_entry_within_staging(path, staging_root)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

// =========================================================================
// ffprobe probing through ToolRunner
// =========================================================================

/// Probed audio properties for one file.
pub(crate) struct ProbeResult {
    pub sample_rate: u32,
    pub expected_samples: Option<u64>,
    pub bit_depth: Option<u32>,
    pub coding: Option<SourceAudioCoding>,
}

/// Probe a single audio file via ffprobe through `ToolRunner`.
async fn probe_audio_file(
    path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<ProbeResult, MaterializeError> {
    let cmd = ToolCommand {
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        binary: ToolBinary::Ffprobe,
        args: vec![
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            "a:0".into(),
            "-show_entries".into(),
            "stream=codec_name,sample_fmt,sample_rate,duration,bits_per_raw_sample,bits_per_sample".into(),
            "-show_entries".into(),
            "format=duration".into(),
            "-of".into(),
            "json".into(),
            path.display().to_string(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(30),
    };

    let output = match runner.run(cmd, cancel).await {
        Ok(o) => o,
        Err(ToolRunnerError::Cancelled { .. }) => return Err(MaterializeError::Cancelled),
        Err(e) => return Err(e.into()),
    };

    parse_ffprobe_json(&output.stdout_tail)
}

/// Parse the JSON output of ffprobe to extract sample_rate and duration,
/// then compute expected_samples.
fn parse_ffprobe_json(json_str: &str) -> Result<ProbeResult, MaterializeError> {
    let val: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| MaterializeError::Parse(format!("ffprobe JSON parse failed: {e}")))?;

    // Sample rate: streams[0].sample_rate (string in ffprobe JSON).
    let sample_rate = val
        .pointer("/streams/0/sample_rate")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    if sample_rate == 0 {
        return Err(MaterializeError::Parse(
            "ffprobe returned no valid sample_rate".into(),
        ));
    }

    // Duration: prefer stream duration, fall back to format duration.
    let duration_secs: Option<f64> = val
        .pointer("/streams/0/duration")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| {
            val.pointer("/format/duration")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
        });

    let expected_samples = duration_secs.map(|d| (d * sample_rate as f64).round() as u64);
    let integer_bit_depth = val
        .pointer("/streams/0/bits_per_raw_sample")
        .and_then(json_u32)
        .filter(|bits| *bits > 0)
        .or_else(|| {
            val.pointer("/streams/0/bits_per_sample")
                .and_then(json_u32)
                .filter(|bits| *bits > 0)
        });
    let codec_name = val
        .pointer("/streams/0/codec_name")
        .and_then(|value| value.as_str());
    let sample_fmt = val
        .pointer("/streams/0/sample_fmt")
        .and_then(|value| value.as_str());
    let (coding, bit_depth) =
        classify_source_audio_probe(codec_name, sample_fmt, integer_bit_depth);
    let (sample_rate, expected_samples) =
        crate::convert::pipeline::normalize_dsd_probe_rate(coding, sample_rate, expected_samples);

    Ok(ProbeResult {
        sample_rate,
        expected_samples,
        bit_depth,
        coding: Some(coding),
    })
}

fn json_u32(value: &serde_json::Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .or_else(|| value.as_str().and_then(|text| text.parse::<u32>().ok()))
}

// =========================================================================
// Metadata reading via lofty (in-process, no ToolRunner)
// =========================================================================

/// Read tags from an audio file. Generic Lofty carriers retain the historical
/// empty-metadata fallback for unrecognised tags. DSF structural identity failures
/// remain materialization errors; benign metadata-container quirks degrade to
/// empty or best-effort tags with an explicit pipeline warning.
#[cfg(test)]
fn read_track_metadata(path: &Path) -> Result<TrackMetadata, MaterializeError> {
    let (metadata, warnings) = read_track_metadata_with_warnings(path)?;
    for warning in &warnings {
        log::warn!(
            "DSF metadata degraded for archived track '{}'; audio conversion will continue: {}",
            path.display(),
            warning
        );
    }
    Ok(metadata)
}

fn read_track_metadata_with_warnings(
    path: &Path,
) -> Result<(TrackMetadata, Vec<String>), MaterializeError> {
    // The archive path is not a fallback-recovered SingleFile source, so the
    // recovery-authority flag is intentionally discarded here; §1 authority is
    // scoped to marked SingleFile sources in the single-file materializer.
    let (metadata, warnings, _recovered_by_fallback) =
        super::materializer_single::read_track_metadata_with_warnings(path)?;
    Ok((metadata, warnings))
}

fn apply_track_selection(
    tracks: Vec<PreparedTrack>,
    selection: &TrackSelection,
) -> Result<Vec<PreparedTrack>, MaterializeError> {
    match selection {
        TrackSelection::All => Ok(tracks),
        TrackSelection::Range { start, end } => {
            if *start == 0 || *end == 0 || start > end {
                return Err(MaterializeError::InvalidTrackSelection(format!(
                    "invalid range {start}-{end}"
                )));
            }
            let max_ordinal = tracks.len() as u32;
            if *start > max_ordinal {
                return Err(MaterializeError::InvalidTrackSelection(format!(
                    "range start {start} exceeds track count {max_ordinal}"
                )));
            }
            Ok(tracks
                .into_iter()
                .filter(|t| t.id.source_ordinal >= *start && t.id.source_ordinal <= *end)
                .collect())
        }
        TrackSelection::Set(indices) => {
            if indices.is_empty() {
                return Err(MaterializeError::InvalidTrackSelection(
                    "empty track set".into(),
                ));
            }
            let max_ordinal = tracks.len() as u32;
            for &idx in indices {
                if idx == 0 || idx > max_ordinal {
                    return Err(MaterializeError::InvalidTrackSelection(format!(
                        "track {idx} outside valid range 1-{max_ordinal}"
                    )));
                }
            }
            Ok(tracks
                .into_iter()
                .filter(|t| indices.contains(&t.id.source_ordinal))
                .collect())
        }
    }
}

// =========================================================================
// Album metadata derivation
// =========================================================================

/// Derive `AlbumMetadata` from common tag values across all tracks.
fn derive_album_metadata(tracks: &[PreparedTrack]) -> AlbumMetadata {
    if tracks.is_empty() {
        return AlbumMetadata::default();
    }

    // Helper: if all tracks agree on a field, return it.
    fn common<F>(tracks: &[PreparedTrack], f: F) -> Option<String>
    where
        F: Fn(&TrackMetadata) -> &Option<String>,
    {
        let first = f(&tracks[0].metadata).as_ref()?;
        if tracks
            .iter()
            .all(|t| f(&t.metadata).as_deref() == Some(first))
        {
            Some(first.clone())
        } else {
            None
        }
    }

    let total_tracks = tracks.len() as u32;
    let total_discs = tracks.iter().filter_map(|t| t.id.disc_number).max();

    // Album name lives in extra["album"] (TrackMetadata has no
    // dedicated album field). Extract if all tracks agree.
    let album = {
        let first = tracks[0].metadata.extra.get("album");
        if let Some(a) = first {
            if tracks
                .iter()
                .all(|t| t.metadata.extra.get("album") == Some(a))
            {
                Some(a.clone())
            } else {
                None
            }
        } else {
            None
        }
    };

    // Promote album-wide extra tags so folder templates can use custom
    // variables from archive-contained audio files. A tag is album-wide only when
    // every prepared track that carries the key agrees on the same value.
    // This keeps per-track-only values out of folder paths while enabling
    // common release fields such as CATALOGNUMBER, BARCODE,
    // MUSICBRAINZ_ALBUMID, and RELEASECOUNTRY.
    let mut extra = BTreeMap::new();
    for key in tracks.iter().flat_map(|track| track.metadata.extra.keys()) {
        if extra.contains_key(key) {
            continue;
        }
        let Some(first) = tracks[0].metadata.extra.get(key) else {
            continue;
        };
        if tracks
            .iter()
            .all(|track| track.metadata.extra.get(key) == Some(first))
        {
            extra.insert(key.clone(), first.clone());
        }
    }

    AlbumMetadata {
        album,
        album_artist: common(tracks, |m| &m.album_artist).or_else(|| common(tracks, |m| &m.artist)),
        genre: common(tracks, |m| &m.genre),
        date: common(tracks, |m| &m.date),
        total_tracks,
        total_discs,
        disc_number: if total_discs.is_some() {
            tracks[0].id.disc_number
        } else {
            None
        },
        extra,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::stages::Materializer;
    use super::super::tool::{CommandRecord, ProcessExit, ToolOutput};
    use std::ffi::OsStr;
    use std::io::Write;
    use std::process::Command;
    use std::sync::Mutex;

    #[test]
    fn archived_source_text_items_preserve_custom_tag_provenance_and_promote_pre_emphasis() {
        use lofty::config::WriteOptions;
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::tag::{ItemKey, ItemValue, TagItem};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("archived-source.flac");
        std::fs::write(&path, include_bytes!("../../../tests/fixtures/silence.flac"))
            .expect("write FLAC fixture");
        let mut tagged = lofty::read_from_path(&path).expect("read FLAC fixture");
        if tagged.primary_tag().is_none() {
            let tag_type = tagged.primary_tag_type();
            tagged.insert_tag(lofty::tag::Tag::new(tag_type));
        }
        let tag = tagged.primary_tag_mut().expect("primary FLAC tag");
        for (key, value) in [("PRE_EMPHASIS", "1"), ("MY_NOTE", "keep me")] {
            let key = ItemKey::Unknown(key.to_string());
            tag.remove_key(&key);
            tag.insert_unchecked(TagItem::new(key, ItemValue::Text(value.to_string())));
        }
        tagged
            .save_to_path(&path, WriteOptions::default())
            .expect("save FLAC fixture tags");

        let metadata = read_track_metadata(&path).expect("read archived source metadata");
        assert!(metadata.pre_emphasis);
        assert_eq!(metadata.extra.get("my_note").map(String::as_str), Some("keep me"));
        assert_eq!(
            metadata
                .extra
                .get(&format!("{SOURCE_TEXT_TAG_EXTRA_PREFIX}my_note"))
                .map(String::as_str),
            Some("keep me")
        );
    }

    #[test]
    fn corrupt_extracted_dsf_metadata_degrades_to_empty_metadata_for_conversion() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("corrupt.dsf");
        crate::dsf_tags::write_test_dsf_fixture(&path, Some(b"NOT-AN-ID3-TAG"))
            .expect("write DSF with corrupt metadata area");

        let metadata = read_track_metadata(&path)
            .expect("unreadable DSF tag bytes must not block audio conversion");
        // Split across two asserts: 15-element tuples have no PartialEq/Debug.
        assert_eq!(
            (
                metadata.title.as_deref(),
                metadata.artist.as_deref(),
                metadata.album_artist.as_deref(),
                metadata.composer.as_deref(),
                metadata.performer.as_deref(),
                metadata.genre.as_deref(),
                metadata.date.as_deref(),
                metadata.track_number,
                metadata.disc_number,
                metadata.isrc.as_deref(),
            ),
            (None, None, None, None, None, None, None, None, None, None),
        );
        assert_eq!(
            (
                metadata.publisher.as_deref(),
                metadata.copyright.as_deref(),
                metadata.comment.as_deref(),
                metadata.pre_emphasis,
                metadata.extra.len(),
            ),
            (None, None, None, false, 0),
        );
    }

    struct VersionOnlyRunner(HashMap<ToolBinary, String>);

    #[async_trait::async_trait]
    impl ToolRunner for VersionOnlyRunner {
        async fn run(
            &self,
            _cmd: ToolCommand,
            _cancel: &CancellationToken,
        ) -> Result<super::super::tool::ToolOutput, ToolRunnerError> {
            panic!("VersionOnlyRunner must not execute commands")
        }

        fn tool_version(&self, binary: ToolBinary) -> Option<String> {
            self.0.get(&binary).cloned()
        }
    }

    struct ArchiveMaterializationFixture {
        _temp: tempfile::TempDir,
        prepared: PreparedSource,
        runner: SimulatedArchiveRunner,
        staging_root: PathBuf,
    }

    struct SimulatedArchiveRunner {
        staging_root: PathBuf,
        commands: Mutex<Vec<(ToolBinary, Vec<String>)>>,
    }

    impl SimulatedArchiveRunner {
        fn new(staging_root: PathBuf) -> Self {
            Self {
                staging_root,
                commands: Mutex::new(Vec::new()),
            }
        }

        fn command_args_for(&self, binary: ToolBinary) -> Vec<Vec<String>> {
            self.commands
                .lock()
                .expect("command transcript")
                .iter()
                .filter(|(cmd_binary, _)| cmd_binary == &binary)
                .map(|(_, args)| args.clone())
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl ToolRunner for SimulatedArchiveRunner {
        async fn run(
            &self,
            cmd: ToolCommand,
            _cancel: &CancellationToken,
        ) -> Result<ToolOutput, ToolRunnerError> {
            let binary = cmd.binary.clone();
            let args = cmd.args.clone();
            self.commands
                .lock()
                .expect("command transcript")
                .push((binary.clone(), args.clone()));

            match binary {
                ToolBinary::SevenZip => {
                    let source = args
                        .get(1)
                        .map(|arg| PathBuf::from(arg.as_str()))
                        .expect("7z extraction source argument");
                    if is_compressed_tar_source(&source) {
                        let tar_path = compressed_tar_intermediate_candidates(
                            &source,
                            &self.staging_root,
                        )
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| self.staging_root.join("payload.tar"));
                        fs::write(tar_path, b"tar payload").expect("write intermediate tar");
                    } else {
                        let track = self.staging_root.join("Disc 1").join("01.flac");
                        fs::create_dir_all(track.parent().expect("track parent"))
                            .expect("track parent");
                        fs::write(track, b"fake extracted audio").expect("write extracted audio");
                    }
                    Ok(ok_tool_output(ToolBinary::SevenZip, args, String::new()))
                }
                ToolBinary::Ffprobe => Ok(ok_tool_output(
                    ToolBinary::Ffprobe,
                    args,
                    r#"{"streams":[{"codec_name":"flac","sample_rate":"96000","duration":"1.25","bits_per_raw_sample":"24"}],"format":{"duration":"1.25"}}"#
                        .to_string(),
                )),
                _ => panic!("unexpected tool in archive materializer test"),
            }
        }

        fn tool_version(&self, binary: ToolBinary) -> Option<String> {
            match binary {
                ToolBinary::SevenZip => Some("25.01".to_string()),
                _ => None,
            }
        }
    }

    fn ok_tool_output(binary: ToolBinary, args: Vec<String>, stdout: String) -> ToolOutput {
        ToolOutput {
            exit: ProcessExit::Code(0),
            stdout_tail: stdout,
            stderr_tail: String::new(),
            elapsed: Duration::from_millis(10),
            command: CommandRecord {
                environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
                environment: std::collections::BTreeMap::new(),

                description: None,
                binary,
                sanitized_args: args,
                cwd: None,
                env_keys: Vec::new(),
                exit: Some(ProcessExit::Code(0)),
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                elapsed: Duration::from_millis(10),
            },
        }
    }

    fn archive_materializer_test_request(root: &Path, archive_name: &str) -> PipelineRequest {
        let container = root.join(archive_name);
        fs::write(&container, b"archive fixture").expect("archive fixture");
        PipelineRequest {
            actions: crate::convert::pipeline::ActionPipeline::default(),
            job_id: format!("job-{archive_name}"),
            item_id: format!("item-{archive_name}"),
            container,
            source: SourceOptions {
                archive_password: None,
                sacd_area: None,
                dvda_group_selection: DvdaGroupSelection::Default,
                dvda_group: None,
                dvda_assume_decrypted: false,
                dvda_downmix_policy: DvdaDownmixPolicy::Auto,
                dvdv_vts: None,
                dvdv_title: None,
                dvdv_audio_stream: None,
                dvdv_angle: None,
                bluray_playlist: None,
                bluray_audio_pid: None,
                bluray_audio_stream: None,
                bluray_angle: None,
                cue_sidecar: CueSidecarPolicy::IgnoreCue,
                track_selection: TrackSelection::All,
            },
            settings: tonepoet_pipeline::PipelineSettings::default(),
            worker_count: Some(1),
            scratch_staging: None,
            merge: false,
            output_root: root.join("out"),
            naming: NamingPolicy {
                template: "%NN% - %TITLE%".to_string(),
                folder_template: None,
                per_album_subdir: true,
                collision_policy: NamingCollisionPolicy::Fail,
                windows_portable: false,
            },
            publish: PublishPolicy {
                overwrite: OverwritePolicy::FailIfExists,
                same_filesystem_required: false,
                write_manifest: false,
            },
            log: LogPolicy {
                root: root.join("logs"),
                write_for_blocked: true,
                write_json_log: false,
                write_conversion_log: true,
            },
            stages: StagePolicy {
                metadata: StageRequirement::Enabled,
                replaygain: StageRequirement::Disabled,
                features: StageRequirement::Disabled,
                generate_cue: false,
            },
            failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
            pre_extracted_staging: None,
            archive_metadata_overrides: Vec::new(),
            metadata_overrides: Default::default(),
            batch_resolved_identity: None,
            album_batch: None,
            album_batch_track: None,
            suppress_incremental_conversion_log_append: false,
            expected_album_track_count: None,
            companion: CompanionCopyPolicy::default(),
            container_extension: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }

    fn archive_override(source_ordinal: u32, relative_path: &str) -> ArchiveTrackMetadataOverride {
        ArchiveTrackMetadataOverride {
            source_ordinal,
            relative_path: PathBuf::from(relative_path),
            title: MetadataTextOverride::Set("Edited Title".to_string()),
            artist: MetadataTextOverride::Keep,
            album: MetadataTextOverride::Keep,
            genre: MetadataTextOverride::Keep,
            date: MetadataTextOverride::Keep,
        }
    }

    #[test]
    fn archive_metadata_override_requires_matching_relative_path_when_available() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut req = archive_materializer_test_request(temp.path(), "source.zip");
        req.archive_metadata_overrides = vec![archive_override(1, "Disc 1/02.flac")];

        let extraction_root = temp.path().join("stage");
        let track_path = extraction_root.join("Disc 1").join("01.flac");
        let found =
            archive_metadata_override_for_track(&req, 1, &track_path, &extraction_root);

        assert!(
            found.is_none(),
            "a mismatched relative path must not fall back to source_ordinal alone"
        );
    }

    #[test]
    fn archive_metadata_override_falls_back_to_ordinal_when_relative_path_is_unavailable() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut req = archive_materializer_test_request(temp.path(), "source.zip");
        req.archive_metadata_overrides = vec![archive_override(1, "Disc 1/01.flac")];

        let extraction_root = temp.path().join("stage");
        let track_path = temp.path().join("external").join("01.flac");
        let found =
            archive_metadata_override_for_track(&req, 1, &track_path, &extraction_root);

        assert!(
            found.is_some(),
            "source_ordinal fallback remains available when no relative path can be computed"
        );
    }

    #[test]
    fn archive_metadata_override_matches_after_extraction_root_canonicalization() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut req = archive_materializer_test_request(temp.path(), "source.zip");
        req.archive_metadata_overrides = vec![archive_override(1, "Disc 1/01.flac")];

        fs::create_dir_all(temp.path().join("scratch")).expect("scratch dir");
        let extraction_root = temp.path().join("stage");
        let track_path = extraction_root.join("Disc 1").join("01.flac");
        fs::create_dir_all(track_path.parent().expect("track parent")).expect("track dir");
        fs::write(&track_path, b"audio").expect("track fixture");

        let noncanonical_root = temp.path().join("scratch").join("..").join("stage");
        let canonical_track = fs::canonicalize(&track_path).expect("canonical track");
        let found = archive_metadata_override_for_track(
            &req,
            1,
            &canonical_track,
            &noncanonical_root,
        );

        assert!(
            found.is_some(),
            "canonicalizing the extraction root should preserve exact path-guarded matches"
        );
    }

    async fn materialize_archive_fixture(archive_name: &str) -> ArchiveMaterializationFixture {
        let temp = tempfile::tempdir().expect("temp dir");
        let req = archive_materializer_test_request(temp.path(), archive_name);
        let staging_root = temp.path().join("stage");
        let staging = StagingDir::borrowed(staging_root.clone(), req.job_id.clone());
        let runner = SimulatedArchiveRunner::new(staging_root.clone());
        let cancel = CancellationToken::new();
        let tool_paths = HashMap::new();

        let prepared = ArchiveMaterializer
            .materialize(&req, &staging, &runner, None, &tool_paths, &cancel)
            .await
            .expect("archive materialization succeeds");

        ArchiveMaterializationFixture {
            _temp: temp,
            prepared,
            runner,
            staging_root,
        }
    }

    fn assert_single_archive_track(prepared: &PreparedSource, staging_root: &Path) {
        assert_eq!(prepared.kind, SourceKind::Archive);
        assert_eq!(prepared.tracks.len(), 1);
        let track = &prepared.tracks[0];
        assert_eq!(track.expected_samples, Some(120_000));
        assert_eq!(track.sample_rate, Some(96_000));
        assert_eq!(track.bit_depth, Some(24));
        assert_eq!(
            track.source_audio,
            SourceAudioDescriptor::from_scalar(
                Some(96_000),
                Some(24),
                Some(SourceAudioCoding::Pcm),
            )
        );
        let TrackSourceRef::StagedFile(path) = &track.source_ref else {
            panic!("archive materializer must stage extracted files");
        };
        let canonical_staging = fs::canonicalize(staging_root).expect("canonical staging root");
        assert!(
            path.starts_with(&canonical_staging),
            "staged track must remain under staging root: {}",
            path.display()
        );
    }


    #[test]
    fn archive_metadata_override_applies_compact_edits_and_explicit_clears() {
        let mut metadata = TrackMetadata {
            title: Some("Original Title".to_string()),
            artist: Some("Original Artist".to_string()),
            genre: Some("Rock".to_string()),
            date: Some("1984".to_string()),
            extra: BTreeMap::from([("album".to_string(), "Original Album".to_string())]),
            ..TrackMetadata::default()
        };
        let override_set = ArchiveTrackMetadataOverride {
            source_ordinal: 1,
            relative_path: PathBuf::from("Disc 1/01.flac"),
            title: MetadataTextOverride::Set("Edited Title".to_string()),
            artist: MetadataTextOverride::Clear,
            album: MetadataTextOverride::Set("Edited Album".to_string()),
            genre: MetadataTextOverride::Keep,
            date: MetadataTextOverride::Set("2026".to_string()),
        };

        apply_archive_metadata_override(&mut metadata, &override_set);

        assert_eq!(metadata.title.as_deref(), Some("Edited Title"));
        assert_eq!(metadata.artist, None);
        assert_eq!(metadata.extra.get("album").map(String::as_str), Some("Edited Album"));
        assert_eq!(metadata.genre.as_deref(), Some("Rock"));
        assert_eq!(metadata.date.as_deref(), Some("2026"));
    }

    fn assert_single_real_archive_track(prepared: &PreparedSource, staging_root: &Path) {
        assert_eq!(prepared.kind, SourceKind::Archive);
        assert_eq!(prepared.tracks.len(), 1);
        let track = &prepared.tracks[0];
        assert_eq!(track.sample_rate, Some(44_100));
        assert_eq!(track.bit_depth, Some(16));
        assert_eq!(
            track.source_audio,
            SourceAudioDescriptor::from_scalar(
                Some(44_100),
                Some(16),
                Some(SourceAudioCoding::Pcm),
            )
        );
        assert_eq!(track.expected_samples, Some(2_205));
        let TrackSourceRef::StagedFile(path) = &track.source_ref else {
            panic!("archive materializer must stage extracted files");
        };
        let canonical_staging = fs::canonicalize(staging_root).expect("canonical staging root");
        assert!(
            path.starts_with(&canonical_staging),
            "staged track must remain under staging root: {}",
            path.display()
        );
        assert_eq!(
            path.file_name().and_then(OsStr::to_str),
            Some("01.wav"),
            "real archive extraction should discover the extracted WAV fixture"
        );
    }

    struct RealArchiveIntegrationRunner {
        seven_zip: PathBuf,
        ffprobe: PathBuf,
        commands: Mutex<Vec<(ToolBinary, Vec<String>)>>,
    }

    impl RealArchiveIntegrationRunner {
        fn new(seven_zip: PathBuf, ffprobe: PathBuf) -> Self {
            Self {
                seven_zip,
                ffprobe,
                commands: Mutex::new(Vec::new()),
            }
        }

        fn command_args_for(&self, binary: ToolBinary) -> Vec<Vec<String>> {
            self.commands
                .lock()
                .expect("command transcript")
                .iter()
                .filter(|(cmd_binary, _)| cmd_binary == &binary)
                .map(|(_, args)| args.clone())
                .collect()
        }

        fn path_for(&self, binary: &ToolBinary) -> Option<&Path> {
            match binary {
                ToolBinary::SevenZip => Some(&self.seven_zip),
                ToolBinary::Ffprobe => Some(&self.ffprobe),
                _ => None,
            }
        }
    }

    #[async_trait::async_trait]
    impl ToolRunner for RealArchiveIntegrationRunner {
        async fn run(
            &self,
            cmd: ToolCommand,
            cancel: &CancellationToken,
        ) -> Result<ToolOutput, ToolRunnerError> {
            if cancel.is_cancelled() {
                let environment_policy = cmd.environment_policy;
                let environment = cmd.sanitized_environment();
                let env_keys = cmd.env_keys();
                return Err(ToolRunnerError::Cancelled {
                    command: CommandRecord {
                        environment_policy,
                        environment,
                        description: None,
                        binary: cmd.binary,
                        sanitized_args: cmd.args,
                        cwd: cmd.cwd,
                        env_keys,
                        exit: None,
                        stdout_tail: String::new(),
                        stderr_tail: String::new(),
                        elapsed: Duration::from_millis(0),
                    },
                });
            }

            let binary = cmd.binary.clone();
            let args = cmd.args.clone();
            self.commands
                .lock()
                .expect("command transcript")
                .push((binary.clone(), args.clone()));

            let Some(binary_path) = self.path_for(&binary) else {
                return Err(ToolRunnerError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no integration-test binary for {binary:?}"),
                )));
            };

            let started = std::time::Instant::now();
            let mut command = Command::new(binary_path);
            command.args(&args);
            if let Some(cwd) = &cmd.cwd {
                command.current_dir(cwd);
            }
            if cmd.environment_policy
                == tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
            {
                command.env_clear();
            }
            for env in &cmd.env {
                command.env(&env.key, env.value.expose());
            }

            let output = command.output().map_err(ToolRunnerError::Io)?;
            let elapsed = started.elapsed();
            let code = output.status.code().unwrap_or(-1);
            let exit = ProcessExit::Code(code);
            let record_exit = ProcessExit::Code(code);
            let stdout_tail = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr_tail = String::from_utf8_lossy(&output.stderr).into_owned();
            let environment_policy = cmd.environment_policy;
            let environment = cmd.sanitized_environment();
            let env_keys = cmd.env_keys();
            let record = CommandRecord {
                environment_policy,
                environment,
                description: None,
                binary: binary.clone(),
                sanitized_args: args.clone(),
                cwd: cmd.cwd,
                env_keys,
                exit: Some(record_exit),
                stdout_tail: stdout_tail.clone(),
                stderr_tail: stderr_tail.clone(),
                elapsed,
            };

            if output.status.success() {
                Ok(ToolOutput {
                    exit,
                    stdout_tail,
                    stderr_tail,
                    elapsed,
                    command: record,
                })
            } else {
                Err(ToolRunnerError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!(
                        "integration command failed: {:?} {:?}: {}",
                        binary_path, args, stderr_tail
                    ),
                )))
            }
        }

        fn tool_version(&self, binary: ToolBinary) -> Option<String> {
            match binary {
                ToolBinary::SevenZip => Some("integration-test-7z".to_string()),
                ToolBinary::Ffprobe => Some("integration-test-ffprobe".to_string()),
                _ => None,
            }
        }
    }

    fn find_executable(candidates: &[&str]) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        let exe_suffix = std::env::consts::EXE_SUFFIX;
        for dir in std::env::split_paths(&path) {
            for candidate in candidates {
                let direct = dir.join(candidate);
                if direct.is_file() {
                    return Some(direct);
                }
                if !exe_suffix.is_empty() && !candidate.ends_with(exe_suffix) {
                    let with_suffix = dir.join(format!("{candidate}{exe_suffix}"));
                    if with_suffix.is_file() {
                        return Some(with_suffix);
                    }
                }
            }
        }
        None
    }

    fn required_archive_integration_tools() -> Option<(PathBuf, PathBuf)> {
        let seven_zip = find_executable(&["7zz", "7z"]);
        let ffprobe = find_executable(&["ffprobe"]);
        match (seven_zip, ffprobe) {
            (Some(seven_zip), Some(ffprobe)) => Some((seven_zip, ffprobe)),
            _ => None,
        }
    }

    fn run_fixture_command(
        program: &Path,
        args: &[&OsStr],
        cwd: Option<&Path>,
    ) -> std::io::Result<()> {
        let mut command = Command::new(program);
        command.args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let output = command.output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!(
                    "fixture command failed: {:?} {:?}: {}",
                    program,
                    args,
                    String::from_utf8_lossy(&output.stderr)
                ),
            ))
        }
    }

    fn write_pcm_wav(path: &Path) -> std::io::Result<()> {
        let sample_rate = 44_100u32;
        let samples = 2_205u32;
        let channels = 1u16;
        let bits_per_sample = 16u16;
        let bytes_per_sample = u32::from(bits_per_sample / 8);
        let data_len = samples * u32::from(channels) * bytes_per_sample;
        let byte_rate = sample_rate * u32::from(channels) * bytes_per_sample;
        let block_align = channels * (bits_per_sample / 8);

        let mut file = fs::File::create(path)?;
        file.write_all(b"RIFF")?;
        file.write_all(&(36 + data_len).to_le_bytes())?;
        file.write_all(b"WAVE")?;
        file.write_all(b"fmt ")?;
        file.write_all(&16u32.to_le_bytes())?;
        file.write_all(&1u16.to_le_bytes())?;
        file.write_all(&channels.to_le_bytes())?;
        file.write_all(&sample_rate.to_le_bytes())?;
        file.write_all(&byte_rate.to_le_bytes())?;
        file.write_all(&block_align.to_le_bytes())?;
        file.write_all(&bits_per_sample.to_le_bytes())?;
        file.write_all(b"data")?;
        file.write_all(&data_len.to_le_bytes())?;
        for _ in 0..samples {
            file.write_all(&0i16.to_le_bytes())?;
        }
        Ok(())
    }

    fn create_real_zip_fixture(seven_zip: &Path, root: &Path) -> std::io::Result<PathBuf> {
        let archive = root.join("Album.zip");
        let source_dir = root.join("zip-src");
        fs::create_dir_all(source_dir.join("Disc 1"))?;
        write_pcm_wav(&source_dir.join("Disc 1").join("01.wav"))?;
        run_fixture_command(
            seven_zip,
            &[
                OsStr::new("a"),
                OsStr::new("-tzip"),
                archive.as_os_str(),
                OsStr::new("Disc 1/01.wav"),
            ],
            Some(&source_dir),
        )?;
        Ok(archive)
    }

    fn create_real_tar_fixture(seven_zip: &Path, root: &Path) -> std::io::Result<PathBuf> {
        let archive = root.join("Album.tar");
        let source_dir = root.join("tar-src");
        fs::create_dir_all(source_dir.join("Disc 1"))?;
        write_pcm_wav(&source_dir.join("Disc 1").join("01.wav"))?;
        run_fixture_command(
            seven_zip,
            &[
                OsStr::new("a"),
                OsStr::new("-ttar"),
                archive.as_os_str(),
                OsStr::new("Disc 1/01.wav"),
            ],
            Some(&source_dir),
        )?;
        Ok(archive)
    }

    fn create_real_targz_fixture(seven_zip: &Path, root: &Path) -> std::io::Result<PathBuf> {
        let tar = create_real_tar_fixture(seven_zip, root)?;
        let targz = root.join("Album.tar.gz");
        run_fixture_command(
            seven_zip,
            &[
                OsStr::new("a"),
                OsStr::new("-tgzip"),
                targz.as_os_str(),
                tar.file_name().expect("tar fixture file name"),
            ],
            tar.parent(),
        )?;
        fs::remove_file(tar)?;
        Ok(targz)
    }

    fn create_real_rar_fixture(root: &Path) -> std::io::Result<Option<PathBuf>> {
        let archive = root.join("Album.rar");
        if let Some(fixture) = std::env::var_os("TONEPOET_TEST_RAR_FIXTURE") {
            fs::copy(PathBuf::from(fixture), &archive)?;
            return Ok(Some(archive));
        }

        let Some(rar) = find_executable(&["rar"]) else {
            return Ok(None);
        };

        let source_dir = root.join("rar-src");
        fs::create_dir_all(source_dir.join("Disc 1"))?;
        write_pcm_wav(&source_dir.join("Disc 1").join("01.wav"))?;
        run_fixture_command(
            &rar,
            &[
                OsStr::new("a"),
                OsStr::new("-idq"),
                archive.as_os_str(),
                OsStr::new("Disc 1/01.wav"),
            ],
            Some(&source_dir),
        )?;
        Ok(Some(archive))
    }

    struct RealArchiveMaterializationFixture {
        _temp: tempfile::TempDir,
        prepared: PreparedSource,
        runner: RealArchiveIntegrationRunner,
        staging_root: PathBuf,
    }

    async fn materialize_real_archive_fixture(
        archive: PathBuf,
        seven_zip: PathBuf,
        ffprobe: PathBuf,
    ) -> RealArchiveMaterializationFixture {
        let temp = tempfile::tempdir().expect("temp dir");
        let archive_name = archive
            .file_name()
            .and_then(OsStr::to_str)
            .expect("archive name")
            .to_string();
        let req = archive_materializer_test_request(temp.path(), &archive_name);
        fs::copy(&archive, &req.container).expect("copy archive fixture into request root");
        let staging_root = temp.path().join("stage");
        let staging = StagingDir::borrowed(staging_root.clone(), req.job_id.clone());
        let runner = RealArchiveIntegrationRunner::new(seven_zip, ffprobe);
        let cancel = CancellationToken::new();
        let tool_paths = HashMap::new();

        let prepared = ArchiveMaterializer
            .materialize(&req, &staging, &runner, None, &tool_paths, &cancel)
            .await
            .expect("real archive materialization succeeds");

        RealArchiveMaterializationFixture {
            _temp: temp,
            prepared,
            runner,
            staging_root,
        }
    }

    // These integration tests create real archive fixtures and exercise the
    // same 7z/7zz extraction command path used by production. They return
    // early when the required local tools are not installed.
    #[tokio::test]
    async fn archive_materializer_extracts_real_zip_tar_and_targz_with_real_7z_when_available() {
        let Some((seven_zip, ffprobe)) = required_archive_integration_tools() else {
            eprintln!(
                "skipping real archive extraction integration test: 7z/7zz and ffprobe are required"
            );
            return;
        };

        for (archive_label, make_fixture, expected_archive_commands) in [
            (
                "zip",
                create_real_zip_fixture as fn(&Path, &Path) -> std::io::Result<PathBuf>,
                1usize,
            ),
            (
                "tar",
                create_real_tar_fixture as fn(&Path, &Path) -> std::io::Result<PathBuf>,
                1usize,
            ),
            (
                "tar.gz",
                create_real_targz_fixture as fn(&Path, &Path) -> std::io::Result<PathBuf>,
                2usize,
            ),
        ] {
            let fixture_root = tempfile::tempdir().expect("fixture temp dir");
            let archive = make_fixture(&seven_zip, fixture_root.path())
                .unwrap_or_else(|err| panic!("create real {archive_label} fixture: {err}"));
            let materialized = materialize_real_archive_fixture(
                archive,
                seven_zip.clone(),
                ffprobe.clone(),
            )
            .await;

            assert_single_real_archive_track(&materialized.prepared, &materialized.staging_root);
            assert_eq!(
                materialized
                    .runner
                    .command_args_for(ToolBinary::SevenZip)
                    .len(),
                expected_archive_commands,
                "real {archive_label} materialization should use the expected extractor passes"
            );
            assert_eq!(
                materialized
                    .runner
                    .command_args_for(ToolBinary::Ffprobe)
                    .len(),
                1,
                "real {archive_label} materialization should probe the extracted audio once"
            );
        }
    }

    #[tokio::test]
    async fn archive_materializer_extracts_real_rar_fixture_when_available() {
        let Some((seven_zip, ffprobe)) = required_archive_integration_tools() else {
            eprintln!(
                "skipping real RAR extraction integration test: 7z/7zz and ffprobe are required"
            );
            return;
        };

        let fixture_root = tempfile::tempdir().expect("fixture temp dir");
        let Some(archive) = create_real_rar_fixture(fixture_root.path())
            .expect("create or load real RAR fixture")
        else {
            eprintln!(
                "skipping real RAR extraction integration test: provide TONEPOET_TEST_RAR_FIXTURE or install rar to create the fixture"
            );
            return;
        };

        let materialized = materialize_real_archive_fixture(archive, seven_zip, ffprobe).await;
        assert_single_real_archive_track(&materialized.prepared, &materialized.staging_root);
        assert_eq!(
            materialized
                .runner
                .command_args_for(ToolBinary::SevenZip)
                .len(),
            1,
            "real RAR materialization should require one archive extraction pass"
        );
        assert_eq!(
            materialized
                .runner
                .command_args_for(ToolBinary::Ffprobe)
                .len(),
            1,
            "real RAR materialization should probe the extracted audio once"
        );
    }

    fn write_repackage_staging(root: &Path, content: &str) -> std::io::Result<PathBuf> {
        let staging = root.join("stage");
        let track_dir = staging.join("Disc 1");
        fs::create_dir_all(&track_dir)?;
        fs::write(track_dir.join("01.txt"), content)?;
        fs::write(staging.join("manifest.txt"), "manifest")?;
        Ok(staging)
    }

    fn extract_tar_archive(tar: &Path, archive: &Path, out_dir: &Path, gz: bool) -> std::io::Result<()> {
        fs::create_dir_all(out_dir)?;
        if gz {
            run_fixture_command(
                tar,
                &[
                    OsStr::new("xzf"),
                    archive.as_os_str(),
                    OsStr::new("-C"),
                    out_dir.as_os_str(),
                ],
                None,
            )
        } else {
            run_fixture_command(
                tar,
                &[
                    OsStr::new("xf"),
                    archive.as_os_str(),
                    OsStr::new("-C"),
                    out_dir.as_os_str(),
                ],
                None,
            )
        }
    }

    fn extract_seven_zip_archive(seven_zip: &Path, archive: &Path, out_dir: &Path) -> std::io::Result<()> {
        fs::create_dir_all(out_dir)?;
        let output_arg = format!("-o{}", out_dir.display());
        run_fixture_command(
            seven_zip,
            &[
                OsStr::new("x"),
                OsStr::new("-y"),
                OsStr::new(&output_arg),
                archive.as_os_str(),
            ],
            None,
        )
    }

    fn assert_repackaged_content(out_dir: &Path, expected: &str) {
        let nested = fs::read_to_string(out_dir.join("Disc 1").join("01.txt"))
            .expect("repackaged nested file");
        let manifest = fs::read_to_string(out_dir.join("manifest.txt"))
            .expect("repackaged top-level file");
        assert_eq!(nested, expected);
        assert_eq!(manifest, "manifest");
    }

    #[tokio::test]
    async fn repackage_archive_recreates_real_tar_and_targz_and_replaces_original_atomically() {
        let Some(tar) = find_executable(&["tar"]) else {
            eprintln!("skipping real tar repackage integration test: tar is required");
            return;
        };

        for (archive_name, gz) in [("Album.tar", false), ("Album.tar.gz", true)] {
            let temp = tempfile::tempdir().expect("temp dir");
            let original = temp.path().join(archive_name);
            fs::write(&original, b"old archive bytes").expect("original archive placeholder");
            let expected = format!("edited payload for {archive_name}");
            let staging = write_repackage_staging(temp.path(), &expected)
                .expect("write repackage staging");
            let tool_paths = HashMap::from([("tar".to_string(), tar.clone())]);

            let mut progress = Vec::new();
            let report = repackage_archive_with_progress(&staging, &original, &tool_paths, |snapshot| {
                progress.push(snapshot);
            })
            .await
            .unwrap_or_else(|err| panic!("repackage {archive_name}: {err}"));

            assert!(
                report.backup_cleanup_warning.is_none(),
                "successful fixture repackage should not report cleanup warnings"
            );
            assert!(
                progress.iter().any(|snapshot| snapshot.stage == ArchiveRepackageStage::Validating)
                    && progress.iter().any(|snapshot| snapshot.stage == ArchiveRepackageStage::Compressing)
                    && progress.iter().any(|snapshot| snapshot.stage == ArchiveRepackageStage::Verifying)
                    && progress.iter().any(|snapshot| snapshot.stage == ArchiveRepackageStage::Installing)
                    && progress.iter().any(|snapshot| snapshot.bytes_total.is_some()),
                "repackage should emit typed progress snapshots with phases and byte totals: {progress:?}"
            );
            assert!(original.exists(), "original archive path must be restored after replacement");
            assert!(
                fs::read_dir(temp.path())
                    .expect("temp dir entries")
                    .filter_map(Result::ok)
                    .all(|entry| !entry.file_name().to_string_lossy().contains("tonepoet-backup")),
                "successful repackage must remove backup files"
            );
            let out = temp.path().join("extract");
            extract_tar_archive(&tar, &original, &out, gz)
                .unwrap_or_else(|err| panic!("extract repackaged {archive_name}: {err}"));
            assert_repackaged_content(&out, &expected);
        }
    }


    #[tokio::test]
    async fn repackage_archive_cancelled_before_create_preserves_original_and_reports_cancel() {
        let temp = tempfile::tempdir().expect("temp dir");
        let original = temp.path().join("Album.tar");
        fs::write(&original, b"old archive bytes").expect("original archive placeholder");
        let staging = write_repackage_staging(temp.path(), "edited payload")
            .expect("write repackage staging");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut progress = Vec::new();

        let err = repackage_archive_with_progress_and_cancel(
            &staging,
            &original,
            &HashMap::new(),
            &cancel,
            |snapshot| progress.push(snapshot),
        )
        .await
        .expect_err("pre-cancelled repackage should abort");

        assert_eq!(err, ARCHIVE_REPACKAGE_CANCELLED);
        assert!(original.exists(), "cancel must preserve the original archive");
        assert!(
            fs::read_dir(temp.path())
                .expect("temp dir entries")
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().contains("tonepoet-repack")),
            "cancel before create must not leave temp repack artifacts"
        );
        assert!(
            progress
                .iter()
                .any(|snapshot| snapshot.stage == ArchiveRepackageStage::Validating),
            "cancel should still emit the initial validating snapshot"
        );
    }

    #[tokio::test]
    async fn repackage_archive_recreates_real_zip_and_7z_when_7z_is_available() {
        let Some(seven_zip) = find_executable(&["7zz", "7z"]) else {
            eprintln!("skipping real zip/7z repackage integration test: 7z/7zz is required");
            return;
        };

        for archive_name in ["Album.zip", "Album.7z"] {
            let temp = tempfile::tempdir().expect("temp dir");
            let original = temp.path().join(archive_name);
            fs::write(&original, b"old archive bytes").expect("original archive placeholder");
            let expected = format!("edited payload for {archive_name}");
            let staging = write_repackage_staging(temp.path(), &expected)
                .expect("write repackage staging");
            let tool_paths = HashMap::from([
                ("7zz".to_string(), seven_zip.clone()),
                ("7z".to_string(), seven_zip.clone()),
            ]);

            repackage_archive(&staging, &original, &tool_paths)
                .await
                .unwrap_or_else(|err| panic!("repackage {archive_name}: {err}"));

            let out = temp.path().join("extract");
            extract_seven_zip_archive(&seven_zip, &original, &out)
                .unwrap_or_else(|err| panic!("extract repackaged {archive_name}: {err}"));
            assert_repackaged_content(&out, &expected);
        }
    }

    #[test]
    fn preflight_reports_missing_rar_creator_before_extraction_work() {
        let temp = tempfile::tempdir().expect("temp dir");
        let original = temp.path().join("Album.rar");
        fs::write(&original, b"rar placeholder").expect("archive placeholder");
        let missing_rar = temp.path().join("definitely-missing-rar-binary");
        let tool_paths = HashMap::from([("rar".to_string(), missing_rar)]);

        let err = preflight_archive_repackage_capability(&original, &tool_paths)
            .expect_err("missing rar creator must be reported before extraction");

        assert!(
            err.contains("RAR archive creation requires the `rar` executable"),
            "missing rar preflight error should be actionable: {err}"
        );
    }

    #[tokio::test]
    async fn repackage_archive_reports_missing_rar_creator_without_replacing_original() {
        let temp = tempfile::tempdir().expect("temp dir");
        let original = temp.path().join("Album.rar");
        fs::write(&original, b"original rar placeholder").expect("original archive placeholder");
        let staging = write_repackage_staging(temp.path(), "edited rar payload")
            .expect("write repackage staging");
        let missing_rar = temp.path().join("definitely-missing-rar-binary");
        let tool_paths = HashMap::from([("rar".to_string(), missing_rar)]);

        let err = repackage_archive(&staging, &original, &tool_paths)
            .await
            .expect_err("missing rar tool should fail");

        assert!(
            err.contains("RAR archive creation requires the `rar` executable"),
            "missing rar error should be actionable: {err}"
        );
        assert_eq!(
            fs::read(&original).expect("original archive after failed rar repackage"),
            b"original rar placeholder",
            "failed rar creation must not replace the original archive"
        );
    }

    #[test]
    fn replace_archive_atomically_restores_original_when_temp_install_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        let original = temp.path().join("Album.zip");
        let missing_temp = temp.path().join("missing-new-archive.zip");
        let backup = temp.path().join("Album.zip.backup");
        fs::write(&original, b"original archive").expect("original archive");

        let err = replace_archive_atomically(&original, &missing_temp, &backup, None)
            .expect_err("missing temp archive should fail install");

        assert!(
            err.contains("restored original"),
            "install failure should report successful restoration: {err}"
        );
        assert_eq!(
            fs::read(&original).expect("restored original archive"),
            b"original archive"
        );
        assert!(
            !backup.exists(),
            "backup path should be consumed by restoration after failed install"
        );
    }

    #[cfg(unix)]
    #[test]
    fn archive_install_metadata_preserves_mode_and_modified_time() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        use std::time::UNIX_EPOCH;

        let temp = tempfile::tempdir().expect("temp dir");
        let original = temp.path().join("Album.zip");
        let temp_archive = temp.path().join("new.zip");
        let backup = temp.path().join("Album.zip.backup");
        fs::write(&original, b"original archive").expect("original archive");
        fs::write(&temp_archive, b"new archive").expect("new archive");
        fs::set_permissions(&original, fs::Permissions::from_mode(0o640))
            .expect("set original permissions");

        let timestamp = UNIX_EPOCH + Duration::from_secs(1_234_567);
        fs::File::open(&original)
            .expect("open original")
            .set_times(
                fs::FileTimes::new()
                    .set_accessed(timestamp)
                    .set_modified(timestamp),
            )
            .expect("set original timestamps");
        let original_meta = fs::metadata(&original).expect("original metadata");
        let expected_uid = original_meta.uid();
        let expected_gid = original_meta.gid();

        let install_metadata = capture_archive_install_metadata(&original)
            .expect("capture original install metadata");
        let warning = apply_archive_install_metadata(&temp_archive, &install_metadata);
        let report = replace_archive_atomically(&original, &temp_archive, &backup, warning)
            .expect("install archive");

        assert!(
            report.warning_summary().is_none(),
            "same-owner metadata preservation should not warn: {:?}",
            report.warning_summary()
        );
        let installed = fs::metadata(&original).expect("installed metadata");
        assert_eq!(installed.permissions().mode() & 0o777, 0o640);
        assert_eq!(installed.uid(), expected_uid);
        assert_eq!(installed.gid(), expected_gid);
        assert_eq!(
            installed
                .modified()
                .expect("installed modified time")
                .duration_since(UNIX_EPOCH)
                .expect("mtime after epoch")
                .as_secs(),
            1_234_567
        );
    }

    // These deterministic tests use a simulated runner. They validate
    // orchestration, staging discovery, ffprobe parsing, metadata plumbing,
    // and compressed-TAR second-pass behavior without external tools.
    #[tokio::test]
    async fn archive_materializer_orchestrates_zip_rar_and_plain_tar_extraction_with_simulated_runner() {
        for archive_name in ["Album.zip", "Album.rar", "Album.tar"] {
            let fixture = materialize_archive_fixture(archive_name).await;
            assert_single_archive_track(&fixture.prepared, &fixture.staging_root);

            let archive_extracts = fixture.runner.command_args_for(ToolBinary::SevenZip);
            assert_eq!(
                archive_extracts.len(),
                1,
                "{archive_name} should require one generic archive extraction command"
            );
            assert!(
                archive_extracts[0][1].ends_with(archive_name),
                "first 7z source should be the requested archive: {:?}",
                archive_extracts[0]
            );
            assert_eq!(fixture.runner.command_args_for(ToolBinary::Ffprobe).len(), 1);
        }
    }

    #[tokio::test]
    async fn archive_materializer_orchestrates_compressed_tar_second_pass_with_simulated_runner() {
        for archive_name in ["Album.tar.gz", "Album.tar.bz2", "Album.tar.xz", "Album.tar.zst"] {
            let fixture = materialize_archive_fixture(archive_name).await;
            assert_single_archive_track(&fixture.prepared, &fixture.staging_root);

            let archive_extracts = fixture.runner.command_args_for(ToolBinary::SevenZip);
            assert_eq!(
                archive_extracts.len(),
                2,
                "{archive_name} must expand the wrapper and then the tar payload"
            );
            assert!(archive_extracts[0][1].ends_with(archive_name));
            assert!(archive_extracts[1][1].ends_with("Album.tar"));
            assert!(
                !fixture.staging_root.join("Album.tar").exists(),
                "intermediate tar payload should be removed after expansion"
            );
            assert_eq!(fixture.runner.command_args_for(ToolBinary::Ffprobe).len(), 1);
        }
    }

    #[test]
    fn archive_materializer_provenance_records_detected_7z_version() {
        let runner = VersionOnlyRunner(HashMap::from([
            (ToolBinary::SevenZip, "25.01".to_string()),
        ]));
        let versions = archive_tool_versions(&runner);

        assert_eq!(versions.get("7z").map(String::as_str), Some("25.01"));
    }

    #[test]
    fn archive_materializer_provenance_omits_missing_external_version() {
        let runner = VersionOnlyRunner(HashMap::new());
        let versions = archive_tool_versions(&runner);

        assert!(versions.is_empty(), "missing external versions must not be mislabeled as in-process");
    }

    #[test]
    fn compressed_tar_candidate_names_preserve_archive_stem() {
        let staging = Path::new("/stage");

        assert_eq!(
            compressed_tar_intermediate_candidates(Path::new("Album.tar.gz"), staging),
            vec![PathBuf::from("/stage/Album.tar")]
        );
        assert_eq!(
            compressed_tar_intermediate_candidates(Path::new("Album.TGZ"), staging),
            vec![PathBuf::from("/stage/Album.tar")]
        );
        assert!(compressed_tar_intermediate_candidates(Path::new("Album.zip"), staging).is_empty());
    }

    #[test]
    fn compressed_tar_intermediate_prefers_expected_tar_payload() {
        let temp = tempfile::tempdir().expect("temp dir");
        let expected = temp.path().join("Album.tar");
        let unrelated = temp.path().join("Unrelated.tar");
        fs::write(&expected, b"tar").expect("expected tar");
        fs::write(&unrelated, b"tar").expect("unrelated tar");

        assert_eq!(
            intermediate_tar_files_for_compressed_tar(Path::new("Album.tar.gz"), temp.path())
                .expect("intermediate tar lookup"),
            vec![expected]
        );
    }

    #[test]
    fn compressed_tar_intermediate_uses_single_top_level_tar_fallback() {
        let temp = tempfile::tempdir().expect("temp dir");
        let fallback = temp.path().join("payload.tar");
        fs::write(&fallback, b"tar").expect("fallback tar");

        assert_eq!(
            intermediate_tar_files_for_compressed_tar(Path::new("Album.tar.zst"), temp.path())
                .expect("intermediate tar lookup"),
            vec![fallback]
        );
    }

    #[test]
    fn compressed_tar_intermediate_ignores_ambiguous_fallbacks() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::write(temp.path().join("one.tar"), b"tar").expect("one tar");
        fs::write(temp.path().join("two.tar"), b"tar").expect("two tar");

        assert!(
            intermediate_tar_files_for_compressed_tar(Path::new("Album.tar.xz"), temp.path())
                .expect("intermediate tar lookup")
                .is_empty()
        );
        assert!(
            intermediate_tar_files_for_compressed_tar(Path::new("Album.zip"), temp.path())
                .expect("non-compressed archive lookup")
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn compressed_tar_intermediate_rejects_tar_symlink_escape() {
        let temp = tempfile::tempdir().expect("temp dir");
        let staging = temp.path().join("stage");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&staging).expect("staging dir");
        fs::create_dir_all(&outside).expect("outside dir");
        let outside_tar = outside.join("Album.tar");
        fs::write(&outside_tar, b"outside tar").expect("outside tar");
        std::os::unix::fs::symlink(&outside_tar, staging.join("Album.tar"))
            .expect("tar symlink");

        let err = intermediate_tar_files_for_compressed_tar(
            Path::new("Album.tar.gz"),
            &staging,
        )
        .expect_err("tar symlink escape rejected");
        assert!(
            matches!(&err, MaterializeError::Extraction(message) if message.contains("outside staging root")),
            "unexpected error for tar symlink escape: {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn discover_audio_files_skips_directory_symlinks_without_duplication() {
        let temp = tempfile::tempdir().expect("temp dir");
        let staging = temp.path().join("stage");
        let real_dir = staging.join("real");
        fs::create_dir_all(&real_dir).expect("real dir");
        fs::write(real_dir.join("01.flac"), b"audio").expect("audio file");
        std::os::unix::fs::symlink(&real_dir, staging.join("linked-real"))
            .expect("directory symlink");

        let files = discover_audio_files(&staging).expect("discover audio files");
        assert_eq!(
            files.len(),
            1,
            "directory symlinks must not be followed and duplicate extracted tracks"
        );
        assert!(files[0].ends_with("01.flac"));
    }

    #[cfg(unix)]
    #[test]
    fn discover_audio_files_rejects_symlink_escape_from_staging_root() {
        let temp = tempfile::tempdir().expect("temp dir");
        let staging = temp.path().join("stage");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&staging).expect("staging dir");
        fs::create_dir_all(&outside).expect("outside dir");
        let outside_audio = outside.join("evil.flac");
        fs::write(&outside_audio, b"outside audio").expect("outside audio");
        std::os::unix::fs::symlink(&outside_audio, staging.join("evil.flac"))
            .expect("file symlink");

        let err = discover_audio_files(&staging).expect_err("symlink escape rejected");
        assert!(
            matches!(&err, MaterializeError::Extraction(message) if message.contains("outside staging root")),
            "unexpected error for symlink escape: {err:?}"
        );
    }

    #[test]
    fn parse_ffprobe_json_extracts_bit_depth_from_raw_sample_field() {
        let json = r#"{
            "streams": [{
                "codec_name": "flac",
                "sample_rate": "96000",
                "duration": "1.5",
                "bits_per_raw_sample": "24"
            }]
        }"#;
        let probe = parse_ffprobe_json(json).unwrap();
        assert_eq!(probe.sample_rate, 96_000);
        assert_eq!(probe.expected_samples, Some(144_000));
        assert_eq!(probe.bit_depth, Some(24));
    }

    #[test]
    fn parse_ffprobe_json_falls_back_to_bits_per_sample() {
        let json = r#"{
            "streams": [{
                "codec_name": "pcm_s16le",
                "sample_rate": "44100",
                "duration": "2.0",
                "bits_per_sample": 16
            }]
        }"#;
        let probe = parse_ffprobe_json(json).unwrap();
        assert_eq!(probe.sample_rate, 44_100);
        assert_eq!(probe.expected_samples, Some(88_200));
        assert_eq!(probe.bit_depth, Some(16));
    }

    #[test]
    fn derive_album_metadata_promotes_common_extra_tags_for_folder_templates() {
        let make_track = |ordinal: u32, catalog: &str, barcode: &str| PreparedTrack {
            id: TrackId {
                source_ordinal: ordinal,
                disc_number: Some(1),
                track_number: ordinal,
            },
            source_ref: TrackSourceRef::StagedFile(PathBuf::from(format!(
                "/stage/{ordinal:02}.flac"
            ))),
            metadata: TrackMetadata {
                title: Some(format!("Track {ordinal}")),
                artist: Some("Miles Davis".to_string()),
                album_artist: Some("Miles Davis".to_string()),
                genre: Some("Jazz".to_string()),
                date: Some("1971".to_string()),
                track_number: Some(ordinal),
                disc_number: Some(1),
                extra: BTreeMap::from([
                    ("album".to_string(), "A Tribute to Jack Johnson".to_string()),
                    ("catalognumber".to_string(), catalog.to_string()),
                    ("barcode".to_string(), barcode.to_string()),
                ]),
                ..TrackMetadata::default()
            },
            expected_samples: None,
            sample_rate: Some(44_100),
            source_audio: SourceAudioDescriptor::from_scalar(
                Some(44_100),
                Some(24),
                Some(SourceAudioCoding::Pcm),
            ),
            bit_depth: Some(24),
            warnings: Vec::new(),
        };

        let tracks = vec![
            make_track(1, "CK-1234", "074646123426"),
            make_track(2, "CK-1234", "074646123426"),
        ];
        let album = derive_album_metadata(&tracks);

        assert_eq!(
            album.extra.get("catalognumber").map(String::as_str),
            Some("CK-1234")
        );
        assert_eq!(
            album.extra.get("barcode").map(String::as_str),
            Some("074646123426")
        );
        assert_eq!(album.album.as_deref(), Some("A Tribute to Jack Johnson"));
    }

    #[test]
    fn derive_album_metadata_does_not_promote_track_specific_extra_tags() {
        let make_track = |ordinal: u32, isrc: &str| PreparedTrack {
            id: TrackId {
                source_ordinal: ordinal,
                disc_number: Some(1),
                track_number: ordinal,
            },
            source_ref: TrackSourceRef::StagedFile(PathBuf::from(format!(
                "/stage/{ordinal:02}.flac"
            ))),
            metadata: TrackMetadata {
                title: Some(format!("Track {ordinal}")),
                artist: Some("Miles Davis".to_string()),
                album_artist: Some("Miles Davis".to_string()),
                extra: BTreeMap::from([
                    ("album".to_string(), "A Tribute to Jack Johnson".to_string()),
                    ("isrc".to_string(), isrc.to_string()),
                ]),
                ..TrackMetadata::default()
            },
            expected_samples: None,
            sample_rate: Some(44_100),
            source_audio: SourceAudioDescriptor::from_scalar(
                Some(44_100),
                Some(24),
                Some(SourceAudioCoding::Pcm),
            ),
            bit_depth: Some(24),
            warnings: Vec::new(),
        };

        let tracks = vec![make_track(1, "USSM17100001"), make_track(2, "USSM17100002")];
        let album = derive_album_metadata(&tracks);

        assert!(!album.extra.contains_key("isrc"));
        assert_eq!(
            album.extra.get("album").map(String::as_str),
            Some("A Tribute to Jack Johnson")
        );
    }
}
