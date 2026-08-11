//! Conversion-planning filesystem identity helpers.
//!
//! Identity and presentation ordering have different contracts. Filesystem
//! identity must preserve the case semantics of the filesystem that owns an
//! existing path; deterministic sort/display keys may case-fold separately.

use std::path::{Component, Path, PathBuf};

/// Derive a deterministic, serde-safe key for a conversion-planning path.
///
/// Existing paths are canonicalized so aliases (including symlinks and
/// relative components) collapse to the filesystem's actual identity. Planned
/// paths that do not exist yet are normalized lexically without case-folding.
/// The lossy string representation preserves the existing serialized key
/// contract while avoiding platform-specific `Path` hashing.
pub(crate) fn filesystem_identity_key(path: &Path) -> String {
    let identity = path
        .canonicalize()
        .unwrap_or_else(|_| lexical_normalize_path(path));
    identity.to_string_lossy().into_owned()
}

fn lexical_normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(normalized.components().next_back(), Some(Component::Normal(_))) {
                    normalized.pop();
                } else if !normalized.has_root() {
                    // Preserve unresolved leading parents for relative paths
                    // instead of silently turning `../x` into `x`.
                    normalized.push("..");
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }

    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prospective_identity_is_lexical_and_case_preserving() {
        let base = tempfile::tempdir().expect("temp dir");
        let upper = base.path().join("out").join("Album").join(".").join("Disc");
        let lower = base.path().join("out").join("album").join("Disc");

        assert!(!upper.exists());
        assert!(!lower.exists());
        assert_ne!(filesystem_identity_key(&upper), filesystem_identity_key(&lower));
        assert_eq!(
            filesystem_identity_key(&base.path().join("out").join("Album").join("x").join("..").join("Disc")),
            filesystem_identity_key(&upper)
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_symlink_aliases_collapse_to_one_identity() {
        use std::os::unix::fs::symlink;

        let base = tempfile::tempdir().expect("temp dir");
        let real = base.path().join("Album");
        std::fs::create_dir(&real).expect("real directory");
        let track = real.join("01.flac");
        std::fs::write(&track, b"identity fixture").expect("track fixture");
        let alias = base.path().join("alias");
        symlink(&real, &alias).expect("symlink alias");

        assert_eq!(
            filesystem_identity_key(&track),
            filesystem_identity_key(&alias.join("01.flac"))
        );
    }
}
