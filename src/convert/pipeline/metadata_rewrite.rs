//! Atomic same-directory replacement for external metadata mutators.

#![allow(unsafe_code)]

use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{fs, io};

use sha2::{Digest as _, Sha256};

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
    xattrs: LinuxXattrSnapshot,
    allow_capability_fallback: bool,
    content_sha256: Option<[u8; 32]>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
enum LinuxXattrSnapshot {
    Unsupported,
    Captured(Vec<(std::ffi::OsString, Vec<u8>)>),
}

#[cfg(target_os = "linux")]
impl LinuxXattrSnapshot {
    fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported)
    }
}

impl MetadataRewriteAttributes {
    fn capture(path: &Path, allow_capability_fallback: bool) -> io::Result<Self> {
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
            xattrs: if allow_capability_fallback {
                capture_linux_xattrs(path)?
            } else {
                LinuxXattrSnapshot::Captured(linux_xattrs(path)?)
            },
            allow_capability_fallback,
            content_sha256: None,
        })
    }

    fn use_content_guard(&mut self, digest: [u8; 32]) {
        self.content_sha256 = Some(digest);
    }

    fn verify_guarded_content(&self, path: &Path) -> io::Result<()> {
        let Some(expected) = self.content_sha256 else {
            return Ok(());
        };
        if sha256_file(path)? != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "metadata rewrite target content fingerprint changed before replacement: {}",
                    path.display()
                ),
            ));
        }
        Ok(())
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
            let identity_changed = self.content_sha256.is_none()
                && (metadata.dev() != self.device
                    || metadata.ino() != self.inode
                    || metadata.ctime() != self.change_time_seconds
                    || metadata.ctime_nsec() != self.change_time_nanoseconds);
            if identity_changed
                || metadata.uid() != self.uid
                || metadata.gid() != self.gid
                || metadata.permissions().mode() != self.permissions.mode()
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

        if metadata.len() != self.len
            || (self.content_sha256.is_none() && metadata.modified()? != self.modified)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "metadata rewrite target content changed before replacement: {}",
                    path.display()
                ),
            ));
        }
        #[cfg(target_os = "linux")]
        verify_linux_xattr_snapshot(
            path,
            &self.xattrs,
            "target",
            self.allow_capability_fallback,
        )?;

        self.verify_guarded_content(path)?;

        Ok(())
    }

    fn apply_and_verify(&self, path: &Path) -> io::Result<()> {
        let file = if self.allow_capability_fallback {
            open_rw_nofollow(path)?
        } else {
            fs::OpenOptions::new().read(true).write(true).open(path)?
        };

        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd as _;
            use std::os::unix::fs::MetadataExt as _;
            let current = file.metadata()?;
            if current.uid() != self.uid || current.gid() != self.gid {
                if !self.allow_capability_fallback {
                    // Keep the established single-file writer behavior. The
                    // capability-adaptive ownership path is batch-only.
                    std::os::unix::fs::chown(path, Some(self.uid), Some(self.gid))?;
                } else {
                    let result = unsafe {
                        libc::fchown(
                            file.as_raw_fd(),
                            self.uid as libc::uid_t,
                            self.gid as libc::gid_t,
                        )
                    };
                    if result != 0 {
                        let error = io::Error::last_os_error();
                        let after = file.metadata()?;
                        if after.uid() != self.uid || after.gid() != self.gid {
                            return Err(io::Error::new(
                                error.kind(),
                                format!(
                                    "metadata rewrite could not preserve ownership for '{}': {error}",
                                    path.display()
                                ),
                            ));
                        }
                    }
                }
            }
        }

        set_permissions_without_widening(
            &file,
            path,
            &self.permissions,
            self.allow_capability_fallback,
        )?;
        #[cfg(target_os = "linux")]
        apply_linux_xattr_snapshot(path, &self.xattrs, self.allow_capability_fallback)?;
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
        verify_linux_xattr_snapshot(
            path,
            &self.xattrs,
            "temporary output",
            self.allow_capability_fallback,
        )?;

        Ok(())
    }
}

fn open_read_nofollow(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn open_rw_nofollow(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn open_write_truncate_nofollow(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn sha256_file(path: &Path) -> io::Result<[u8; 32]> {
    use std::io::Read as _;

    let mut file = open_read_nofollow(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("metadata fingerprint source is not a regular file: {}", path.display()),
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut out = [0_u8; 32];
    out.copy_from_slice(&digest);
    Ok(out)
}

#[cfg(unix)]
fn set_permissions_without_widening(
    file: &fs::File,
    path: &Path,
    expected: &fs::Permissions,
    allow_best_effort: bool,
) -> io::Result<()> {
    if !allow_best_effort {
        return file.set_permissions(expected.clone());
    }
    #[cfg(test)]
    if metadata_test_force_permission_unsupported() {
        return verify_permission_failure_is_safe(
            path,
            expected,
            io::Error::from_raw_os_error(libc::ENOTSUP),
        );
    }

    match file.set_permissions(expected.clone()) {
        Ok(()) => Ok(()),
        Err(error) => verify_permission_failure_is_safe(path, expected, error),
    }
}

#[cfg(unix)]
fn verify_permission_failure_is_safe(
    path: &Path,
    expected: &fs::Permissions,
    error: io::Error,
) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let actual = fs::symlink_metadata(path)?.permissions().mode();
    let expected = expected.mode();
    if error.raw_os_error().is_some_and(|code| {
        code == libc::EACCES || code == libc::ENOSPC || code == libc::EIO || code == libc::EROFS
    }) {
        return Err(error);
    }
    if actual == expected {
        return Ok(());
    }
    let actual_access = actual & 0o7777;
    let expected_access = expected & 0o7777;
    if actual_access & !expected_access != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "metadata rewrite refused to publish wider permissions after chmod failed for \
                 '{}': expected {:04o}, got {:04o}: {error}",
                path.display(),
                expected_access,
                actual_access
            ),
        ));
    }
    Err(io::Error::new(
        error.kind(),
        format!(
            "metadata rewrite could not preserve permission bits for '{}': expected {:04o}, got {:04o}: {error}",
            path.display(),
            expected_access,
            actual_access
        ),
    ))
}

