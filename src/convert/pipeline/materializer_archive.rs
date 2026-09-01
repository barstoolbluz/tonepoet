//! PR 3 — `ArchiveMaterializer` implementation.
//!
//! Materializes generic archives through a read-only lazy FUSE view when available,
//! falling back to the established 7z/7zz extraction backend on capability misses;
//! discovers audio files, probes them with ffprobe, reads metadata with lofty, and
//! returns a `PreparedSource` with `TrackSourceRef::StagedFile` entries.
//!
//! Does not convert, tag, merge, run ReplayGain, generate feature
//! files, publish, write durable logs, or emit terminal events.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, Instant, SystemTime};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::errors::{MaterializeError, ToolRunnerError};
use super::progress::{heartbeat, OperationProgressTracker};
use super::reporter::PipelineReporter;
use super::tool::{RealToolRunner, ToolBinary, ToolCommand, ToolRunner};
use super::types::*;
use super::materializer_cue::CueImageMaterializer;

// =========================================================================
// Audio file extensions accepted from extracted archives
// =========================================================================

const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "wav", "aiff", "aif", "wv", "mp3", "m4a", "aac", "opus", "ogg", "ape", "dsf", "dff",
    "w64", "rf64",
];

/// Reserved album-level Tonepoet record identifying an adjacent ISO-WV CUE as
/// a complete effective metadata snapshot rather than an ordinary unrelated
/// CUE file. The value is deliberately simple and human-readable.
pub(crate) const ISO_WV_METADATA_SNAPSHOT_KEY: &str =
    "TONEPOET_ISO_WV_METADATA_SNAPSHOT_V1";

fn is_audio_extension(ext: &str) -> bool {
    AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str())
}

/// Archive-relative CUE repair produced for an ISO-WV structural rename.
///
/// Resolution of a `FILE` reference is intentionally supplied by the caller:
/// the established staged path resolves against real extracted files, while
/// the native path resolves against the archive member table. Both paths then
/// share the exact same remapping and replacement semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IsoWvCueReferenceRenamePlan {
    pub(crate) cue_after_relative: PathBuf,
    pub(crate) replacements: BTreeMap<String, String>,
}

fn remap_archive_relative_path_for_rename(
    path: &Path,
    old_path: &Path,
    new_path: &Path,
) -> PathBuf {
    if let Ok(suffix) = path.strip_prefix(old_path) {
        new_path.join(suffix)
    } else {
        path.to_path_buf()
    }
}

fn normalized_cue_reference_for_archive_rename(value: &str) -> String {
    let mut normalized = value.replace('\\', "/");
    while let Some(rest) = normalized.strip_prefix("./") {
        normalized = rest.to_string();
    }
    normalized
}

fn archive_relative_cue_reference(cue_parent: &Path, target: &Path) -> Result<String, String> {
    use std::path::Component;

    let collect = |path: &Path| -> Result<Vec<String>, String> {
        path.components()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(
                    part.to_str()
                        .map(str::to_string)
                        .ok_or_else(|| "ISO-WV CUE path is not valid UTF-8".to_string()),
                ),
                Component::CurDir => None,
                _ => Some(Err("ISO-WV CUE path escaped archive staging".to_string())),
            })
            .collect()
    };
    let from = collect(cue_parent)?;
    let to = collect(target)?;
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts = Vec::new();
    parts.extend(std::iter::repeat("..".to_string()).take(from.len() - common));
    parts.extend(to.into_iter().skip(common));
    if parts.is_empty() {
        return Err("ISO-WV CUE FILE target collapsed to the CUE directory".to_string());
    }
    Ok(parts.join("/"))
}

pub(crate) fn plan_iso_wv_cue_reference_rename<F>(
    cue_text: &str,
    cue_before_relative: &Path,
    old_relative: &Path,
    new_relative: &Path,
    mut resolve_target_relative: F,
) -> Result<IsoWvCueReferenceRenamePlan, String>
where
    F: FnMut(&str) -> Result<PathBuf, String>,
{
    let sheet = crate::convert::cue_parser::parse_cue(cue_text);
    if sheet.tracks.is_empty() {
        return Err(
            "ISO-WV rename refused because the authoritative CUE has no audio tracks".to_string(),
        );
    }

    let cue_after_relative = remap_archive_relative_path_for_rename(
        cue_before_relative,
        old_relative,
        new_relative,
    );
    let cue_after_parent = cue_after_relative
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let mut replacements = BTreeMap::<String, String>::new();
    for track in &sheet.tracks {
        let Some(file_ref) = track
            .file
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        else {
            return Err(format!(
                "ISO-WV CUE track {} has no FILE reference",
                track.number
            ));
        };
        if replacements.contains_key(file_ref) {
            continue;
        }
        let target_relative = resolve_target_relative(file_ref)?;
        let target_after_relative = remap_archive_relative_path_for_rename(
            &target_relative,
            old_relative,
            new_relative,
        );
        let mut new_ref =
            archive_relative_cue_reference(cue_after_parent, &target_after_relative)?;
        if file_ref.contains('\\') && !file_ref.contains('/') {
            new_ref = new_ref.replace('/', "\\");
        }
        if normalized_cue_reference_for_archive_rename(file_ref)
            != normalized_cue_reference_for_archive_rename(&new_ref)
        {
            if file_ref.contains('"') || new_ref.contains('"') {
                return Err(
                    "ISO-WV rename would require a CUE FILE name containing a quote".to_string(),
                );
            }
            replacements.insert(file_ref.to_string(), new_ref);
        }
    }

    Ok(IsoWvCueReferenceRenamePlan {
        cue_after_relative,
        replacements,
    })
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
        // ISO-WV keeps original payload and CUE work in sibling trees: generated
        // segment carriers must never become eligible companion artifacts.
        let is_iso_wv = crate::convert::classify::is_iso_wv_container(&req.container);
        let materializer_extraction_root = if is_iso_wv {
            staging.root.join("iso-wv-payload")
        } else {
            staging.root.clone()
        };
        let reused_extraction = reusable_pre_extracted_staging(req, &materializer_extraction_root)
            .transpose()?;
        let mut extraction_root = reused_extraction
            .clone()
            .unwrap_or_else(|| materializer_extraction_root.clone());
        let mut iso_wv_access = IsoWvPayloadAccess::Extracted;

        if reused_extraction.is_none() {
            if is_iso_wv {
                std::fs::create_dir_all(&extraction_root)?;
                if let Some(lease) = try_mount_iso_wv_readonly(
                    &req.container,
                    &extraction_root,
                    runner,
                    cancel,
                )
                .await?
                {
                    staging.retain_fuse_mount(lease);
                    iso_wv_access = IsoWvPayloadAccess::Mounted;
                } else {
                    extract_archive_to_staging(
                        &req.container,
                        &extraction_root,
                        req.item_id.as_str(),
                        req.source.archive_password.as_ref().map(|pw| pw.expose()),
                        runner,
                        reporter,
                        cancel,
                    )
                    .await?;
                }
            } else {
                // Generic archives get the same read-in-place shape as ISO-WV:
                // prefer a read-only lazy FUSE view and fall back to the
                // established password-aware extraction path on any capability
                // miss. fuse-archive cannot consume Tonepoet's stored password
                // noninteractively for every format (notably encrypted 7z/RAR),
                // so encrypted requests deliberately keep the proven fallback.
                let archive_mount_root = staging.root.join("archive-payload");
                let mounted = if req.source.archive_password.is_none() {
                    try_mount_archive_readonly(
                        &req.container,
                        &archive_mount_root,
                        runner,
                        cancel,
                    )
                    .await?
                } else {
                    None
                };
                if let Some(lease) = mounted {
                    extraction_root = archive_mount_root;
                    staging.retain_fuse_mount(lease);
                } else {
                    // Keep the historical extraction layout on fallback and
                    // for reusable queue-time previews. The dedicated mount
                    // child exists only to keep later pipeline scratch writes
                    // off the read-only FUSE filesystem.
                    let _ = fs::remove_dir(&archive_mount_root);
                    extract_archive(req, staging, runner, reporter, tool_paths, cancel).await?;
                    extraction_root = staging.root.clone();
                }
            }
        }

        // Check cancellation between major steps.
        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }

        // `.iso.wv` is an ISO filesystem image, not WavPack. Its payload is
        // interpreted by the existing CUE materializer after extraction so CUE
        // track geometry, WavPack fallback decoding, metadata, and artwork
        // semantics remain singular. Generic archives deliberately retain their
        // historical independent-audio-member behavior.
        if is_iso_wv {
            return materialize_iso_wv_cue_payload(
                req,
                &extraction_root,
                staging,
                runner,
                reporter,
                tool_paths,
                cancel,
                iso_wv_access,
            )
            .await;
        }

        // 2. Discover audio files in the mounted or extracted archive tree.
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
                match crate::convert::pipeline::plan_bridge::authoritative_dsd_sample_timing_from_path(
                    path,
                ) {
                    Ok(Some((sample_rate_hz, sample_count))) => {
                        probe.sample_rate = sample_rate_hz;
                        probe.expected_samples = Some(sample_count);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        return Err(MaterializeError::Extraction(format!(
                            "cannot establish exact DSD sample timing for {}: {error}",
                            path.display()
                        )));
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IsoWvPayloadAccess {
    Mounted,
    Extracted,
}

fn fuse_archive_mount_options_for_version(version: &str) -> Option<&'static str> {
    let numeric = version
        .trim()
        .trim_start_matches(|ch: char| !ch.is_ascii_digit());
    let mut components = numeric.split(|ch: char| !ch.is_ascii_digit());
    let major = components.next()?.parse::<u32>().ok()?;
    let minor = components.next()?.parse::<u32>().ok()?;

    // Incremental caching arrived in 1.14. Tree trimming was introduced only
    // in 1.20, together with `notrim`. Older supported releases already expose
    // archive paths verbatim and reject an unknown `notrim` option, so select
    // the option set by capability version rather than forcing a lockfile bump.
    if major == 0 || (major == 1 && minor < 14) {
        return None;
    }
    if major > 1 || (major == 1 && minor >= 20) {
        Some("lazycache,notrim,auto_unmount")
    } else {
        Some("lazycache,auto_unmount")
    }
}

/// Mount a generic archive as a read-only lazy FUSE view. A missing binary,
/// unavailable FUSE device, unsupported archive, or mount failure is a soft
/// capability miss; callers fall back to the established extraction path.
/// Cancellation is terminal. Lazy caching is mandatory here: fuse-archive's
/// eager cache mode can otherwise perform the same whole-archive work this
/// path exists to avoid.
#[allow(unsafe_code)] // libc::getpid + pre_exec(prctl PR_SET_PDEATHSIG) for the foreground FUSE child
pub(crate) async fn try_mount_archive_readonly(
    archive_path: &Path,
    mount_point: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<Option<std::sync::Arc<FuseMountLease>>, MaterializeError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (archive_path, mount_point, runner, cancel);
        return Ok(None);
    }

    #[cfg(target_os = "linux")]
    {
        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }
        let Some(fuse_archive) = runner.resolved_tool_path(ToolBinary::FuseArchive) else {
            return Ok(None);
        };
        let Some(fuse_archive_version) = runner.tool_version(ToolBinary::FuseArchive) else {
            log::debug!("archive mount version is unknown; falling back to extraction");
            return Ok(None);
        };
        let Some(mount_options) = fuse_archive_mount_options_for_version(&fuse_archive_version) else {
            log::debug!(
                "fuse-archive {fuse_archive_version} is too old for lazy caching; falling back to extraction"
            );
            return Ok(None);
        };
        if !Path::new("/dev/fuse").exists() {
            return Ok(None);
        }
        fs::create_dir_all(mount_point).map_err(MaterializeError::Io)?;
        if fs::read_dir(mount_point)
            .map_err(MaterializeError::Io)?
            .next()
            .is_some()
        {
            return Ok(None);
        }

        let parent_pid = unsafe { libc::getpid() };
        let mut command = std::process::Command::new(fuse_archive);
        command
            .arg("-f")
            .arg("-o")
            .arg(mount_options)
            .arg(archive_path)
            .arg(mount_point)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent_pid {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "tonepoet parent exited before archive FUSE supervision was armed",
                    ));
                }
                Ok(())
            });
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                log::debug!("archive mount unavailable; falling back to extraction: {err}");
                return Ok(None);
            }
        };

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if cancel.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(MaterializeError::Cancelled);
            }
            if super::types::linux_mountinfo_contains(mount_point) {
                return Ok(Some(std::sync::Arc::new(FuseMountLease::new(
                    mount_point.to_path_buf(),
                    child,
                ))));
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    log::debug!(
                        "archive mount exited before becoming ready ({status}); falling back to extraction"
                    );
                    return Ok(None);
                }
                Ok(None) => {}
                Err(err) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    log::debug!(
                        "archive mount readiness check failed; falling back to extraction: {err}"
                    );
                    return Ok(None);
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                log::debug!("archive mount timed out; falling back to extraction");
                return Ok(None);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

/// Mount an ISO-WV payload without copying it.  Failure to acquire FUSE is a
/// soft capability miss: callers deliberately fall back to the established 7z
/// extraction path.  Cancellation remains terminal.
#[allow(unsafe_code)] // libc::getpid + pre_exec(prctl PR_SET_PDEATHSIG) for the foreground FUSE child
pub(crate) async fn try_mount_iso_wv_readonly(
    iso_path: &Path,
    mount_point: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<Option<std::sync::Arc<FuseMountLease>>, MaterializeError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (iso_path, mount_point, runner, cancel);
        return Ok(None);
    }

    #[cfg(target_os = "linux")]
    {
        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }
        let Some(fuseiso) = runner.resolved_tool_path(ToolBinary::FuseIso) else {
            return Ok(None);
        };
        if !Path::new("/dev/fuse").exists() {
            return Ok(None);
        }
        fs::create_dir_all(mount_point).map_err(MaterializeError::Io)?;
        if fs::read_dir(mount_point)
            .map_err(MaterializeError::Io)?
            .next()
            .is_some()
        {
            return Ok(None);
        }

        let parent_pid = unsafe { libc::getpid() };
        let mut command = std::process::Command::new(fuseiso);
        command
            .arg("-n")
            .arg("-f")
            .arg("-o")
            .arg("auto_unmount")
            .arg(iso_path)
            .arg(mount_point)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        // A foreground FUSE child would otherwise survive a SIGKILL of the
        // parent.  PDEATHSIG closes that gap; auto_unmount then releases the
        // mount when the child terminates.  Re-check PPID after prctl to cover
        // the parent-death race between fork and the pre-exec hook.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent_pid {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "tonepoet parent exited before fuseiso supervision was armed",
                    ));
                }
                Ok(())
            });
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                log::debug!("ISO-WV mount unavailable; falling back to extraction: {err}");
                return Ok(None);
            }
        };

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if cancel.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(MaterializeError::Cancelled);
            }
            if super::types::linux_mountinfo_contains(mount_point) {
                return Ok(Some(std::sync::Arc::new(FuseMountLease::new(
                    mount_point.to_path_buf(),
                    child,
                ))));
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    log::debug!(
                        "ISO-WV mount exited before becoming ready ({status}); falling back to extraction"
                    );
                    return Ok(None);
                }
                Ok(None) => {}
                Err(err) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    log::debug!(
                        "ISO-WV mount readiness check failed; falling back to extraction: {err}"
                    );
                    return Ok(None);
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                log::debug!("ISO-WV mount timed out; falling back to extraction");
                return Ok(None);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

