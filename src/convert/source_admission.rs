//! Conversion-queue source admission policy.
//!
//! This module is the single source of truth for direct file inputs that may be
//! admitted to the conversion queue. It is intentionally narrower than Browse
//! classification: Browse may classify many archive-looking files as
//! `EntryKind::Archive` for navigation or contextual UI, but queue admission
//! only accepts concrete sources the queue/pipeline can actually process.

use std::path::Path;

use crate::convert::classify::{classify_file, is_cue_sheet_path, EntryKind};

/// Return true when `path` names a concrete file source that the conversion
/// queue can accept directly.
///
/// This is a source-admission predicate, not a broad file classifier. It admits
/// classifier-backed audio files, CUE control files, supported queue archive
/// containers, and disc-image sources only after the same lightweight probes the
/// queue path relies on have identified them as supported disc formats.
#[must_use]
pub fn is_direct_queue_source_path(path: &Path) -> bool {
    if is_cue_sheet_path(path) {
        return true;
    }

    match classify_file(path) {
        EntryKind::AudioFile(_) => true,
        EntryKind::Archive => is_supported_queue_archive_or_disc_image(path),
        _ => false,
    }
}

fn is_supported_queue_archive_or_disc_image(path: &Path) -> bool {
    let Some(ext) = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
    else {
        return false;
    };

    match ext.as_str() {
        // The conversion pipeline has an explicit 7z extraction/materialization
        // path, so 7z remains a direct queue source.
        "7z" => true,
        // Other archive-looking files are Browse/navigation concepts, not queue
        // sources. A bare ISO becomes queueable only once a supported disc-image
        // probe succeeds.
        "iso" => is_supported_disc_image_source(path),
        _ => false,
    }
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
            assert!(is_direct_queue_source_path(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn direct_queue_policy_admits_cue_and_supported_queue_archives() {
        for name in ["album.cue", "album.CUE", "album.7z", "album.7Z"] {
            assert!(is_direct_queue_source_path(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn direct_queue_policy_rejects_broad_browse_archives_that_are_not_queue_sources() {
        for name in [
            "album.zip",
            "album.rar",
            "album.tar",
            "album.tar.gz",
            "album.tar.bz2",
            "album.dmg",
            "album.cab",
            "notes.txt",
            "README",
        ] {
            assert!(!is_direct_queue_source_path(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn generic_iso_is_not_queueable_without_supported_disc_probe() {
        assert!(!is_direct_queue_source_path(Path::new("album.iso")));
    }
}
