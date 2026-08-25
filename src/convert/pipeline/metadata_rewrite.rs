//! Atomic same-directory replacement for external metadata mutators.

#![allow(unsafe_code)]

use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{fs, io};

fn metadata_rewrite_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[derive(Debug)]
pub(super) struct MetadataRewriteTemp {
    path: PathBuf,
    original_attributes: MetadataRewriteAttributes,
}

impl MetadataRewriteTemp {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn cleanup_best_effort(&self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl Drop for MetadataRewriteTemp {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
struct MetadataRewriteAttributes {
    permissions: fs::Permissions,
    accessed: SystemTime,
    modified: SystemTime,
    len: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
    #[cfg(unix)]
    change_time_seconds: i64,
    #[cfg(unix)]
    change_time_nanoseconds: i64,
    #[cfg(target_os = "linux")]
    xattrs: Vec<(std::ffi::OsString, Vec<u8>)>,
}

impl MetadataRewriteAttributes {
    fn capture(path: &Path) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "metadata rewrite requires an existing regular file, not a symlink or special file: {}",
                    path.display()
                ),
            ));
        }

        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;

        Ok(Self {
            permissions: metadata.permissions(),
            accessed: metadata.accessed()?,
            modified: metadata.modified()?,
            len: metadata.len(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            uid: metadata.uid(),
            #[cfg(unix)]
            gid: metadata.gid(),
            #[cfg(unix)]
            change_time_seconds: metadata.ctime(),
            #[cfg(unix)]
            change_time_nanoseconds: metadata.ctime_nsec(),
            #[cfg(target_os = "linux")]
            xattrs: linux_xattrs(path)?,
        })
    }

    fn verify_source_unchanged(&self, path: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "metadata rewrite target changed type before replacement: {}",
                    path.display()
                ),
            ));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
            if metadata.dev() != self.device
                || metadata.ino() != self.inode
                || metadata.uid() != self.uid
                || metadata.gid() != self.gid
                || metadata.permissions().mode() != self.permissions.mode()
                || metadata.ctime() != self.change_time_seconds
                || metadata.ctime_nsec() != self.change_time_nanoseconds
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "metadata rewrite target identity or attributes changed before replacement: {}",
                        path.display()
                    ),
                ));
            }
        }
        #[cfg(not(unix))]
        if metadata.permissions().readonly() != self.permissions.readonly() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "metadata rewrite target permissions changed before replacement: {}",
                    path.display()
                ),
            ));
        }

        if metadata.len() != self.len || metadata.modified()? != self.modified {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "metadata rewrite target content changed before replacement: {}",
                    path.display()
                ),
            ));
        }
        #[cfg(target_os = "linux")]
        if linux_xattrs(path)? != self.xattrs {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "metadata rewrite target extended attributes changed before replacement: {}",
                    path.display()
                ),
            ));
        }

        Ok(())
    }

    fn apply_and_verify(&self, path: &Path) -> io::Result<()> {
        let file = fs::OpenOptions::new().read(true).write(true).open(path)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let current = file.metadata()?;
            if current.uid() != self.uid || current.gid() != self.gid {
                std::os::unix::fs::chown(path, Some(self.uid), Some(self.gid))?;
            }
        }

        file.set_permissions(self.permissions.clone())?;
        #[cfg(target_os = "linux")]
        set_linux_xattrs_exact(path, &self.xattrs)?;
        file.set_times(
            fs::FileTimes::new()
                .set_accessed(self.accessed)
                .set_modified(self.modified),
        )?;
        file.sync_all()?;
        self.verify_applied(path)
    }

    fn verify_applied(&self, path: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "metadata rewrite temporary output changed type before replacement: {}",
                    path.display()
                ),
            ));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
            if metadata.uid() != self.uid
                || metadata.gid() != self.gid
                || metadata.permissions().mode() != self.permissions.mode()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "metadata rewrite could not preserve ownership or permission bits: {}",
                        path.display()
                    ),
                ));
            }
        }
        #[cfg(not(unix))]
        if metadata.permissions().readonly() != self.permissions.readonly() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "metadata rewrite could not preserve permissions: {}",
                    path.display()
                ),
            ));
        }

        if metadata.accessed()? != self.accessed || metadata.modified()? != self.modified {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "metadata rewrite could not preserve access/modification timestamps: {}",
                    path.display()
                ),
            ));
        }
        #[cfg(target_os = "linux")]
        if linux_xattrs(path)? != self.xattrs {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "metadata rewrite could not preserve extended attributes or POSIX ACLs: {}",
                    path.display()
                ),
            ));
        }

        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn linux_path_cstring(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path contains an interior NUL byte: {}", path.display()),
        )
    })
}