async fn materialize_iso_wv_cue_payload(
    req: &PipelineRequest,
    extraction_root: &Path,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    reporter: Option<&dyn PipelineReporter>,
    tool_paths: &HashMap<String, PathBuf>,
    cancel: &CancellationToken,
    access: IsoWvPayloadAccess,
) -> Result<PreparedSource, MaterializeError> {
    let cue_path = find_single_visible_cue(extraction_root)?;
    let mut cue_req = req.clone();
    cue_req.container = cue_path.clone();
    cue_req.pre_extracted_staging = None;
    // A visible CUE is the authority inside this container. Do not let an
    // external per-image sidecar policy accidentally disable the container's
    // own sheet.
    cue_req.source.cue_sidecar = CueSidecarPolicy::PreferSidecar;

    let mut prepared = super::stages::Materializer::materialize(
        &CueImageMaterializer,
        &cue_req,
        staging,
        runner,
        reporter,
        tool_paths,
        cancel,
    )
    .await?;

    // A Tonepoet-owned adjacent CUE is an external metadata persistence layer.
    // Apply it before queue-time preview overrides so unsaved Convert edits
    // remain the last writer, exactly as they were before sidecar persistence
    // existed. The internal CUE still owns geometry and audio realization.
    apply_iso_wv_metadata_sidecar(&req.container, &cue_path, &mut prepared)?;

    // Archive-preview metadata edits are keyed by displayed track ordinal for
    // ISO-WV because all CUE tracks can share one physical image. Preserve that
    // established editor behavior without teaching the CUE materializer about
    // archive UI state.
    for track in &mut prepared.tracks {
        if let Some(override_set) = req
            .archive_metadata_overrides
            .iter()
            .find(|override_set| override_set.source_ordinal == track.id.source_ordinal)
        {
            apply_archive_metadata_override(&mut track.metadata, override_set);
        }
    }
    if !req.archive_metadata_overrides.is_empty() {
        let edited_album = derive_album_metadata(&prepared.tracks);
        prepared.album_metadata.album = edited_album.album;
        prepared.album_metadata.album_artist = edited_album.album_artist;
        prepared.album_metadata.genre = edited_album.genre;
        prepared.album_metadata.date = edited_album.date;
    }

    // Keep the outer source kind/container identity as Archive so companion
    // payload under Artwork/, Readme/, Graphs/, etc. is snapshotted from the
    // extracted ISO tree before staging cleanup. Track refs remain the CUE
    // materializer's exact segment refs, so audio realization is unchanged.
    prepared.container = req.container.clone();
    prepared.kind = SourceKind::Archive;
    prepared.provenance.source_kind = SourceKind::Archive;
    match access {
        IsoWvPayloadAccess::Mounted => {
            if let Some(version) = runner.tool_version(ToolBinary::FuseIso) {
                prepared
                    .provenance
                    .tool_versions
                    .insert("fuseiso".to_string(), version);
            }
        }
        IsoWvPayloadAccess::Extracted => {
            if let Some(version) = runner.tool_version(ToolBinary::SevenZip) {
                prepared
                    .provenance
                    .tool_versions
                    .insert("7z".to_string(), version);
            }
        }
    }
    Ok(prepared)
}

/// Adjacent metadata companion used only for ISO-WV persistence. Appending the
/// suffix keeps the compound source name intact (`album.iso.wv.cue`) and
/// cannot collide with ordinary `album.cue` sidecars for a neighboring audio
/// file.
pub(crate) fn iso_wv_metadata_sidecar_path(archive_path: &Path) -> PathBuf {
    let mut name = archive_path.as_os_str().to_os_string();
    name.push(".cue");
    PathBuf::from(name)
}

/// Structural identity required before an external metadata snapshot can be
/// applied to an ISO-WV. Metadata fields may differ by design; FILE/TRACK/index
/// geometry may not. This prevents a stale sidecar from shifting metadata onto
/// a different release that happens to reuse the same filename.
pub(crate) fn iso_wv_cue_geometry_matches(
    expected: &crate::tui::cue_parser::CueSheet,
    candidate: &crate::tui::cue_parser::CueSheet,
) -> bool {
    expected.tracks.len() == candidate.tracks.len()
        && expected
            .tracks
            .iter()
            .zip(candidate.tracks.iter())
            .all(|(expected, candidate)| {
                expected.number == candidate.number
                    && expected.file == candidate.file
                    && expected.index00_frames == candidate.index00_frames
                    && expected.index01_frames == candidate.index01_frames
            })
}

fn apply_iso_wv_metadata_sidecar(
    archive_path: &Path,
    internal_cue_path: &Path,
    prepared: &mut PreparedSource,
) -> Result<(), MaterializeError> {
    let sidecar_path = iso_wv_metadata_sidecar_path(archive_path);
    if !sidecar_path.exists() {
        return Ok(());
    }

    let internal = crate::tui::cue_parser::parse_cue_file(internal_cue_path)
        .map_err(|err| MaterializeError::Parse(format!(
            "failed to re-read internal ISO-WV CUE for sidecar admission: {err}"
        )))?;
    let sidecar = crate::tui::cue_parser::parse_cue_file(&sidecar_path)
        .map_err(|err| MaterializeError::Parse(format!(
            "failed to parse ISO-WV metadata sidecar '{}': {err}",
            sidecar_path.display()
        )))?;

    // The appended filename is intentionally private to this feature, but a
    // pre-existing user file with that name must not silently become metadata
    // authority. Only sidecars carrying our explicit snapshot marker opt in.
    let is_tonepoet_snapshot = crate::convert::cue_parser::cue_user_metadata_values(
        &sidecar.user_metadata,
        ISO_WV_METADATA_SNAPSHOT_KEY,
    )
    .is_some_and(|values| values.iter().any(|value| value.trim() == "1"));
    if !is_tonepoet_snapshot {
        log::debug!(
            "ignoring adjacent ISO-WV CUE without Tonepoet snapshot marker: {}",
            sidecar_path.display()
        );
        return Ok(());
    }

    if !iso_wv_cue_geometry_matches(&internal, &sidecar) {
        return Err(MaterializeError::Parse(format!(
            "ISO-WV metadata sidecar '{}' no longer matches the image CUE track geometry; refusing stale metadata authority",
            sidecar_path.display()
        )));
    }
    if sidecar.tracks.len() != prepared.tracks.len() {
        return Err(MaterializeError::Parse(format!(
            "ISO-WV metadata sidecar '{}' has {} tracks but materialization produced {}; refusing positional metadata overlay",
            sidecar_path.display(),
            sidecar.tracks.len(),
            prepared.tracks.len()
        )));
    }

    for (index, prepared_track) in prepared.tracks.iter_mut().enumerate() {
        let old_extra = prepared_track.metadata.extra.clone();
        let mut mapped = super::materializer_cue::cue_sheet_track_metadata_for_conversion(
            &sidecar,
            index,
            prepared_track.metadata.pre_emphasis,
        )
        .ok_or_else(|| MaterializeError::Parse(format!(
            "ISO-WV metadata sidecar '{}' lost track position {} during mapping",
            sidecar_path.display(),
            index + 1
        )))?;

        // Raw REM annotations are not part of CueSheet's typed metadata model.
        // They came from the same internal CUE whose geometry was just proven
        // equal, so preserve only that annotation namespace; do not merge the
        // old image-tag extras, because absence in the snapshot represents an
        // intentional metadata deletion.
        for (key, value) in old_extra {
            if key.starts_with("rem_") && !mapped.extra.contains_key(&key) {
                mapped.extra.insert(key, value);
            }
        }
        prepared_track.metadata = mapped;
    }

    let old_album_extra = prepared.album_metadata.extra.clone();
    let mut mapped_album =
        super::materializer_cue::cue_sheet_album_metadata_for_conversion(&sidecar);
    remove_cue_user_metadata(
        &mut mapped_album.extra,
        ISO_WV_METADATA_SNAPSHOT_KEY,
    );
    for (key, value) in old_album_extra {
        if key.starts_with("rem_") && !mapped_album.extra.contains_key(&key) {
            mapped_album.extra.insert(key, value);
        }
    }
    for track in &mut prepared.tracks {
        track.metadata.disc_number = mapped_album.disc_number;
    }
    prepared.album_metadata = mapped_album;
    Ok(())
}

/// Locate the single user-visible CUE authority in a self-contained ISO-WV
/// payload. Refuse ambiguity rather than selecting by directory order.
pub(crate) fn find_single_visible_cue(root: &Path) -> Result<PathBuf, MaterializeError> {
    let mut cues = Vec::new();
    collect_visible_cues(root, &mut cues)?;
    cues.sort();
    cues.dedup();
    match cues.as_slice() {
        [cue] => Ok(cue.clone()),
        [] => Err(MaterializeError::Extraction(
            "ISO-WV payload contains no user-visible CUE sheet".to_string(),
        )),
        _ => Err(MaterializeError::Extraction(format!(
            "ISO-WV payload contains {} user-visible CUE sheets; album authority is ambiguous",
            cues.len()
        ))),
    }
}

