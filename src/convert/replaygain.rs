//! Shared ReplayGain command and post-scan policy.
//!
//! The conversion pipeline and the metadata editor must invoke `loudgain`
//! with the same argument semantics. Track-only scans also share one cleanup
//! path for inherited album-level tags so the same file cannot acquire
//! different ReplayGain state depending on which UI initiated the scan.

use std::io;
use std::path::{Path, PathBuf};

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::ItemKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoudgainGrouping {
    Track,
    Album,
}

/// Build the canonical `loudgain` argv used by every caller.
#[must_use]
pub(crate) fn loudgain_args(
    grouping: LoudgainGrouping,
    prevent_clipping: bool,
    paths: &[PathBuf],
) -> Vec<String> {
    let mut args = Vec::with_capacity(paths.len().saturating_add(4));
    if grouping == LoudgainGrouping::Album {
        args.push("-a".to_string());
    }
    if prevent_clipping {
        args.push("-k".to_string());
    }
    args.push("-s".to_string());
    args.push("i".to_string());
    args.extend(paths.iter().map(|path| path.to_string_lossy().into_owned()));
    args
}

/// Remove stale album-level ReplayGain tags after a track-only scan.
///
/// Files without either album tag are not rewritten. Each changed file is read
/// once and rewritten once through Lofty; callers decide how to surface errors.
pub(crate) fn remove_stale_album_tags(paths: &[PathBuf]) -> io::Result<()> {
    for path in paths {
        remove_stale_album_tags_from_path(path)?;
    }
    Ok(())
}

fn remove_stale_album_tags_from_path(path: &Path) -> io::Result<()> {
    let mut tagged = lofty::read_from_path(path).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("read ReplayGain output '{}': {error}", path.display()),
        )
    })?;
    let Some(tag) = tagged.primary_tag_mut() else {
        return Ok(());
    };
    let has_album_gain = tag
        .get_string(&ItemKey::ReplayGainAlbumGain)
        .is_some();
    let has_album_peak = tag
        .get_string(&ItemKey::ReplayGainAlbumPeak)
        .is_some();
    if !has_album_gain && !has_album_peak {
        return Ok(());
    }
    tag.remove_key(&ItemKey::ReplayGainAlbumGain);
    tag.remove_key(&ItemKey::ReplayGainAlbumPeak);
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "remove stale album ReplayGain tags from '{}': {error}",
                    path.display()
                ),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loudgain_args_honor_grouping_and_clipping_policy() {
        let paths = vec![
            PathBuf::from("/music/01.flac"),
            PathBuf::from("/music/02.flac"),
        ];

        assert_eq!(
            loudgain_args(LoudgainGrouping::Album, true, &paths),
            vec![
                "-a".to_string(),
                "-k".to_string(),
                "-s".to_string(),
                "i".to_string(),
                "/music/01.flac".to_string(),
                "/music/02.flac".to_string(),
            ]
        );
        assert_eq!(
            loudgain_args(LoudgainGrouping::Track, false, &paths),
            vec![
                "-s".to_string(),
                "i".to_string(),
                "/music/01.flac".to_string(),
                "/music/02.flac".to_string(),
            ]
        );
    }

    #[test]
    fn track_cleanup_removes_only_album_level_tags() {
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::tag::{ItemValue, TagItem};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("track.flac");
        std::fs::write(&path, include_bytes!("../../tests/fixtures/silence.flac"))
            .expect("copy FLAC fixture");

        let mut tagged = lofty::read_from_path(&path).expect("read fixture");
        if tagged.primary_tag().is_none() {
            let tag_type = tagged.primary_tag_type();
            tagged.insert_tag(lofty::tag::Tag::new(tag_type));
        }
        let tag = tagged.primary_tag_mut().expect("fixture primary tag");
        for (key, value) in [
            (ItemKey::ReplayGainTrackGain, "-7.25 dB"),
            (ItemKey::ReplayGainTrackPeak, "0.9231"),
            (ItemKey::ReplayGainAlbumGain, "-6.80 dB"),
            (ItemKey::ReplayGainAlbumPeak, "0.9772"),
        ] {
            tag.insert_unchecked(TagItem::new(key, ItemValue::Text(value.to_string())));
        }
        tagged
            .save_to_path(&path, WriteOptions::default())
            .expect("seed ReplayGain tags");

        remove_stale_album_tags(std::slice::from_ref(&path)).expect("remove album tags");

        let tagged = lofty::read_from_path(&path).expect("read cleaned fixture");
        let tag = tagged.primary_tag().expect("cleaned primary tag");
        assert_eq!(
            tag.get_string(&ItemKey::ReplayGainTrackGain),
            Some("-7.25 dB")
        );
        assert_eq!(
            tag.get_string(&ItemKey::ReplayGainTrackPeak),
            Some("0.9231")
        );
        assert!(tag.get_string(&ItemKey::ReplayGainAlbumGain).is_none());
        assert!(tag.get_string(&ItemKey::ReplayGainAlbumPeak).is_none());
    }

}
