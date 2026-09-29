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