fn collect_visible_cues(dir: &Path, cues: &mut Vec<PathBuf>) -> Result<(), MaterializeError> {
    for entry in fs::read_dir(dir).map_err(MaterializeError::Io)? {
        let entry = entry.map_err(MaterializeError::Io)?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(MaterializeError::Io)?;
        if file_type.is_dir() {
            collect_visible_cues(&path, cues)?;
        } else if file_type.is_file() && crate::convert::classify::is_cue_sheet_path(&path) {
            cues.push(path);
        }
    }
    Ok(())
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
    if let Some(version) = runner.tool_version(ToolBinary::FuseArchive) {
        tool_versions.insert("fuse-archive".to_string(), version);
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
    override_set.artist.apply_to_value_list(&mut metadata.artist);
    override_set.genre.apply_to_value_list(&mut metadata.genre);
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
    IsoWv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ZipEncryptionMethod {
    ZipCrypto,
    Aes128,
    Aes192,
    Aes256,
}

impl ZipEncryptionMethod {
    fn seven_zip_method_switch(self) -> &'static str {
        match self {
            Self::ZipCrypto => "-mem=ZipCrypto",
            Self::Aes128 => "-mem=AES128",
            Self::Aes192 => "-mem=AES192",
            Self::Aes256 => "-mem=AES256",
        }
    }
}

#[derive(Debug, Clone)]
struct ArchiveRepackageEncryptionPolicy {
    password: SecretString,
    header_encryption: bool,
    zip_method: Option<ZipEncryptionMethod>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ArchiveEncryptionProbeFacts {
    any_encrypted: bool,
    any_unencrypted: bool,
    zip_methods: BTreeSet<ZipEncryptionMethod>,
    unknown_encrypted_zip_method: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArchiveEncryptionProbeResult {
    success: bool,
    facts: ArchiveEncryptionProbeFacts,
}

#[derive(Debug, Default)]
struct ArchiveEncryptionListingParser {
    facts: ArchiveEncryptionProbeFacts,
    in_entries: bool,
    have_entry: bool,
    current_encrypted: Option<bool>,
    current_is_dir: Option<bool>,
    current_attributes_is_dir: Option<bool>,
    current_method: Option<String>,
}

impl ArchiveEncryptionListingParser {
    fn finish_current(&mut self, format: RepackageArchiveFormat) {
        if !self.have_entry {
            return;
        }
        // ZIP listings expose `Folder = +/-`, while current 7-Zip 7z
        // listings may omit that field and identify directories only via
        // `Attributes = D ...`. Prefer the explicit Folder field when it is
        // present and otherwise fall back to the attributes classification.
        let current_is_dir = self
            .current_is_dir
            .unwrap_or(self.current_attributes_is_dir == Some(true));
        if !current_is_dir {
            match self.current_encrypted {
                Some(true) => {
                    self.facts.any_encrypted = true;
                    if format == RepackageArchiveFormat::Zip {
                        match self
                            .current_method
                            .as_deref()
                            .and_then(zip_encryption_method_from_listing)
                        {
                            Some(method) => {
                                self.facts.zip_methods.insert(method);
                            }
                            None => self.facts.unknown_encrypted_zip_method = true,
                        }
                    }
                }
                Some(false) => self.facts.any_unencrypted = true,
                None => {}
            }
        }
        self.have_entry = false;
        self.current_encrypted = None;
        self.current_is_dir = None;
        self.current_attributes_is_dir = None;
        self.current_method = None;
    }

    fn push_line(&mut self, format: RepackageArchiveFormat, line: &str) {
        if line.trim() == "----------" {
            self.finish_current(format);
            self.in_entries = true;
            return;
        }
        if !self.in_entries {
            // `7z l -slt` emits archive-level Path/Method fields before
            // the dashed item-list delimiter. They describe the container,
            // not a member, and must not participate in encryption policy.
            return;
        }
        let Some((key, value)) = line.split_once(" = ") else {
            return;
        };
        let key = key.trim();
        if key == "Path" {
            // Item records are introduced by successive `Path = ...` fields;
            // blank lines are presentation only.
            self.finish_current(format);
            self.have_entry = true;
            return;
        }
        if !self.have_entry {
            return;
        }
        match key {
            "Encrypted" => self.current_encrypted = Some(value.trim() == "+"),
            "Folder" => self.current_is_dir = Some(value.trim() == "+"),
            "Attributes" => {
                self.current_attributes_is_dir = Some(
                    value
                        .split_ascii_whitespace()
                        .next()
                        .is_some_and(|flags| flags.starts_with('D')),
                )
            }
            "Method" => self.current_method = Some(value.trim().to_string()),
            _ => {}
        }
    }

    fn finish(mut self, format: RepackageArchiveFormat) -> ArchiveEncryptionProbeFacts {
        self.finish_current(format);
        self.facts
    }
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
    require_repackage_format_tool_available(format, tool_paths)
}

fn require_repackage_format_tool_available(
    format: RepackageArchiveFormat,
    tool_paths: &HashMap<String, PathBuf>,
) -> Result<(), String> {
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
        RepackageArchiveFormat::IsoWv => require_repackage_tool_available(
            tool_paths,
            &["xorriso"],
            "ISO-WV repackaging requires the `xorriso` executable",
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveNativeRenameProgressSnapshot {
    pub status: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

impl ArchiveNativeRenameProgressSnapshot {
    fn new(status: impl Into<String>, bytes_done: u64, bytes_total: u64) -> Self {
        Self {
            status: status.into(),
            bytes_done,
            bytes_total,
        }
    }
}

/// Return whether the format has a format-native rename primitive that
/// Tonepoet can use without extracting the archive. RAR deliberately keeps
/// the established exact repackage fallback when a configured writer exists;
/// Tonepoet never changes its container format implicitly.
pub fn archive_native_rename_available(
    original_archive: &Path,
    tool_paths: &HashMap<String, PathBuf>,
) -> Result<bool, String> {
    let format = repackage_archive_format(original_archive)?;
    match format {
        RepackageArchiveFormat::SevenZip | RepackageArchiveFormat::Zip => {
            require_repackage_tool_available(
                tool_paths,
                &["7zz", "7z"],
                "archive rename requires `7zz` or `7z`",
            )?;
            Ok(true)
        }
        RepackageArchiveFormat::IsoWv => {
            require_repackage_tool_available(
                tool_paths,
                &["xorriso"],
                "ISO-WV rename requires the `xorriso` executable",
            )?;
            Ok(true)
        }
        RepackageArchiveFormat::Tar
        | RepackageArchiveFormat::TarGz
        | RepackageArchiveFormat::Rar => Ok(false),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArchiveNativeRenamePair {
    pub(crate) old_inner_path: String,
    pub(crate) new_inner_path: String,
}

impl ArchiveNativeRenamePair {
    pub(crate) fn new(old_inner_path: impl Into<String>, new_inner_path: impl Into<String>) -> Self {
        Self {
            old_inner_path: old_inner_path.into(),
            new_inner_path: new_inner_path.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArchiveNativeMember {
    pub(crate) path: String,
    pub(crate) is_dir: bool,
}

fn normalize_archive_relative_path(path: &Path) -> Result<PathBuf, String> {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err("archive-relative path escapes the archive root".to_string());
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err("absolute CUE FILE references are not valid inside ISO-WV".to_string());
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err("archive-relative path resolved to the archive root".to_string());
    }
    Ok(normalized)
}

fn archive_member_is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(is_audio_extension)
}

fn resolve_iso_wv_cue_reference_from_members(
    cue_before_relative: &Path,
    file_ref: &str,
    member_files: &[PathBuf],
) -> Result<PathBuf, String> {
    let normalized_ref = file_ref.replace('\\', "/");
    let raw = PathBuf::from(&normalized_ref);
    if raw.is_absolute() {
        return Err(format!(
            "ISO-WV CUE FILE {:?} resolves outside the archive",
            file_ref
        ));
    }
    let cue_parent = cue_before_relative.parent().unwrap_or_else(|| Path::new(""));
    let direct = normalize_archive_relative_path(&cue_parent.join(&raw)).map_err(|_| {
        format!(
            "ISO-WV CUE FILE {:?} resolves outside the archive",
            file_ref
        )
    })?;
    if member_files.iter().any(|candidate| candidate == &direct) {
        return if archive_member_is_audio(&direct) {
            Ok(direct)
        } else {
            Err(format!(
                "ISO-WV CUE FILE {:?} is not a supported audio source ({})",
                file_ref,
                direct.display()
            ))
        };
    }

    let search_dir = direct.parent().unwrap_or_else(|| Path::new(""));
    let wanted_name = raw.file_name().and_then(|value| value.to_str());
    if let Some(wanted_name) = wanted_name {
        let mut matches = member_files
            .iter()
            .filter(|candidate| archive_member_is_audio(candidate))
            .filter(|candidate| candidate.parent().unwrap_or_else(|| Path::new("")) == search_dir)
            .filter(|candidate| {
                candidate
                    .file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value.eq_ignore_ascii_case(wanted_name))
            })
            .cloned()
            .collect::<Vec<_>>();
        matches.sort();
        matches.dedup();
        match matches.as_slice() {
            [only] => return Ok(only.clone()),
            [] => {}
            _ => {
                return Err(format!(
                    "ISO-WV CUE FILE {:?} is ambiguous before rename ({} candidates)",
                    file_ref,
                    matches.len()
                ));
            }
        }
    }

    if let Some(wanted_stem) = raw.file_stem().and_then(|value| value.to_str()) {
        let mut matches = member_files
            .iter()
            .filter(|candidate| archive_member_is_audio(candidate))
            .filter(|candidate| candidate.parent().unwrap_or_else(|| Path::new("")) == search_dir)
            .filter(|candidate| {
                candidate
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value.eq_ignore_ascii_case(wanted_stem))
            })
            .cloned()
            .collect::<Vec<_>>();
        matches.sort();
        matches.dedup();
        match matches.as_slice() {
            [only] => return Ok(only.clone()),
            [] => {}
            _ => {
                return Err(format!(
                    "ISO-WV CUE FILE {:?} is ambiguous before rename ({} candidates)",
                    file_ref,
                    matches.len()
                ));
            }
        }
    }

    Err(format!(
        "ISO-WV CUE FILE {:?} is already missing before rename",
        file_ref
    ))
}

fn native_member_files(members: &[ArchiveNativeMember]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for member in members.iter().filter(|member| !member.is_dir) {
        let normalized = normalize_archive_relative_path(Path::new(&member.path)).map_err(|err| {
            format!("invalid archive member path {:?}: {err}", member.path)
        })?;
        files.push(normalized);
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn single_iso_wv_cue_member(member_files: &[PathBuf]) -> Result<PathBuf, String> {
    let mut cues = member_files
        .iter()
        .filter(|path| crate::convert::classify::is_cue_sheet_path(path))
        .cloned()
        .collect::<Vec<_>>();
    cues.sort();
    cues.dedup();
    match cues.as_slice() {
        [cue] => Ok(cue.clone()),
        [] => Err("ISO-WV payload contains no user-visible CUE sheet".to_string()),
        _ => Err(format!(
            "ISO-WV payload contains {} user-visible CUE sheets; album authority is ambiguous",
            cues.len()
        )),
    }
}

fn remap_member_path_through_native_pairs(
    path: &Path,
    rename_pairs: &[ArchiveNativeRenamePair],
) -> PathBuf {
    for pair in rename_pairs {
        let old = Path::new(&pair.old_inner_path);
        if path == old {
            return PathBuf::from(&pair.new_inner_path);
        }
        if let Ok(suffix) = path.strip_prefix(old) {
            return PathBuf::from(&pair.new_inner_path).join(suffix);
        }
    }
    path.to_path_buf()
}

struct NativeIsoWvCueTempGuard {
    path: PathBuf,
}

impl NativeIsoWvCueTempGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn into_path(mut self) -> PathBuf {
        std::mem::take(&mut self.path)
    }
}

impl Drop for NativeIsoWvCueTempGuard {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct NativeIsoWvCueRepair {
    cue_before_relative: PathBuf,
    cue_after_relative: PathBuf,
    original_bytes: Vec<u8>,
    rewritten_bytes: Vec<u8>,
    replacements: BTreeMap<String, String>,
    disk_path: PathBuf,
}

impl Drop for NativeIsoWvCueRepair {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.disk_path);
    }
}

async fn target_read_iso_member(
    archive: &Path,
    member: &Path,
    disk_path: &Path,
    xorriso: &Path,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, String> {
    let iso_path = format!("/{}", member.to_string_lossy().trim_start_matches('/'));
    let _ = fs::remove_file(disk_path);
    run_native_archive_edit_command(
        ToolBinary::Xorriso,
        xorriso.to_path_buf(),
        vec![
            "-osirrox".into(),
            "on".into(),
            "-indev".into(),
            archive.display().to_string(),
            "-extract_single".into(),
            iso_path,
            disk_path.display().to_string(),
        ],
        Vec::new(),
        "target-read ISO-WV CUE",
        cancel,
    )
    .await?;
    fs::read(disk_path).map_err(|err| {
        format!(
            "read target-extracted ISO-WV CUE '{}' failed: {err}",
            disk_path.display()
        )
    })
}

async fn prepare_native_iso_wv_cue_repair(
    transactional_archive: &Path,
    rename_pairs: &[ArchiveNativeRenamePair],
    members: &[ArchiveNativeMember],
    xorriso: &Path,
    cancel: &CancellationToken,
) -> Result<NativeIsoWvCueRepair, String> {
    if rename_pairs.len() != 1 {
        return Err("ISO-WV native rename requires exactly one rename pair".to_string());
    }
    let member_files = native_member_files(members)?;
    let cue_before_relative = single_iso_wv_cue_member(&member_files)?;
    let parent = transactional_archive
        .parent()
        .ok_or_else(|| "transactional ISO-WV copy has no parent directory".to_string())?;
    // xorriso restores the member's recorded mode on target-read. ISO files
    // are commonly mode 0444, so never reuse that extracted path as the map
    // source for a rewritten CUE. Keep the read target and writable rewrite
    // staging path separate; this also avoids chmod semantics on sshfs and
    // other remote filesystems.
    let read_path_guard = NativeIsoWvCueTempGuard::new(parent.join(format!(
        ".tonepoet-native-cue-read-{}.tmp",
        uuid::Uuid::new_v4()
    )));
    let original_bytes = target_read_iso_member(
        transactional_archive,
        &cue_before_relative,
        read_path_guard.path(),
        xorriso,
        cancel,
    )
    .await?;
    let disk_path_guard = NativeIsoWvCueTempGuard::new(parent.join(format!(
        ".tonepoet-native-cue-write-{}.tmp",
        uuid::Uuid::new_v4()
    )));
    let cue_text = crate::convert::cue_parser::decode_cue_bytes_for_path(
        &original_bytes,
        &cue_before_relative,
    )?;
    let pair = &rename_pairs[0];
    let shared_plan = plan_iso_wv_cue_reference_rename(
        &cue_text,
        &cue_before_relative,
        Path::new(&pair.old_inner_path),
        Path::new(&pair.new_inner_path),
        |file_ref| {
            resolve_iso_wv_cue_reference_from_members(
                &cue_before_relative,
                file_ref,
                &member_files,
            )
        },
    )?;
    let (_outcome, rewritten_bytes) = crate::convert::cue_parser::rewrite_cue_file_reference_bytes(
        &original_bytes,
        &cue_before_relative,
        &shared_plan.replacements,
    )?;
    if rewritten_bytes != original_bytes {
        fs::write(disk_path_guard.path(), &rewritten_bytes)
            .map_err(|err| format!("write rewritten ISO-WV CUE staging file failed: {err}"))?;
    }

    // Validate the rewritten authority against the post-rename member tree
    // before asking xorriso to modify the transactional copy. This is the
    // archive-member equivalent of the staged resolver's post-write proof.
    let post_member_files = member_files
        .iter()
        .map(|path| remap_member_path_through_native_pairs(path, rename_pairs))
        .collect::<Vec<_>>();
    let rewritten_text = crate::convert::cue_parser::decode_cue_bytes_for_path(
        &rewritten_bytes,
        &shared_plan.cue_after_relative,
    )?;
    let rewritten_sheet = crate::convert::cue_parser::parse_cue(&rewritten_text);
    if rewritten_sheet.tracks.is_empty() {
        return Err("rewritten ISO-WV CUE has no audio tracks".to_string());
    }
    for track in &rewritten_sheet.tracks {
        let file_ref = track
            .file
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("rewritten ISO-WV CUE track {} has no FILE reference", track.number))?;
        resolve_iso_wv_cue_reference_from_members(
            &shared_plan.cue_after_relative,
            file_ref,
            &post_member_files,
        )?;
    }

    Ok(NativeIsoWvCueRepair {
        cue_before_relative,
        cue_after_relative: shared_plan.cue_after_relative,
        original_bytes,
        rewritten_bytes,
        replacements: shared_plan.replacements,
        disk_path: disk_path_guard.into_path(),
    })
}

struct NativeIsoWvSidecarRewrite {
    _claim: Option<crate::concurrency::MutationClaimGuard>,
    admitted_path: PathBuf,
    original_bytes: Vec<u8>,
    rewritten_bytes: Vec<u8>,
}

fn prepare_and_apply_native_iso_wv_sidecar_rewrite(
    logical_archive_path: &Path,
    cue_repair: &NativeIsoWvCueRepair,
) -> Result<Option<NativeIsoWvSidecarRewrite>, String> {
    if cue_repair.replacements.is_empty() {
        return Ok(None);
    }
    let sidecar_path = iso_wv_metadata_sidecar_path(logical_archive_path);
    if !sidecar_path.exists() {
        return Ok(None);
    }
    let (claim, admitted_path) =
        crate::convert::cue_parser::acquire_cue_sidecar_write_claim(&sidecar_path)?;
    let original_bytes = fs::read(&admitted_path).map_err(|err| {
        format!(
            "read adjacent ISO-WV metadata CUE '{}' failed: {err}",
            sidecar_path.display()
        )
    })?;
    let original_text = crate::convert::cue_parser::decode_cue_bytes_for_path(
        &original_bytes,
        &sidecar_path,
    )?;
    let original_sidecar = crate::convert::cue_parser::parse_cue(&original_text);
    let is_tonepoet_snapshot = crate::convert::cue_parser::cue_user_metadata_values(
        &original_sidecar.user_metadata,
        ISO_WV_METADATA_SNAPSHOT_KEY,
    )
    .is_some_and(|values| values.iter().any(|value| value.trim() == "1"));
    if !is_tonepoet_snapshot {
        return Ok(None);
    }
    let internal_before_text = crate::convert::cue_parser::decode_cue_bytes_for_path(
        &cue_repair.original_bytes,
        &cue_repair.cue_before_relative,
    )?;
    let internal_before = crate::convert::cue_parser::parse_cue(&internal_before_text);
    if !iso_wv_cue_geometry_matches(&internal_before, &original_sidecar) {
        return Err(format!(
            "Tonepoet ISO-WV metadata snapshot '{}' is already geometry-stale; native rename declined",
            sidecar_path.display()
        ));
    }

    let (_outcome, rewritten_bytes) = crate::convert::cue_parser::rewrite_cue_file_reference_bytes(
        &original_bytes,
        &sidecar_path,
        &cue_repair.replacements,
    )?;
    let rewritten_sidecar_text = crate::convert::cue_parser::decode_cue_bytes_for_path(
        &rewritten_bytes,
        &sidecar_path,
    )?;
    let rewritten_sidecar = crate::convert::cue_parser::parse_cue(&rewritten_sidecar_text);
    let internal_after_text = crate::convert::cue_parser::decode_cue_bytes_for_path(
        &cue_repair.rewritten_bytes,
        &cue_repair.cue_after_relative,
    )?;
    let internal_after = crate::convert::cue_parser::parse_cue(&internal_after_text);
    if !iso_wv_cue_geometry_matches(&internal_after, &rewritten_sidecar) {
        return Err(format!(
            "Tonepoet ISO-WV metadata snapshot '{}' could not be kept geometry-compatible; native rename declined",
            sidecar_path.display()
        ));
    }
    if rewritten_bytes == original_bytes {
        return Err(format!(
            "Tonepoet ISO-WV metadata snapshot '{}' required a FILE rewrite but remained unchanged; native rename declined",
            sidecar_path.display()
        ));
    }
    crate::convert::cue_parser::atomic_replace_if_unchanged(
        &admitted_path,
        &rewritten_bytes,
        Some(&original_bytes),
    )?;
    Ok(Some(NativeIsoWvSidecarRewrite {
        _claim: claim,
        admitted_path,
        original_bytes,
        rewritten_bytes,
    }))
}

fn rollback_native_iso_wv_sidecar_rewrite(
    rewrite: &NativeIsoWvSidecarRewrite,
) -> Result<(), String> {
    crate::convert::cue_parser::atomic_replace_if_unchanged(
        &rewrite.admitted_path,
        &rewrite.original_bytes,
        Some(&rewrite.rewritten_bytes),
    )
}

/// Apply one or more archive-member rename pairs without extracting payload data
/// while preserving the existing exact install/restore transaction semantics.
///
/// The user's original is never mutated in place. Tonepoet first creates a
/// sibling transactional copy (Linux reflink first, then kernel/server-side
/// copy offload when available, then a cancellable buffered fallback), applies
/// the format-native header edit to that
/// copy, rechecks the original fingerprint, preserves install metadata, and
/// then uses the same backup/install/restore swap as full repackaging.
///
/// `Ok(None)` means the native path deliberately declined this request
/// (unsupported/encrypted format or an ISO-WV CUE safety check) and the caller
/// should use the existing extract/edit/repackage fallback.
pub async fn rename_archive_entry_native_transactional<F>(
    original_archive: &Path,
    logical_archive_path: &Path,
    rename_pairs: &[ArchiveNativeRenamePair],
    archive_members: &[ArchiveNativeMember],
    expected_fingerprint: (i64, u32, u64),
    archive_password: Option<&str>,
    tool_paths: &HashMap<String, PathBuf>,
    cancel: &CancellationToken,
    progress: F,
) -> Result<Option<ArchiveRepackageReport>, String>
where
    F: FnMut(ArchiveNativeRenameProgressSnapshot) + Send + 'static,
{
    if rename_pairs.is_empty() {
        return Err("native archive rename plan is empty".to_string());
    }
    if archive_password.is_some() {
        // Encrypted and header-encrypted containers keep the established
        // password-aware extract/repackage path until native header-only
        // mutation semantics are explicitly validated for those cases.
        return Ok(None);
    }

    let progress = std::sync::Arc::new(std::sync::Mutex::new(progress));
    let emit_progress = |snapshot| {
        if let Ok(mut callback) = progress.lock() {
            (*callback)(snapshot);
        }
    };
    if !archive_native_rename_available(original_archive, tool_paths)? {
        return Ok(None);
    }
    check_repackage_cancelled(cancel)?;
    if archive_fingerprint_for_native_edit(original_archive)? != expected_fingerprint {
        return Err("archive changed externally before rename began; rename was not applied".to_string());
    }

    let format = repackage_archive_format(original_archive)?;
    let parent = original_archive.parent().ok_or_else(|| {
        format!("archive has no parent directory: {}", original_archive.display())
    })?;
    let file_name = original_archive
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("archive name is not valid Unicode: {}", original_archive.display()))?;
    let install_metadata = capture_archive_install_metadata(original_archive)?;
    let archive_size = expected_fingerprint.2;
    let nonce = uuid::Uuid::new_v4();
    let temp_archive = parent.join(format!(
        ".{file_name}.tonepoet-native-rename-{nonce}{}",
        repackage_format_suffix(format)
    ));
    let backup_archive = parent.join(format!(".{file_name}.tonepoet-backup-{nonce}"));

    let source = original_archive.to_path_buf();
    let temp_for_copy = temp_archive.clone();
    let copy_cancel = cancel.clone();
    emit_progress(ArchiveNativeRenameProgressSnapshot::new(
        "Preparing transactional archive copy...",
        0,
        archive_size,
    ));
    let copy_progress = std::sync::Arc::clone(&progress);
    let copy_result = tokio::task::spawn_blocking(move || {
        copy_archive_for_native_edit(
            &source,
            &temp_for_copy,
            archive_size,
            &copy_cancel,
            |bytes_done| {
                if let Ok(mut callback) = copy_progress.lock() {
                    (*callback)(ArchiveNativeRenameProgressSnapshot::new(
                        "Copying archive transactionally...",
                        bytes_done,
                        archive_size,
                    ));
                }
            },
        )
    })
    .await
    .map_err(|err| format!("archive rename copy worker failed: {err}"))?;
    if let Err(err) = copy_result {
        let _ = fs::remove_file(&temp_archive);
        return Err(err);
    }
    emit_progress(ArchiveNativeRenameProgressSnapshot::new(
        "Transactional archive copy ready",
        archive_size,
        archive_size,
    ));
    check_repackage_cancelled(cancel).inspect_err(|_| {
        let _ = fs::remove_file(&temp_archive);
    })?;

    let mut iso_cue_repair = if format == RepackageArchiveFormat::IsoWv {
        let xorriso = repackage_tool_path(tool_paths, &["xorriso"]);
        match prepare_native_iso_wv_cue_repair(
            &temp_archive,
            rename_pairs,
            archive_members,
            &xorriso,
            cancel,
        )
        .await
        {
            Ok(repair) => Some(repair),
            Err(err) => {
                let _ = fs::remove_file(&temp_archive);
                if cancel.is_cancelled() {
                    return Err(ARCHIVE_REPACKAGE_CANCELLED.to_string());
                }
                log::debug!(
                    "native ISO-WV rename declined before mutation; using extraction fallback: {err}"
                );
                return Ok(None);
            }
        }
    } else {
        None
    };

    let command_result = match format {
        RepackageArchiveFormat::SevenZip | RepackageArchiveFormat::Zip => {
            let seven_zip = repackage_tool_path(tool_paths, &["7zz", "7z"]);
            // Archive member names are user data. Disable 7-Zip wildcard
            // parsing so names containing `*` or `?` are renamed literally
            // rather than being interpreted as masks.
            let mut args = vec!["rn".to_string(), "-spd".to_string()];
            let mut secret_args = Vec::new();
            if let Some(password) = archive_password {
                secret_args.push(args.len());
                args.push(format!("-p{password}"));
            }
            args.push("--".to_string());
            args.push(temp_archive.display().to_string());
            for pair in rename_pairs {
                args.push(pair.old_inner_path.clone());
                args.push(pair.new_inner_path.clone());
            }
            emit_progress(ArchiveNativeRenameProgressSnapshot::new(
                "Applying native archive rename...",
                archive_size,
                archive_size,
            ));
            run_native_archive_edit_command(
                ToolBinary::SevenZip,
                seven_zip,
                args,
                secret_args,
                "native 7z/zip rename",
                cancel,
            )
            .await
        }
        RepackageArchiveFormat::IsoWv => {
            let xorriso = repackage_tool_path(tool_paths, &["xorriso"]);
            let pair = rename_pairs
                .first()
                .ok_or_else(|| "native ISO-WV rename plan is empty".to_string())?;
            if rename_pairs.len() != 1 {
                let _ = fs::remove_file(&temp_archive);
                return Ok(None);
            }
            let old_path = format!("/{}", pair.old_inner_path.trim_start_matches('/'));
            let new_path = format!("/{}", pair.new_inner_path.trim_start_matches('/'));
            emit_progress(ArchiveNativeRenameProgressSnapshot::new(
                "Applying native ISO rename...",
                archive_size,
                archive_size,
            ));
            let mut args = vec![
                "-dev".into(),
                temp_archive.display().to_string(),
                "-mv".into(),
                old_path,
                new_path,
                "--".into(),
            ];
            if let Some(repair) = iso_cue_repair.as_ref() {
                if !repair.replacements.is_empty() {
                    let cue_after = format!(
                        "/{}",
                        repair
                            .cue_after_relative
                            .to_string_lossy()
                            .trim_start_matches('/')
                    );
                    args.extend([
                        "-overwrite".into(),
                        "nondir".into(),
                        "-map".into(),
                        repair.disk_path.display().to_string(),
                        cue_after,
                    ]);
                }
            }
            args.push("-commit".into());
            run_native_archive_edit_command(
                ToolBinary::Xorriso,
                xorriso,
                args,
                Vec::new(),
                "native ISO-WV rename",
                cancel,
            )
            .await
        }
        RepackageArchiveFormat::Tar | RepackageArchiveFormat::TarGz => return Ok(None),
        RepackageArchiveFormat::Rar => unreachable!("RAR rejected by native rename preflight"),
    };
    if let Err(err) = command_result {
        let _ = fs::remove_file(&temp_archive);
        return Err(err);
    }

    if let Some(repair) = iso_cue_repair.as_mut() {
        let xorriso = repackage_tool_path(tool_paths, &["xorriso"]);
        let actual = match target_read_iso_member(
            &temp_archive,
            &repair.cue_after_relative,
            &repair.disk_path,
            &xorriso,
            cancel,
        )
        .await
        {
            Ok(actual) => actual,
            Err(err) => {
                let _ = fs::remove_file(&temp_archive);
                if cancel.is_cancelled() {
                    return Err(ARCHIVE_REPACKAGE_CANCELLED.to_string());
                }
                log::debug!(
                    "native ISO-WV rename could not re-read rewritten CUE; using extraction fallback: {err}"
                );
                return Ok(None);
            }
        };
        if actual != repair.rewritten_bytes {
            let _ = fs::remove_file(&temp_archive);
            log::debug!(
                "native ISO-WV rename CUE target-read did not match planned bytes; using extraction fallback"
            );
            return Ok(None);
        }
    }

    emit_progress(ArchiveNativeRenameProgressSnapshot::new(
        "Verifying renamed archive header...",
        archive_size,
        archive_size,
    ));
    if let Err(err) = verify_native_archive_header(
        format,
        &temp_archive,
        archive_password,
        tool_paths,
        cancel,
    )
    .await
    {
        let _ = fs::remove_file(&temp_archive);
        return Err(err);
    }

    check_repackage_cancelled(cancel).inspect_err(|_| {
        let _ = fs::remove_file(&temp_archive);
    })?;
    if archive_fingerprint_for_native_edit(original_archive)? != expected_fingerprint {
        let _ = fs::remove_file(&temp_archive);
        return Err("archive changed externally while rename was being prepared; original was left untouched".to_string());
    }
    if fs::metadata(&temp_archive)
        .map_err(|err| format!("inspect native-rename archive failed: {err}"))?
        .len()
        == 0
    {
        let _ = fs::remove_file(&temp_archive);
        return Err("native archive rename produced an empty archive".to_string());
    }

    let install_metadata_warning = apply_archive_install_metadata(&temp_archive, &install_metadata);
    if let Ok(file) = fs::OpenOptions::new().read(true).open(&temp_archive) {
        let _ = file.sync_all();
    }

    // If Tonepoet owns an adjacent ISO-WV metadata snapshot, keep its FILE
    // geometry synchronized before the archive install. The sidecar mutation
    // claim is held through the archive swap, and an install failure restores
    // the exact original sidecar bytes before returning.
    let sidecar_rewrite = if let Some(repair) = iso_cue_repair.as_ref() {
        match prepare_and_apply_native_iso_wv_sidecar_rewrite(logical_archive_path, repair) {
            Ok(rewrite) => rewrite,
            Err(err) => {
                let _ = fs::remove_file(&temp_archive);
                log::debug!(
                    "native ISO-WV rename could not safely synchronize metadata snapshot; using extraction fallback: {err}"
                );
                return Ok(None);
            }
        }
    } else {
        None
    };
    emit_progress(ArchiveNativeRenameProgressSnapshot::new(
        "Installing renamed archive...",
        archive_size,
        archive_size,
    ));
    let report = replace_archive_atomically(
        original_archive,
        &temp_archive,
        &backup_archive,
        install_metadata_warning,
    );
    if report.is_err() {
        let _ = fs::remove_file(&temp_archive);
        if backup_archive.exists() && !original_archive.exists() {
            let _ = fs::rename(&backup_archive, original_archive);
        }
        if let Some(rewrite) = sidecar_rewrite.as_ref() {
            if let Err(restore_err) = rollback_native_iso_wv_sidecar_rewrite(rewrite) {
                let install_err = report
                    .as_ref()
                    .err()
                    .cloned()
                    .unwrap_or_else(|| "archive install failed".to_string());
                return Err(format!(
                    "{install_err}; adjacent ISO-WV metadata snapshot rollback also failed: {restore_err}"
                ));
            }
        }
    }
    report.map(Some)
}

async fn verify_native_archive_header(
    format: RepackageArchiveFormat,
    archive: &Path,
    archive_password: Option<&str>,
    tool_paths: &HashMap<String, PathBuf>,
    cancel: &CancellationToken,
) -> Result<(), String> {
    match format {
        RepackageArchiveFormat::SevenZip | RepackageArchiveFormat::Zip => {
            // `7z t` would decompress payload data and can turn a header-only
            // rename on a solid archive back into a full-archive operation.
            // A structured listing validates that the rewritten container
            // header/tree remains readable without paying that cost.
            let seven_zip = repackage_tool_path(tool_paths, &["7zz", "7z"]);
            let mut args = vec!["l".to_string(), "-slt".to_string()];
            let mut secret_args = Vec::new();
            if let Some(password) = archive_password {
                secret_args.push(args.len());
                args.push(format!("-p{password}"));
            }
            args.push(archive.display().to_string());
            run_native_archive_edit_command(
                ToolBinary::SevenZip,
                seven_zip,
                args,
                secret_args,
                "verify native 7z/zip rename header",
                cancel,
            )
            .await
        }
        RepackageArchiveFormat::IsoWv => {
            let xorriso = repackage_tool_path(tool_paths, &["xorriso"]);
            run_native_archive_edit_command(
                ToolBinary::Xorriso,
                xorriso,
                vec![
                    "-indev".into(),
                    archive.display().to_string(),
                    "-find".into(),
                    "/".into(),
                    "-exec".into(),
                    "report_lba".into(),
                    "--".into(),
                ],
                Vec::new(),
                "verify native ISO-WV rename header",
                cancel,
            )
            .await
        }
        RepackageArchiveFormat::Tar | RepackageArchiveFormat::TarGz => Ok(()),
        RepackageArchiveFormat::Rar => unreachable!("RAR rejected by native rename preflight"),
    }
}

async fn run_native_archive_edit_command(
    binary: ToolBinary,
    binary_path: PathBuf,
    args: Vec<String>,
    secret_args: Vec<usize>,
    label: &str,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let runner = RealToolRunner::new(HashMap::new());
    let command = ToolCommand {
        binary,
        args,
        secret_args,
        cwd: None,
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        env: Vec::new(),
        timeout: Duration::from_secs(24 * 60 * 60),
    };
    match runner.run_with_binary_path(command, binary_path, cancel).await {
        Ok(_) => Ok(()),
        Err(ToolRunnerError::Cancelled { .. }) => Err(ARCHIVE_REPACKAGE_CANCELLED.to_string()),
        Err(error) => Err(format!("{label}: {error}")),
    }
}

fn archive_fingerprint_for_native_edit(path: &Path) -> Result<(i64, u32, u64), String> {
    let metadata = fs::metadata(path)
        .map_err(|err| format!("stat archive for conflict detection failed: {err}"))?;
    let modified = metadata
        .modified()
        .map_err(|err| format!("read archive mtime for conflict detection failed: {err}"))?;
    let duration = modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| "archive mtime predates UNIX epoch".to_string())?;
    Ok((duration.as_secs() as i64, duration.subsec_nanos(), metadata.len()))
}

#[allow(unsafe_code)] // Linux FICLONE/copy_file_range for transactional archive preparation
fn copy_archive_for_native_edit<F>(
    source: &Path,
    destination: &Path,
    total: u64,
    cancel: &CancellationToken,
    mut progress: F,
) -> Result<(), String>
where
    F: FnMut(u64),
{
    if cancel.is_cancelled() {
        return Err(ARCHIVE_REPACKAGE_CANCELLED.to_string());
    }
    let mut source_file = fs::File::open(source)
        .map_err(|err| format!("open archive for transactional copy failed: {err}"))?;
    let mut destination_file = fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(destination)
        .map_err(|err| format!("create transactional archive copy failed: {err}"))?;

    #[cfg(target_os = "linux")]
    {
        const FICLONE_IOCTL: libc::c_ulong = 0x4004_9409;
        // SAFETY: both descriptors are valid open regular files owned by this
        // function; FICLONE only asks the kernel to clone source extents into
        // the newly-created empty destination.
        if unsafe {
            libc::ioctl(
                destination_file.as_raw_fd(),
                FICLONE_IOCTL,
                source_file.as_raw_fd(),
            )
        } == 0
        {
            progress(total);
            return Ok(());
        }
    }

    destination_file
        .set_len(0)
        .map_err(|err| format!("reset transactional archive copy failed: {err}"))?;

    let mut copied = 0u64;
    #[cfg(target_os = "linux")]
    {
        // copy_file_range can be satisfied entirely inside the kernel and,
        // for supporting NFS/SMB servers, by server-side copy offload.  That
        // preserves the exact rollback copy without unnecessarily pulling a
        // multi-gigabyte archive through Tonepoet's userspace buffers.
        while copied < total {
            if cancel.is_cancelled() {
                return Err(ARCHIVE_REPACKAGE_CANCELLED.to_string());
            }
            let remaining = total.saturating_sub(copied).min(64 * 1024 * 1024) as usize;
            // SAFETY: both descriptors are valid regular files owned by this
            // function. Null offsets request that the kernel advance the file
            // descriptions' current offsets, exactly like read/write.
            let count = unsafe {
                libc::copy_file_range(
                    source_file.as_raw_fd(),
                    std::ptr::null_mut(),
                    destination_file.as_raw_fd(),
                    std::ptr::null_mut(),
                    remaining,
                    0,
                )
            };
            if count > 0 {
                copied = copied.saturating_add(count as u64);
                progress(copied.min(total));
                continue;
            }
            if count == 0 {
                break;
            }

            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                // These mean copy offload is unavailable for this filesystem
                // or source/destination pair. Continue from the current file
                // offsets with the portable buffered copy below.
                Some(libc::EXDEV)
                | Some(libc::EINVAL)
                | Some(libc::ENOSYS)
                | Some(libc::EOPNOTSUPP) => break,
                _ => {
                    return Err(format!(
                        "kernel-assisted transactional archive copy failed: {error}"
                    ))
                }
            }
        }
        if copied == total {
            destination_file
                .sync_all()
                .map_err(|err| format!("sync transactional archive copy failed: {err}"))?;
            return Ok(());
        }
    }

    let mut buffer = vec![0u8; 8 * 1024 * 1024];
    while copied < total {
        if cancel.is_cancelled() {
            return Err(ARCHIVE_REPACKAGE_CANCELLED.to_string());
        }
        let count = source_file
            .read(&mut buffer)
            .map_err(|err| format!("read archive during transactional copy failed: {err}"))?;
        if count == 0 {
            break;
        }
        destination_file
            .write_all(&buffer[..count])
            .map_err(|err| format!("write transactional archive copy failed: {err}"))?;
        copied = copied.saturating_add(count as u64);
        progress(copied.min(total));
    }
    destination_file
        .sync_all()
        .map_err(|err| format!("sync transactional archive copy failed: {err}"))?;
    if copied != total {
        return Err(format!(
            "transactional archive copy size mismatch: expected {total} bytes, copied {copied}"
        ));
    }
    Ok(())
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
    progress: F,
) -> Result<ArchiveRepackageReport, String>
where
    F: FnMut(ArchiveRepackageProgressSnapshot) + Send,
{
    repackage_archive_with_progress_and_cancel_with_password(
        staging_dir,
        original_archive,
        None,
        tool_paths,
        cancel,
        progress,
    )
    .await
}

/// Password-aware archive repackage used by Browse edit sessions.
///
/// The raw password is strictly in-memory and is never copied into recovery
/// state. Before any replacement archive is created, Tonepoet establishes the
/// original container's reproducible encryption policy. If that cannot be
/// done, writeback fails closed and the staged edit remains available.
pub async fn repackage_archive_with_progress_and_cancel_with_password<F>(
    staging_dir: &Path,
    original_archive: &Path,
    archive_password: Option<&str>,
    tool_paths: &HashMap<String, PathBuf>,
    cancel: &CancellationToken,
    mut progress: F,
) -> Result<ArchiveRepackageReport, String>
where
    F: FnMut(ArchiveRepackageProgressSnapshot) + Send,
{
    // Entering the operation is itself observable progress. Emit this before
    // the first cancellation check so a pre-cancelled request still has the
    // same initial state transition as every other repackage attempt.
    progress(ArchiveRepackageProgressSnapshot::new(
        ArchiveRepackageStage::Validating,
        ArchiveRepackageStage::Validating.status_label(),
    ));
    check_repackage_cancelled(cancel)?;

    // Fail before staging traversal or any child-process work when the target
    // format cannot be created. This keeps the direct save path aligned with
    // the UI preflight and preserves actionable tool-resolution errors even
    // when ToolRunner intentionally redacts spawn details.
    let format = repackage_archive_format(original_archive)?;
    require_repackage_format_tool_available(format, tool_paths)?;
    let encryption_policy = detect_repackage_encryption_policy(
        format,
        original_archive,
        archive_password,
        tool_paths,
        cancel,
    )
    .await?;

    let archive_label = original_archive
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| original_archive.display().to_string());
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
            encryption_policy.as_ref(),
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
            encryption_policy.as_ref(),
            tool_paths,
            cancel,
            &archive_label,
            created_size.max(1),
            &mut progress,
        )
        .await?;
        verify_repackaged_encryption_policy(
            format,
            &temp_archive,
            encryption_policy.as_ref(),
            tool_paths,
            cancel,
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
    if crate::convert::classify::is_iso_wv_container(path) {
        return Ok(RepackageArchiveFormat::IsoWv);
    }
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
        RepackageArchiveFormat::IsoWv => ".iso.wv",
    }
}

fn zip_encryption_method_from_listing(method: &str) -> Option<ZipEncryptionMethod> {
    let normalized = method.to_ascii_lowercase();
    if normalized.contains("zipcrypto") {
        Some(ZipEncryptionMethod::ZipCrypto)
    } else if normalized.contains("aes-128") || normalized.contains("aes128") {
        Some(ZipEncryptionMethod::Aes128)
    } else if normalized.contains("aes-192") || normalized.contains("aes192") {
        Some(ZipEncryptionMethod::Aes192)
    } else if normalized.contains("aes-256") || normalized.contains("aes256") {
        Some(ZipEncryptionMethod::Aes256)
    } else {
        None
    }
}

async fn probe_archive_encryption_listing(
    format: RepackageArchiveFormat,
    archive: &Path,
    password: Option<&SecretString>,
    tool_paths: &HashMap<String, PathBuf>,
    cancel: &CancellationToken,
) -> Result<ArchiveEncryptionProbeResult, String> {
    if !matches!(
        format,
        RepackageArchiveFormat::SevenZip | RepackageArchiveFormat::Zip | RepackageArchiveFormat::Rar
    ) {
        return Ok(ArchiveEncryptionProbeResult {
            success: true,
            facts: ArchiveEncryptionProbeFacts::default(),
        });
    }

    let seven_zip = repackage_tool_path(tool_paths, &["7zz", "7z"]);
    if !command_path_available(&seven_zip) {
        return Err(
            "cannot establish archive encryption policy because `7zz`/`7z` is unavailable"
                .to_string(),
        );
    }

    let mut command = tokio::process::Command::new(&seven_zip);
    command
        .arg("l")
        .arg("-slt")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        // Listing failures are interpreted only as a capability/authentication
        // boundary here. Do not retain stderr: it can contain tool-specific
        // password diagnostics, while the caller already has a safe error.
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(password) = password {
        command.arg(format!("-p{}", password.expose()));
    }
    command.arg("--").arg(archive);

    let mut child = command.spawn().map_err(|err| {
        format!(
            "cannot establish archive encryption policy: failed to start 7-Zip listing probe: {err}"
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "cannot establish archive encryption policy: 7-Zip probe stdout unavailable".to_string())?;

    let parser = tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;

        let mut reader = tokio::io::BufReader::new(stdout).lines();
        let mut parser = ArchiveEncryptionListingParser::default();
        while let Some(line) = reader.next_line().await.map_err(|err| {
            format!("read archive encryption listing probe failed: {err}")
        })? {
            parser.push_line(format, &line);
        }
        Ok::<_, String>(parser.finish(format))
    });

    let status = tokio::select! {
        status = child.wait() => status.map_err(|err| {
            format!("wait for archive encryption listing probe failed: {err}")
        })?,
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            let _ = parser.await;
            return Err(ARCHIVE_REPACKAGE_CANCELLED.to_string());
        }
    };
    let facts = parser
        .await
        .map_err(|err| format!("archive encryption listing parser failed: {err}"))??;
    Ok(ArchiveEncryptionProbeResult {
        success: status.success(),
        facts,
    })
}

fn visible_header_encryption_policy(
    format: RepackageArchiveFormat,
    facts: &ArchiveEncryptionProbeFacts,
    password: &SecretString,
) -> Result<ArchiveRepackageEncryptionPolicy, String> {
    if !facts.any_encrypted {
        return Err(
            "archive encryption probe did not identify encrypted payload entries".to_string(),
        );
    }
    if facts.any_unencrypted {
        return Err(
            "archive mixes encrypted and unencrypted members; Tonepoet cannot reproduce that per-member encryption policy safely"
                .to_string(),
        );
    }
    let zip_method = if format == RepackageArchiveFormat::Zip {
        if facts.unknown_encrypted_zip_method || facts.zip_methods.len() != 1 {
            return Err(
                "ZIP encryption method is mixed or unsupported; refusing to replace it with a different protection policy"
                    .to_string(),
            );
        }
        facts.zip_methods.iter().next().copied()
    } else {
        None
    };
    Ok(ArchiveRepackageEncryptionPolicy {
        password: password.clone(),
        header_encryption: false,
        zip_method,
    })
}

async fn detect_repackage_encryption_policy(
    format: RepackageArchiveFormat,
    original_archive: &Path,
    archive_password: Option<&str>,
    tool_paths: &HashMap<String, PathBuf>,
    cancel: &CancellationToken,
) -> Result<Option<ArchiveRepackageEncryptionPolicy>, String> {
    if !matches!(
        format,
        RepackageArchiveFormat::SevenZip | RepackageArchiveFormat::Zip | RepackageArchiveFormat::Rar
    ) {
        return Ok(None);
    }

    let without_password = probe_archive_encryption_listing(
        format,
        original_archive,
        None,
        tool_paths,
        cancel,
    )
    .await?;

    if without_password.success {
        if !without_password.facts.any_encrypted {
            return Ok(None);
        }
        let password = archive_password
            .filter(|value| !value.is_empty())
            .map(SecretString::new)
            .ok_or_else(|| {
                "archive contains encrypted members, but no in-memory password is available for protection-preserving writeback; staged edits were left untouched"
                    .to_string()
            })?;
        return visible_header_encryption_policy(format, &without_password.facts, &password)
            .map(Some);
    }

    let password = archive_password
        .filter(|value| !value.is_empty())
        .map(SecretString::new)
        .ok_or_else(|| {
            "archive headers cannot be listed without authentication and no in-memory password is available; refusing a writeback that could remove encryption"
                .to_string()
        })?;
    let with_password = probe_archive_encryption_listing(
        format,
        original_archive,
        Some(&password),
        tool_paths,
        cancel,
    )
    .await?;
    if !with_password.success {
        return Err(
            "archive encryption policy could not be established with the supplied password; staged edits were left untouched"
                .to_string(),
        );
    }

    match format {
        RepackageArchiveFormat::SevenZip | RepackageArchiveFormat::Rar => {
            if !with_password.facts.any_encrypted {
                return Err(
                    "archive member encryption could not be confirmed after authenticated header listing; refusing protection-changing writeback"
                        .to_string(),
                );
            }
            if with_password.facts.any_unencrypted {
                return Err(
                    "archive mixes encrypted and unencrypted members; Tonepoet cannot reproduce that per-member encryption policy safely"
                        .to_string(),
                );
            }
            Ok(Some(ArchiveRepackageEncryptionPolicy {
                password,
                header_encryption: true,
                zip_method: None,
            }))
        }
        RepackageArchiveFormat::Zip => Err(
            "ZIP member headers are not readable without authentication; Tonepoet cannot reliably reproduce that encryption policy with the configured writer, so the archive was not replaced"
                .to_string(),
        ),
        RepackageArchiveFormat::Tar
        | RepackageArchiveFormat::TarGz
        | RepackageArchiveFormat::IsoWv => unreachable!("non-encrypting formats returned above"),
    }
}

async fn create_repackaged_archive<F>(
    format: RepackageArchiveFormat,
    staging_dir: &Path,
    temp_archive: &Path,
    encryption_policy: Option<&ArchiveRepackageEncryptionPolicy>,
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
            let mut args = vec![
                "a".into(),
                "-t7z".into(),
                temp_archive.display().to_string(),
                "-mmt=on".into(),
            ];
            let mut secret_args = Vec::new();
            if let Some(policy) = encryption_policy {
                secret_args.push(args.len());
                args.push(format!("-p{}", policy.password.expose()));
                args.push(if policy.header_encryption {
                    "-mhe=on".into()
                } else {
                    "-mhe=off".into()
                });
            }
            args.push(".".into());
            run_repackage_command(
                ToolBinary::SevenZip, seven_zip,
                args, secret_args,
                Some(staging_dir.to_path_buf()), "create 7z archive", cancel, monitor, progress
            ).await
        }
        RepackageArchiveFormat::Zip => {
            let seven_zip = repackage_tool_path(tool_paths, &["7zz", "7z"]);
            let mut args = vec![
                "a".into(),
                "-tzip".into(),
                temp_archive.display().to_string(),
            ];
            let mut secret_args = Vec::new();
            if let Some(policy) = encryption_policy {
                let method = policy.zip_method.ok_or_else(|| {
                    "ZIP encryption policy is missing its established encryption method"
                        .to_string()
                })?;
                secret_args.push(args.len());
                args.push(format!("-p{}", policy.password.expose()));
                args.push(method.seven_zip_method_switch().to_string());
            }
            args.push(".".into());
            run_repackage_command(
                ToolBinary::SevenZip, seven_zip,
                args, secret_args,
                Some(staging_dir.to_path_buf()), "create zip archive", cancel, monitor, progress
            ).await
        }
        RepackageArchiveFormat::Tar => {
            let tar = repackage_tool_path(tool_paths, &["tar"]);
            run_repackage_command(
                ToolBinary::Tar, tar,
                vec!["cf".into(), temp_archive.display().to_string(), "-C".into(), staging_dir.display().to_string(), ".".into()],
                Vec::new(),
                None, "create tar archive", cancel, monitor, progress
            ).await
        }
        RepackageArchiveFormat::TarGz => {
            let tar = repackage_tool_path(tool_paths, &["tar"]);
            run_repackage_command(
                ToolBinary::Tar, tar,
                vec!["czf".into(), temp_archive.display().to_string(), "-C".into(), staging_dir.display().to_string(), ".".into()],
                Vec::new(),
                None, "create tar.gz archive", cancel, monitor, progress
            ).await
        }
        RepackageArchiveFormat::Rar => {
            let rar = repackage_tool_path(tool_paths, &["rar"]);
            let mut args = vec!["a".into(), "-r".into()];
            let mut secret_args = Vec::new();
            if let Some(policy) = encryption_policy {
                secret_args.push(args.len());
                args.push(if policy.header_encryption {
                    format!("-hp{}", policy.password.expose())
                } else {
                    format!("-p{}", policy.password.expose())
                });
            }
            args.push(temp_archive.display().to_string());
            args.push(".".into());
            run_repackage_command(
                ToolBinary::Rar, rar,
                args, secret_args,
                Some(staging_dir.to_path_buf()), "create rar archive", cancel, monitor, progress
            )
                .await
                .map_err(|err| {
                    if err.contains("not found") || err.contains("No such file") {
                        "RAR archive creation requires the `rar` executable; install rar or convert the archive to 7z before editing metadata".to_string()
                    } else {
                        err
                    }
                })
        }
        RepackageArchiveFormat::IsoWv => {
            let xorriso = repackage_tool_path(tool_paths, &["xorriso"]);
            run_repackage_command(
                ToolBinary::Xorriso,
                xorriso,
                vec![
                    "-as".into(),
                    "mkisofs".into(),
                    "-iso-level".into(),
                    "3".into(),
                    "-full-iso9660-filenames".into(),
                    "-J".into(),
                    "-r".into(),
                    "-o".into(),
                    temp_archive.display().to_string(),
                    ".".into(),
                ],
                Vec::new(),
                Some(staging_dir.to_path_buf()),
                "create ISO-WV image",
                cancel,
                monitor,
                progress,
            )
            .await
        }
    }
}