#[cfg(not(unix))]
fn set_permissions_without_widening(
    file: &fs::File,
    _path: &Path,
    expected: &fs::Permissions,
    _allow_best_effort: bool,
) -> io::Result<()> {
    file.set_permissions(expected.clone())
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
fn linux_xattr_capability_unsupported(error: &io::Error) -> bool {
    error
        .raw_os_error()
        .is_some_and(|code| code == libc::ENOTSUP || code == libc::EOPNOTSUPP || code == libc::ENOSYS)
}

#[cfg(target_os = "linux")]
fn capture_linux_xattrs(path: &Path) -> io::Result<LinuxXattrSnapshot> {
    match linux_xattrs(path) {
        Ok(xattrs) => Ok(LinuxXattrSnapshot::Captured(xattrs)),
        Err(error) if linux_xattr_capability_unsupported(&error) => {
            Ok(LinuxXattrSnapshot::Unsupported)
        }
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn verify_linux_xattr_snapshot(
    path: &Path,
    expected: &LinuxXattrSnapshot,
    subject: &str,
    allow_unsupported_empty: bool,
) -> io::Result<()> {
    match (expected, capture_linux_xattrs(path)?) {
        (LinuxXattrSnapshot::Unsupported, LinuxXattrSnapshot::Unsupported) => Ok(()),
        (LinuxXattrSnapshot::Unsupported, LinuxXattrSnapshot::Captured(actual))
            if actual.is_empty() =>
        {
            // Capability may vary by object on FUSE/network mounts. An object
            // that can now report an empty xattr set loses no captured state.
            Ok(())
        }
        (LinuxXattrSnapshot::Unsupported, LinuxXattrSnapshot::Captured(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "metadata rewrite {subject} gained extended attributes while the transaction was active: {}",
                path.display()
            ),
        )),
        (LinuxXattrSnapshot::Captured(expected), LinuxXattrSnapshot::Captured(actual))
            if actual.as_slice() == expected.as_slice() =>
        {
            Ok(())
        }
        (LinuxXattrSnapshot::Captured(expected), LinuxXattrSnapshot::Unsupported)
            if allow_unsupported_empty && expected.is_empty() =>
        {
            Ok(())
        }
        (LinuxXattrSnapshot::Captured(_), LinuxXattrSnapshot::Unsupported) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "metadata rewrite could not verify restricting extended attributes or ACLs on {subject}: {}",
                path.display()
            ),
        )),
        (LinuxXattrSnapshot::Captured(_), LinuxXattrSnapshot::Captured(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "metadata rewrite {subject} extended attributes changed during the transaction: {}",
                path.display()
            ),
        )),
    }
}

#[cfg(target_os = "linux")]
fn apply_linux_xattr_snapshot(
    path: &Path,
    expected: &LinuxXattrSnapshot,
    allow_unsupported_empty: bool,
) -> io::Result<()> {
    match expected {
        LinuxXattrSnapshot::Unsupported => Ok(()),
        LinuxXattrSnapshot::Captured(expected) => match set_linux_xattrs_exact(path, expected) {
            Ok(()) => Ok(()),
            Err(error)
                if allow_unsupported_empty
                    && linux_xattr_capability_unsupported(&error)
                    && expected.is_empty() =>
            {
                Ok(())
            }
            Err(error) if linux_xattr_capability_unsupported(&error) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "metadata rewrite could not preserve restricting extended attributes or ACLs for '{}': {error}",
                    path.display()
                ),
            )),
            Err(error) => Err(error),
        },
    }
}

#[cfg(target_os = "linux")]
fn linux_xattrs(path: &Path) -> io::Result<Vec<(std::ffi::OsString, Vec<u8>)>> {
    use std::os::unix::ffi::OsStringExt as _;

    #[cfg(test)]
    if metadata_test_xattr_fault() == MetadataTestXattrFault::UnsupportedAll {
        return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
    }

    let path_c = linux_path_cstring(path)?;
    let mut list_capability_observed = false;
    let names = loop {
        let len = unsafe { libc::llistxattr(path_c.as_ptr(), std::ptr::null_mut(), 0) };
        if len < 0 {
            let error = io::Error::last_os_error();
            if list_capability_observed && linux_xattr_capability_unsupported(&error) {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "extended-attribute enumeration became unsupported for '{}': {error}",
                        path.display()
                    ),
                ));
            }
            return Err(error);
        }
        list_capability_observed = true;
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
            // The initial list-size call already proved that this object can
            // enumerate xattrs. Do not reinterpret a later per-object/namespace
            // failure as globally unsupported and thereby forget attributes.
            if linux_xattr_capability_unsupported(&error) {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "extended-attribute enumeration became unsupported for '{}': {error}",
                        path.display()
                    ),
                ));
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
                let error = io::Error::last_os_error();
                if linux_xattr_capability_unsupported(&error) {
                    return Err(io::Error::new(
                        error.kind(),
                        format!(
                            "extended attribute '{}' cannot be read for '{}': {error}",
                            std::ffi::OsString::from_vec(raw_name.to_vec()).to_string_lossy(),
                            path.display()
                        ),
                    ));
                }
                return Err(error);
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
                if linux_xattr_capability_unsupported(&error) {
                    return Err(io::Error::new(
                        error.kind(),
                        format!(
                            "extended attribute '{}' became unreadable for '{}': {error}",
                            std::ffi::OsString::from_vec(raw_name.to_vec()).to_string_lossy(),
                            path.display()
                        ),
                    ));
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

    #[cfg(test)]
    if metadata_test_xattr_fault() == MetadataTestXattrFault::WriteUnsupported {
        return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
    }

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
    // The capability-adaptive behavior is intentionally scoped to the new
    // multi-carrier transaction. Preserve the established single-file writer
    // contract here.
    let original_attributes = MetadataRewriteAttributes::capture(path, false)?;
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
    /// Fingerprint of a create-only staged carrier. This is populated after
    /// the mutator finishes and is used to verify publish and rollback.
    published_sha256: Option<[u8; 32]>,
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

#[cfg(unix)]
fn parent_preserves_subsecond_timestamps(target: &Path) -> io::Result<bool> {
    use std::time::{Duration, UNIX_EPOCH};

    #[cfg(test)]
    if metadata_test_force_coarse_timestamps() {
        return Ok(false);
    }

    let parent = metadata_rewrite_parent(target);
    let probe = tempfile::Builder::new()
        .prefix(".tonepoet-metadata-time-probe.")
        .tempfile_in(parent)?;
    let requested = UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789);
    match probe.as_file().set_times(
        fs::FileTimes::new()
            .set_accessed(requested)
            .set_modified(requested),
    ) {
        Ok(()) => Ok(probe.as_file().metadata()?.modified()? == requested),
        Err(error)
            if error.raw_os_error().is_some_and(|code| {
                code == libc::EINVAL || code == libc::ENOTSUP || code == libc::EOPNOTSUPP
            }) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(not(unix))]
fn parent_preserves_subsecond_timestamps(_target: &Path) -> io::Result<bool> {
    Ok(true)
}

#[cfg(unix)]
fn parent_preserves_stable_file_identity(target: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    #[cfg(test)]
    if metadata_test_force_unstable_identity() {
        return Ok(false);
    }

    // Check the real carrier as well as a private rename probe. Some FUSE
    // implementations synthesize inode identities per lookup even when their
    // temporary files happen to look stable.
    let target_first = fs::symlink_metadata(target)?;
    let target_second = fs::symlink_metadata(target)?;
    if target_first.dev() == 0
        || target_first.ino() == 0
        || target_first.dev() != target_second.dev()
        || target_first.ino() != target_second.ino()
    {
        return Ok(false);
    }

    let first = metadata_batch_temp_path(target, "metadata-identity-probe-a")?;
    let second = match metadata_batch_temp_path(target, "metadata-identity-probe-b") {
        Ok(path) => path,
        Err(error) => {
            let _ = fs::remove_file(&first);
            return Err(error);
        }
    };
    let result = (|| -> io::Result<bool> {
        fs::remove_file(&second)?;
        let before = fs::symlink_metadata(&first)?;
        let repeated = fs::symlink_metadata(&first)?;
        if before.dev() == 0
            || before.ino() == 0
            || before.dev() != repeated.dev()
            || before.ino() != repeated.ino()
        {
            return Ok(false);
        }
        fs::rename(&first, &second)?;
        let after = fs::symlink_metadata(&second)?;
        let stable = before.dev() == after.dev() && before.ino() == after.ino();
        fs::rename(&second, &first)?;
        Ok(stable)
    })();
    let _ = fs::remove_file(&first);
    let _ = fs::remove_file(&second);
    result
}

#[cfg(not(unix))]
fn parent_preserves_stable_file_identity(_target: &Path) -> io::Result<bool> {
    Ok(true)
}

fn copy_nofollow(source: &Path, destination: &Path) -> io::Result<()> {
    let mut source_file = open_read_nofollow(source)?;
    let source_metadata = source_file.metadata()?;
    if !source_metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("metadata copy source is not a regular file: {}", source.display()),
        ));
    }
    let mut destination_file = open_write_truncate_nofollow(destination)?;
    // Keep std's platform-optimized File-to-File copy path while holding
    // nofollow descriptors, instead of reopening either pathname via fs::copy.
    io::copy(&mut source_file, &mut destination_file)?;
    set_permissions_without_widening(
        &destination_file,
        destination,
        &source_metadata.permissions(),
        true,
    )
}

