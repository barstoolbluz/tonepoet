//! Compact, deterministic names for publish/action coordination artifacts.
//!
//! User-visible album components may legitimately approach the filesystem's
//! component limit. Internal siblings must therefore never be formed by
//! prefixing/suffixing that component. A fixed-size SHA-256-derived token keeps
//! lock, marker, temp, and backup names comfortably below every supported
//! filesystem limit while remaining stable for a given album path.

use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::path::Path;

const TOKEN_HEX_CHARS: usize = 24;

fn os_str_bytes(value: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        value.to_string_lossy().as_bytes().to_vec()
    }
}

#[must_use]
pub(super) fn component_coordination_token(component: &OsStr) -> String {
    let digest = hex::encode(Sha256::digest(os_str_bytes(component)));
    digest[..TOKEN_HEX_CHARS].to_owned()
}

#[must_use]
pub(super) fn album_coordination_token(album_dir: &Path) -> String {
    album_dir
        .file_name()
        .map(component_coordination_token)
        .unwrap_or_else(|| component_coordination_token(album_dir.as_os_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn token_is_fixed_size_stable_and_discriminating() {
        let a = component_coordination_token(OsStr::new("A very long album name"));
        let a_again = component_coordination_token(OsStr::new("A very long album name"));
        let b = component_coordination_token(OsStr::new("A different album name"));
        assert_eq!(a.len(), TOKEN_HEX_CHARS);
        assert_eq!(a, a_again);
        assert_ne!(a, b);
        assert!(a.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}
