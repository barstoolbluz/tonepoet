// SPDX-License-Identifier: GPL-3.0-or-later
//
// DVD-Audio metabase integration for the conversion materializer.
//
// This module is deliberately small and side-effect free except for the initial
// metabase load. The DVD-Audio materializer owns disc parsing, group selection,
// AOB realization, and track selection; this module maps the already-resolved
// foo_input_dvda-compatible metabase tags into pipeline TrackMetadata and
// AlbumMetadata so converted outputs receive tags.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;

use super::{MaterializeError, AlbumMetadata, TrackMetadata};
use crate::tui::dvda::DvdaVolume;
use crate::tui::dvda_metabase::{
    self, DvdaMetabase, DvdaResolvedTrackMetadata, LoadedDvdaMetabase,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DvdaTrackMetadataKeys {
    pub group_nr: u8,
    pub titleset: u8,
    pub title: u8,
    pub track: u8,
    pub source_ordinal: u32,
    pub track_number: u32,
    pub total_tracks: u32,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub channel_count: Option<u8>,
    pub codec: Option<String>,
    pub channel_layout: Option<String>,
}

impl DvdaTrackMetadataKeys {
    pub fn metabase_track_id(&self) -> String {
        format!("{}.{}.{}", self.titleset, self.title, self.track)
    }
}

/// Resolve and parse the foo_input_dvda-compatible metabase for a DVD-Audio
/// source. Call this once per materialization, immediately after constructing
/// the `DvdaVolume` and before building `PreparedTrack`s.
pub fn load_for_materializer(
    volume: &dyn DvdaVolume,
    source_path: &Path,
) -> Result<Option<LoadedDvdaMetabase>, MaterializeError> {
    dvda_metabase::load_metabase(volume, source_path)
        .map_err(|err| MaterializeError::Parse(err.to_string()))
}

/// Build pipeline TrackMetadata for a DVD-Audio PreparedTrack.
///
/// Structural DVD-Audio facts are always present in `extra`. Text and
/// MusicBrainz fields come from the metabase when available, with IFO-derived
/// track numbering as fallback.
#[allow(dead_code)]
pub fn track_metadata(
    keys: &DvdaTrackMetadataKeys,
    metabase: Option<&DvdaMetabase>,
) -> TrackMetadata {
    let track_id = keys.metabase_track_id();
    let resolved = dvda_metabase::resolved_track_metadata(
        metabase,
        &track_id,
        keys.track_number,
        keys.total_tracks,
    );
    let mut extra = structural_track_extra(keys, &track_id);
    merge_resolved_extra(&mut extra, &resolved);

    TrackMetadata {
        title: resolved.title,
        artist: resolved.artist.clone().into(),
        album_artist: resolved.album_artist.into(),
        composer: resolved.composer.into(),
        performer: resolved.performer.or(resolved.artist).into(),
        genre: resolved.genre.into(),
        date: resolved.date,
        track_number: resolved.track_number.or(Some(keys.track_number)),
        disc_number: resolved.disc_number,
        isrc: resolved.isrc,
        publisher: resolved.publisher,
        copyright: resolved.copyright,
        comment: resolved.comment,
        pre_emphasis: false,
        extra,
    }
}


/// Overlay metabase-derived values onto the real DVD-Audio materializer's
/// existing TrackMetadata without discarding structural IFO/SAMG/AOB facts that
/// the materializer already recorded in `extra`.
pub fn overlay_track_metadata(
    mut base: TrackMetadata,
    keys: &DvdaTrackMetadataKeys,
    metabase: Option<&DvdaMetabase>,
) -> TrackMetadata {
    let track_id = keys.metabase_track_id();
    let resolved = dvda_metabase::resolved_track_metadata(
        metabase,
        &track_id,
        keys.track_number,
        keys.total_tracks,
    );

    for (key, value) in structural_track_extra(keys, &track_id) {
        base.extra.entry(key).or_insert(value);
    }
    merge_resolved_extra(&mut base.extra, &resolved);

    if let Some(value) = resolved.title {
        base.title = Some(value);
    }
    if let Some(value) = resolved.artist.clone() {
        base.artist = Some(value).into();
    }
    if let Some(value) = resolved.album_artist {
        base.album_artist = Some(value).into();
    }
    if let Some(value) = resolved.composer {
        base.composer = Some(value).into();
    }
    if let Some(value) = resolved.performer.or(resolved.artist) {
        base.performer = Some(value).into();
    }
    if let Some(value) = resolved.genre {
        base.genre = Some(value).into();
    }
    if let Some(value) = resolved.date {
        base.date = Some(value);
    }
    base.track_number = resolved.track_number.or(base.track_number).or(Some(keys.track_number));
    if let Some(value) = resolved.disc_number {
        base.disc_number = Some(value);
    }
    if let Some(value) = resolved.isrc {
        base.isrc = Some(value);
    }
    if let Some(value) = resolved.publisher {
        base.publisher = Some(value);
    }
    if let Some(value) = resolved.copyright {
        base.copyright = Some(value);
    }
    if let Some(value) = resolved.comment {
        base.comment = Some(value);
    }
    base
}

/// Overlay metabase-derived album values onto the materializer's structural
/// AlbumMetadata. This keeps DVD-Audio provenance/format extras intact while
/// letting foo_input_dvda XML supply user-visible tags.
pub fn overlay_album_metadata(
    mut base: AlbumMetadata,
    metabase: Option<&DvdaMetabase>,
    loaded: Option<&LoadedDvdaMetabase>,
) -> AlbumMetadata {
    if let Some(loaded) = loaded {
        insert_nonempty(&mut base.extra, "dvda_metabase_store_id", loaded.store_id.clone());
        insert_nonempty(
            &mut base.extra,
            "dvda_metabase_path",
            loaded.path.display().to_string(),
        );
    }
    if let Some(value) = dvda_metabase::album_value(metabase, &["ALBUM"]) {
        base.album = Some(value);
    }
    if let Some(value) = dvda_metabase::album_value(
        metabase,
        &["ALBUMARTIST", "ALBUM ARTIST", "ARTIST"],
    ) {
        base.album_artist = Some(value).into();
    }
    if let Some(value) = dvda_metabase::album_value(metabase, &["GENRE"]) {
        base.genre = Some(value).into();
    }
    if let Some(value) = dvda_metabase::album_value(metabase, &["DATE", "YEAR"]) {
        base.date = Some(value);
    }
    if let Some(value) = dvda_metabase::album_value(
        metabase,
        &["MUSICBRAINZ_ALBUMID", "MUSICBRAINZ RELEASE ID"],
    ) {
        insert_nonempty(&mut base.extra, "musicbrainz_albumid", value);
    }
    if let Some(value) = dvda_metabase::album_value(
        metabase,
        &["MUSICBRAINZ_ALBUMARTISTID", "MUSICBRAINZ ALBUM ARTIST ID"],
    ) {
        insert_nonempty(&mut base.extra, "musicbrainz_albumartistid", value);
    }
    if let Some(value) = dvda_metabase::album_value(
        metabase,
        &["MUSICBRAINZ_RELEASEGROUPID", "MUSICBRAINZ RELEASE GROUP ID"],
    ) {
        insert_nonempty(&mut base.extra, "musicbrainz_releasegroupid", value);
    }
    base
}
/// Build pipeline AlbumMetadata from the metabase plus materializer structural
/// facts. Existing materializer extras should be passed as `base_extra` so this
/// function only adds/overrides metadata-related fields.
#[allow(dead_code)]
pub fn album_metadata(
    metabase: Option<&DvdaMetabase>,
    loaded: Option<&LoadedDvdaMetabase>,
    total_tracks: u32,
    mut base_extra: BTreeMap<String, String>,
) -> AlbumMetadata {
    if let Some(loaded) = loaded {
        insert_nonempty(&mut base_extra, "dvda_metabase_store_id", loaded.store_id.clone());
        insert_nonempty(
            &mut base_extra,
            "dvda_metabase_path",
            loaded.path.display().to_string(),
        );
    }

    if let Some(value) = dvda_metabase::album_value(
        metabase,
        &["MUSICBRAINZ_ALBUMID", "MUSICBRAINZ RELEASE ID"],
    ) {
        insert_nonempty(&mut base_extra, "musicbrainz_albumid", value);
    }
    if let Some(value) = dvda_metabase::album_value(
        metabase,
        &["MUSICBRAINZ_ALBUMARTISTID", "MUSICBRAINZ ALBUM ARTIST ID"],
    ) {
        insert_nonempty(&mut base_extra, "musicbrainz_albumartistid", value);
    }
    if let Some(value) = dvda_metabase::album_value(
        metabase,
        &["MUSICBRAINZ_RELEASEGROUPID", "MUSICBRAINZ RELEASE GROUP ID"],
    ) {
        insert_nonempty(&mut base_extra, "musicbrainz_releasegroupid", value);
    }

    AlbumMetadata {
        album: dvda_metabase::album_value(metabase, &["ALBUM"]),
        album_artist: dvda_metabase::album_value(
            metabase,
            &["ALBUMARTIST", "ALBUM ARTIST", "ARTIST"],
        )
        .into(),
        genre: dvda_metabase::album_value(metabase, &["GENRE"]).into(),
        date: dvda_metabase::album_value(metabase, &["DATE", "YEAR"]),
        total_tracks,
        total_discs: None,
        disc_number: None,
        extra: base_extra,
    }
}

fn structural_track_extra(keys: &DvdaTrackMetadataKeys, track_id: &str) -> BTreeMap<String, String> {
    let mut extra = BTreeMap::new();
    insert_nonempty(&mut extra, "dvda_group", keys.group_nr.to_string());
    insert_nonempty(&mut extra, "dvda_titleset", keys.titleset.to_string());
    insert_nonempty(&mut extra, "dvda_title", keys.title.to_string());
    insert_nonempty(&mut extra, "dvda_track", keys.track.to_string());
    insert_nonempty(&mut extra, "dvda_track_id", track_id.to_string());
    insert_nonempty(&mut extra, "dvda_source_ordinal", keys.source_ordinal.to_string());
    if let Some(sample_rate) = keys.sample_rate {
        insert_nonempty(&mut extra, "dvda_sample_rate", sample_rate.to_string());
    }
    if let Some(bit_depth) = keys.bit_depth {
        insert_nonempty(&mut extra, "dvda_bit_depth", bit_depth.to_string());
    }
    if let Some(channel_count) = keys.channel_count {
        insert_nonempty(&mut extra, "dvda_channel_count", channel_count.to_string());
    }
    if let Some(codec) = &keys.codec {
        insert_nonempty(&mut extra, "dvda_codec", codec.clone());
    }
    if let Some(channel_layout) = &keys.channel_layout {
        insert_nonempty(&mut extra, "dvda_channel_layout", channel_layout.clone());
    }
    extra
}

fn merge_resolved_extra(extra: &mut BTreeMap<String, String>, resolved: &DvdaResolvedTrackMetadata) {
    if let Some(value) = &resolved.total_tracks {
        insert_nonempty(extra, "totaltracks", value.to_string());
    }
    if let Some(value) = &resolved.musicbrainz_release_id {
        insert_nonempty(extra, "musicbrainz_albumid", value.clone());
    }
    if let Some(value) = &resolved.musicbrainz_recording_id {
        // MUSICBRAINZ_TRACKID is the MusicBrainz recording MBID in
        // Picard/foobar-compatible tags. Keep the explicit alias for
        // downstream code that prefers semantic names.
        insert_nonempty(extra, "musicbrainz_trackid", value.clone());
        insert_nonempty(extra, "musicbrainz_recordingid", value.clone());
    }
    if let Some(value) = &resolved.musicbrainz_release_track_id {
        insert_nonempty(extra, "musicbrainz_releasetrackid", value.clone());
    }
    if let Some(value) = &resolved.musicbrainz_artist_id {
        insert_nonempty(extra, "musicbrainz_artistid", value.clone());
    }
    if let Some(value) = &resolved.musicbrainz_album_artist_id {
        insert_nonempty(extra, "musicbrainz_albumartistid", value.clone());
    }
    if let Some(value) = &resolved.musicbrainz_release_group_id {
        insert_nonempty(extra, "musicbrainz_releasegroupid", value.clone());
    }

    for (key, value) in &resolved.extra {
        insert_nonempty(extra, key, value.clone());
    }
}

fn insert_nonempty(extra: &mut BTreeMap<String, String>, key: &str, value: String) {
    if !value.trim().is_empty() {
        extra.insert(key.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::dvda_metabase::{DvdaMetabase, DvdaMetabaseTrack};

    fn keys() -> DvdaTrackMetadataKeys {
        DvdaTrackMetadataKeys {
            group_nr: 1,
            titleset: 1,
            title: 2,
            track: 3,
            source_ordinal: 3,
            track_number: 3,
            total_tracks: 9,
            sample_rate: Some(96_000),
            bit_depth: Some(24),
            channel_count: Some(2),
            codec: Some("MLP".to_string()),
            channel_layout: Some("Stereo".to_string()),
        }
    }

    #[test]
    fn track_metadata_prefers_metabase_text_tags() {
        let mut meta = BTreeMap::new();
        meta.insert("TITLE".to_string(), "Track Title".to_string());
        meta.insert("ARTIST".to_string(), "Track Artist".to_string());
        meta.insert("ALBUM".to_string(), "Album".to_string());
        meta.insert("DATE".to_string(), "1979".to_string());
        meta.insert("TRACKNUMBER".to_string(), "7".to_string());
        meta.insert("TOTALTRACKS".to_string(), "10".to_string());
        meta.insert("MUSICBRAINZ_TRACKID".to_string(), "mb-recording".to_string());
        meta.insert("MUSICBRAINZ_RELEASETRACKID".to_string(), "mb-release-track".to_string());
        let metabase = DvdaMetabase {
            store_id: "0123456789ABCDEF0123456789ABCDEF".to_string(),
            tracks: vec![DvdaMetabaseTrack {
                id: "1.2.3".to_string(),
                meta,
            }],
        };

        let metadata = track_metadata(&keys(), Some(&metabase));
        assert_eq!(metadata.title.as_deref(), Some("Track Title"));
        assert_eq!(metadata.artist.as_deref(), Some("Track Artist"));
        assert_eq!(metadata.date.as_deref(), Some("1979"));
        assert_eq!(metadata.track_number, Some(7));
        assert_eq!(metadata.extra.get("totaltracks").map(String::as_str), Some("10"));
        assert_eq!(
            metadata.extra.get("musicbrainz_trackid").map(String::as_str),
            Some("mb-recording")
        );
        assert_eq!(
            metadata.extra.get("musicbrainz_recordingid").map(String::as_str),
            Some("mb-recording")
        );
        assert_eq!(
            metadata.extra.get("musicbrainz_releasetrackid").map(String::as_str),
            Some("mb-release-track")
        );
        assert_eq!(metadata.extra.get("dvda_track_id").map(String::as_str), Some("1.2.3"));
    }
    #[test]
    fn musicbrainz_trackid_maps_to_recording_extra_not_release_track_extra() {
        let mut meta = BTreeMap::new();
        meta.insert("MUSICBRAINZ_TRACKID".to_string(), "recording-only".to_string());
        let metabase = DvdaMetabase {
            store_id: "0123456789ABCDEF0123456789ABCDEF".to_string(),
            tracks: vec![DvdaMetabaseTrack {
                id: "1.2.3".to_string(),
                meta,
            }],
        };

        let metadata = track_metadata(&keys(), Some(&metabase));
        assert_eq!(
            metadata.extra.get("musicbrainz_trackid").map(String::as_str),
            Some("recording-only")
        );
        assert_eq!(
            metadata.extra.get("musicbrainz_recordingid").map(String::as_str),
            Some("recording-only")
        );
        assert_eq!(metadata.extra.get("musicbrainz_releasetrackid"), None);
    }

}