fn copy_with_sha256(source: &Path, destination: &Path) -> io::Result<[u8; 32]> {
    use std::io::{Read as _, Write as _};

    let mut source_file = open_read_nofollow(source)?;
    let source_metadata = source_file.metadata()?;
    if !source_metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("metadata copy source is not a regular file: {}", source.display()),
        ));
    }
    let mut destination_file = open_write_truncate_nofollow(destination)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = source_file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        destination_file.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
    }
    destination_file.flush()?;
    set_permissions_without_widening(
        &destination_file,
        destination,
        &source_metadata.permissions(),
        true,
    )?;
    let digest = hasher.finalize();
    let mut out = [0_u8; 32];
    out.copy_from_slice(&digest);
    Ok(out)
}

fn clone_or_copy_file_with_guard(
    source: &Path,
    destination: &Path,
    content_guard: bool,
) -> io::Result<Option<[u8; 32]>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd as _;
        let source_file = open_read_nofollow(source)?;
        let destination_file = open_write_truncate_nofollow(destination)?;
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
            if !content_guard {
                return Ok(None);
            }
            // The reflink is the immutable staging snapshot. Hash it once;
            // the caller refreshes source attributes and verifies the source
            // against this digest before returning the staged transaction.
            return sha256_file(destination).map(Some);
        }
    }
    if content_guard {
        // Hash while copying so the guarded path pays for no extra staged-file
        // read. The caller performs the single source re-read needed to prove
        // that the copied bytes and refreshed source attributes agree.
        copy_with_sha256(source, destination).map(Some)
    } else {
        copy_nofollow(source, destination).map(|()| None)
    }
}

fn clone_or_copy_file(source: &Path, destination: &Path) -> io::Result<()> {
    clone_or_copy_file_with_guard(source, destination, false).map(|_| ())
}

fn clone_or_copy_file_matching_digest(
    source: &Path,
    destination: &Path,
    expected: [u8; 32],
) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd as _;
        let source_file = open_read_nofollow(source)?;
        let destination_file = open_write_truncate_nofollow(destination)?;
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
            if sha256_file(destination)? == expected {
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "metadata rollback backup does not match the staged source snapshot: {}",
                    source.display()
                ),
            ));
        }
    }

    if copy_with_sha256(source, destination)? == expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "metadata rollback backup does not match the staged source snapshot: {}",
                source.display()
            ),
        ))
    }
}