#[cfg(target_os = "linux")]
fn linux_xattrs(path: &Path) -> io::Result<Vec<(std::ffi::OsString, Vec<u8>)>> {
    use std::os::unix::ffi::OsStringExt as _;

    let path_c = linux_path_cstring(path)?;
    let names = loop {
        let len = unsafe { libc::llistxattr(path_c.as_ptr(), std::ptr::null_mut(), 0) };
        if len < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut names = vec![0_u8; usize::try_from(len).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "extended-attribute list is too large")
        })?];
        if names.is_empty() {
            break names;
        }
        let got = unsafe {
            libc::llistxattr(
                path_c.as_ptr(),
                names.as_mut_ptr().cast::<libc::c_char>(),
                names.len(),
            )
        };
        if got < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ERANGE) {
                continue;
            }
            return Err(error);
        }
        names.truncate(usize::try_from(got).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "extended-attribute list is too large")
        })?);
        break names;
    };

    let mut out = Vec::new();
    for raw_name in names.split(|byte| *byte == 0).filter(|name| !name.is_empty()) {
        let name_c = std::ffi::CString::new(raw_name).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "extended-attribute name contains an interior NUL byte",
            )
        })?;
        let value = loop {
            let len = unsafe {
                libc::lgetxattr(
                    path_c.as_ptr(),
                    name_c.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                )
            };
            if len < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut value = vec![0_u8; usize::try_from(len).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "extended-attribute value is too large")
            })?];
            if value.is_empty() {
                break value;
            }
            let got = unsafe {
                libc::lgetxattr(
                    path_c.as_ptr(),
                    name_c.as_ptr(),
                    value.as_mut_ptr().cast::<libc::c_void>(),
                    value.len(),
                )
            };
            if got < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ERANGE) {
                    continue;
                }
                return Err(error);
            }
            value.truncate(usize::try_from(got).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "extended-attribute value is too large")
            })?);
            break value;
        };
        out.push((std::ffi::OsString::from_vec(raw_name.to_vec()), value));
    }
    out.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(out)
}

#[cfg(target_os = "linux")]
fn set_linux_xattrs_exact(
    path: &Path,
    expected: &[(std::ffi::OsString, Vec<u8>)],
) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;

    let path_c = linux_path_cstring(path)?;
    let current = linux_xattrs(path)?;
    for (name, _) in &current {
        if expected.iter().any(|(candidate, _)| candidate == name) {
            continue;
        }
        let name_c = std::ffi::CString::new(name.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "extended-attribute name contains an interior NUL byte",
            )
        })?;
        let result = unsafe { libc::lremovexattr(path_c.as_ptr(), name_c.as_ptr()) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    for (name, value) in expected {
        if current
            .iter()
            .any(|(current_name, current_value)| current_name == name && current_value == value)
        {
            continue;
        }
        let name_c = std::ffi::CString::new(name.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "extended-attribute name contains an interior NUL byte",
            )
        })?;
        let result = unsafe {
            libc::lsetxattr(
                path_c.as_ptr(),
                name_c.as_ptr(),
                value.as_ptr().cast::<libc::c_void>(),
                value.len(),
                0,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    if linux_xattrs(path)? != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "extended-attribute verification failed for metadata rewrite temporary output: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

pub(super) fn metadata_rewrite_temp_path(path: &Path) -> io::Result<MetadataRewriteTemp> {
    let original_attributes = MetadataRewriteAttributes::capture(path)?;
    let parent = metadata_rewrite_parent(path);
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("audio");
    let ext = path.extension().and_then(|value| value.to_str()).unwrap_or("tmp");
    let prefix = format!(".{file_name}.tonepoet-metadata.");
    let suffix = format!(".tmp.{ext}");
    let temp = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(&suffix)
        .tempfile_in(parent)?;
    let path = temp.into_temp_path().keep().map_err(|err| err.error)?;
    Ok(MetadataRewriteTemp {
        path,
        original_attributes,
    })
}

pub(super) fn sync_parent_dir(path: &Path) -> io::Result<()> {
    let parent = metadata_rewrite_parent(path);
    let dir = fs::File::open(parent)?;
    match dir.sync_all() {
        Ok(()) => Ok(()),
        // Directory fsync is unsupported on descriptor-namespace routes
        // (/proc/self/fd/N parents) and some special filesystems; real
        // directories never report these errnos. Same tolerance as
        // cap_fs::directory_sync_unsupported. (ENOTSUP aliases EOPNOTSUPP on
        // Linux, hence the boolean form rather than a pattern.)
        Err(error)
            if error.raw_os_error().is_some_and(|code| {
                code == libc::EINVAL || code == libc::ENOTSUP || code == libc::EOPNOTSUPP
            }) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Atomically replace a rewritten staging output while preserving the original
/// file's governed attributes. The snapshot is captured before the external
/// mutator reads the target. Replacement fails closed if the target identity,
/// content metadata, or Linux xattrs/ACLs change while the mutator runs.
///
/// The portable contract preserves permission state and access/modification
/// timestamps. Unix builds also preserve uid/gid. Linux builds preserve and
/// verify the complete xattr set, including the POSIX ACL access xattr.
pub(super) fn replace_rewritten_metadata_file(
    path: &Path,
    tmp: MetadataRewriteTemp,
) -> io::Result<()> {
    if !tmp.path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "metadata rewrite did not create temporary file: {}",
                tmp.path.display()
            ),
        ));
    }
    let metadata = fs::symlink_metadata(&tmp.path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "metadata rewrite produced an empty, symlinked, or non-file temporary output: {}",
                tmp.path.display()
            ),
        ));
    }
    let original_attributes = &tmp.original_attributes;
    original_attributes.verify_source_unchanged(path)?;
    original_attributes.apply_and_verify(&tmp.path)?;
    // Attribute application and fsync may be nontrivial on files with ACLs or
    // many xattrs. Revalidate immediately before publication so a substitution
    // during that work is rejected rather than replacing the newer target.
    original_attributes.verify_source_unchanged(path)?;

    // The rewrite temp is created in the same directory as the target, so this
    // rename is same-filesystem. On POSIX platforms it atomically replaces an
    // existing target and never exposes a target-absent window.
    fs::rename(&tmp.path, path)?;
    sync_parent_dir(path)?;
    original_attributes.verify_applied(path)
}

