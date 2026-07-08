//! File classification shared by the conversion queue planner and TUI browse views.

use std::path::Path;

use crate::convert::formats::AudioFormat;

/// Domain classification for a filesystem entry that may be shown or queued
#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    /// `..` entry (parent directory).
    ///
    /// Presentation-only: Browse constructs this variant for its listing
    /// rows. `classify_file` never returns it and queue expansion never
    /// consumes it — it lives here only so Browse's entry model and the
    /// domain classification share one enum.
    ParentDir,
    /// A subdirectory
    Directory,
    /// An audio file (format detected from extension)
    AudioFile(AudioFormat),
    /// A 7z archive (or similar)
    Archive,
    /// SACD ISO image (Super Audio CD). Detected via ScarletBook
    /// magic-byte probe at LSN 510/520/530, not by extension alone
    /// (some `.iso` files are DVD-V or generic ISO9660). Population
    /// happens lazily after settled focus or explicit actions, keyed by
    /// (path, mtime) against `BrowseState.sacd_classify_cache`.
    SacdIso,
    /// DVD-Audio ISO image. Lightweight classification happens lazily after
    /// settled focus and is cached by path + mtime + len.
    DvdAudioIso,
    /// Filesystem DVD-Audio directory (contains AUDIO_TS/AUDIO_TS.IFO).
    DvdAudioDir,
    /// DVD-Video ISO image. Hybrid DVD-Audio/DVD-Video ISOs remain DVD-Audio.
    DvdVideoIso,
    /// Filesystem DVD-Video directory (contains VIDEO_TS/VIDEO_TS.IFO and no
    /// non-empty AUDIO_TS DVD-Audio root).
    DvdVideoDir,
    /// Blu-ray ISO image.
    BlurayIso,
    /// Filesystem Blu-ray directory (contains BDMV/).
    BlurayDir,
    /// Any other file
    OtherFile,
}


/// Return true when `path` names a CUE sheet.
pub fn is_cue_sheet_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("cue"))
        .unwrap_or(false)
}

/// Return true when `path` is classified as an audio file by extension.
pub fn is_audio_file_path(path: &Path) -> bool {
    matches!(classify_file(path), EntryKind::AudioFile(_))
}

pub fn classify_file(path: &Path) -> EntryKind {
    // Check for double-extension archives first (e.g., .tar.gz).
    if is_tar_compound(path) {
        return EntryKind::Archive;
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    match ext.as_deref() {
        Some("flac") => EntryKind::AudioFile(AudioFormat::Flac),
        Some("wav") | Some("wave") => EntryKind::AudioFile(AudioFormat::Wav),
        Some("aiff") | Some("aif") | Some("aifc") => EntryKind::AudioFile(AudioFormat::Aiff),
        Some("wv") => EntryKind::AudioFile(AudioFormat::WavPack),
        Some("mp3") => EntryKind::AudioFile(AudioFormat::Mp3),
        Some("m4a") | Some("mp4") | Some("aac") => EntryKind::AudioFile(AudioFormat::Aac),
        Some("opus") => EntryKind::AudioFile(AudioFormat::Opus),
        Some("7z") | Some("zip") | Some("rar") | Some("tar") | Some("iso") | Some("cab")
        | Some("dmg") | Some("tgz") | Some("tbz2") | Some("txz") => EntryKind::Archive,
        _ => EntryKind::OtherFile,
    }
}

/// Public accessor for compound tar check (used by keybindings file-routing).
pub fn is_tar_compound_pub(path: &Path) -> bool {
    is_tar_compound(path)
}

/// Check for compound tar extensions (.tar.gz, .tar.bz2, .tar.xz, .tar.zst).
/// `Path::extension()` only returns the last component, so "file.tar.gz"
/// gives "gz" which would be classified as OtherFile without this check.
pub(crate) fn is_tar_compound(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_lowercase())
        .unwrap_or_default();
    name.ends_with(".tar.gz")
        || name.ends_with(".tar.bz2")
        || name.ends_with(".tar.xz")
        || name.ends_with(".tar.zst")
        || name.ends_with(".tar.lz")
        || name.ends_with(".tar.lzma")
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_file_maps_supported_audio_extensions_case_insensitively() {
        assert_eq!(classify_file(Path::new("track.FLAC")), EntryKind::AudioFile(AudioFormat::Flac));
        assert_eq!(classify_file(Path::new("track.wave")), EntryKind::AudioFile(AudioFormat::Wav));
        assert_eq!(classify_file(Path::new("track.AIFC")), EntryKind::AudioFile(AudioFormat::Aiff));
        assert_eq!(classify_file(Path::new("track.wv")), EntryKind::AudioFile(AudioFormat::WavPack));
        assert_eq!(classify_file(Path::new("track.MP3")), EntryKind::AudioFile(AudioFormat::Mp3));
        assert_eq!(classify_file(Path::new("track.m4a")), EntryKind::AudioFile(AudioFormat::Aac));
        assert_eq!(classify_file(Path::new("track.OPUS")), EntryKind::AudioFile(AudioFormat::Opus));
    }

    #[test]
    fn classify_file_treats_supported_container_extensions_as_archives() {
        for name in [
            "album.7z",
            "album.zip",
            "album.rar",
            "album.tar",
            "album.iso",
            "album.cab",
            "album.dmg",
            "album.tgz",
            "album.tbz2",
            "album.txz",
        ] {
            assert_eq!(classify_file(Path::new(name)), EntryKind::Archive, "{name}");
        }
    }

    #[test]
    fn classify_file_detects_compound_tar_archives_before_final_extension() {
        for name in [
            "album.tar.gz",
            "album.tar.bz2",
            "album.tar.xz",
            "album.tar.zst",
            "album.tar.lz",
            "album.tar.lzma",
        ] {
            let path = Path::new(name);
            assert!(is_tar_compound(path), "{name}");
            assert!(is_tar_compound_pub(path), "{name}");
            assert_eq!(classify_file(path), EntryKind::Archive, "{name}");
        }

        assert!(!is_tar_compound(Path::new("album.gz")));
        assert_eq!(classify_file(Path::new("album.gz")), EntryKind::OtherFile);
    }

    #[test]
    fn cue_detection_is_case_insensitive_but_classification_remains_other_file() {
        let cue = Path::new("Album.CUE");
        assert!(is_cue_sheet_path(cue));
        assert_eq!(classify_file(cue), EntryKind::OtherFile);
        assert!(!is_audio_file_path(cue));
    }

    #[test]
    fn unknown_or_missing_extensions_classify_as_other_file() {
        assert_eq!(classify_file(Path::new("README")), EntryKind::OtherFile);
        assert_eq!(classify_file(Path::new("notes.txt")), EntryKind::OtherFile);
        assert!(!is_cue_sheet_path(Path::new("notes.txt")));
    }
}