/// Stage an existing regular carrier without changing it.
pub(crate) fn stage_existing_metadata_batch_file(
    target: &Path,
) -> io::Result<MetadataRewriteBatchStage> {
    let metadata = fs::symlink_metadata(target)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "metadata batch target must be an existing regular file: {}",
                target.display()
            ),
        ));
    }
    if metadata.permissions().readonly() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("metadata batch target is read-only: {}", target.display()),
        ));
    }
    let mut original_attributes = MetadataRewriteAttributes::capture(target, true)?;
    let original_accessed = original_attributes.accessed;
    #[cfg(target_os = "linux")]
    let content_guard = original_attributes.xattrs.is_unsupported()
        || !parent_preserves_subsecond_timestamps(target)?
        || !parent_preserves_stable_file_identity(target)?;
    #[cfg(not(target_os = "linux"))]
    let content_guard = !parent_preserves_subsecond_timestamps(target)?
        || !parent_preserves_stable_file_identity(target)?;
    let staged = metadata_batch_temp_path(target, "metadata-stage")?;
    let snapshot_digest = match clone_or_copy_file_with_guard(target, &staged, content_guard) {
        Ok(digest) => digest,
        Err(error) => {
            let _ = fs::remove_file(&staged);
            return Err(error);
        }
    };
    if let Some(digest) = snapshot_digest {
        // Weak metadata identity means the byte snapshot is authoritative.
        // Refresh the attribute snapshot after copying/cloning so a writer
        // that ran between the initial capability capture and the snapshot
        // cannot be compared against stale uid/mode/xattr metadata. The final
        // verification below then proves the refreshed attributes and digest
        // still describe the same source.
        original_attributes = match MetadataRewriteAttributes::capture(target, true) {
            Ok(mut attributes) => {
                // Reads performed for staging/fingerprinting must not make the
                // transaction preserve its own atime side effect.
                attributes.accessed = original_accessed;
                attributes
            }
            Err(error) => {
                let _ = fs::remove_file(&staged);
                return Err(error);
            }
        };
        original_attributes.use_content_guard(digest);
    }
    if let Err(error) = original_attributes.verify_source_unchanged(target) {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    Ok(MetadataRewriteBatchStage {
        target: target.to_path_buf(),
        staged,
        original_attributes: Some(original_attributes),
        published_sha256: None,
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
        published_sha256: None,
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
    let file = open_rw_nofollow(&stage.staged)?;
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
#[derive(Debug, Clone, Copy)]
enum LinuxRenameProbe {
    Exchange,
    NoReplace,
}

#[cfg(target_os = "linux")]
fn linux_renameat2_flag_supported(target: &Path, probe: LinuxRenameProbe) -> io::Result<bool> {
    const RENAME_NOREPLACE: libc::c_uint = 1;
    const RENAME_EXCHANGE: libc::c_uint = 2;
    let parent = metadata_rewrite_parent(target);
    let mut first = tempfile::Builder::new()
        .prefix(".tonepoet-rename-probe-a.")
        .tempfile_in(parent)?;
    let mut second = tempfile::Builder::new()
        .prefix(".tonepoet-rename-probe-b.")
        .tempfile_in(parent)?;
    use std::io::Write as _;
    first.write_all(b"A")?;
    second.write_all(b"B")?;

    #[cfg(test)]
    let forced_unsupported = metadata_test_force_renameat2_unsupported();
    #[cfg(not(test))]
    let forced_unsupported = false;
    let result = if forced_unsupported {
        Err(io::Error::from_raw_os_error(libc::EINVAL))
    } else {
        match probe {
            LinuxRenameProbe::Exchange => {
                linux_renameat2(first.path(), second.path(), RENAME_EXCHANGE)
            }
            LinuxRenameProbe::NoReplace => {
                linux_renameat2(first.path(), second.path(), RENAME_NOREPLACE)
            }
        }
    };
    match (probe, result) {
        (LinuxRenameProbe::Exchange, Ok(())) => {
            if fs::read(first.path())? == b"B" && fs::read(second.path())? == b"A" {
                Ok(true)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "renameat2(RENAME_EXCHANGE) did not exchange private probe files in '{}'",
                        parent.display()
                    ),
                ))
            }
        }
        (LinuxRenameProbe::NoReplace, Err(error))
            if error.kind() == io::ErrorKind::AlreadyExists =>
        {
            // Prove both halves of the contract: the flag rejects clobbering
            // and can also publish to an absent private name.
            fs::remove_file(second.path())?;
            match linux_renameat2(first.path(), second.path(), RENAME_NOREPLACE) {
                Ok(())
                    if fs::read(second.path())? == b"A"
                        && fs::symlink_metadata(first.path())
                            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound) =>
                {
                    Ok(true)
                }
                Ok(()) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "renameat2(RENAME_NOREPLACE) produced an invalid private probe result in '{}'",
                        parent.display()
                    ),
                )),
                Err(error)
                    if error
                        .raw_os_error()
                        .is_some_and(|code| code == libc::EINVAL || code == libc::ENOSYS) =>
                {
                    Ok(false)
                }
                Err(error) => Err(error),
            }
        }
        (_, Err(error))
            if error
                .raw_os_error()
                .is_some_and(|code| code == libc::EINVAL || code == libc::ENOSYS) =>
        {
            Ok(false)
        }
        (LinuxRenameProbe::NoReplace, Ok(())) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "renameat2(RENAME_NOREPLACE) overwrote a private probe target in '{}'",
                parent.display()
            ),
        )),
        (_, Err(error)) => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn rollback_linux_metadata_batch(
    stages: &mut [MetadataRewriteBatchStage],
    committed: &[usize],
) -> Vec<String> {
    const RENAME_NOREPLACE: libc::c_uint = 1;
    const RENAME_EXCHANGE: libc::c_uint = 2;
    let mut failures = Vec::new();
    for index in committed.iter().rev().copied() {
        let stage = &mut stages[index];
        let result = if stage.original_attributes.is_some() {
            linux_renameat2(&stage.target, &stage.staged, RENAME_EXCHANGE)
        } else {
            // A create rollback removes only the carrier we published. If the
            // name now contains different bytes or a different object type,
            // leave it untouched and report the incomplete rollback.
            verify_created_stage_at_target(stage).and_then(|()| {
                linux_renameat2(&stage.target, &stage.staged, RENAME_NOREPLACE)
            })
        };
        if let Err(error) = result {
            // On an existing target the stage path may contain the only
            // recoverable original. On a created target a concurrent actor may
            // have occupied the private stage name. In either case, a failed
            // rollback must not let Drop unlink data whose ownership is now
            // uncertain.
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
    static METADATA_FORCE_RENAMEAT2_UNSUPPORTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static METADATA_FORCE_COARSE_TIMESTAMPS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static METADATA_FORCE_UNSTABLE_IDENTITY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static METADATA_FORCE_PERMISSION_UNSUPPORTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static METADATA_PORTABLE_CREATE_TARGET_AT_COMMIT: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
    static METADATA_FAIL_PORTABLE_ROLLBACK_INDEX: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static METADATA_MUTATE_CREATED_AFTER_COMMIT_INDEX: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
    static METADATA_XATTR_FAULT: std::cell::Cell<MetadataTestXattrFault> =
        const { std::cell::Cell::new(MetadataTestXattrFault::None) };
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetadataTestXattrFault {
    None,
    UnsupportedAll,
    WriteUnsupported,
}

#[cfg(test)]
fn metadata_test_force_renameat2_unsupported() -> bool {
    METADATA_FORCE_RENAMEAT2_UNSUPPORTED.with(std::cell::Cell::get)
}

#[cfg(test)]
fn metadata_test_force_coarse_timestamps() -> bool {
    METADATA_FORCE_COARSE_TIMESTAMPS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn metadata_test_force_unstable_identity() -> bool {
    METADATA_FORCE_UNSTABLE_IDENTITY.with(std::cell::Cell::get)
}

#[cfg(test)]
fn metadata_test_force_permission_unsupported() -> bool {
    METADATA_FORCE_PERMISSION_UNSUPPORTED.with(std::cell::Cell::get)
}

#[cfg(all(test, target_os = "linux"))]
fn metadata_test_xattr_fault() -> MetadataTestXattrFault {
    METADATA_XATTR_FAULT.with(std::cell::Cell::get)
}

#[cfg(test)]
fn metadata_test_maybe_create_portable_target(
    index: usize,
    stage: &MetadataRewriteBatchStage,
) -> io::Result<()> {
    let inject = METADATA_PORTABLE_CREATE_TARGET_AT_COMMIT.with(|slot| {
        if slot.get() == Some(index) {
            slot.set(None);
            true
        } else {
            false
        }
    });
    if inject {
        fs::write(&stage.target, b"third-party carrier")?;
    }
    Ok(())
}

#[cfg(test)]
fn metadata_test_maybe_mutate_created_after_commit(
    index: usize,
    stage: &MetadataRewriteBatchStage,
) -> io::Result<()> {
    let inject = METADATA_MUTATE_CREATED_AFTER_COMMIT_INDEX.with(|slot| {
        if slot.get() == Some(index) {
            slot.set(None);
            true
        } else {
            false
        }
    });
    if inject && stage.original_attributes.is_none() {
        fs::write(&stage.target, b"third-party modified carrier")?;
    }
    Ok(())
}

#[cfg(test)]
fn metadata_batch_injected_rollback_failure(index: usize) -> Option<io::Error> {
    METADATA_FAIL_PORTABLE_ROLLBACK_INDEX.with(|slot| {
        (slot.get() == Some(index)).then(|| {
            slot.set(None);
            io::Error::new(
                io::ErrorKind::Other,
                format!("injected metadata portable rollback failure at index {index}"),
            )
        })
    })
}

#[cfg(not(test))]
fn metadata_batch_injected_rollback_failure(_index: usize) -> Option<io::Error> {
    None
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
    let reduced_mount_durability = stages.iter().any(|stage| {
        stage
            .original_attributes
            .as_ref()
            .is_some_and(|attributes| attributes.content_sha256.is_some())
    });
    let mut committed = Vec::with_capacity(stages.len());
    for index in 0..stages.len() {
        let target = stages[index].target.clone();

        let immediate_validation = if index == 0 {
            // Full-set prevalidation runs in reverse order, so member 0 was
            // checked last immediately before entering this commit loop.
            Ok(())
        } else {
            stages[index]
                .original_attributes
                .as_ref()
                .map_or(Ok(()), |attributes| {
                    // Revalidate later sources immediately before their
                    // namespace replacement. Weak mounts include SHA-256 here;
                    // strong mounts retain the cheap metadata/xattr guard.
                    attributes.verify_source_unchanged(&stages[index].target)
                })
        };
        if let Err(error) = immediate_validation {
            let rollback = rollback_linux_metadata_batch(stages, &committed);
            let suffix = if rollback.is_empty() {
                String::new()
            } else {
                format!("; rollback incomplete: {}", rollback.join("; "))
            };
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "metadata batch source changed before commit at '{}': {error}{suffix}",
                    target.display()
                ),
            ));
        }

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

        if reduced_mount_durability {
            let verify = {
                let stage = &stages[index];
                if let Some(attributes) = stage.original_attributes.as_ref() {
                    attributes.verify_applied(&stage.target)
                } else {
                    verify_created_stage_at_target(stage)
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
                verify_created_stage_at_target(stage)
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

    // Strong local filesystems retain the original end-of-batch durability
    // boundary. Weak-identity mounts were already synced after each member,
    // including the final namespace state, so do not pay for duplicate fsyncs.
    if !reduced_mount_durability {
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

fn verify_created_stage_at_target(stage: &MetadataRewriteBatchStage) -> io::Result<()> {
    validate_staged_batch_file_at_target(&stage.target)?;
    let expected = stage.published_sha256.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "metadata batch create fingerprint missing",
        )
    })?;
    if sha256_file(&stage.target)? == expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "metadata batch created carrier changed after publish: {}",
                stage.target.display()
            ),
        ))
    }
}