/// One staged member of an all-or-nothing metadata-carrier batch.
///
/// Existing targets are cloned/copied into a same-directory staging file and
/// retain a full source-attribute snapshot for TOCTOU revalidation. Missing
/// targets are represented by a create-only staging file. No authoritative
/// carrier is modified until `commit_metadata_rewrite_batch` is called.
#[derive(Debug)]
pub(crate) struct MetadataRewriteBatchStage {
    target: PathBuf,
    staged: PathBuf,
    original_attributes: Option<MetadataRewriteAttributes>,
    /// A failed rollback can leave the only recoverable original at the
    /// private stage path. Never unlink that evidence on Drop.
    preserve_staged_on_drop: bool,
}

impl MetadataRewriteBatchStage {
    pub(crate) fn staged_path(&self) -> &Path {
        &self.staged
    }
}

impl Drop for MetadataRewriteBatchStage {
    fn drop(&mut self) {
        if !self.preserve_staged_on_drop {
            let _ = fs::remove_file(&self.staged);
        }
    }
}

fn metadata_batch_temp_path(target: &Path, purpose: &str) -> io::Result<PathBuf> {
    let parent = metadata_rewrite_parent(target);
    fs::create_dir_all(parent)?;
    let file_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("carrier");
    let ext = target
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("tmp");
    let prefix = format!(".{file_name}.tonepoet-{purpose}.");
    let suffix = format!(".tmp.{ext}");
    let temp = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(&suffix)
        .tempfile_in(parent)?;
    temp.into_temp_path().keep().map_err(|err| err.error)
}

fn clone_or_copy_file(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd as _;
        let source_file = fs::OpenOptions::new().read(true).open(source)?;
        let destination_file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(destination)?;
        // Linux FICLONE. A reflink keeps Album-view staging effectively O(1)
        // on CoW filesystems; unsupported filesystems fall back to a byte copy.
        const FICLONE_IOCTL: libc::c_ulong = 0x4004_9409;
        let cloned = unsafe {
            libc::ioctl(
                destination_file.as_raw_fd(),
                FICLONE_IOCTL,
                source_file.as_raw_fd(),
            )
        } == 0;
        drop(destination_file);
        drop(source_file);
        if cloned {
            return Ok(());
        }
    }
    fs::copy(source, destination).map(|_| ())
}

/// Stage an existing regular carrier without changing it.
pub(crate) fn stage_existing_metadata_batch_file(
    target: &Path,
) -> io::Result<MetadataRewriteBatchStage> {
    let metadata = fs::metadata(target)?;
    if metadata.permissions().readonly() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("metadata batch target is read-only: {}", target.display()),
        ));
    }
    let original_attributes = MetadataRewriteAttributes::capture(target)?;
    let staged = metadata_batch_temp_path(target, "metadata-stage")?;
    if let Err(error) = clone_or_copy_file(target, &staged) {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    // Detect substitution/content change while the staging snapshot was made.
    if let Err(error) = original_attributes.verify_source_unchanged(target) {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    Ok(MetadataRewriteBatchStage {
        target: target.to_path_buf(),
        staged,
        original_attributes: Some(original_attributes),
        preserve_staged_on_drop: false,
    })
}

/// Stage a carrier that must still be absent at commit time.
pub(crate) fn stage_new_metadata_batch_file(
    target: &Path,
) -> io::Result<MetadataRewriteBatchStage> {
    match fs::symlink_metadata(target) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "metadata batch create target appeared before staging: {}",
                    target.display()
                ),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let staged = metadata_batch_temp_path(target, "metadata-create")?;
    // New-carrier mutators use create-only semantics themselves. Keep the
    // reserved same-directory name, but make it absent until the mutator
    // materializes the staged carrier.
    fs::remove_file(&staged)?;
    Ok(MetadataRewriteBatchStage {
        target: target.to_path_buf(),
        staged,
        original_attributes: None,
        preserve_staged_on_drop: false,
    })
}

fn validate_staged_batch_file(stage: &MetadataRewriteBatchStage) -> io::Result<()> {
    let metadata = fs::symlink_metadata(&stage.staged)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "metadata batch staged output is empty, symlinked, or not a regular file: {}",
                stage.staged.display()
            ),
        ));
    }
    let file = fs::OpenOptions::new().read(true).write(true).open(&stage.staged)?;
    file.sync_all()
}

