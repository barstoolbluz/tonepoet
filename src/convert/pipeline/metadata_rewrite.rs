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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

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