async fn verify_repackaged_archive<F>(
    format: RepackageArchiveFormat,
    temp_archive: &Path,
    encryption_policy: Option<&ArchiveRepackageEncryptionPolicy>,
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
            let mut args = vec!["t".into()];
            let mut secret_args = Vec::new();
            if let Some(policy) = encryption_policy {
                secret_args.push(args.len());
                args.push(format!("-p{}", policy.password.expose()));
            }
            args.push(temp_archive.display().to_string());
            run_repackage_command(
                ToolBinary::SevenZip, seven_zip, args, secret_args,
                None, "verify repackaged archive", cancel, monitor, progress
            ).await
        }
        RepackageArchiveFormat::Tar | RepackageArchiveFormat::TarGz => {
            let tar = repackage_tool_path(tool_paths, &["tar"]);
            run_repackage_command(
                ToolBinary::Tar, tar, vec!["tf".into(), temp_archive.display().to_string()],
                Vec::new(),
                None, "verify repackaged tar archive", cancel, monitor, progress
            ).await
        }
        RepackageArchiveFormat::Rar => {
            let rar = repackage_tool_path(tool_paths, &["rar"]);
            let mut args = vec!["t".into()];
            let mut secret_args = Vec::new();
            if let Some(policy) = encryption_policy {
                secret_args.push(args.len());
                args.push(format!("-p{}", policy.password.expose()));
            }
            args.push(temp_archive.display().to_string());
            run_repackage_command(
                ToolBinary::Rar, rar, args, secret_args,
                None, "verify repackaged rar archive", cancel, monitor, progress
            ).await
        }
        RepackageArchiveFormat::IsoWv => {
            let xorriso = repackage_tool_path(tool_paths, &["xorriso"]);
            run_repackage_command(
                ToolBinary::Xorriso,
                xorriso,
                vec![
                    "-indev".into(),
                    temp_archive.display().to_string(),
                    "-find".into(),
                    "/".into(),
                    "-type".into(),
                    "f".into(),
                    "-exec".into(),
                    "report_lba".into(),
                    "--".into(),
                ],
                Vec::new(),
                None,
                "verify repackaged ISO-WV image",
                cancel,
                monitor,
                progress,
            )
            .await
        }
    }
}