#[cfg(target_os = "linux")]
fn linux_renameat2(old: &Path, new: &Path, flags: libc::c_uint) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
        std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("path contains an interior NUL byte: {}", path.display()),
            )
        })
    }
    let old = c_path(old)?;
    let new = c_path(new)?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            old.as_ptr(),
            libc::AT_FDCWD,
            new.as_ptr(),
            flags,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn rollback_linux_metadata_batch(
    stages: &mut [MetadataRewriteBatchStage],
    committed: &[usize],
) -> Vec<String> {
    const RENAME_EXCHANGE: libc::c_uint = 2;
    let mut failures = Vec::new();
    for index in committed.iter().rev().copied() {
        let stage = &mut stages[index];
        let result = if stage.original_attributes.is_some() {
            linux_renameat2(&stage.target, &stage.staged, RENAME_EXCHANGE)
        } else {
            fs::rename(&stage.target, &stage.staged)
        };
        if let Err(error) = result {
            // For an existing target the stage path still contains the
            // pre-transaction original after the failed exchange. Preserve it
            // rather than letting Drop destroy the last recovery copy.
            stage.preserve_staged_on_drop = true;
            failures.push(format!("{}: rollback rename failed: {error}", stage.target.display()));
        }
    }

    // A rollback is not complete merely because the namespace looks restored
    // in page cache. Sync every parent touched by a committed member before
    // reporting the failed transaction back to the editor.
    let mut synced_parents = std::collections::BTreeSet::new();
    for index in committed.iter().copied() {
        let stage = &stages[index];
        let parent = metadata_rewrite_parent(&stage.target).to_path_buf();
        if synced_parents.insert(parent) {
            if let Err(error) = sync_parent_dir(&stage.target) {
                failures.push(format!(
                    "{}: rollback directory sync failed: {error}",
                    stage.target.display()
                ));
            }
        }
    }
    failures
}

#[cfg(test)]
thread_local! {
    static METADATA_BATCH_FAIL_AFTER_COMMIT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn metadata_batch_injected_commit_failure(committed_count: usize) -> Option<io::Error> {
    METADATA_BATCH_FAIL_AFTER_COMMIT.with(|slot| {
        (slot.get() == Some(committed_count)).then(|| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("injected metadata batch failure after {committed_count} commit(s)"),
            )
        })
    })
}

#[cfg(not(test))]
fn metadata_batch_injected_commit_failure(_committed_count: usize) -> Option<io::Error> {
    None
}

#[cfg(target_os = "linux")]
fn commit_metadata_rewrite_batch_linux(
    stages: &mut [MetadataRewriteBatchStage],
) -> io::Result<()> {
    const RENAME_NOREPLACE: libc::c_uint = 1;
    const RENAME_EXCHANGE: libc::c_uint = 2;
    let mut committed = Vec::with_capacity(stages.len());
    for index in 0..stages.len() {
        let target = stages[index].target.clone();
        let result = {
            let stage = &stages[index];
            if stage.original_attributes.is_some() {
                linux_renameat2(&stage.target, &stage.staged, RENAME_EXCHANGE)
            } else {
                linux_renameat2(&stage.staged, &stage.target, RENAME_NOREPLACE)
            }
        };
        if let Err(error) = result {
            let rollback = rollback_linux_metadata_batch(stages, &committed);
            let suffix = if rollback.is_empty() {
                String::new()
            } else {
                format!("; rollback incomplete: {}", rollback.join("; "))
            };
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "metadata batch commit failed at '{}': {error}{suffix}",
                    target.display()
                ),
            ));
        }
        committed.push(index);
        if let Some(error) = metadata_batch_injected_commit_failure(committed.len()) {
            let rollback = rollback_linux_metadata_batch(stages, &committed);
            let suffix = if rollback.is_empty() {
                String::new()
            } else {
                format!("; rollback incomplete: {}", rollback.join("; "))
            };
            return Err(io::Error::new(
                error.kind(),
                format!("{error}{suffix}"),
            ));
        }
    }

    // Verify the published carriers while the swapped-out originals are still
    // available at each stage path for rollback.
    for index in 0..stages.len() {
        let target = stages[index].target.clone();
        let verify = {
            let stage = &stages[index];
            if let Some(attributes) = stage.original_attributes.as_ref() {
                attributes.verify_applied(&stage.target)
            } else {
                validate_staged_batch_file_at_target(&stage.target)
            }
        };
        if let Err(error) = verify {
            let rollback = rollback_linux_metadata_batch(stages, &committed);
            let suffix = if rollback.is_empty() {
                String::new()
            } else {
                format!("; rollback incomplete: {}", rollback.join("; "))
            };
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "metadata batch post-commit verification failed for '{}': {error}{suffix}",
                    target.display()
                ),
            ));
        }
    }

    // Parent-directory durability is part of success.  Keep the exchanged
    // originals at their private stage names until every distinct parent has
    // been synced so even a late durability failure can still roll the whole
    // carrier set back.
    let mut synced_parents = std::collections::BTreeSet::new();
    for index in 0..stages.len() {
        let target = stages[index].target.clone();
        let parent = metadata_rewrite_parent(&target).to_path_buf();
        if synced_parents.insert(parent) {
            if let Err(error) = sync_parent_dir(&target) {
                let rollback = rollback_linux_metadata_batch(stages, &committed);
                let suffix = if rollback.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; rollback incomplete or durability uncertain: {}",
                        rollback.join("; ")
                    )
                };
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "metadata batch directory sync failed for '{}': {error}{suffix}",
                        target.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn validate_staged_batch_file_at_target(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("metadata batch published invalid file: {}", path.display()),
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn rollback_portable_metadata_batch(
    stages: &[MetadataRewriteBatchStage],
    committed: &[usize],
    backups: &[(usize, PathBuf)],
) -> Vec<String> {
    let mut failures = Vec::new();
    for committed_index in committed.iter().rev().copied() {
        let committed_stage = &stages[committed_index];
        let rollback = if committed_stage.original_attributes.is_some() {
            let backup = backups
                .iter()
                .find(|(idx, _)| *idx == committed_index)
                .map(|(_, path)| path);
            match backup {
                Some(backup) => fs::rename(backup, &committed_stage.target),
                None => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "metadata batch rollback backup missing",
                )),
            }
        } else {
            fs::remove_file(&committed_stage.target)
        };
        if let Err(error) = rollback {
            failures.push(format!(
                "{}: rollback restore failed: {error}",
                committed_stage.target.display()
            ));
        }
    }

    let mut synced_parents = std::collections::BTreeSet::new();
    for committed_index in committed.iter().copied() {
        let stage = &stages[committed_index];
        let parent = metadata_rewrite_parent(&stage.target).to_path_buf();
        if synced_parents.insert(parent) {
            if let Err(error) = sync_parent_dir(&stage.target) {
                failures.push(format!(
                    "{}: rollback directory sync failed: {error}",
                    stage.target.display()
                ));
            }
        }
    }
    failures
}