fn rollback_portable_metadata_batch(
    stages: &[MetadataRewriteBatchStage],
    committed: &[usize],
    backups: &[(usize, PathBuf)],
) -> Vec<String> {
    let mut failures = Vec::new();
    for committed_index in committed.iter().rev().copied() {
        let committed_stage = &stages[committed_index];
        let rollback = if let Some(attributes) = committed_stage.original_attributes.as_ref() {
            if let Some(error) = metadata_batch_injected_rollback_failure(committed_index) {
                Err(error)
            } else {
                let backup = backups
                    .iter()
                    .find(|(idx, _)| *idx == committed_index)
                    .map(|(_, path)| path);
                match backup {
                    Some(backup) => {
                        let restore =
                            metadata_batch_temp_path(&committed_stage.target, "metadata-restore");
                        match restore {
                            Ok(restore) => {
                                let restored = (|| -> io::Result<()> {
                                    clone_or_copy_file(backup, &restore)?;
                                    attributes.apply_and_verify(&restore)?;
                                    attributes.verify_guarded_content(&restore)?;
                                    fs::rename(&restore, &committed_stage.target)?;
                                    attributes.verify_applied(&committed_stage.target)?;
                                    attributes.verify_guarded_content(&committed_stage.target)
                                })();
                                if restored.is_err() {
                                    let _ = fs::remove_file(&restore);
                                }
                                restored
                            }
                            Err(error) => Err(error),
                        }
                    }
                    None => Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "metadata batch rollback backup missing",
                    )),
                }
            }
        } else {
            verify_created_stage_at_target(committed_stage)
                .and_then(|()| fs::remove_file(&committed_stage.target))
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

fn commit_metadata_rewrite_batch_portable(
    stages: &mut [MetadataRewriteBatchStage],
) -> io::Result<()> {
    let mut backups = Vec::<(usize, PathBuf)>::new();

    // Prepare complete, independently retained rollback material before the
    // first authoritative target is touched. Any preparation failure removes
    // private copies because no authoritative name has changed yet.
    let prepare = (|| -> io::Result<()> {
        for (index, stage) in stages.iter().enumerate() {
            if let Some(attributes) = stage.original_attributes.as_ref() {
                let backup = metadata_batch_temp_path(&stage.target, "metadata-backup")?;
                let prepared = (|| -> io::Result<()> {
                    if let Some(expected) = attributes.content_sha256 {
                        clone_or_copy_file_matching_digest(&stage.target, &backup, expected)?;
                    } else {
                        clone_or_copy_file(&stage.target, &backup)?;
                    }
                    attributes.apply_and_verify(&backup)
                })();
                if let Err(error) = prepared {
                    let _ = fs::remove_file(&backup);
                    return Err(error);
                }
                backups.push((index, backup));
            }
        }
        Ok(())
    })();
    if let Err(error) = prepare {
        cleanup_portable_metadata_backups(&backups, &[]);
        return Err(error);
    }

    // Reverse commit order keeps the first member's all-source validation
    // adjacent to its publish while still proving the complete set before any
    // authoritative name changes.
    for stage in stages.iter().rev() {
        let validation = if let Some(attributes) = stage.original_attributes.as_ref() {
            attributes.verify_source_unchanged(&stage.target)
        } else {
            match fs::symlink_metadata(&stage.target) {
                Ok(_) => Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "metadata batch create target appeared: {}",
                        stage.target.display()
                    ),
                )),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }
        };
        if let Err(error) = validation {
            cleanup_portable_metadata_backups(&backups, &[]);
            return Err(error);
        }
    }

    let mut committed = Vec::new();
    for index in 0..stages.len() {
        let stage = &stages[index];
        let immediate_validation = if index == 0 {
            // Reverse-order all-source validation made member 0 the freshest
            // preflight check immediately before this loop.
            Ok(())
        } else if let Some(attributes) = stage.original_attributes.as_ref() {
            // On weak-identity mounts this includes a content hash immediately
            // before the atomic namespace replacement.
            attributes.verify_source_unchanged(&stage.target)
        } else {
            Ok(())
        };
        if let Err(error) = immediate_validation {
            let rollback_failures = rollback_portable_metadata_batch(stages, &committed, &backups);
            cleanup_portable_metadata_backups(&backups, &rollback_failures);
            let suffix = if rollback_failures.is_empty() {
                String::new()
            } else {
                format!("; rollback incomplete: {}", rollback_failures.join("; "))
            };
            return Err(io::Error::new(error.kind(), format!("{error}{suffix}")));
        }

        #[cfg(test)]
        if let Err(error) = metadata_test_maybe_create_portable_target(index, stage) {
            let rollback_failures = rollback_portable_metadata_batch(stages, &committed, &backups);
            cleanup_portable_metadata_backups(&backups, &rollback_failures);
            return Err(error);
        }

        let result = if stage.original_attributes.is_some() {
            // Same-directory rename atomically replaces the existing name. The
            // independent backup remains untouched until full success.
            fs::rename(&stage.staged, &stage.target)
        } else {
            // link() is create-only atomically; unlike a check+rename pair it
            // cannot clobber a carrier that appears concurrently. We do not
            // rely on st_nlink, which sshfs may synthesize incorrectly.
            fs::hard_link(&stage.staged, &stage.target)
        };
        if let Err(error) = result {
            let rollback_failures = rollback_portable_metadata_batch(stages, &committed, &backups);
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

        let post_commit = if let Some(attributes) = stage.original_attributes.as_ref() {
            attributes.verify_applied(&stage.target)
        } else {
            verify_created_stage_at_target(stage)
        };
        if let Err(error) = post_commit {
            let rollback_failures = rollback_portable_metadata_batch(stages, &committed, &backups);
            cleanup_portable_metadata_backups(&backups, &rollback_failures);
            let suffix = if rollback_failures.is_empty() {
                String::new()
            } else {
                format!("; rollback incomplete: {}", rollback_failures.join("; "))
            };
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "metadata batch post-commit verification failed for '{}': {error}{suffix}",
                    stage.target.display()
                ),
            ));
        }

        // Reduced-capability/network mounts get a durability boundary after
        // each member so a disconnect cannot leave a later carrier silently
        // ahead of an earlier one. The guarantee remains only as strong as the
        // mount's fsync implementation.
        if let Err(error) = sync_parent_dir(&stage.target) {
            let rollback_failures = rollback_portable_metadata_batch(stages, &committed, &backups);
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

        #[cfg(test)]
        if let Err(error) = metadata_test_maybe_mutate_created_after_commit(index, stage) {
            let rollback_failures = rollback_portable_metadata_batch(stages, &committed, &backups);
            cleanup_portable_metadata_backups(&backups, &rollback_failures);
            return Err(io::Error::new(
                error.kind(),
                format!("metadata batch post-commit fault injection failed: {error}"),
            ));
        }

        if let Some(error) = metadata_batch_injected_commit_failure(committed.len()) {
            let rollback_failures = rollback_portable_metadata_batch(stages, &committed, &backups);
            cleanup_portable_metadata_backups(&backups, &rollback_failures);
            let suffix = if rollback_failures.is_empty() {
                String::new()
            } else {
                format!("; rollback incomplete: {}", rollback_failures.join("; "))
            };
            return Err(io::Error::new(error.kind(), format!("{error}{suffix}")));
        }
    }

    cleanup_portable_metadata_backups(&backups, &[]);
    Ok(())
}

