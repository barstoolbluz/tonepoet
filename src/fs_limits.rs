//! Filesystem component-length discovery used by output naming preflight.
//!
//! Tonepoet must decide whether a rendered user-visible component fits before
//! any audio work begins.  POSIX filesystems expose that limit through
//! `pathconf(_PC_NAME_MAX)`, but the requested output root may not exist yet, so
//! discovery walks to the nearest existing ancestor.  Failure is deliberately
//! conservative: 255 bytes is the ubiquitous ext4/APFS-style component limit
//! and matches Tonepoet's historical practical ceiling.

use std::path::Path;

pub(crate) const CONSERVATIVE_NAME_MAX_BYTES: usize = 255;

#[must_use]
pub(crate) fn name_max_bytes_for(path: &Path) -> usize {
    platform_name_max_bytes_for(path).unwrap_or(CONSERVATIVE_NAME_MAX_BYTES)
}

/// A non-destructive, human-readable sibling for a pre-existing album folder.
/// The suffix changes only the final folder component, never track filenames.
/// Recheck existence at publication: planning cannot reserve a namespace.
pub(crate) fn keep_both_album_folder(base: &Path) -> std::io::Result<std::path::PathBuf> {
    use std::io;
    let parent = base.parent().ok_or_else(|| io::Error::new(
        io::ErrorKind::InvalidInput, "album has no parent directory",
    ))?;
    let name = base.file_name().and_then(|name| name.to_str()).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "album folder must be valid UTF-8")
    })?;
    let exists = |path: &Path| -> io::Result<bool> {
        match std::fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        }
    };
    if !exists(base)? { return Ok(base.to_path_buf()); }
    let name_max = name_max_bytes_for(parent);
    for index in 2..=100_000_u32 {
        let suffix = format!(" ({index})");
        let available = name_max.checked_sub(suffix.len()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "folder component limit too short for keep-both suffix")
        })?;
        let mut stem = name;
        while stem.len() > available {
            let Some((boundary, _)) = stem.char_indices().last() else { break };
            stem = &stem[..boundary];
        }
        let stem = stem.trim_end_matches(|ch: char| ch == ' ' || ch == '.');
        if stem.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "folder has no room for suffix"));
        }
        let candidate = parent.join(format!("{stem}{suffix}"));
        if !exists(&candidate)? { return Ok(candidate); }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "too many matching album folders"))
}

#[cfg(unix)]
fn platform_name_max_bytes_for(path: &Path) -> Option<usize> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let mut probe = Some(path);
    while let Some(candidate) = probe {
        if candidate.exists() {
            let bytes = candidate.as_os_str().as_bytes();
            let c_path = CString::new(bytes).ok()?;
            // SAFETY: `c_path` is NUL-terminated and lives for the duration of
            // the call. `pathconf` neither retains nor mutates the pointer.
            let value = unsafe { libc::pathconf(c_path.as_ptr(), libc::_PC_NAME_MAX) };
            if value > 0 {
                let value = usize::try_from(value).ok()?;
                // Reject implausible/corrupt answers rather than allowing a
                // bogus mount response to disable preflight protection.
                if (16..=1_048_576).contains(&value) {
                    return Some(value);
                }
            }
            return None;
        }
        probe = candidate.parent();
    }
    None
}

#[cfg(not(unix))]
fn platform_name_max_bytes_for(_path: &Path) -> Option<usize> {
    // Supported production targets are Unix. Keep other targets compiling and
    // fail conservatively until a native component-limit query is commissioned.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_both_siblings_are_numbered_and_utf8_component_safe() {
        let scratch = tempfile::tempdir().expect("keep-both scratch");
        let name = "界".repeat(84); // 252 UTF-8 bytes, before a numeric suffix
        let original = scratch.path().join(&name);
        std::fs::create_dir(&original).expect("original album");
        let second = keep_both_album_folder(&original).expect("Album (2)");
        assert!(second.file_name().unwrap().to_str().unwrap().ends_with(" (2)"));
        assert!(second.file_name().unwrap().to_str().unwrap().len()
            <= name_max_bytes_for(scratch.path()));
        std::fs::create_dir(&second).expect("reserve (2)");
        let third = keep_both_album_folder(&original).expect("Album (3)");
        assert_ne!(second, third);
        assert!(third.file_name().unwrap().to_str().unwrap().ends_with(" (3)"));
        assert!(third.file_name().unwrap().to_str().unwrap().len()
            <= name_max_bytes_for(scratch.path()));
        assert!(second.starts_with(scratch.path()));
        assert!(third.starts_with(scratch.path()));
        assert_eq!(keep_both_album_folder(&scratch.path().join("brand-new")).unwrap(),
            scratch.path().join("brand-new"));
    }

    #[test]
    fn name_max_is_sane_for_existing_directory() {
        let value = name_max_bytes_for(Path::new("."));
        assert!((16..=1_048_576).contains(&value));
    }

    #[test]
    fn nonexistent_descendant_uses_existing_ancestor() {
        let value = name_max_bytes_for(Path::new("./this/path/does/not/exist"));
        assert!((16..=1_048_576).contains(&value));
    }
}
