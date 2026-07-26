//! Authoritative conversion-source admission policy.
//!
//! Every UI and queue entrance that accepts a concrete source path must route
//! through this module. Browse classification remains responsible for naming
//! the file kind; this module decides whether that kind is a supported direct
//! conversion source and which source workflow owns it.

use std::path::Path;

use crate::convert::classify::{classify_file, is_cue_sheet_path, EntryKind};

/// Supported direct-source workflow selected at the admission boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectSourceKind {
    /// A directly probeable audio file.
    Audio,
    /// A CUE control file whose referenced media is resolved by the CUE path.
    Cue,
    /// A supported archive container that enters the extract-and-preview path.
    ArchivePreview,
    /// A positively identified SACD/DVD-A/DVD-V/Blu-ray image.
    DiscImage,
}

/// Classify `path` at the single direct-source admission boundary.
///
/// All archive extensions recognized by [`classify_file`] are supported
/// archive-preview sources except `.iso`. ISO is intentionally special: it is
/// admitted only after the existing lightweight disc probes positively identify
/// a supported disc-image format. This preserves generic-ISO rejection without
/// splitting archive policy across UI entrances.
#[must_use]
pub fn direct_source_kind(path: &Path) -> Option<DirectSourceKind> {
    if is_cue_sheet_path(path) {
        return Some(DirectSourceKind::Cue);
    }

    match classify_file(path) {
        EntryKind::AudioFile(_) => Some(DirectSourceKind::Audio),
        EntryKind::Archive if is_iso_path(path) => {
            is_supported_disc_image_source(path).then_some(DirectSourceKind::DiscImage)
        }
        EntryKind::Archive => Some(DirectSourceKind::ArchivePreview),
        EntryKind::SacdIso
        | EntryKind::DvdAudioIso
        | EntryKind::DvdVideoIso
        | EntryKind::BlurayIso => Some(DirectSourceKind::DiscImage),
        EntryKind::ParentDir
        | EntryKind::Directory
        | EntryKind::DvdAudioDir
        | EntryKind::DvdVideoDir
        | EntryKind::BlurayDir
        | EntryKind::OtherFile => None,
    }
}

/// Return true when `path` is a supported concrete file source.
#[must_use]
pub fn is_direct_queue_source_path(path: &Path) -> bool {
    direct_source_kind(path).is_some()
}

/// Return true when `path` is admitted specifically to the archive-preview
/// workflow. Callers must not maintain a second extension list.
#[must_use]
pub fn is_archive_preview_source_path(path: &Path) -> bool {
    matches!(classify_file(path), EntryKind::Archive) && !is_iso_path(path)
}

fn is_iso_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("iso"))
}

fn is_supported_disc_image_source(path: &Path) -> bool {
    crate::convert::sacd::is_sacd_iso(path)
        || crate::disc::dvda_utils::is_dvda_iso(path)
        || crate::disc::dvdv_utils::is_dvdv_iso(path)
        || crate::disc::bluray_utils::is_bluray_iso(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUPPORTED_ARCHIVE_PREVIEW_NAMES: &[&str] = &[
        "album.7z",
        "album.zip",
        "album.rar",
        "album.tar",
        "album.cab",
        "album.dmg",
        "album.tgz",
        "album.tbz2",
        "album.txz",
        "album.tar.gz",
        "album.tar.bz2",
        "album.tar.xz",
        "album.tar.zst",
        "album.tar.lz",
        "album.tar.lzma",
    ];

    #[test]
    fn direct_queue_policy_admits_supported_audio_inputs() {
        for name in [
            "album.flac",
            "album.wav",
            "album.aiff",
            "album.wv",
            "album.ape",
            "album.dsf",
            "album.dff",
            "album.shn",
            "album.ogg",
            "album.oga",
            "album.tta",
            "album.mp3",
            "album.m4a",
            "album.alac",
            "album.opus",
            "album.w64",
            "album.rf64",
        ] {
            assert_eq!(
                direct_source_kind(Path::new(name)),
                Some(DirectSourceKind::Audio),
                "{name}"
            );
        }
    }

    #[test]
    fn direct_queue_policy_admits_cue() {
        for name in ["album.cue", "album.CUE"] {
            assert_eq!(
                direct_source_kind(Path::new(name)),
                Some(DirectSourceKind::Cue),
                "{name}"
            );
        }
    }

    #[test]
    fn every_classifier_supported_archive_enters_archive_preview() {
        for name in SUPPORTED_ARCHIVE_PREVIEW_NAMES {
            let path = Path::new(name);
            assert_eq!(classify_file(path), EntryKind::Archive, "{name}");
            assert_eq!(
                direct_source_kind(path),
                Some(DirectSourceKind::ArchivePreview),
                "{name}"
            );
            assert!(is_direct_queue_source_path(path), "{name}");
            assert!(is_archive_preview_source_path(path), "{name}");
        }
    }

    #[test]
    fn unsupported_files_are_rejected() {
        for name in ["notes.txt", "cover.jpg", "README", "album.gz"] {
            assert_eq!(direct_source_kind(Path::new(name)), None, "{name}");
            assert!(!is_direct_queue_source_path(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn supported_sacd_iso_is_admitted_as_disc_image_after_real_probe() {
        use std::io::{Seek, SeekFrom, Write};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.iso");
        const SECTOR_SIZE: u64 = 2_048;
        const MASTER_TOC_LSN: u64 = 510;
        const MASTER_TOC_MAGIC: &[u8; 8] = b"SACDMTOC";
        let mut file = std::fs::File::create(&path).expect("ISO fixture");
        file.set_len((MASTER_TOC_LSN + 1) * SECTOR_SIZE)
            .expect("size ISO fixture");
        file.seek(SeekFrom::Start(MASTER_TOC_LSN * SECTOR_SIZE))
            .expect("seek ISO fixture");
        file.write_all(MASTER_TOC_MAGIC)
            .expect("write SACD magic");
        drop(file);

        assert_eq!(
            direct_source_kind(&path),
            Some(DirectSourceKind::DiscImage)
        );
        assert!(!is_archive_preview_source_path(&path));
    }

    #[test]
    fn generic_iso_is_not_a_direct_source_without_supported_disc_probe() {
        let path = Path::new("album.iso");
        assert_eq!(direct_source_kind(path), None);
        assert!(!is_direct_queue_source_path(path));
        assert!(!is_archive_preview_source_path(path));
    }
}