/// Publish a fully staged carrier set transactionally.
///
/// The complete source set is revalidated before the first publication; later
/// existing members are revalidated again immediately before their own publish.
/// Linux keeps the existing `renameat2` exchange/no-replace path only when
/// private probes prove the required flag in every target directory. If any
/// member lacks that capability, the whole batch uses the rollback-backed
/// portable path so all originals exist independently before the first target
/// is mutated.
pub(crate) fn commit_metadata_rewrite_batch(
    mut stages: Vec<MetadataRewriteBatchStage>,
) -> io::Result<()> {
    if stages.is_empty() {
        return Ok(());
    }
    let mut targets = std::collections::BTreeSet::new();
    for stage in &mut stages {
        if !targets.insert(stage.target.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("duplicate metadata batch target: {}", stage.target.display()),
            ));
        }
        validate_staged_batch_file(stage)?;
        if let Some(attributes) = stage.original_attributes.as_ref() {
            attributes.apply_and_verify(&stage.staged)?;
        } else {
            stage.published_sha256 = Some(sha256_file(&stage.staged)?);
        }
    }
    #[cfg(target_os = "linux")]
    let use_strong_linux_publish = {
        let mut supported = true;
        for stage in &stages {
            let probe = if stage.original_attributes.is_some() {
                LinuxRenameProbe::Exchange
            } else {
                LinuxRenameProbe::NoReplace
            };
            if !linux_renameat2_flag_supported(&stage.target, probe)? {
                supported = false;
                break;
            }
        }
        supported
    };

    #[cfg(target_os = "linux")]
    if use_strong_linux_publish {
        // Preserve the established strong-filesystem validation and publish
        // path exactly when every required renameat2 flag was proven.
        // Validate in reverse commit order so member 0 is the freshest
        // full-set check and need not be immediately re-read again.
        for stage in stages.iter().rev() {
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
        commit_metadata_rewrite_batch_linux(&mut stages)?;
    } else {
        commit_metadata_rewrite_batch_portable(&mut stages)?;
    }
    #[cfg(not(target_os = "linux"))]
    commit_metadata_rewrite_batch_portable(&mut stages)?;

    // Strong Linux exchanges leave originals in the stage paths; portable
    // commits retain independent backups. Both keep rollback material through
    // their durability boundary and Drop removes only private artifacts.
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

    struct MetadataCapabilityFaultGuard;

    impl MetadataCapabilityFaultGuard {
        #[cfg(target_os = "linux")]
        fn reduced_linux_mount() -> Self {
            METADATA_FORCE_RENAMEAT2_UNSUPPORTED.with(|slot| slot.set(true));
            METADATA_FORCE_COARSE_TIMESTAMPS.with(|slot| slot.set(true));
            METADATA_XATTR_FAULT.with(|slot| slot.set(MetadataTestXattrFault::UnsupportedAll));
            Self
        }

        fn coarse_timestamps() -> Self {
            METADATA_FORCE_COARSE_TIMESTAMPS.with(|slot| slot.set(true));
            Self
        }

        #[cfg(unix)]
        fn unstable_identity() -> Self {
            METADATA_FORCE_UNSTABLE_IDENTITY.with(|slot| slot.set(true));
            Self
        }

        #[cfg(unix)]
        fn permission_unsupported() -> Self {
            METADATA_FORCE_PERMISSION_UNSUPPORTED.with(|slot| slot.set(true));
            Self
        }

        #[cfg(target_os = "linux")]
        fn xattr_write_unsupported() -> Self {
            METADATA_XATTR_FAULT.with(|slot| slot.set(MetadataTestXattrFault::WriteUnsupported));
            Self
        }

        fn create_target_at_portable_commit(index: usize) {
            METADATA_PORTABLE_CREATE_TARGET_AT_COMMIT.with(|slot| slot.set(Some(index)));
        }

        fn fail_portable_rollback(index: usize) {
            METADATA_FAIL_PORTABLE_ROLLBACK_INDEX.with(|slot| slot.set(Some(index)));
        }

        fn mutate_created_after_commit(index: usize) {
            METADATA_MUTATE_CREATED_AFTER_COMMIT_INDEX.with(|slot| slot.set(Some(index)));
        }
    }

    impl Drop for MetadataCapabilityFaultGuard {
        fn drop(&mut self) {
            METADATA_FORCE_RENAMEAT2_UNSUPPORTED.with(|slot| slot.set(false));
            METADATA_FORCE_COARSE_TIMESTAMPS.with(|slot| slot.set(false));
            METADATA_FORCE_UNSTABLE_IDENTITY.with(|slot| slot.set(false));
            METADATA_FORCE_PERMISSION_UNSUPPORTED.with(|slot| slot.set(false));
            METADATA_PORTABLE_CREATE_TARGET_AT_COMMIT.with(|slot| slot.set(None));
            METADATA_FAIL_PORTABLE_ROLLBACK_INDEX.with(|slot| slot.set(None));
            METADATA_MUTATE_CREATED_AFTER_COMMIT_INDEX.with(|slot| slot.set(None));
            METADATA_XATTR_FAULT.with(|slot| slot.set(MetadataTestXattrFault::None));
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

    #[cfg(target_os = "linux")]
    #[test]
    fn metadata_batch_reduced_mount_falls_back_and_commits_without_xattrs() {
        let _capabilities = MetadataCapabilityFaultGuard::reduced_linux_mount();
        let dir = tempfile::tempdir().expect("metadata reduced mount tempdir");
        let existing = dir.path().join("side-a.cue");
        let created = dir.path().join("side-b.cue");
        fs::write(&existing, b"old side A").expect("write existing carrier");

        let existing_stage =
            stage_existing_metadata_batch_file(&existing).expect("stage without xattrs");
        assert!(
            existing_stage
                .original_attributes
                .as_ref()
                .expect("existing attributes")
                .content_sha256
                .is_some(),
            "reduced identity must use a content guard"
        );
        fs::write(existing_stage.staged_path(), b"new side A").expect("rewrite existing stage");

        let created_stage = stage_new_metadata_batch_file(&created).expect("stage new carrier");
        fs::write(created_stage.staged_path(), b"new side B").expect("write new stage");

        commit_metadata_rewrite_batch(vec![existing_stage, created_stage])
            .expect("reduced-capability batch must commit safely");
        assert_eq!(fs::read(&existing).expect("read existing"), b"new side A");
        assert_eq!(fs::read(&created).expect("read created"), b"new side B");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn metadata_batch_portable_create_race_does_not_clobber_and_rolls_back_earlier_carrier() {
        let _capabilities = MetadataCapabilityFaultGuard::reduced_linux_mount();
        let dir = tempfile::tempdir().expect("metadata reduced mount tempdir");
        let existing = dir.path().join("side-a.cue");
        let created = dir.path().join("side-b.cue");
        fs::write(&existing, b"old side A").expect("write existing carrier");

        let existing_stage =
            stage_existing_metadata_batch_file(&existing).expect("stage existing carrier");
        fs::write(existing_stage.staged_path(), b"new side A").expect("rewrite existing stage");
        let created_stage = stage_new_metadata_batch_file(&created).expect("stage new carrier");
        fs::write(created_stage.staged_path(), b"new side B").expect("write new stage");

        MetadataCapabilityFaultGuard::create_target_at_portable_commit(1);
        let error = commit_metadata_rewrite_batch(vec![existing_stage, created_stage])
            .expect_err("concurrent create must fail without clobbering");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&existing).expect("read restored existing"), b"old side A");
        assert_eq!(
            fs::read(&created).expect("read third-party carrier"),
            b"third-party carrier"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn metadata_batch_created_rollback_preserves_third_party_modification() {
        let _capabilities = MetadataCapabilityFaultGuard::reduced_linux_mount();
        let dir = tempfile::tempdir().expect("metadata created rollback tempdir");
        let existing = dir.path().join("side-a.cue");
        let created = dir.path().join("side-b.cue");
        fs::write(&existing, b"old side A").expect("write existing original");

        let existing_stage =
            stage_existing_metadata_batch_file(&existing).expect("stage existing carrier");
        fs::write(existing_stage.staged_path(), b"new side A").expect("rewrite existing stage");
        let created_stage = stage_new_metadata_batch_file(&created).expect("stage new carrier");
        fs::write(created_stage.staged_path(), b"new side B").expect("write created stage");

        MetadataCapabilityFaultGuard::mutate_created_after_commit(1);
        let _failure = MetadataBatchCommitFailureGuard::after(2);
        let error = commit_metadata_rewrite_batch(vec![existing_stage, created_stage])
            .expect_err("rollback must not remove a third-party-modified created carrier");
        assert!(error.to_string().contains("rollback incomplete"));
        assert_eq!(fs::read(&existing).expect("read restored existing"), b"old side A");
        assert_eq!(
            fs::read(&created).expect("read modified created carrier"),
            b"third-party modified carrier"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn metadata_batch_failed_portable_rollback_retains_independent_backup() {
        let _capabilities = MetadataCapabilityFaultGuard::reduced_linux_mount();
        let dir = tempfile::tempdir().expect("metadata rollback retention tempdir");
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

        MetadataCapabilityFaultGuard::fail_portable_rollback(0);
        let _failure = MetadataBatchCommitFailureGuard::after(1);
        let error = commit_metadata_rewrite_batch(vec![first_stage, second_stage])
            .expect_err("failed rollback must retain independent backup material");
        assert!(error.to_string().contains("rollback incomplete"));
        assert_eq!(fs::read(&first).expect("read unrolled first carrier"), b"new side A");
        assert_eq!(fs::read(&second).expect("read untouched second carrier"), b"old side B");

        let retained = fs::read_dir(dir.path())
            .expect("list rollback directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().contains("tonepoet-metadata-backup"))
            })
            .collect::<Vec<_>>();
        assert!(!retained.is_empty(), "failed rollback must retain its backups");
        assert!(
            retained
                .iter()
                .any(|path| fs::read(path).is_ok_and(|bytes| bytes == b"old side A")),
            "retained rollback material must contain the original carrier bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn metadata_batch_never_publishes_wider_mode_when_chmod_is_unsupported() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("metadata permissions tempdir");
        let target = dir.path().join("side-a.cue");
        fs::write(&target, b"old side A").expect("write target");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640))
            .expect("set original mode");
        let stage = stage_existing_metadata_batch_file(&target).expect("stage target");
        fs::write(stage.staged_path(), b"new side A").expect("rewrite stage");
        fs::set_permissions(stage.staged_path(), fs::Permissions::from_mode(0o777))
            .expect("simulate permissive mount default");

        let _permission_fault = MetadataCapabilityFaultGuard::permission_unsupported();
        let error = commit_metadata_rewrite_batch(vec![stage])
            .expect_err("wider permissions must fail before publication");
        assert!(error.to_string().contains("wider permissions"));
        let metadata = fs::symlink_metadata(&target).expect("stat original target");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
        assert_eq!(fs::read(&target).expect("read original target"), b"old side A");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn metadata_batch_restricting_security_xattr_loss_is_surfaced_before_publish() {
        let dir = tempfile::tempdir().expect("metadata xattr tempdir");
        let target = dir.path().join("side-a.cue");
        fs::write(&target, b"old side A").expect("write target");
        let mut stage = stage_existing_metadata_batch_file(&target).expect("stage target");
        fs::write(stage.staged_path(), b"new side A").expect("rewrite stage");
        stage
            .original_attributes
            .as_mut()
            .expect("existing attributes")
            .xattrs = LinuxXattrSnapshot::Captured(vec![(
            std::ffi::OsString::from("security.tonepoet-test"),
            b"restricted".to_vec(),
        )]);

        let _xattr_fault = MetadataCapabilityFaultGuard::xattr_write_unsupported();
        let error = commit_metadata_rewrite_batch(vec![stage])
            .expect_err("restricting xattr loss must be surfaced");
        assert!(error
            .to_string()
            .contains("restricting extended attributes or ACLs"));
        assert_eq!(fs::read(&target).expect("read original target"), b"old side A");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn metadata_batch_preserves_exact_xattrs_when_supported() {
        let dir = tempfile::tempdir().expect("metadata strong xattr tempdir");
        let target = dir.path().join("side-a.cue");
        fs::write(&target, b"old side A").expect("write target");
        if let Err(error) = set_test_xattr(&target, "user.tonepoet-batch", b"preserve-me") {
            if xattr_unsupported(&error) {
                return;
            }
            panic!("set batch xattr: {error}");
        }
        let before = linux_xattrs(&target).expect("read batch xattrs before save");

        let stage = stage_existing_metadata_batch_file(&target).expect("stage target");
        fs::write(stage.staged_path(), b"new side A").expect("rewrite stage");
        commit_metadata_rewrite_batch(vec![stage]).expect("commit xattr-preserving batch");

        assert_eq!(linux_xattrs(&target).expect("read batch xattrs after save"), before);
        assert_eq!(fs::read(&target).expect("read rewritten target"), b"new side A");
    }

    #[cfg(unix)]
    #[test]
    fn metadata_batch_coarse_timestamp_guard_detects_same_length_content_change() {
        let _coarse = MetadataCapabilityFaultGuard::coarse_timestamps();
        let dir = tempfile::tempdir().expect("metadata coarse timestamp tempdir");
        let target = dir.path().join("side-a.cue");
        fs::write(&target, b"old-data").expect("write original target");
        let original_modified = fs::symlink_metadata(&target)
            .expect("stat original target")
            .modified()
            .expect("original mtime");
        let stage = stage_existing_metadata_batch_file(&target).expect("stage target");
        assert!(
            stage
                .original_attributes
                .as_ref()
                .expect("existing attributes")
                .content_sha256
                .is_some(),
            "coarse timestamp capability must enable content verification"
        );
        fs::write(stage.staged_path(), b"new-data").expect("rewrite stage");

        fs::write(&target, b"bad-data").expect("same-length concurrent write");
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&target)
            .expect("open changed target")
            .set_times(fs::FileTimes::new().set_modified(original_modified))
            .expect("restore coarse-visible mtime");

        let error = commit_metadata_rewrite_batch(vec![stage])
            .expect_err("content fingerprint must detect same-length change");
        assert!(error.to_string().contains("content fingerprint changed"));
        assert_eq!(fs::read(&target).expect("read external target"), b"bad-data");
    }

    #[cfg(unix)]
    #[test]
    fn metadata_batch_unstable_inode_capability_uses_content_identity_instead() {
        let _identity = MetadataCapabilityFaultGuard::unstable_identity();
        let dir = tempfile::tempdir().expect("metadata unstable identity tempdir");
        let target = dir.path().join("side-a.cue");
        fs::write(&target, b"old side A").expect("write target");
        let mut stage = stage_existing_metadata_batch_file(&target).expect("stage target");
        let attributes = stage
            .original_attributes
            .as_mut()
            .expect("existing attributes");
        assert!(attributes.content_sha256.is_some());
        attributes.device = attributes.device.wrapping_add(1);
        attributes.inode = attributes.inode.wrapping_add(1);
        fs::write(stage.staged_path(), b"new side A").expect("rewrite stage");

        commit_metadata_rewrite_batch(vec![stage])
            .expect("content identity must tolerate synthesized inode drift");
        assert_eq!(fs::read(&target).expect("read target"), b"new side A");
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
