use chrono::Utc;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub const PARTIAL_SUFFIX: &str = ".tonepoet.partial";
pub const VALIDATED_SUFFIX: &str = ".tonepoet.validated";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionalTrackPaths {
    pub final_staged_path: PathBuf,
    pub partial_path: PathBuf,
    pub validated_path: PathBuf,
}

pub fn transactional_track_paths(final_staged_path: impl AsRef<Path>) -> TransactionalTrackPaths {
    let final_staged_path = final_staged_path.as_ref().to_path_buf();
    TransactionalTrackPaths {
        partial_path: append_suffix(&final_staged_path, PARTIAL_SUFFIX),
        validated_path: append_suffix(&final_staged_path, VALIDATED_SUFFIX),
        final_staged_path,
    }
}

pub fn begin_track_output(final_staged_path: &Path) -> io::Result<TransactionalTrackPaths> {
    let paths = transactional_track_paths(final_staged_path);
    if let Some(parent) = paths.final_staged_path.parent() {
        fs::create_dir_all(parent)?;
    }
    cleanup_track_state(&paths)?;
    Ok(paths)
}

pub fn mark_track_validated(paths: &TransactionalTrackPaths) -> io::Result<()> {
    sync_file_best_effort(&paths.partial_path);
    rename_replace(&paths.partial_path, &paths.validated_path)?;
    sync_parent_dir_best_effort(&paths.validated_path);
    Ok(())
}

pub fn materialize_validated_final(paths: &TransactionalTrackPaths) -> io::Result<()> {
    sync_file_best_effort(&paths.validated_path);
    rename_replace(&paths.validated_path, &paths.final_staged_path)?;
    sync_parent_dir_best_effort(&paths.final_staged_path);
    Ok(())
}

/// Publish a validated file to a final path without ever writing bytes directly
/// into the final path. Cross-device fallback copies into a hidden temp file in
/// the final directory, fsyncs it, then atomically renames temp -> final.
pub fn publish_validated_to_final(validated_path: &Path, final_path: &Path) -> io::Result<()> {
    publish_validated_to_final_checked(validated_path, final_path, || Ok(()))
}

/// The same atomic publisher with a cooperative checkpoint. Native extraction
/// uses this hook at each 64 KiB cross-volume copy boundary to honor pause,
/// skip-current, and abort without exposing a partially published filename.
/// Ordinary conversion publication uses the no-op checkpoint above.
pub fn publish_validated_to_final_checked(
    validated_path: &Path,
    final_path: &Path,
    checkpoint: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    publish_validated_to_final_checked_observed(
        validated_path, final_path, checkpoint, None, |_| Ok(()),
    )
}

/// Optional, extraction-only observation of cross-device publication. The
/// provided private name is journal-owned and is recorded *before* creation.
/// `on_temporary(None)` is called only once a temporary path is gone; failed
/// deletion leaves the durable recovery obligation in place. Convert passes
/// no observer through the original entrypoint and retains its old filenames.
pub(crate) fn publish_validated_to_final_checked_observed(
    validated_path: &Path,
    final_path: &Path,
    mut checkpoint: impl FnMut() -> io::Result<()>,
    private_temp: Option<PathBuf>,
    on_temporary: impl FnMut(Option<&Path>) -> io::Result<()>,
) -> io::Result<()> {
    checkpoint()?;
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent)?;
    }
    checkpoint()?;
    match fs::rename(validated_path, final_path) {
        Ok(()) => {
            sync_parent_dir_best_effort(final_path);
            Ok(())
        }
        Err(err) if is_cross_device_error(&err) =>
            copy_across_devices_atomically_checked_observed(
                validated_path, final_path, checkpoint, private_temp, on_temporary),
        Err(err) => Err(err),
    }
}

pub fn cleanup_track_state(paths: &TransactionalTrackPaths) -> io::Result<()> {
    remove_file_if_exists(&paths.partial_path)?;
    remove_file_if_exists(&paths.validated_path)?;
    remove_file_if_exists(&paths.final_staged_path)?;
    Ok(())
}

