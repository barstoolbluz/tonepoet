//! Helpers for re-executing the running Tonepoet binary.

use std::io;
use std::path::PathBuf;

/// Resolve a path that re-executes the same Tonepoet image that is currently
/// running.
///
/// On Linux, `current_exe()` resolves `/proc/self/exe` to the executable's
/// original pathname. After an atomic rebuild or package upgrade unlinks that
/// inode, the kernel reports the resolved path with a `" (deleted)"` suffix.
/// That string is diagnostic only and cannot be passed back to `execve(2)`.
/// Re-executing the procfs magic link instead keeps using the still-live inode,
/// so parent and helper remain on exactly the same binary/protocol version.
pub(crate) fn current_executable_for_reexec() -> io::Result<PathBuf> {
    let current = std::env::current_exe()?;
    Ok(reexec_path_for_current(current))
}

#[cfg(target_os = "linux")]
fn reexec_path_for_current(current: PathBuf) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;

    if current.as_os_str().as_bytes().ends_with(b" (deleted)") {
        PathBuf::from("/proc/self/exe")
    } else {
        current
    }
}

#[cfg(not(target_os = "linux"))]
fn reexec_path_for_current(current: PathBuf) -> PathBuf {
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_current_executable_path_is_preserved() {
        let current = PathBuf::from("/tmp/tonepoet");
        assert_eq!(reexec_path_for_current(current.clone()), current);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn deleted_linux_executable_uses_proc_self_exe() {
        let current = PathBuf::from("/tmp/tonepoet (deleted)");
        assert_eq!(
            reexec_path_for_current(current),
            PathBuf::from("/proc/self/exe")
        );
    }
}