async fn verify_repackaged_encryption_policy(
    format: RepackageArchiveFormat,
    temp_archive: &Path,
    encryption_policy: Option<&ArchiveRepackageEncryptionPolicy>,
    tool_paths: &HashMap<String, PathBuf>,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let Some(policy) = encryption_policy else {
        return Ok(());
    };

    let authenticated = probe_archive_encryption_listing(
        format,
        temp_archive,
        Some(&policy.password),
        tool_paths,
        cancel,
    )
    .await?;
    if !authenticated.success || !authenticated.facts.any_encrypted {
        return Err(
            "repackaged archive did not verify as encrypted with the supplied password; original archive was left untouched"
                .to_string(),
        );
    }
    if authenticated.facts.any_unencrypted {
        return Err(
            "repackaged archive contains unexpected unencrypted members; original archive was left untouched"
                .to_string(),
        );
    }
    if format == RepackageArchiveFormat::Zip {
        let expected = policy.zip_method.ok_or_else(|| {
            "ZIP encryption verification is missing the established encryption method".to_string()
        })?;
        if authenticated.facts.unknown_encrypted_zip_method
            || authenticated.facts.zip_methods.len() != 1
            || !authenticated.facts.zip_methods.contains(&expected)
        {
            return Err(
                "repackaged ZIP did not preserve its established encryption method; original archive was left untouched"
                    .to_string(),
            );
        }
    }

    let unauthenticated = probe_archive_encryption_listing(
        format,
        temp_archive,
        None,
        tool_paths,
        cancel,
    )
    .await?;
    if policy.header_encryption {
        if unauthenticated.success {
            return Err(
                "repackaged archive unexpectedly exposes its member headers without a password; original archive was left untouched"
                    .to_string(),
            );
        }
    } else if !unauthenticated.success || !unauthenticated.facts.any_encrypted {
        return Err(
            "repackaged archive no longer matches the original visible-header encryption policy; original archive was left untouched"
                .to_string(),
        );
    }

    Ok(())
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
    binary: ToolBinary,
    binary_path: PathBuf,
    args: Vec<String>,
    secret_args: Vec<usize>,
    cwd: Option<PathBuf>,
    label: &str,
    cancel: &CancellationToken,
    monitor: RepackageCommandMonitor<'_>,
    progress: &mut F,
) -> Result<(), String>
where
    F: FnMut(ArchiveRepackageProgressSnapshot) + Send,
{
    let runner = RealToolRunner::new(HashMap::new());
    let command = ToolCommand {
        binary,
        args,
        secret_args,
        cwd,
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        env: Vec::new(),
        // Archive repackaging was historically unbounded. Keep a generous
        // safety ceiling while preserving explicit cancellation.
        timeout: Duration::from_secs(24 * 60 * 60),
    };
    let future = runner.run_with_binary_path(command, binary_path, cancel);
    tokio::pin!(future);
    let started = Instant::now();
    let mut last_emit = Instant::now();
    let mut last_bytes = 0u64;
    loop {
        tokio::select! {
            result = &mut future => {
                match result {
                    Ok(_) => {
                        let done = monitor.complete_bytes_done
                            .or_else(|| observed_file_len(monitor.observed_path))
                            .unwrap_or(last_bytes);
                        progress(ArchiveRepackageProgressSnapshot::with_archive_bytes(
                            monitor.stage, monitor.status, monitor.archive_label, done,
                            monitor.bytes_total.or(Some(done.max(1))), None,
                        ));
                        return Ok(());
                    }
                    Err(super::errors::ToolRunnerError::Cancelled { .. }) => {
                        return Err(ARCHIVE_REPACKAGE_CANCELLED.to_string());
                    }
                    Err(error) => return Err(format!("{label}: {error}")),
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                let now = Instant::now();
                if now.saturating_duration_since(last_emit) >= Duration::from_millis(250) {
                    let bytes_done = observed_file_len(monitor.observed_path).unwrap_or(last_bytes);
                    let elapsed = now.saturating_duration_since(started).as_secs_f64();
                    let rate = if elapsed > 0.0 && bytes_done >= last_bytes {
                        Some((bytes_done as f64 / elapsed).round() as u64).filter(|rate| *rate > 0)
                    } else { None };
                    last_bytes = bytes_done;
                    last_emit = now;
                    progress(ArchiveRepackageProgressSnapshot::with_archive_bytes(
                        monitor.stage, monitor.status, monitor.archive_label,
                        monitor.bytes_total.map(|total| bytes_done.min(total)).unwrap_or(bytes_done),
                        monitor.bytes_total, rate,
                    ));
                }
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


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveExtractionProgressSnapshot {
    pub status: String,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    pub elapsed: Duration,
}

/// Browse-facing extraction wrapper that preserves the established extraction
/// implementation while adding coarse, low-overhead progress from the staged
/// byte count. This is intentionally used only on edit slow paths; conversion
/// keeps its existing PipelineReporter contract.
pub(crate) async fn extract_archive_to_staging_with_progress<F>(
    archive_path: &Path,
    staging_root: &Path,
    item_id: &str,
    archive_password: Option<&str>,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    expected_bytes: Option<u64>,
    mut progress: F,
) -> Result<(), MaterializeError>
where
    F: FnMut(ArchiveExtractionProgressSnapshot) + Send,
{
    let started = Instant::now();
    progress(ArchiveExtractionProgressSnapshot {
        status: "Extracting archive...".to_string(),
        bytes_done: 0,
        bytes_total: expected_bytes,
        elapsed: Duration::ZERO,
    });
    let future = extract_archive_to_staging(
        archive_path,
        staging_root,
        item_id,
        archive_password,
        runner,
        None,
        cancel,
    );
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => {
                if result.is_ok() {
                    let done = expected_bytes.unwrap_or_else(|| staged_regular_file_bytes(staging_root));
                    progress(ArchiveExtractionProgressSnapshot {
                        status: "Archive extraction complete".to_string(),
                        bytes_done: done,
                        bytes_total: expected_bytes.or(Some(done.max(1))),
                        elapsed: started.elapsed(),
                    });
                }
                return result;
            }
            _ = tokio::time::sleep(Duration::from_millis(750)) => {
                let observed = staged_regular_file_bytes(staging_root);
                let done = expected_bytes.map(|total| observed.min(total)).unwrap_or(observed);
                progress(ArchiveExtractionProgressSnapshot {
                    status: "Extracting archive...".to_string(),
                    bytes_done: done,
                    bytes_total: expected_bytes,
                    elapsed: started.elapsed(),
                });
            }
        }
    }
}

fn staged_regular_file_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    total
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

    // Helpers: album-scope promotion requires every track to agree exactly.
    // List-valued fields compare the complete ordered list, including duplicate
    // values; scalar fields retain their legacy Option semantics.
    fn common_values<F>(tracks: &[PreparedTrack], f: F) -> MetadataValueList
    where
        F: Fn(&TrackMetadata) -> &MetadataValueList,
    {
        let first = f(&tracks[0].metadata);
        if first.is_empty() {
            return MetadataValueList::default();
        }
        if tracks.iter().all(|track| f(&track.metadata) == first) {
            first.clone()
        } else {
            MetadataValueList::default()
        }
    }

    fn common_scalar<F>(tracks: &[PreparedTrack], f: F) -> Option<String>
    where
        F: Fn(&TrackMetadata) -> &Option<String>,
    {
        let first = f(&tracks[0].metadata).as_ref()?;
        tracks
            .iter()
            .all(|track| f(&track.metadata).as_deref() == Some(first.as_str()))
            .then(|| first.clone())
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
        album_artist: common_values(tracks, |m| &m.album_artist)
            .or_else(|| common_values(tracks, |m| &m.artist)),
        genre: common_values(tracks, |m| &m.genre),
        date: common_scalar(tracks, |m| &m.date),
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
    fn fuse_archive_mount_options_track_versioned_tree_semantics() {
        assert_eq!(fuse_archive_mount_options_for_version("1.13"), None);
        assert_eq!(
            fuse_archive_mount_options_for_version("1.14"),
            Some("lazycache,auto_unmount"),
        );
        assert_eq!(
            fuse_archive_mount_options_for_version("fuse-archive 1.19.0"),
            Some("lazycache,auto_unmount"),
        );
        assert_eq!(
            fuse_archive_mount_options_for_version("1.20"),
            Some("lazycache,notrim,auto_unmount"),
        );
        assert_eq!(
            fuse_archive_mount_options_for_version("2.0"),
            Some("lazycache,notrim,auto_unmount"),
        );
        assert_eq!(fuse_archive_mount_options_for_version("unknown"), None);
    }

    #[test]
    fn iso_wv_repackage_classification_is_compound_suffix_specific() {
        assert_eq!(
            repackage_archive_format(Path::new("Album.ISO.WV")).expect("ISO-WV format"),
            RepackageArchiveFormat::IsoWv,
        );
        assert!(
            repackage_archive_format(Path::new("Album.wv")).is_err(),
            "ordinary WavPack must not become an archive repackage target",
        );
        assert_eq!(
            iso_wv_metadata_sidecar_path(Path::new("Album.iso.wv")),
            PathBuf::from("Album.iso.wv.cue"),
        );
    }

    fn native_iso_cue_plan(
        cue_text: &str,
        cue_path: &str,
        old_path: &str,
        new_path: &str,
        members: &[&str],
    ) -> IsoWvCueReferenceRenamePlan {
        let member_files = members.iter().map(PathBuf::from).collect::<Vec<_>>();
        plan_iso_wv_cue_reference_rename(
            cue_text,
            Path::new(cue_path),
            Path::new(old_path),
            Path::new(new_path),
            |file_ref| {
                resolve_iso_wv_cue_reference_from_members(
                    Path::new(cue_path),
                    file_ref,
                    &member_files,
                )
            },
        )
        .expect("native ISO-WV CUE rename plan")
    }

    #[test]
    fn native_iso_wv_audio_rename_repairs_authoritative_cue_reference() {
        let cue = "FILE \"album.wv\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n";
        let plan = native_iso_cue_plan(
            cue,
            "album.cue",
            "album.wv",
            "renamed.wv",
            &["album.cue", "album.wv"],
        );
        assert_eq!(plan.cue_after_relative, PathBuf::from("album.cue"));
        assert_eq!(
            plan.replacements.get("album.wv").map(String::as_str),
            Some("renamed.wv")
        );
    }

    #[test]
    fn native_iso_wv_unquoted_file_rewrite_preserves_trailing_whitespace_and_crlf() {
        let raw = b"FILE album.wv WAVE   \r\n  TRACK 01 AUDIO\r\n    INDEX 01 00:00:00\r\n";
        let text = crate::convert::cue_parser::decode_cue_bytes_for_path(raw, Path::new("album.cue"))
            .expect("decode fixture");
        let plan = native_iso_cue_plan(
            &text,
            "album.cue",
            "album.wv",
            "renamed.wv",
            &["album.cue", "album.wv"],
        );
        let (_outcome, rewritten) = crate::convert::cue_parser::rewrite_cue_file_reference_bytes(
            raw,
            Path::new("album.cue"),
            &plan.replacements,
        )
        .expect("byte-preserving CUE rewrite");
        assert_eq!(
            rewritten,
            b"FILE renamed.wv WAVE   \r\n  TRACK 01 AUDIO\r\n    INDEX 01 00:00:00\r\n"
        );
    }

    #[test]
    fn native_iso_wv_directory_rename_recalculates_cue_geometry() {
        let cue = "FILE \"Disc 1/album.wv\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n";
        let plan = native_iso_cue_plan(
            cue,
            "album.cue",
            "Disc 1",
            "CD 1",
            &["album.cue", "Disc 1/album.wv"],
        );
        assert_eq!(
            plan.replacements.get("Disc 1/album.wv").map(String::as_str),
            Some("CD 1/album.wv")
        );
    }

    #[test]
    fn native_iso_wv_moving_cue_and_audio_together_leaves_file_line_unchanged() {
        let cue = "FILE \"album.wv\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n";
        let plan = native_iso_cue_plan(
            cue,
            "Disc 1/album.cue",
            "Disc 1",
            "CD 1",
            &["Disc 1/album.cue", "Disc 1/album.wv"],
        );
        assert_eq!(plan.cue_after_relative, PathBuf::from("CD 1/album.cue"));
        assert!(plan.replacements.is_empty());
    }

    #[test]
    fn native_iso_wv_moving_only_cue_recalculates_relative_audio_reference() {
        let cue = "FILE \"album.wv\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n";
        let plan = native_iso_cue_plan(
            cue,
            "album.cue",
            "album.cue",
            "Cues/album.cue",
            &["album.cue", "album.wv"],
        );
        assert_eq!(plan.cue_after_relative, PathBuf::from("Cues/album.cue"));
        assert_eq!(
            plan.replacements.get("album.wv").map(String::as_str),
            Some("../album.wv")
        );
    }

    #[test]
    fn native_iso_wv_unrelated_artwork_rename_does_not_touch_cue() {
        let cue = "FILE \"album.wv\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n";
        let plan = native_iso_cue_plan(
            cue,
            "album.cue",
            "front.jpg",
            "cover.jpg",
            &["album.cue", "album.wv", "front.jpg"],
        );
        assert_eq!(plan.cue_after_relative, PathBuf::from("album.cue"));
        assert!(plan.replacements.is_empty());
    }

    #[test]
    fn native_iso_wv_snapshot_rewrite_preserves_metadata_and_rolls_back_exactly() {
        let _coordination = crate::concurrency::scoped_test_coordination_root();
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("Album.iso.wv");
        fs::write(&archive, b"fixture archive identity").expect("archive fixture");
        let sidecar = iso_wv_metadata_sidecar_path(&archive);
        let original_sidecar = concat!(
            "REM TONEPOET_META_V1 BEGIN\n",
            "REM TONEPOET_META_V1 A TONEPOET_ISO_WV_METADATA_SNAPSHOT_V1 1\n",
            "REM TONEPOET_META_V1 A ALBUM \"Snapshot Title\"\n",
            "REM TONEPOET_META_V1 END\n",
            "FILE \"album.wv\" WAVE\n",
            "  TRACK 01 AUDIO\n",
            "    TITLE \"Track Title\"\n",
            "    INDEX 01 00:00:00\n"
        )
        .as_bytes()
        .to_vec();
        fs::write(&sidecar, &original_sidecar).expect("sidecar fixture");

        let internal_before = b"FILE \"album.wv\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n".to_vec();
        let replacements = BTreeMap::from([("album.wv".to_string(), "renamed.wv".to_string())]);
        let (_outcome, internal_after) = crate::convert::cue_parser::rewrite_cue_file_reference_bytes(
            &internal_before,
            Path::new("album.cue"),
            &replacements,
        )
        .expect("internal rewrite");
        let repair = NativeIsoWvCueRepair {
            cue_before_relative: PathBuf::from("album.cue"),
            cue_after_relative: PathBuf::from("album.cue"),
            original_bytes: internal_before,
            rewritten_bytes: internal_after,
            replacements,
            disk_path: temp.path().join("unused-native-cue.tmp"),
        };

        let rewrite = prepare_and_apply_native_iso_wv_sidecar_rewrite(&archive, &repair)
            .expect("snapshot rewrite")
            .expect("Tonepoet snapshot should be rewritten");
        let rewritten = fs::read(&sidecar).expect("rewritten sidecar");
        let rewritten_text = String::from_utf8(rewritten).expect("UTF-8 sidecar fixture");
        assert!(rewritten_text.contains("FILE \"renamed.wv\" WAVE"));
        assert!(rewritten_text.contains("Snapshot Title"));
        assert!(rewritten_text.contains("Track Title"));

        rollback_native_iso_wv_sidecar_rewrite(&rewrite).expect("exact sidecar rollback");
        assert_eq!(
            fs::read(&sidecar).expect("restored sidecar"),
            original_sidecar,
            "rollback must restore the exact pre-rename metadata snapshot bytes"
        );
    }

    #[test]
    fn zip_encryption_method_parser_accepts_supported_7zip_spellings() {
        assert_eq!(
            zip_encryption_method_from_listing("ZipCrypto Deflate"),
            Some(ZipEncryptionMethod::ZipCrypto)
        );
        assert_eq!(
            zip_encryption_method_from_listing("AES-128 Deflate"),
            Some(ZipEncryptionMethod::Aes128)
        );
        assert_eq!(
            zip_encryption_method_from_listing("AES192 Store"),
            Some(ZipEncryptionMethod::Aes192)
        );
        assert_eq!(
            zip_encryption_method_from_listing("AES-256 Deflate"),
            Some(ZipEncryptionMethod::Aes256)
        );
        assert_eq!(zip_encryption_method_from_listing("StrongCrypto42"), None);
    }

    #[test]
    fn encryption_listing_parser_keeps_multiple_slt_members_distinct() {
        let mut parser = ArchiveEncryptionListingParser::default();
        for line in [
            "Path = Album.zip",
            "Type = zip",
            "Method = Deflate",
            "----------",
            "Path = encrypted.flac",
            "Folder = -",
            "Encrypted = +",
            "Method = AES-256 Deflate",
            "",
            "Path = plain.jpg",
            "Folder = -",
            "Encrypted = -",
            "Method = Deflate",
        ] {
            parser.push_line(RepackageArchiveFormat::Zip, line);
        }
        let facts = parser.finish(RepackageArchiveFormat::Zip);
        assert!(facts.any_encrypted, "first member must remain visible to policy detection");
        assert!(facts.any_unencrypted, "second member must remain a distinct plaintext member");
        assert_eq!(
            facts.zip_methods,
            BTreeSet::from([ZipEncryptionMethod::Aes256]),
            "archive-level Method and later member fields must not overwrite the encrypted member method"
        );
        assert!(!facts.unknown_encrypted_zip_method);
    }

    #[test]
    fn encryption_listing_parser_uses_attributes_when_7z_omits_folder_field() {
        let mut parser = ArchiveEncryptionListingParser::default();
        for line in [
            "Path = Visible.7z",
            "Type = 7z",
            "----------",
            "Path = Disc 1",
            "Attributes = D drwxrwxr-x",
            "Encrypted = -",
            "Path = Disc 1/01.txt",
            "Attributes = A -rw-rw-r--",
            "Encrypted = +",
            "Method = LZMA2:12",
            "Path = manifest.txt",
            "Attributes = A -rw-rw-r--",
            "Encrypted = +",
            "Method = LZMA2:12",
        ] {
            parser.push_line(RepackageArchiveFormat::SevenZip, line);
        }
        let facts = parser.finish(RepackageArchiveFormat::SevenZip);
        assert!(
            facts.any_encrypted,
            "encrypted files must remain visible to policy detection"
        );
        assert!(
            !facts.any_unencrypted,
            "the unencrypted directory record must not be mistaken for a plaintext member"
        );
    }

    #[test]
    fn encryption_policy_refuses_mixed_member_protection() {
        let facts = ArchiveEncryptionProbeFacts {
            any_encrypted: true,
            any_unencrypted: true,
            zip_methods: BTreeSet::from([ZipEncryptionMethod::Aes256]),
            unknown_encrypted_zip_method: false,
        };
        let err = visible_header_encryption_policy(
            RepackageArchiveFormat::Zip,
            &facts,
            &SecretString::new("test-password"),
        )
        .expect_err("mixed encrypted/plain members must fail closed");
        assert!(err.contains("mixes encrypted and unencrypted"));
    }

    #[test]
    fn iso_wv_repackage_preflight_reports_missing_xorriso() {
        let temp = tempfile::tempdir().expect("temp dir");
        let original = temp.path().join("Album.iso.wv");
        fs::write(&original, b"iso placeholder").expect("archive placeholder");
        let missing_xorriso = temp.path().join("definitely-missing-xorriso-binary");
        let tool_paths = HashMap::from([("xorriso".to_string(), missing_xorriso)]);

        let err = preflight_archive_repackage_capability(&original, &tool_paths)
            .expect_err("missing xorriso must be reported before extraction");
        assert!(
            err.contains("ISO-WV repackaging requires the `xorriso` executable"),
            "missing xorriso preflight error should be actionable: {err}",
        );
    }

    #[test]
    fn iso_wv_sidecar_geometry_ignores_metadata_but_rejects_track_drift() {
        let base = crate::convert::cue_parser::parse_cue(
            "TITLE \"Album A\"\nFILE \"album.wv\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 04:00:00\n",
        );
        let metadata_only = crate::convert::cue_parser::parse_cue(
            "TITLE \"Album B\"\nFILE \"album.wv\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Uno\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Dos\"\n    INDEX 01 04:00:00\n",
        );
        assert!(iso_wv_cue_geometry_matches(&base, &metadata_only));

        let drifted = crate::convert::cue_parser::parse_cue(
            "FILE \"album.wv\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 04:00:01\n",
        );
        assert!(!iso_wv_cue_geometry_matches(&base, &drifted));
    }

    #[test]
    fn iso_wv_cue_discovery_accepts_one_visible_sheet_and_ignores_hidden_scratch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nested = temp.path().join("Disc");
        fs::create_dir_all(&nested).expect("nested");
        let cue = nested.join("Album.CUE");
        fs::write(&cue, b"FILE \"album.wv\" WAVE\n").expect("visible cue");
        fs::write(temp.path().join("._Album.cue"), b"scratch").expect("hidden cue");

        assert_eq!(find_single_visible_cue(temp.path()).unwrap(), cue);
    }

    #[test]
    fn iso_wv_cue_discovery_refuses_missing_or_ambiguous_authority() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = find_single_visible_cue(temp.path()).unwrap_err().to_string();
        assert!(missing.contains("no user-visible CUE"));

        fs::write(temp.path().join("a.cue"), b"FILE \"a.wv\" WAVE\n").expect("cue a");
        fs::write(temp.path().join("b.cue"), b"FILE \"b.wv\" WAVE\n").expect("cue b");
        let ambiguous = find_single_visible_cue(temp.path()).unwrap_err().to_string();
        assert!(ambiguous.contains("2 user-visible CUE sheets"));
    }

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
                metadata.arranger.as_deref(),
                metadata.genre.as_deref(),
                metadata.date.as_deref(),
                metadata.track_number,
                metadata.disc_number,
                metadata.isrc.as_deref(),
            ),
            (None, None, None, None, None, None, None, None, None, None, None),
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
                sidecar_cue_track_metadata: None,
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
            artist: Some("Original Artist".to_string()).into(),
            genre: Some("Rock".to_string()).into(),
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
        assert!(metadata.artist.is_empty());
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

    fn run_secret_fixture_output(
        program: &Path,
        args: &[String],
        cwd: Option<&Path>,
    ) -> std::io::Result<std::process::Output> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        // Deliberately do not format `args` into any error: test passwords are
        // secrets for the same logging-contract purposes as production ones.
        command.output()
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

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn iso_wv_real_repackage_mount_resolve_and_decode_after_payload_rename() {
        let required = std::env::var_os("TONEPOET_REQUIRE_TOOLS")
            .map(|value| value != "0" && !value.is_empty())
            .unwrap_or(false);
        let tools = (
            find_executable(&["xorriso"]),
            find_executable(&["fuseiso"]),
            find_executable(&["wavpack"]),
            find_executable(&["ffmpeg"]),
        );
        let (Some(xorriso), Some(fuseiso), Some(wavpack), Some(ffmpeg)) = tools else {
            if required {
                panic!(
                    "ISO-WV real acceptance requires xorriso, fuseiso, wavpack, and ffmpeg because TONEPOET_REQUIRE_TOOLS=1"
                );
            }
            eprintln!(
                "skipping ISO-WV real repackage acceptance; xorriso, fuseiso, wavpack, and ffmpeg are required"
            );
            return;
        };
        if !Path::new("/dev/fuse").exists() {
            if required {
                panic!("ISO-WV real acceptance requires /dev/fuse because TONEPOET_REQUIRE_TOOLS=1");
            }
            eprintln!("skipping ISO-WV real repackage acceptance; /dev/fuse is unavailable");
            return;
        }

        let temp = tempfile::tempdir().expect("temp dir");
        let staging = temp.path().join("staging");
        fs::create_dir_all(&staging).expect("staging dir");

        // Build a tiny deterministic PCM fixture without depending on a media
        // generator, then encode it with the same WavPack CLI shipped by the
        // runtime environment.
        let wav = temp.path().join("fixture.wav");
        let frames = 4_410u32;
        let channels = 1u16;
        let sample_rate = 44_100u32;
        let bits_per_sample = 16u16;
        let data_len = frames * u32::from(channels) * u32::from(bits_per_sample / 8);
        let mut wav_file = fs::File::create(&wav).expect("create WAV fixture");
        wav_file.write_all(b"RIFF").unwrap();
        wav_file.write_all(&(36u32 + data_len).to_le_bytes()).unwrap();
        wav_file.write_all(b"WAVEfmt ").unwrap();
        wav_file.write_all(&16u32.to_le_bytes()).unwrap();
        wav_file.write_all(&1u16.to_le_bytes()).unwrap();
        wav_file.write_all(&channels.to_le_bytes()).unwrap();
        wav_file.write_all(&sample_rate.to_le_bytes()).unwrap();
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample / 8);
        wav_file.write_all(&byte_rate.to_le_bytes()).unwrap();
        let block_align = channels * (bits_per_sample / 8);
        wav_file.write_all(&block_align.to_le_bytes()).unwrap();
        wav_file.write_all(&bits_per_sample.to_le_bytes()).unwrap();
        wav_file.write_all(b"data").unwrap();
        wav_file.write_all(&data_len.to_le_bytes()).unwrap();
        wav_file.write_all(&vec![0u8; data_len as usize]).unwrap();
        drop(wav_file);

        let renamed_wv = staging.join("renamed.wv");
        run_fixture_command(
            &wavpack,
            &[
                OsStr::new("-q"),
                OsStr::new("-y"),
                wav.as_os_str(),
                OsStr::new("-o"),
                renamed_wv.as_os_str(),
            ],
            None,
        )
        .expect("encode WavPack fixture");
        fs::write(
            staging.join("album.cue"),
            "TITLE \"Renamed Album\"\nFILE \"renamed.wv\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n",
        )
        .expect("write corrected CUE fixture");

        let archive = temp.path().join("Album.iso.wv");
        fs::write(&archive, b"pre-repackage placeholder").expect("original placeholder");
        let tool_paths = HashMap::from([("xorriso".to_string(), xorriso)]);
        repackage_archive(&staging, &archive, &tool_paths)
            .await
            .expect("rebuild renamed ISO-WV fixture");

        let mount_point = temp.path().join("mounted");
        let runner = RealToolRunner::new(HashMap::from([("fuseiso".to_string(), fuseiso)]));
        let cancel = CancellationToken::new();
        let lease = match try_mount_iso_wv_readonly(&archive, &mount_point, &runner, &cancel)
            .await
            .expect("attempt corrected ISO-WV mount")
        {
            Some(lease) => lease,
            None if required => {
                panic!("ISO-WV real acceptance could not acquire an unprivileged FUSE mount")
            }
            None => {
                eprintln!("skipping ISO-WV real repackage acceptance; FUSE mount was unavailable");
                return;
            }
        };

        let mounted_cue = find_single_visible_cue(lease.mount_point())
            .expect("mounted rebuilt ISO must contain one CUE authority");
        let sheet = crate::tui::cue_parser::parse_cue_file(&mounted_cue)
            .expect("parse mounted rebuilt CUE");
        let file_ref = sheet
            .tracks
            .first()
            .and_then(|track| track.file.as_deref())
            .expect("mounted rebuilt CUE FILE reference");
        assert_eq!(file_ref, "renamed.wv");
        let resolved = match crate::tui::browse::resolve_cue_file_reference_for_queue(
            mounted_cue.parent().unwrap(),
            file_ref,
        ) {
            crate::tui::browse::CueReferenceResolution::Resolved(path) => path,
            other => panic!("mounted rebuilt CUE did not resolve renamed payload: {other:?}"),
        };

        let decode = Command::new(&ffmpeg)
            .arg("-v")
            .arg("error")
            .arg("-i")
            .arg(&resolved)
            .arg("-t")
            .arg("0.05")
            .arg("-f")
            .arg("null")
            .arg("-")
            .output()
            .expect("run ffmpeg decode against mounted renamed payload");
        assert!(
            decode.status.success(),
            "mounted renamed WavPack did not decode: {}",
            String::from_utf8_lossy(&decode.stderr)
        );

        drop(lease);
        assert!(
            !super::super::types::linux_mountinfo_contains(&mount_point),
            "real ISO-WV acceptance leaked its FUSE mount"
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
            let expected = format!("edited payload for {archive_name}");
            let staging = write_repackage_staging(temp.path(), "original payload")
                .expect("write repackage staging");
            let archive_type = if archive_name.ends_with(".zip") {
                OsStr::new("-tzip")
            } else {
                OsStr::new("-t7z")
            };
            run_fixture_command(
                &seven_zip,
                &[
                    OsStr::new("a"),
                    archive_type,
                    original.as_os_str(),
                    OsStr::new("."),
                ],
                Some(&staging),
            )
            .unwrap_or_else(|err| panic!("create original {archive_name}: {err}"));
            fs::write(staging.join("Disc 1").join("01.txt"), &expected)
                .expect("edit staged nested payload");
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

    #[tokio::test]
    async fn repackage_archive_preserves_real_7z_and_zip_encryption_when_7z_is_available() {
        let Some(seven_zip) = find_executable(&["7zz", "7z"]) else {
            eprintln!("skipping encrypted zip/7z repackage integration test: 7z/7zz is required");
            return;
        };
        let password = "tonepoet-encryption-test";
        let cases = [
            ("Visible.7z", "-t7z", Some("-mhe=off"), false),
            ("HeaderEncrypted.7z", "-t7z", Some("-mhe=on"), true),
            ("Encrypted.zip", "-tzip", Some("-mem=AES256"), false),
        ];

        for (archive_name, archive_type, encryption_switch, header_encrypted) in cases {
            let temp = tempfile::tempdir().expect("temp dir");
            let original = temp.path().join(archive_name);
            let staging = write_repackage_staging(temp.path(), "original encrypted payload")
                .expect("write encrypted repackage staging");
            let mut create_args = vec![
                "a".to_string(),
                archive_type.to_string(),
                original.display().to_string(),
                format!("-p{password}"),
            ];
            if let Some(switch) = encryption_switch {
                create_args.push(switch.to_string());
            }
            create_args.push(".".to_string());
            let create = run_secret_fixture_output(&seven_zip, &create_args, Some(&staging))
                .expect("create encrypted archive fixture");
            assert!(create.status.success(), "encrypted archive fixture creation failed");

            let expected = format!("edited encrypted payload for {archive_name}");
            fs::write(staging.join("Disc 1").join("01.txt"), &expected)
                .expect("edit encrypted staged payload");
            let tool_paths = HashMap::from([
                ("7zz".to_string(), seven_zip.clone()),
                ("7z".to_string(), seven_zip.clone()),
            ]);
            let cancel = CancellationToken::new();
            repackage_archive_with_progress_and_cancel_with_password(
                &staging,
                &original,
                Some(password),
                &tool_paths,
                &cancel,
                |_| {},
            )
            .await
            .unwrap_or_else(|err| panic!("encrypted repackage {archive_name}: {err}"));

            let authenticated_test = run_secret_fixture_output(
                &seven_zip,
                &[
                    "t".to_string(),
                    format!("-p{password}"),
                    original.display().to_string(),
                ],
                None,
            )
            .expect("test encrypted archive with correct password");
            assert!(authenticated_test.status.success(), "correct password must verify {archive_name}");
            let wrong_test = run_secret_fixture_output(
                &seven_zip,
                &[
                    "t".to_string(),
                    "-pdefinitely-wrong".to_string(),
                    original.display().to_string(),
                ],
                None,
            )
            .expect("test encrypted archive with wrong password");
            assert!(!wrong_test.status.success(), "wrong password must fail for {archive_name}");

            let unauthenticated_listing = run_secret_fixture_output(
                &seven_zip,
                &[
                    "l".to_string(),
                    "-slt".to_string(),
                    original.display().to_string(),
                ],
                None,
            )
            .expect("list encrypted archive without password");
            if header_encrypted {
                assert!(
                    !unauthenticated_listing.status.success(),
                    "header-encrypted archive must not expose its member tree without authentication"
                );
            } else {
                assert!(
                    unauthenticated_listing.status.success(),
                    "visible-header encrypted archive must remain listable without authentication"
                );
                assert!(
                    String::from_utf8_lossy(&unauthenticated_listing.stdout)
                        .contains("Encrypted = +"),
                    "visible headers must still identify encrypted payload members"
                );
            }

            let extract_dir = temp.path().join("decrypted");
            fs::create_dir(&extract_dir).expect("decrypted output dir");
            let extract = run_secret_fixture_output(
                &seven_zip,
                &[
                    "x".to_string(),
                    "-y".to_string(),
                    format!("-p{password}"),
                    format!("-o{}", extract_dir.display()),
                    original.display().to_string(),
                ],
                None,
            )
            .expect("extract encrypted replacement");
            assert!(extract.status.success(), "correct password must extract {archive_name}");
            assert_repackaged_content(&extract_dir, &expected);
        }
    }

    #[tokio::test]
    async fn repackage_archive_preserves_real_rar_password_scope_when_tools_are_available() {
        let (Some(seven_zip), Some(rar)) = (
            find_executable(&["7zz", "7z"]),
            find_executable(&["rar"]),
        ) else {
            eprintln!("skipping encrypted RAR repackage integration test: 7z/7zz and rar are required");
            return;
        };
        let password = "tonepoet-rar-encryption-test";

        for (archive_name, password_switch, header_encrypted) in [
            ("Visible.rar", format!("-p{password}"), false),
            ("HeaderEncrypted.rar", format!("-hp{password}"), true),
        ] {
            let temp = tempfile::tempdir().expect("temp dir");
            let original = temp.path().join(archive_name);
            let staging = write_repackage_staging(temp.path(), "original encrypted RAR payload")
                .expect("write RAR repackage staging");
            let create = run_secret_fixture_output(
                &rar,
                &[
                    "a".to_string(),
                    "-r".to_string(),
                    password_switch,
                    original.display().to_string(),
                    ".".to_string(),
                ],
                Some(&staging),
            )
            .expect("create encrypted RAR fixture");
            assert!(create.status.success(), "encrypted RAR fixture creation failed");

            let expected = format!("edited encrypted RAR payload for {archive_name}");
            fs::write(staging.join("Disc 1").join("01.txt"), &expected)
                .expect("edit encrypted RAR staged payload");
            let tool_paths = HashMap::from([
                ("7zz".to_string(), seven_zip.clone()),
                ("7z".to_string(), seven_zip.clone()),
                ("rar".to_string(), rar.clone()),
            ]);
            let cancel = CancellationToken::new();
            repackage_archive_with_progress_and_cancel_with_password(
                &staging,
                &original,
                Some(password),
                &tool_paths,
                &cancel,
                |_| {},
            )
            .await
            .unwrap_or_else(|err| panic!("encrypted RAR repackage {archive_name}: {err}"));

            let correct = run_secret_fixture_output(
                &rar,
                &[
                    "t".to_string(),
                    format!("-p{password}"),
                    original.display().to_string(),
                ],
                None,
            )
            .expect("test RAR with correct password");
            assert!(correct.status.success(), "correct password must verify {archive_name}");
            let wrong = run_secret_fixture_output(
                &rar,
                &[
                    "t".to_string(),
                    "-pdefinitely-wrong".to_string(),
                    original.display().to_string(),
                ],
                None,
            )
            .expect("test RAR with wrong password");
            assert!(!wrong.status.success(), "wrong password must fail for {archive_name}");

            let unauthenticated_listing = run_secret_fixture_output(
                &seven_zip,
                &[
                    "l".to_string(),
                    "-slt".to_string(),
                    original.display().to_string(),
                ],
                None,
            )
            .expect("list RAR without password");
            assert_eq!(
                unauthenticated_listing.status.success(),
                !header_encrypted,
                "RAR header visibility must be preserved"
            );
        }
    }

    #[tokio::test]
    async fn native_iso_wv_real_rename_repairs_cue_and_snapshot_without_extracting_audio() {
        let Some(xorriso) = find_executable(&["xorriso"]) else {
            eprintln!("skipping native ISO-WV rename integration test: xorriso is required");
            return;
        };
        let _coordination = crate::concurrency::scoped_test_coordination_root();
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("iso-source");
        fs::create_dir(&source).expect("ISO source dir");
        fs::write(source.join("album.wv"), b"opaque wavpack fixture payload")
            .expect("audio payload fixture");
        fs::write(
            source.join("album.cue"),
            b"FILE \"album.wv\" WAVE\r\n  TRACK 01 AUDIO\r\n    INDEX 01 00:00:00\r\n",
        )
        .expect("CUE fixture");
        let archive = temp.path().join("Album.iso.wv");
        run_fixture_command(
            &xorriso,
            &[
                OsStr::new("-as"),
                OsStr::new("mkisofs"),
                OsStr::new("-iso-level"),
                OsStr::new("3"),
                OsStr::new("-full-iso9660-filenames"),
                OsStr::new("-J"),
                OsStr::new("-r"),
                OsStr::new("-o"),
                archive.as_os_str(),
                OsStr::new("."),
            ],
            Some(&source),
        )
        .expect("create ISO-WV fixture");

        let snapshot = iso_wv_metadata_sidecar_path(&archive);
        let snapshot_before = concat!(
            "REM TONEPOET_META_V1 BEGIN\r\n",
            "REM TONEPOET_META_V1 A TONEPOET_ISO_WV_METADATA_SNAPSHOT_V1 1\r\n",
            "REM TONEPOET_META_V1 A ALBUM \"Preserved Metadata\"\r\n",
            "REM TONEPOET_META_V1 END\r\n",
            "FILE \"album.wv\" WAVE\r\n",
            "  TRACK 01 AUDIO\r\n",
            "    INDEX 01 00:00:00\r\n"
        );
        fs::write(&snapshot, snapshot_before.as_bytes()).expect("snapshot fixture");

        let fingerprint = archive_fingerprint_for_native_edit(&archive).expect("fingerprint");
        let tool_paths = HashMap::from([("xorriso".to_string(), xorriso.clone())]);
        let cancel = CancellationToken::new();
        let report = rename_archive_entry_native_transactional(
            &archive,
            &archive,
            &[ArchiveNativeRenamePair::new("album.wv", "renamed.wv")],
            &[
                ArchiveNativeMember {
                    path: "album.cue".to_string(),
                    is_dir: false,
                },
                ArchiveNativeMember {
                    path: "album.wv".to_string(),
                    is_dir: false,
                },
            ],
            fingerprint,
            None,
            &tool_paths,
            &cancel,
            |_| {},
        )
        .await
        .expect("native ISO-WV rename")
        .expect("ISO-WV native path should be admitted");
        assert!(!report.has_warnings(), "fixture rename should install cleanly");

        let extracted_cue = temp.path().join("result.cue");
        run_fixture_command(
            &xorriso,
            &[
                OsStr::new("-osirrox"),
                OsStr::new("on"),
                OsStr::new("-indev"),
                archive.as_os_str(),
                OsStr::new("-extract_single"),
                OsStr::new("/album.cue"),
                extracted_cue.as_os_str(),
            ],
            None,
        )
        .expect("target-read repaired CUE");
        assert_eq!(
            fs::read(&extracted_cue).expect("repaired CUE bytes"),
            b"FILE \"renamed.wv\" WAVE\r\n  TRACK 01 AUDIO\r\n    INDEX 01 00:00:00\r\n",
            "native path must preserve CUE byte style while repairing FILE geometry"
        );
        let extracted_audio = temp.path().join("renamed.wv");
        run_fixture_command(
            &xorriso,
            &[
                OsStr::new("-osirrox"),
                OsStr::new("on"),
                OsStr::new("-indev"),
                archive.as_os_str(),
                OsStr::new("-extract_single"),
                OsStr::new("/renamed.wv"),
                extracted_audio.as_os_str(),
            ],
            None,
        )
        .expect("target-read renamed audio");
        assert_eq!(
            fs::read(&extracted_audio).expect("renamed audio bytes"),
            b"opaque wavpack fixture payload"
        );
        let snapshot_after = fs::read_to_string(&snapshot).expect("rewritten snapshot");
        assert!(snapshot_after.contains("FILE \"renamed.wv\" WAVE"));
        assert!(snapshot_after.contains("Preserved Metadata"));
    }

    #[tokio::test]
    async fn native_iso_wv_real_cue_planning_failure_leaves_archive_and_snapshot_byte_exact() {
        let Some(xorriso) = find_executable(&["xorriso"]) else {
            eprintln!("skipping native ISO-WV failure integration test: xorriso is required");
            return;
        };
        let _coordination = crate::concurrency::scoped_test_coordination_root();
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("iso-source");
        fs::create_dir(&source).expect("ISO source dir");
        fs::write(source.join("album.wv"), b"opaque payload").expect("audio payload");
        fs::write(
            source.join("album.cue"),
            b"FILE \"missing.wv\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .expect("malformed authority fixture");
        let archive = temp.path().join("Broken.iso.wv");
        run_fixture_command(
            &xorriso,
            &[
                OsStr::new("-as"),
                OsStr::new("mkisofs"),
                OsStr::new("-iso-level"),
                OsStr::new("3"),
                OsStr::new("-full-iso9660-filenames"),
                OsStr::new("-J"),
                OsStr::new("-r"),
                OsStr::new("-o"),
                archive.as_os_str(),
                OsStr::new("."),
            ],
            Some(&source),
        )
        .expect("create malformed ISO-WV fixture");
        let snapshot = iso_wv_metadata_sidecar_path(&archive);
        let snapshot_bytes = concat!(
            "REM TONEPOET_META_V1 BEGIN\n",
            "REM TONEPOET_META_V1 A TONEPOET_ISO_WV_METADATA_SNAPSHOT_V1 1\n",
            "REM TONEPOET_META_V1 A ALBUM \"Must Survive Decline\"\n",
            "REM TONEPOET_META_V1 END\n",
            "FILE \"missing.wv\" WAVE\n",
            "  TRACK 01 AUDIO\n",
            "    INDEX 01 00:00:00\n"
        )
        .as_bytes()
        .to_vec();
        fs::write(&snapshot, &snapshot_bytes).expect("Tonepoet metadata snapshot fixture");
        let archive_before = fs::read(&archive).expect("archive before native decline");
        let fingerprint = archive_fingerprint_for_native_edit(&archive).expect("fingerprint");
        let tool_paths = HashMap::from([("xorriso".to_string(), xorriso)]);
        let cancel = CancellationToken::new();
        let result = rename_archive_entry_native_transactional(
            &archive,
            &archive,
            &[ArchiveNativeRenamePair::new("album.wv", "renamed.wv")],
            &[
                ArchiveNativeMember {
                    path: "album.cue".to_string(),
                    is_dir: false,
                },
                ArchiveNativeMember {
                    path: "album.wv".to_string(),
                    is_dir: false,
                },
            ],
            fingerprint,
            None,
            &tool_paths,
            &cancel,
            |_| {},
        )
        .await
        .expect("native path should decline safely rather than fail the operation");
        assert!(result.is_none(), "unsafe native CUE repair must fall back");
        assert_eq!(fs::read(&archive).expect("archive after decline"), archive_before);
        assert_eq!(fs::read(&snapshot).expect("snapshot after decline"), snapshot_bytes);
    }

    #[tokio::test]
    async fn native_zip_real_multi_pair_rename_handles_archive_without_directory_records() {
        let (Some(seven_zip), Some(zip)) = (
            find_executable(&["7zz", "7z"]),
            find_executable(&["zip"]),
        ) else {
            eprintln!("skipping implicit-directory ZIP rename integration test: 7z/7zz and zip are required");
            return;
        };
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("source");
        let disc = source.join("Disc 1");
        let nested = disc.join("Live");
        fs::create_dir_all(&nested).expect("nested source dirs");
        fs::write(disc.join("01.flac"), b"track-one-payload").expect("track one");
        fs::write(disc.join("cover.jpg"), b"cover-payload").expect("cover");
        fs::write(nested.join("02.flac"), b"track-two-payload").expect("track two");
        let archive = temp.path().join("Album.zip");
        run_fixture_command(
            &zip,
            &[
                OsStr::new("-D"),
                OsStr::new("-r"),
                archive.as_os_str(),
                OsStr::new("Disc 1"),
            ],
            Some(&source),
        )
        .expect("create ZIP without directory entries");

        let before_listing = run_secret_fixture_output(
            &seven_zip,
            &[
                "l".to_string(),
                "-slt".to_string(),
                archive.display().to_string(),
            ],
            None,
        )
        .expect("list implicit-directory ZIP");
        assert!(before_listing.status.success());
        let before_text = String::from_utf8_lossy(&before_listing.stdout);
        assert!(before_text.contains("Path = Disc 1/01.flac"));
        assert!(before_text.contains("Path = Disc 1/Live/02.flac"));
        assert!(
            !before_text.lines().any(|line| line.trim() == "Path = Disc 1"),
            "fixture must omit the synthesized root directory record"
        );

        let pairs = vec![
            ArchiveNativeRenamePair::new("Disc 1/01.flac", "CD 1/01.flac"),
            ArchiveNativeRenamePair::new("Disc 1/cover.jpg", "CD 1/cover.jpg"),
            ArchiveNativeRenamePair::new("Disc 1/Live/02.flac", "CD 1/Live/02.flac"),
        ];
        let members = vec![
            ArchiveNativeMember {
                path: "Disc 1/01.flac".to_string(),
                is_dir: false,
            },
            ArchiveNativeMember {
                path: "Disc 1/cover.jpg".to_string(),
                is_dir: false,
            },
            ArchiveNativeMember {
                path: "Disc 1/Live/02.flac".to_string(),
                is_dir: false,
            },
        ];
        let fingerprint = archive_fingerprint_for_native_edit(&archive).expect("fingerprint");
        let tool_paths = HashMap::from([
            ("7zz".to_string(), seven_zip.clone()),
            ("7z".to_string(), seven_zip.clone()),
        ]);
        let cancel = CancellationToken::new();
        rename_archive_entry_native_transactional(
            &archive,
            &archive,
            &pairs,
            &members,
            fingerprint,
            None,
            &tool_paths,
            &cancel,
            |_| {},
        )
        .await
        .expect("native multi-pair ZIP rename")
        .expect("ZIP native path");

        let after_listing = run_secret_fixture_output(
            &seven_zip,
            &[
                "l".to_string(),
                "-slt".to_string(),
                archive.display().to_string(),
            ],
            None,
        )
        .expect("list renamed ZIP");
        assert!(after_listing.status.success());
        let after_text = String::from_utf8_lossy(&after_listing.stdout);
        assert!(!after_text.contains("Path = Disc 1/"));
        assert!(after_text.contains("Path = CD 1/01.flac"));
        assert!(after_text.contains("Path = CD 1/cover.jpg"));
        assert!(after_text.contains("Path = CD 1/Live/02.flac"));

        let extracted = temp.path().join("renamed-extract");
        extract_seven_zip_archive(&seven_zip, &archive, &extracted)
            .expect("extract renamed ZIP");
        assert_eq!(fs::read(extracted.join("CD 1/01.flac")).unwrap(), b"track-one-payload");
        assert_eq!(fs::read(extracted.join("CD 1/cover.jpg")).unwrap(), b"cover-payload");
        assert_eq!(
            fs::read(extracted.join("CD 1/Live/02.flac")).unwrap(),
            b"track-two-payload"
        );
    }

    #[test]
    fn native_rename_capability_is_format_explicit() {
        let temp = tempfile::tempdir().expect("temp dir");
        let seven_zip = temp.path().join("7zz");
        let xorriso = temp.path().join("xorriso");
        fs::write(&seven_zip, b"tool").expect("fake 7zz");
        fs::write(&xorriso, b"tool").expect("fake xorriso");
        let tool_paths = HashMap::from([
            ("7zz".to_string(), seven_zip),
            ("xorriso".to_string(), xorriso),
        ]);

        assert!(archive_native_rename_available(&temp.path().join("Album.7z"), &tool_paths)
            .expect("7z native rename capability"));
        assert!(archive_native_rename_available(&temp.path().join("Album.zip"), &tool_paths)
            .expect("zip native rename capability"));
        assert!(archive_native_rename_available(&temp.path().join("Album.iso.wv"), &tool_paths)
            .expect("ISO-WV native rename capability"));
        assert!(!archive_native_rename_available(&temp.path().join("Album.tar"), &tool_paths)
            .expect("tar fallback capability"));
        assert!(!archive_native_rename_available(&temp.path().join("Album.rar"), &tool_paths)
            .expect("RAR has no native rename path"));
    }

    #[test]
    fn transactional_native_copy_is_exact_and_precancel_is_side_effect_free() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("source.7z");
        let destination = temp.path().join("destination.7z");
        let payload: Vec<u8> = (0..(1024 * 1024 + 137))
            .map(|index| (index % 251) as u8)
            .collect();
        fs::write(&source, &payload).expect("source payload");
        let cancel = CancellationToken::new();
        let mut observed = Vec::new();

        copy_archive_for_native_edit(
            &source,
            &destination,
            payload.len() as u64,
            &cancel,
            |bytes| observed.push(bytes),
        )
        .expect("transactional copy");

        assert_eq!(fs::read(&destination).expect("copied payload"), payload);
        assert_eq!(observed.last().copied(), Some(payload.len() as u64));

        let cancelled_destination = temp.path().join("cancelled.7z");
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let err = copy_archive_for_native_edit(
            &source,
            &cancelled_destination,
            payload.len() as u64,
            &cancelled,
            |_| {},
        )
        .expect_err("pre-cancelled copy must stop before creating a destination");
        assert_eq!(err, ARCHIVE_REPACKAGE_CANCELLED);
        assert!(!cancelled_destination.exists());
    }

    #[test]
    fn preflight_refuses_rar_writes_without_a_configured_writer_before_extraction_work() {
        let temp = tempfile::tempdir().expect("temp dir");
        let original = temp.path().join("Album.rar");
        fs::write(&original, b"rar placeholder").expect("archive placeholder");

        let tool_paths = HashMap::from([
            ("rar".to_string(), temp.path().join("missing-rar")),
        ]);
        let err = preflight_archive_repackage_capability(&original, &tool_paths)
            .expect_err("RAR mutation must be refused before extraction when no writer is available");

        assert!(
            err.contains("RAR archive creation requires the `rar` executable")
                && err.contains("convert the archive to 7z"),
            "RAR refusal should explain the missing writer without changing formats: {err}"
        );
    }

    #[tokio::test]
    async fn repackage_archive_refuses_rar_without_writer_without_replacing_original() {
        let temp = tempfile::tempdir().expect("temp dir");
        let original = temp.path().join("Album.rar");
        fs::write(&original, b"original rar placeholder").expect("original archive placeholder");
        let staging = write_repackage_staging(temp.path(), "edited rar payload")
            .expect("write repackage staging");

        let tool_paths = HashMap::from([
            ("rar".to_string(), temp.path().join("missing-rar")),
        ]);
        let err = repackage_archive(&staging, &original, &tool_paths)
            .await
            .expect_err("RAR mutation must be refused when no writer is available");

        assert!(
            err.contains("RAR archive creation requires the `rar` executable")
                && err.contains("convert the archive to 7z"),
            "RAR refusal should be actionable: {err}"
        );
        assert_eq!(
            fs::read(&original).expect("original archive after refused RAR repackage"),
            b"original rar placeholder",
            "refused RAR mutation must not replace the original archive"
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
    fn archive_atomic_install_replaces_symlink_entry_not_referent() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("temp dir");
        let referent = temp.path().join("real.zip");
        let original = temp.path().join("Album.zip");
        let temp_archive = temp.path().join("new.zip");
        let backup = temp.path().join("Album.zip.backup");
        fs::write(&referent, b"referent archive").expect("referent archive");
        fs::write(&temp_archive, b"new archive").expect("new archive");
        symlink(&referent, &original).expect("archive symlink");

        replace_archive_atomically(&original, &temp_archive, &backup, None)
            .expect("install through lexical archive entry");

        assert_eq!(fs::read(&referent).unwrap(), b"referent archive");
        assert_eq!(fs::read(&original).unwrap(), b"new archive");
        assert!(
            !fs::symlink_metadata(&original).unwrap().file_type().is_symlink(),
            "archive publication must replace the symlink entry itself"
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
                artist: Some("Miles Davis".to_string()).into(),
                album_artist: Some("Miles Davis".to_string()).into(),
                genre: Some("Jazz".to_string()).into(),
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
                artist: Some("Miles Davis".to_string()).into(),
                album_artist: Some("Miles Davis".to_string()).into(),
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