#[cfg(not(target_os = "linux"))]
fn cleanup_portable_metadata_backups(
    backups: &[(usize, PathBuf)],
    rollback_failures: &[String],
) {
    // If rollback was incomplete, retain every backup. A little private-file
    // litter is strictly preferable to deleting the only recoverable original.
    if !rollback_failures.is_empty() {
        return;
    }
    for (_, backup) in backups {
        let _ = fs::remove_file(backup);
    }
}

#[cfg(not(target_os = "linux"))]
fn commit_metadata_rewrite_batch_portable(
    stages: &mut [MetadataRewriteBatchStage],
) -> io::Result<()> {
    let mut backups = Vec::<(usize, PathBuf)>::new();
    // Prepare complete rollback material before touching any target.
    for (index, stage) in stages.iter().enumerate() {
        let Some(attributes) = stage.original_attributes.as_ref() else {
            continue;
        };
        let backup = metadata_batch_temp_path(&stage.target, "metadata-backup")?;
        clone_or_copy_file(&stage.target, &backup)?;
        attributes.apply_and_verify(&backup)?;
        backups.push((index, backup));
    }
    for stage in stages.iter() {
        if let Some(attributes) = stage.original_attributes.as_ref() {
            attributes.verify_source_unchanged(&stage.target)?;
        } else if fs::symlink_metadata(&stage.target).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("metadata batch create target appeared: {}", stage.target.display()),
            ));
        }
    }

    let mut committed = Vec::new();
    for index in 0..stages.len() {
        let stage = &stages[index];
        let result = if stage.original_attributes.is_some() {
            fs::rename(&stage.staged, &stage.target)
        } else {
            // A hard link gives create-only semantics without a check/rename
            // race. The staging link remains available until success.
            fs::hard_link(&stage.staged, &stage.target)
        };
        if let Err(error) = result {
            let rollback_failures =
                rollback_portable_metadata_batch(stages, &committed, &backups);
            cleanup_portable_metadata_backups(&backups, &rollback_failures);
            let suffix = if rollback_failures.is_empty() {
                String::new()
            } else {
                format!("; rollback incomplete: {}", rollback_failures.join("; "))
            };
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "metadata batch commit failed at '{}': {error}{suffix}",
                    stage.target.display()
                ),
            ));
        }
        committed.push(index);
        if let Some(error) = metadata_batch_injected_commit_failure(committed.len()) {
            let rollback_failures =
                rollback_portable_metadata_batch(stages, &committed, &backups);
            cleanup_portable_metadata_backups(&backups, &rollback_failures);
            let suffix = if rollback_failures.is_empty() {
                String::new()
            } else {
                format!("; rollback incomplete: {}", rollback_failures.join("; "))
            };
            return Err(io::Error::new(error.kind(), format!("{error}{suffix}")));
        }
    }
    // As on Linux, directory durability belongs to the transactional success
    // boundary.  Keep rollback copies until every parent has synced.
    let mut synced_parents = std::collections::BTreeSet::new();
    for stage in stages.iter() {
        let parent = metadata_rewrite_parent(&stage.target).to_path_buf();
        if synced_parents.insert(parent) {
            if let Err(error) = sync_parent_dir(&stage.target) {
                let rollback_failures =
                    rollback_portable_metadata_batch(stages, &committed, &backups);
                cleanup_portable_metadata_backups(&backups, &rollback_failures);
                let suffix = if rollback_failures.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; rollback incomplete or durability uncertain: {}",
                        rollback_failures.join("; ")
                    )
                };
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "metadata batch directory sync failed for '{}': {error}{suffix}",
                        stage.target.display()
                    ),
                ));
            }
        }
    }
    cleanup_portable_metadata_backups(&backups, &[]);
    Ok(())
}