/// Only deletes state paths derived from known final staged paths. It does not
/// walk arbitrary directories by suffix, so user files such as `notes.partial`
/// or `take.validated` are not touched.
pub fn delete_stale_transactional_track_states<I, P>(final_staged_paths: I) -> io::Result<Vec<PathBuf>>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut deleted = Vec::new();
    for final_staged_path in final_staged_paths {
        let paths = transactional_track_paths(final_staged_path.as_ref());
        if remove_file_if_exists_with_signal(&paths.partial_path)? {
            deleted.push(paths.partial_path);
        }
        if remove_file_if_exists_with_signal(&paths.validated_path)? {
            deleted.push(paths.validated_path);
        }
    }
    Ok(deleted)
}

pub fn is_transactional_state_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    name.ends_with(PARTIAL_SUFFIX) || name.ends_with(VALIDATED_SUFFIX)
}

fn copy_across_devices_atomically_checked_observed(
    src: &Path,
    dst: &Path,
    mut checkpoint: impl FnMut() -> io::Result<()>,
    private_temp: Option<PathBuf>,
    mut on_temporary: impl FnMut(Option<&Path>) -> io::Result<()>,
) -> io::Result<()> {
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let file_name = dst.file_name().and_then(|s| s.to_str()).unwrap_or("tonepoet-output");
    // Only extraction carries a durable temp identity. Ordinary Convert
    // preserves its historical publication/cleanup error contract.
    let journaled_native_temp = private_temp.is_some();
    let tmp = private_temp.unwrap_or_else(|| parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    )));
    if tmp.parent() != Some(parent) || tmp == dst {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "publication temporary path leaves destination parent"));
    }
    on_temporary(Some(&tmp))?;
    let mut created = false;
    let copy_result = (|| {
        let mut in_file = File::open(src)?;
        let mut out_file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        created = true;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            checkpoint()?;
            let n = in_file.read(&mut buffer)?;
            if n == 0 { break; }
            out_file.write_all(&buffer[..n])?;
        }
        out_file.sync_all()?;
        checkpoint()?;
        drop(out_file);
        fs::rename(&tmp, dst)?;
        sync_parent_dir_best_effort(dst);
        if journaled_native_temp {
            // The public filename already contains the complete, synced data.
            // Recovery owns this exact publication, so a failure to remove
            // private source staging must not report the output as missing.
            let _ = fs::remove_file(src);
        } else {
            // The pre-existing Convert path keeps its original failure
            // propagation semantics; native extraction alone opts in above.
            fs::remove_file(src)?;
        }
        Ok(())
    })();
    if copy_result.is_err() && created {
        match fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return copy_result, // retain the recovery record
        }
    }
    // This path is either published or provably not created by this call.
    // An unsuccessful cleanup retains the journaled temporary for Recovery.
    if let Err(error) = on_temporary(None) {
        return Err(error);
    }
    copy_result
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return path.with_extension(suffix.trim_start_matches('.'));
    };
    path.with_file_name(format!("{name}{suffix}"))
}

fn rename_replace(from: &Path, to: &Path) -> io::Result<()> {
    remove_file_if_exists(to)?;
    fs::rename(from, to)
}

fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    let _ = remove_file_if_exists_with_signal(path)?;
    Ok(())
}

fn remove_file_if_exists_with_signal(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

fn sync_file_best_effort(path: &Path) {
    if let Ok(file) = File::open(path) {
        let _ = file.sync_all();
    }
}

fn sync_parent_dir_best_effort(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
}

fn is_cross_device_error(err: &io::Error) -> bool {
    if err.kind() == io::ErrorKind::CrossesDevices { return true; }
    #[cfg(unix)]
    { err.raw_os_error() == Some(libc::EXDEV) }
    #[cfg(not(unix))]
    { false }
}

#[cfg(test)]
mod native_publication_checkpoint_tests {
    use super::*;

    #[test]
    fn stopped_publication_never_exposes_an_output_filename() {
        let temporary = tempfile::tempdir().expect("private output root");
        let source = temporary.path().join("completed.dsf");
        let target = temporary.path().join("destination").join("selected.dsf");
        fs::write(&source, b"complete private media").expect("private media");
        let result = publish_validated_to_final_checked(&source, &target, || {
            Err(io::Error::new(io::ErrorKind::Interrupted, "skip current"))
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert!(source.is_file(), "private completed media remains until owner cleans it");
        assert!(!target.exists(), "aborted item is never published");
    }
}