/// Publish a fully staged carrier set transactionally.
///
/// Every existing source is revalidated immediately before the first commit.
/// Linux uses `renameat2(RENAME_EXCHANGE)` so each replacement is individually
/// atomic and the original remains available for rollback. Other platforms
/// pre-stage rollback copies before committing. A failure rolls all previously
/// published members back before returning.
pub(crate) fn commit_metadata_rewrite_batch(
    mut stages: Vec<MetadataRewriteBatchStage>,
) -> io::Result<()> {
    if stages.is_empty() {
        return Ok(());
    }
    let mut targets = std::collections::BTreeSet::new();
    for stage in &stages {
        if !targets.insert(stage.target.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("duplicate metadata batch target: {}", stage.target.display()),
            ));
        }
        validate_staged_batch_file(stage)?;
        if let Some(attributes) = stage.original_attributes.as_ref() {
            attributes.apply_and_verify(&stage.staged)?;
        }
    }
    // Revalidate the complete source set after all staging/mutation work and
    // before the first authoritative carrier is replaced.
    for stage in &stages {
        if let Some(attributes) = stage.original_attributes.as_ref() {
            attributes.verify_source_unchanged(&stage.target)?;
        } else {
            match fs::symlink_metadata(&stage.target) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!(
                            "metadata batch create target appeared before commit: {}",
                            stage.target.display()
                        ),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }

    #[cfg(target_os = "linux")]
    commit_metadata_rewrite_batch_linux(&mut stages)?;
    #[cfg(not(target_os = "linux"))]
    commit_metadata_rewrite_batch_portable(&mut stages)?;

    // On Linux existing stages now contain the original carrier; on portable
    // new-carrier stages may remain as the second hard link. Both platform
    // implementations keep rollback material until parent-directory sync has
    // succeeded, then Drop removes only the private artifacts.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    struct MetadataBatchCommitFailureGuard;

    impl MetadataBatchCommitFailureGuard {
        fn after(committed_count: usize) -> Self {
            METADATA_BATCH_FAIL_AFTER_COMMIT.with(|slot| {
                assert!(slot.get().is_none(), "metadata batch failure injection already armed");
                slot.set(Some(committed_count));
            });
            Self
        }
    }

    impl Drop for MetadataBatchCommitFailureGuard {
        fn drop(&mut self) {
            METADATA_BATCH_FAIL_AFTER_COMMIT.with(|slot| slot.set(None));
        }
    }

    #[test]
    fn temp_allocation_requires_an_existing_regular_target() {
        let dir = tempfile::tempdir().expect("metadata rewrite tempdir");
        let missing = dir.path().join("missing.mp3");
        let missing_error = metadata_rewrite_temp_path(&missing)
            .expect_err("missing metadata target must be rejected before temp allocation");
        assert_eq!(missing_error.kind(), io::ErrorKind::NotFound);

        let directory = dir.path().join("directory.mp3");
        fs::create_dir(&directory).expect("create non-file target");
        let directory_error = metadata_rewrite_temp_path(&directory)
            .expect_err("non-file metadata target must be rejected");
        assert_eq!(directory_error.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    #[test]
    fn temp_allocation_rejects_symlink_targets() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("metadata rewrite tempdir");
        let real = dir.path().join("real.mp3");
        let link = dir.path().join("link.mp3");
        fs::write(&real, b"source audio").expect("write symlink target");
        symlink(&real, &link).expect("create metadata symlink");
        let error = metadata_rewrite_temp_path(&link)
            .expect_err("metadata rewrite must not follow target symlinks");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn replacement_is_atomic_and_rejects_empty_output() {
        let dir = tempfile::tempdir().expect("metadata rewrite tempdir");
        let target = dir.path().join("track.mp3");
        fs::write(&target, b"old audio").expect("write old target");
        let tmp = metadata_rewrite_temp_path(&target).expect("temp path");
        fs::write(tmp.path(), b"new audio").expect("write rewritten temp");
        replace_rewritten_metadata_file(&target, tmp).expect("replace rewritten file");
        assert_eq!(fs::read(&target).expect("read rewritten target"), b"new audio");

        let empty = metadata_rewrite_temp_path(&target).expect("empty temp path");
        let error = replace_rewritten_metadata_file(&target, empty)
            .expect_err("empty rewrite output must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&target).expect("target remains"), b"new audio");
    }

    #[test]
    fn metadata_batch_commit_publishes_existing_and_new_carriers_together() {
        let dir = tempfile::tempdir().expect("metadata batch tempdir");
        let existing = dir.path().join("side-a.cue");
        let created = dir.path().join("side-b.cue");
        fs::write(&existing, b"old side A").expect("write existing carrier");

        let existing_stage =
            stage_existing_metadata_batch_file(&existing).expect("stage existing carrier");
        fs::write(existing_stage.staged_path(), b"new side A")
            .expect("rewrite staged existing carrier");
        let created_stage = stage_new_metadata_batch_file(&created).expect("stage new carrier");
        fs::write(created_stage.staged_path(), b"new side B")
            .expect("materialize staged new carrier");

        commit_metadata_rewrite_batch(vec![existing_stage, created_stage])
            .expect("commit carrier batch");
        assert_eq!(fs::read(&existing).expect("read existing carrier"), b"new side A");
        assert_eq!(fs::read(&created).expect("read new carrier"), b"new side B");
        assert!(
            fs::read_dir(dir.path())
                .expect("list carrier directory")
                .all(|entry| !entry
                    .expect("directory entry")
                    .file_name()
                    .to_string_lossy()
                    .contains("tonepoet-metadata")),
            "successful commit must remove private staging/rollback files"
        );
    }

    #[test]
    fn metadata_batch_mid_commit_failure_restores_every_original() {
        let dir = tempfile::tempdir().expect("metadata batch tempdir");
        let first = dir.path().join("side-a.cue");
        let second = dir.path().join("side-b.cue");
        fs::write(&first, b"old side A").expect("write first original");
        fs::write(&second, b"old side B").expect("write second original");

        let first_stage =
            stage_existing_metadata_batch_file(&first).expect("stage first carrier");
        let second_stage =
            stage_existing_metadata_batch_file(&second).expect("stage second carrier");
        fs::write(first_stage.staged_path(), b"new side A").expect("rewrite first stage");
        fs::write(second_stage.staged_path(), b"new side B").expect("rewrite second stage");

        let _failure = MetadataBatchCommitFailureGuard::after(1);
        let error = commit_metadata_rewrite_batch(vec![first_stage, second_stage])
            .expect_err("injected mid-commit failure must abort the batch");
        assert!(error.to_string().contains("injected metadata batch failure"));
        assert_eq!(fs::read(&first).expect("read first original"), b"old side A");
        assert_eq!(fs::read(&second).expect("read second original"), b"old side B");
        assert!(
            fs::read_dir(dir.path())
                .expect("list carrier directory")
                .all(|entry| !entry
                    .expect("directory entry")
                    .file_name()
                    .to_string_lossy()
                    .contains("tonepoet-metadata")),
            "failed commit must remove private staging/rollback files"
        );
    }

    #[test]
    fn metadata_batch_mid_commit_failure_removes_new_carriers_and_restores_existing_ones() {
        let dir = tempfile::tempdir().expect("metadata batch tempdir");
        let existing = dir.path().join("side-a.cue");
        let created = dir.path().join("side-b.cue");
        fs::write(&existing, b"old side A").expect("write existing original");

        let existing_stage =
            stage_existing_metadata_batch_file(&existing).expect("stage existing carrier");
        fs::write(existing_stage.staged_path(), b"new side A")
            .expect("rewrite staged existing carrier");
        let created_stage = stage_new_metadata_batch_file(&created).expect("stage new carrier");
        fs::write(created_stage.staged_path(), b"new side B")
            .expect("materialize staged new carrier");

        let _failure = MetadataBatchCommitFailureGuard::after(2);
        let error = commit_metadata_rewrite_batch(vec![existing_stage, created_stage])
            .expect_err("failure after both commits must roll back existing + created targets");
        assert!(error.to_string().contains("injected metadata batch failure"));
        assert_eq!(fs::read(&existing).expect("read restored existing"), b"old side A");
        assert!(
            !created.exists(),
            "carrier created inside a failed batch must be absent after rollback"
        );
        assert!(
            fs::read_dir(dir.path())
                .expect("list carrier directory")
                .all(|entry| !entry
                    .expect("directory entry")
                    .file_name()
                    .to_string_lossy()
                    .contains("tonepoet-metadata")),
            "successful rollback must remove private staging/rollback files"
        );
    }

    #[test]
    fn metadata_batch_staging_rejects_read_only_targets_before_mutation() {
        let dir = tempfile::tempdir().expect("metadata batch tempdir");
        let target = dir.path().join("readonly.cue");
        fs::write(&target, b"original").expect("write target");
        let mut permissions = fs::metadata(&target).expect("stat target").permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&target, permissions).expect("make target read-only");

        let error = stage_existing_metadata_batch_file(&target)
            .expect_err("read-only target must be rejected before staging");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&target).expect("read target"), b"original");

        let mut permissions = fs::metadata(&target).expect("stat target").permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&target, permissions).expect("restore writable target");
    }

    #[cfg(unix)]
    #[test]
    fn replacement_preserves_mode_ownership_and_timestamps() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let dir = tempfile::tempdir().expect("metadata rewrite tempdir");
        let target = dir.path().join("track.mp3");
        fs::write(&target, b"old audio").expect("write old target");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640))
            .expect("set target permissions");
        let accessed = UNIX_EPOCH + Duration::from_secs(1_700_000_001);
        let modified = UNIX_EPOCH + Duration::from_secs(1_700_000_002);
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&target)
            .expect("open target")
            .set_times(
                fs::FileTimes::new()
                    .set_accessed(accessed)
                    .set_modified(modified),
            )
            .expect("set target timestamps");
        let before = fs::symlink_metadata(&target).expect("stat target before rewrite");

        let tmp = metadata_rewrite_temp_path(&target).expect("temp path");
        fs::write(tmp.path(), b"new audio").expect("write rewritten temp");
        replace_rewritten_metadata_file(&target, tmp).expect("replace rewritten file");

        let after = fs::symlink_metadata(&target).expect("stat target after rewrite");
        assert_eq!(after.permissions().mode(), before.permissions().mode());
        assert_eq!(after.uid(), before.uid());
        assert_eq!(after.gid(), before.gid());
        assert_eq!(after.accessed().expect("access time"), accessed);
        assert_eq!(after.modified().expect("modification time"), modified);
        assert_eq!(fs::read(&target).expect("read rewritten target"), b"new audio");
    }

    #[cfg(target_os = "linux")]
    fn set_test_xattr(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
        let path_c = linux_path_cstring(path)?;
        let name_c = std::ffi::CString::new(name).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "test xattr name contains NUL")
        })?;
        let result = unsafe {
            libc::lsetxattr(
                path_c.as_ptr(),
                name_c.as_ptr(),
                value.as_ptr().cast::<libc::c_void>(),
                value.len(),
                0,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(target_os = "linux")]
    fn xattr_unsupported(error: &io::Error) -> bool {
        error.raw_os_error().is_some_and(|code| {
            code == libc::ENOTSUP
                || code == libc::EOPNOTSUPP
                || code == libc::EPERM
                || code == libc::EACCES
                || code == libc::EINVAL
        })
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn replacement_preserves_linux_xattrs_and_posix_acl_xattr() {
        use std::os::unix::fs::MetadataExt as _;

        let dir = tempfile::tempdir().expect("metadata rewrite tempdir");
        let target = dir.path().join("track.mp3");
        fs::write(&target, b"old audio").expect("write old target");
        if let Err(error) = set_test_xattr(&target, "user.tonepoet-test", b"preserve-me") {
            if xattr_unsupported(&error) {
                return;
            }
            panic!("set user test xattr: {error}");
        }

        // Linux POSIX ACL xattr v2: owner rw-, one named user r--,
        // group r--, mask r--, other ---. The named UID need not resolve to
        // an account; the kernel stores the numeric ACL identity.
        let named_uid = fs::symlink_metadata(&target)
            .expect("stat ACL target")
            .uid()
            .saturating_add(1);
        let mut acl = 2_u32.to_le_bytes().to_vec();
        for (tag, permissions, id) in [
            (0x01_u16, 0x06_u16, u32::MAX),
            (0x02_u16, 0x04_u16, named_uid),
            (0x04_u16, 0x04_u16, u32::MAX),
            (0x10_u16, 0x04_u16, u32::MAX),
            (0x20_u16, 0x00_u16, u32::MAX),
        ] {
            acl.extend_from_slice(&tag.to_le_bytes());
            acl.extend_from_slice(&permissions.to_le_bytes());
            acl.extend_from_slice(&id.to_le_bytes());
        }
        let acl_supported = match set_test_xattr(&target, "system.posix_acl_access", &acl) {
            Ok(()) => true,
            Err(error) if xattr_unsupported(&error) => false,
            Err(error) => panic!("set POSIX ACL xattr: {error}"),
        };

        let before = linux_xattrs(&target).expect("read xattrs before rewrite");
        assert!(before.iter().any(|(key, actual)| {
            key.as_os_str() == std::ffi::OsStr::new("user.tonepoet-test")
                && actual.as_slice() == b"preserve-me"
        }));
        if acl_supported {
            assert!(before.iter().any(|(key, actual)| {
                key.as_os_str() == std::ffi::OsStr::new("system.posix_acl_access")
                    && actual.as_slice() == acl.as_slice()
            }));
        }

        let tmp = metadata_rewrite_temp_path(&target).expect("temp path");
        fs::write(tmp.path(), b"new audio").expect("write rewritten temp");
        replace_rewritten_metadata_file(&target, tmp).expect("replace rewritten file");

        assert_eq!(linux_xattrs(&target).expect("read xattrs after rewrite"), before);
    }

    #[cfg(unix)]
    #[test]
    fn replacement_rejects_target_substitution() {
        let dir = tempfile::tempdir().expect("metadata rewrite tempdir");
        let target = dir.path().join("track.mp3");
        fs::write(&target, b"old audio").expect("write old target");
        let tmp = metadata_rewrite_temp_path(&target).expect("temp path");
        fs::write(tmp.path(), b"rewritten audio").expect("write rewritten temp");

        fs::remove_file(&target).expect("remove original target");
        fs::write(&target, b"outside replacement").expect("replace target externally");
        let error = replace_rewritten_metadata_file(&target, tmp)
            .expect_err("target substitution must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            fs::read(&target).expect("read outside replacement"),
            b"outside replacement"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacement_rejects_in_place_permission_drift() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("metadata rewrite tempdir");
        let target = dir.path().join("track.mp3");
        fs::write(&target, b"old audio").expect("write old target");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640))
            .expect("set original target mode");
        let tmp = metadata_rewrite_temp_path(&target).expect("temp path");
        fs::write(tmp.path(), b"rewritten audio").expect("write rewritten temp");

        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))
            .expect("change target mode externally");
        let error = replace_rewritten_metadata_file(&target, tmp)
            .expect_err("in-place permission drift must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let metadata = fs::symlink_metadata(&target).expect("stat externally changed target");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read(&target).expect("read unchanged target"), b"old audio");
    }
}
