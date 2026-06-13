// SPDX-License-Identifier: GPL-3.0-or-later
//
// DVD-Audio foo_input_dvda-compatible metabase XML support.
//
// foo_input_dvda names each metadata store by the uppercase MD5 digest of the
// complete AUDIO_TS.IFO file. Track ids use the stable DVD-Audio address
// `{titleset}.{title}.{track}` where `title` is the ATS title ordinal.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::tui::dvda::{
    DirectoryDvdaVolume, DvdaDisc, DvdaGroup, DvdaVolume, IsoUdfDvdaVolume, TitleRefKind,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DvdaMetabase {
    pub store_id: String,
    pub tracks: Vec<DvdaMetabaseTrack>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DvdaMetabaseTrack {
    pub id: String,
    pub meta: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DvdaMetabaseError {
    Io(String),
    NotMetabase,
    NoAudioGroups,
    Malformed(String),
    InvalidStoreId(String),
    GroupNotFound(u8),
}

impl std::fmt::Display for DvdaMetabaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "DVD-Audio metabase I/O: {}", msg),
            Self::NotMetabase => write!(f, "not a DVD-Audio metabase XML"),
            Self::NoAudioGroups => write!(f, "DVD-Audio has no non-empty audio groups"),
            Self::Malformed(msg) => write!(f, "DVD-Audio metabase malformed: {}", msg),
            Self::InvalidStoreId(id) => write!(f, "invalid DVD-Audio metabase store id: {}", id),
            Self::GroupNotFound(n) => write!(f, "DVD-Audio group {} was not found or has no tracks", n),
        }
    }
}

impl std::error::Error for DvdaMetabaseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DvdaTrackAddr {
    pub id: String,
    pub titleset: u8,
    pub title: u8,
    pub track: u8,
    pub len_in_pts: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DvdaGroupSummary {
    pub group_nr: u8,
    pub track_count: usize,
    pub duration_pts: u64,
    pub duration_secs: f64,
    pub track_ids: Vec<String>,
}

pub fn compute_store_id(volume: &dyn DvdaVolume) -> Option<String> {
    let mut file = volume.open_audio_ts_file("AUDIO_TS.IFO").ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(compute_store_id_from_audio_ts_ifo(&bytes))
}

pub fn compute_store_id_from_audio_ts_ifo(bytes: &[u8]) -> String {
    md5_hex_upper(bytes)
}

pub fn expected_sidecar_path_for_source(source_path: &Path, store_id: &str) -> Option<PathBuf> {
    if !is_valid_store_id(store_id) {
        return None;
    }

    if source_path.is_file() {
        return source_path.parent().map(|p| p.join(format!("{}.xml", store_id)));
    }

    let name = source_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if name.eq_ignore_ascii_case("AUDIO_TS") {
        return source_path
            .parent()
            .map(|p| p.join(format!("{}.xml", store_id)));
    }

    Some(source_path.join(format!("{}.xml", store_id)))
}

pub fn central_catalog_dir() -> Option<PathBuf> {
    if let Some(base) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(base).join("tonepoet").join("dvda_metabase"));
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config").join("tonepoet").join("dvda_metabase"))
}

pub fn expected_catalog_path(store_id: &str) -> Option<PathBuf> {
    if !is_valid_store_id(store_id) {
        return None;
    }
    central_catalog_dir().map(|dir| dir.join(format!("{}.xml", store_id)))
}

pub fn find_metabase(source_path: &Path, store_id: &str) -> Option<PathBuf> {
    expected_sidecar_path_for_source(source_path, store_id)
        .filter(|p| p.is_file())
        .or_else(|| expected_catalog_path(store_id).filter(|p| p.is_file()))
}

pub fn parse_metabase(path: &Path) -> Result<DvdaMetabase, DvdaMetabaseError> {
    let bytes = std::fs::read(path).map_err(|e| DvdaMetabaseError::Io(format!("read: {}", e)))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|e| DvdaMetabaseError::Malformed(format!("not UTF-8: {}", e)))?;
    parse_metabase_str(text)
}

pub fn parse_metabase_str(text: &str) -> Result<DvdaMetabase, DvdaMetabaseError> {
    let mut out = DvdaMetabase::default();
    let mut current: Option<DvdaMetabaseTrack> = None;
    let mut saw_store = false;
    let mut store_type = String::new();

    for tag in iter_tags(text)? {
        match tag {
            Tag::Open { name, attrs } if name == "store" => {
                saw_store = true;
                out.store_id = attrs.get("id").cloned().unwrap_or_default();
                store_type = attrs.get("type").cloned().unwrap_or_default();
            }
            Tag::Close { name } if name == "store" => {
                if let Some(track) = current.take() {
                    out.tracks.push(track);
                }
            }
            Tag::Open { name, attrs } if name == "track" => {
                if let Some(track) = current.take() {
                    out.tracks.push(track);
                }
                current = Some(DvdaMetabaseTrack {
                    id: attrs.get("id").cloned().unwrap_or_default(),
                    meta: BTreeMap::new(),
                });
            }
            Tag::Close { name } if name == "track" => {
                if let Some(track) = current.take() {
                    out.tracks.push(track);
                }
            }
            Tag::SelfClose { name, attrs } if name == "meta" => {
                if let Some(track) = current.as_mut() {
                    if let (Some(k), Some(v)) = (attrs.get("name"), attrs.get("value")) {
                        // Preserve imported foo_input_dvda key spelling/case for
                        // round-trip compatibility. Lookup helpers below compare
                        // case-insensitively, so tonepoet can still read standard
                        // tags regardless of importer or editor spelling.
                        track.meta.insert(k.trim().to_string(), v.clone());
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(track) = current.take() {
        out.tracks.push(track);
    }
    if !saw_store || !store_type.eq_ignore_ascii_case("DVD") {
        return Err(DvdaMetabaseError::NotMetabase);
    }
    if !out.store_id.is_empty() && !is_valid_store_id(&out.store_id) {
        return Err(DvdaMetabaseError::InvalidStoreId(out.store_id));
    }
    Ok(out)
}

pub fn write_metabase(metabase: &DvdaMetabase, path: &Path) -> Result<(), DvdaMetabaseError> {
    if !is_valid_store_id(&metabase.store_id) {
        return Err(DvdaMetabaseError::InvalidStoreId(metabase.store_id.clone()));
    }
    let parent = path.parent().ok_or_else(|| {
        DvdaMetabaseError::Io(format!("target has no parent: {}", path.display()))
    })?;
    std::fs::create_dir_all(parent)
        .map_err(|e| DvdaMetabaseError::Io(format!("mkdir {}: {}", parent.display(), e)))?;

    let xml = serialize_metabase(metabase);
    let tmp = atomic_temp_path(path);
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| DvdaMetabaseError::Io(format!("create {}: {}", tmp.display(), e)))?;
        file.write_all(xml.as_bytes())
            .map_err(|e| DvdaMetabaseError::Io(format!("write {}: {}", tmp.display(), e)))?;
        file.flush()
            .map_err(|e| DvdaMetabaseError::Io(format!("flush {}: {}", tmp.display(), e)))?;
        file.sync_all()
            .map_err(|e| DvdaMetabaseError::Io(format!("sync {}: {}", tmp.display(), e)))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        DvdaMetabaseError::Io(format!("rename {} -> {}: {}", tmp.display(), path.display(), e))
    })?;

    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

pub fn serialize_metabase(metabase: &DvdaMetabase) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<!--DVD-Audio metabase file-->\n");
    out.push_str("<root>\n");
    out.push_str("  <store id=\"");
    out.push_str(&escape_xml(&metabase.store_id));
    out.push_str("\" type=\"DVD\" version=\"1.1\">\n");
    for track in &metabase.tracks {
        out.push_str("    <track id=\"");
        out.push_str(&escape_xml(&track.id));
        out.push_str("\">\n");
        for (key, value) in &track.meta {
            out.push_str("      <meta name=\"");
            out.push_str(&escape_xml(key));
            out.push_str("\" value=\"");
            out.push_str(&escape_xml(value));
            out.push_str("\"/>\n");
        }
        out.push_str("    </track>\n");
    }
    out.push_str("  </store>\n");
    out.push_str("</root>\n");
    out
}

pub fn seed_from_disc(disc: &DvdaDisc, store_id: &str) -> DvdaMetabase {
    let mut tracks = Vec::new();
    let mut seen = BTreeSet::new();
    for group in &disc.groups {
        let addrs = group_track_addrs(disc, group);
        let total = addrs.len().to_string();
        for (idx, addr) in addrs.into_iter().enumerate() {
            if !seen.insert(addr.id.clone()) {
                continue;
            }
            let mut meta = BTreeMap::new();
            meta.insert("TRACKNUMBER".to_string(), (idx + 1).to_string());
            meta.insert("TOTALTRACKS".to_string(), total.clone());
            meta.insert("dvda_titleset".to_string(), addr.titleset.to_string());
            meta.insert("dvda_title".to_string(), addr.title.to_string());
            meta.insert("dvda_track".to_string(), addr.track.to_string());
            tracks.push(DvdaMetabaseTrack { id: addr.id, meta });
        }
    }
    DvdaMetabase {
        store_id: store_id.to_ascii_uppercase(),
        tracks,
    }
}

pub fn available_groups(disc: &DvdaDisc) -> Vec<DvdaGroupSummary> {
    let mut groups: Vec<DvdaGroupSummary> = disc
        .groups
        .iter()
        .filter_map(|group| {
            let addrs = group_track_addrs(disc, group);
            if addrs.is_empty() {
                return None;
            }
            let duration_pts = addrs.iter().map(|addr| u64::from(addr.len_in_pts)).sum();
            Some(DvdaGroupSummary {
                group_nr: group.group_nr,
                track_count: addrs.len(),
                duration_pts,
                duration_secs: duration_pts as f64 / 90_000.0,
                track_ids: addrs.into_iter().map(|addr| addr.id).collect(),
            })
        })
        .collect();
    groups.sort_by_key(|group| group.group_nr);
    groups
}

pub fn default_group(disc: &DvdaDisc) -> Option<&DvdaGroup> {
    disc.groups
        .iter()
        .filter(|group| group_track_count(disc, group) > 0)
        .max_by_key(|group| group_duration_pts(disc, group))
}

pub fn select_group(disc: &DvdaDisc, group_nr: Option<u8>) -> Result<&DvdaGroup, DvdaMetabaseError> {
    if let Some(nr) = group_nr {
        return disc
            .groups
            .iter()
            .find(|group| group.group_nr == nr && group_track_count(disc, group) > 0)
            .ok_or(DvdaMetabaseError::GroupNotFound(nr));
    }
    default_group(disc).ok_or(DvdaMetabaseError::NoAudioGroups)
}

pub fn group_label(disc: &DvdaDisc, group: &DvdaGroup) -> String {
    let tracks = group_track_count(disc, group);
    let secs = group_duration_pts(disc, group) as f64 / 90_000.0;
    format!("Group {} ({} track{}, {})",
        group.group_nr,
        tracks,
        if tracks == 1 { "" } else { "s" },
        format_duration_mmss(secs),
    )
}

pub fn group_choice_hint(disc: &DvdaDisc) -> String {
    let groups = available_groups(disc);
    if groups.is_empty() {
        return "no non-empty DVD-Audio groups".to_string();
    }
    groups
        .into_iter()
        .map(|group| {
            format!(
                "{}: {} track{}, {}",
                group.group_nr,
                group.track_count,
                if group.track_count == 1 { "" } else { "s" },
                format_duration_mmss(group.duration_secs),
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn format_duration_mmss(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

pub fn group_track_count(disc: &DvdaDisc, group: &DvdaGroup) -> usize {
    group_track_addrs(disc, group).len()
}

pub fn group_duration_pts(disc: &DvdaDisc, group: &DvdaGroup) -> u64 {
    group_track_addrs(disc, group)
        .into_iter()
        .map(|addr| u64::from(addr.len_in_pts))
        .sum()
}

pub fn group_track_ids(disc: &DvdaDisc, group: &DvdaGroup) -> Vec<String> {
    group_track_addrs(disc, group)
        .into_iter()
        .map(|addr| addr.id)
        .collect()
}

pub fn group_track_pts(disc: &DvdaDisc, group: &DvdaGroup) -> Vec<u32> {
    group_track_addrs(disc, group)
        .into_iter()
        .map(|addr| addr.len_in_pts)
        .collect()
}

pub fn group_track_addrs(disc: &DvdaDisc, group: &DvdaGroup) -> Vec<DvdaTrackAddr> {
    let mut out = Vec::new();
    for title_ref in &group.title_refs {
        let Some(ts) = disc
            .title_sets
            .iter()
            .find(|ts| ts.number == title_ref.title_set_nr)
        else {
            continue;
        };
        let title = match title_ref.kind {
            TitleRefKind::AottTitleOrdinal => ts
                .titles
                .iter()
                .find(|t| t.title_ordinal == title_ref.title_nr),
            TitleRefKind::AtsPgcTitleNr => ts.titles.iter().find(|t| t.title_nr == title_ref.title_nr),
        };
        let Some(title) = title else {
            continue;
        };
        for chapter in &title.chapters {
            let title_ordinal = title.title_ordinal;
            out.push(DvdaTrackAddr {
                id: format!("{}.{}.{}", ts.number, title_ordinal, chapter.track_nr),
                titleset: ts.number,
                title: title_ordinal,
                track: chapter.track_nr,
                len_in_pts: chapter.len_in_pts,
            });
        }
    }
    out
}

pub fn track<'a>(metabase: &'a DvdaMetabase, id: &str) -> Option<&'a DvdaMetabaseTrack> {
    metabase.tracks.iter().find(|track| track.id == id)
}

pub fn track_mut<'a>(metabase: &'a mut DvdaMetabase, id: &str) -> Option<&'a mut DvdaMetabaseTrack> {
    metabase.tracks.iter_mut().find(|track| track.id == id)
}

/// Return a uniform album-level value across all tracks in the metabase.
///
/// This is only appropriate for whole-disc reads. Multi-presentation callers
/// should use `album_value_for_track_ids` so stereo and multichannel groups keep
/// independent album-level metadata.
pub fn album_value(metabase: Option<&DvdaMetabase>, keys: &[&str]) -> Option<String> {
    let metabase = metabase?;
    uniform_album_value_from_tracks(metabase.tracks.iter(), keys)
}

/// Return a uniform album-level value scoped to the supplied DVD-Audio track ids.
///
/// The foo_input_dvda metabase stores every group in one XML file. Album-like
/// keys such as ALBUM, ALBUMARTIST, DATE, and MusicBrainz release ids therefore
/// must be read only from the active group's `{titleset}.{title}.{track}` ids.
/// Mixed values within the requested ids are reported as `None`, matching the
/// existing whole-disc behavior without leaking values from sibling groups.
pub fn album_value_for_track_ids(
    metabase: Option<&DvdaMetabase>,
    track_ids: &[String],
    keys: &[&str],
) -> Option<String> {
    let metabase = metabase?;
    if track_ids.is_empty() {
        return None;
    }
    let wanted: BTreeSet<&str> = track_ids.iter().map(String::as_str).collect();
    uniform_album_value_from_tracks(
        metabase
            .tracks
            .iter()
            .filter(|track| wanted.contains(track.id.as_str())),
        keys,
    )
}

fn uniform_album_value_from_tracks<'a, I>(tracks: I, keys: &[&str]) -> Option<String>
where
    I: IntoIterator<Item = &'a DvdaMetabaseTrack>,
{
    let mut vals = Vec::new();
    for tr in tracks {
        for key in keys {
            if let Some(value) = meta_lookup(&tr.meta, key).filter(|v| !v.trim().is_empty()) {
                vals.push(value.clone());
                break;
            }
        }
    }
    let first = vals.first()?.clone();
    if vals.iter().all(|value| value == &first) {
        Some(first)
    } else {
        None
    }
}

pub fn track_value(metabase: Option<&DvdaMetabase>, id: &str, keys: &[&str]) -> Option<String> {
    let metabase = metabase?;
    let tr = track(metabase, id)?;
    for key in keys {
        if let Some(value) = meta_lookup(&tr.meta, key).filter(|v| !v.trim().is_empty()) {
            return Some(value.clone());
        }
    }
    None
}

pub fn pts_durations_to_cd_sectors(durations_pts: &[u32]) -> Vec<u32> {
    let mut sectors = Vec::with_capacity(durations_pts.len() + 1);
    let mut cur: u32 = 150;
    sectors.push(cur);
    for &pts in durations_pts {
        let frames = ((u64::from(pts) * 75) + 45_000) / 90_000;
        cur = cur.saturating_add(frames.min(u64::from(u32::MAX)) as u32);
        sectors.push(cur);
    }
    sectors
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedDvdaMetabase {
    pub store_id: String,
    pub path: PathBuf,
    pub metabase: DvdaMetabase,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DvdaResolvedTrackMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub composer: Option<String>,
    pub performer: Option<String>,
    pub genre: Option<String>,
    pub date: Option<String>,
    pub track_number: Option<u32>,
    pub total_tracks: Option<u32>,
    pub disc_number: Option<u32>,
    pub isrc: Option<String>,
    pub publisher: Option<String>,
    pub copyright: Option<String>,
    pub comment: Option<String>,
    pub musicbrainz_release_id: Option<String>,
    /// MusicBrainz release-track MBID (`MUSICBRAINZ_RELEASETRACKID`).
    pub musicbrainz_release_track_id: Option<String>,
    /// MusicBrainz recording MBID (`MUSICBRAINZ_TRACKID` in Picard/foobar tagging).
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_artist_id: Option<String>,
    pub musicbrainz_album_artist_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub extra: BTreeMap<String, String>,
}

pub fn load_metabase(
    volume: &dyn DvdaVolume,
    source_path: &Path,
) -> Result<Option<LoadedDvdaMetabase>, DvdaMetabaseError> {
    let Some(store_id) = compute_store_id(volume) else {
        return Ok(None);
    };
    let Some(path) = find_metabase(source_path, &store_id) else {
        return Ok(None);
    };

    let mut metabase = parse_metabase(&path)?;
    if metabase.store_id.is_empty() {
        metabase.store_id = store_id.clone();
    }
    metabase.store_id = metabase.store_id.to_ascii_uppercase();
    if metabase.store_id != store_id {
        return Err(DvdaMetabaseError::InvalidStoreId(metabase.store_id));
    }

    Ok(Some(LoadedDvdaMetabase {
        store_id,
        path,
        metabase,
    }))
}

/// Resolve and load the DVD-Audio metabase for an ISO image or filesystem
/// DVD-Audio directory. This keeps legacy direct `map_dvda_disc()` callers
/// metabase-aware even when they do not pass a preloaded volume.
pub fn load_metabase_for_source_path(
    source_path: &Path,
) -> Result<Option<LoadedDvdaMetabase>, DvdaMetabaseError> {
    if source_path.is_dir() {
        let volume = DirectoryDvdaVolume::new(source_path);
        return load_metabase(&volume, source_path);
    }

    let volume = IsoUdfDvdaVolume::open(source_path).map_err(|e| {
        DvdaMetabaseError::Io(format!(
            "DVD-Audio source open failed for '{}': {}",
            source_path.display(),
            e
        ))
    })?;
    load_metabase(&volume, source_path)
}

pub fn resolved_track_metadata(
    metabase: Option<&DvdaMetabase>,
    track_id: &str,
    fallback_track_number: u32,
    fallback_total_tracks: u32,
) -> DvdaResolvedTrackMetadata {
    let mut out = DvdaResolvedTrackMetadata {
        track_number: Some(fallback_track_number),
        total_tracks: Some(fallback_total_tracks),
        ..DvdaResolvedTrackMetadata::default()
    };

    out.title = track_value(metabase, track_id, &["TITLE"]);
    out.artist = track_value(metabase, track_id, &["ARTIST"]);
    out.album_artist = track_value(metabase, track_id, &["ALBUMARTIST", "ALBUM ARTIST"])
        .or_else(|| album_value(metabase, &["ALBUMARTIST", "ALBUM ARTIST", "ARTIST"]));
    out.composer = track_value(metabase, track_id, &["COMPOSER"]);
    out.performer = track_value(metabase, track_id, &["PERFORMER"]).or_else(|| out.artist.clone());
    out.genre = track_value(metabase, track_id, &["GENRE"]).or_else(|| album_value(metabase, &["GENRE"]));
    out.date = track_value(metabase, track_id, &["DATE", "YEAR"])
        .or_else(|| album_value(metabase, &["DATE", "YEAR"]));
    out.track_number = track_value(metabase, track_id, &["TRACKNUMBER"])
        .and_then(|value| parse_u32_prefix(&value))
        .or(out.track_number);
    out.total_tracks = track_value(metabase, track_id, &["TOTALTRACKS", "TRACKTOTAL"])
        .and_then(|value| parse_u32_prefix(&value))
        .or(out.total_tracks);
    out.disc_number = track_value(metabase, track_id, &["DISCNUMBER", "DISC"])
        .and_then(|value| parse_u32_prefix(&value));
    out.isrc = track_value(metabase, track_id, &["ISRC"]);
    out.publisher = track_value(metabase, track_id, &["PUBLISHER", "LABEL"]);
    out.copyright = track_value(metabase, track_id, &["COPYRIGHT"]);
    out.comment = track_value(metabase, track_id, &["COMMENT", "DESCRIPTION"]);
    out.musicbrainz_release_id = track_value(metabase, track_id, &["MUSICBRAINZ_ALBUMID", "MUSICBRAINZ RELEASE ID"])
        .or_else(|| album_value(metabase, &["MUSICBRAINZ_ALBUMID", "MUSICBRAINZ RELEASE ID"]));
    // Picard/foobar convention: MUSICBRAINZ_TRACKID is the recording MBID.
    // The release-track MBID has its own key, MUSICBRAINZ_RELEASETRACKID.
    out.musicbrainz_recording_id = track_value(
        metabase,
        track_id,
        &["MUSICBRAINZ_TRACKID", "MUSICBRAINZ_RECORDINGID", "MUSICBRAINZ RECORDING ID"],
    );
    out.musicbrainz_release_track_id = track_value(
        metabase,
        track_id,
        &[
            "MUSICBRAINZ_RELEASETRACKID",
            "MUSICBRAINZ RELEASE TRACK ID",
            "MUSICBRAINZ TRACK ID",
        ],
    );
    out.musicbrainz_artist_id = track_value(metabase, track_id, &["MUSICBRAINZ_ARTISTID", "MUSICBRAINZ ARTIST ID"]);
    out.musicbrainz_album_artist_id = track_value(metabase, track_id, &["MUSICBRAINZ_ALBUMARTISTID", "MUSICBRAINZ ALBUM ARTIST ID"])
        .or_else(|| album_value(metabase, &["MUSICBRAINZ_ALBUMARTISTID", "MUSICBRAINZ ALBUM ARTIST ID"]));
    out.musicbrainz_release_group_id = track_value(metabase, track_id, &["MUSICBRAINZ_RELEASEGROUPID", "MUSICBRAINZ RELEASE GROUP ID"])
        .or_else(|| album_value(metabase, &["MUSICBRAINZ_RELEASEGROUPID", "MUSICBRAINZ RELEASE GROUP ID"]));

    if let Some(metabase) = metabase {
        if let Some(track) = track(metabase, track_id) {
            for (key, value) in &track.meta {
                if value.trim().is_empty() || is_structured_track_key(key) {
                    continue;
                }
                out.extra
                    .entry(format!("dvda_metabase_{}", extra_key_name(key)))
                    .or_insert_with(|| value.clone());
            }
        }
    }

    out
}

fn meta_lookup<'a>(meta: &'a BTreeMap<String, String>, key: &str) -> Option<&'a String> {
    if let Some(value) = meta.get(key) {
        return Some(value);
    }
    let wanted = normalize_meta_key(key);
    meta.iter()
        .find(|(existing, _)| normalize_meta_key(existing) == wanted)
        .map(|(_, value)| value)
}

fn parse_u32_prefix(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    let digits: String = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<u32>().ok()
    }
}

fn is_structured_track_key(key: &str) -> bool {
    matches!(
        normalize_meta_key(key).as_str(),
        "TITLE"
            | "ARTIST"
            | "ALBUMARTIST"
            | "ALBUM ARTIST"
            | "COMPOSER"
            | "PERFORMER"
            | "GENRE"
            | "DATE"
            | "YEAR"
            | "TRACKNUMBER"
            | "TOTALTRACKS"
            | "TRACKTOTAL"
            | "DISCNUMBER"
            | "DISC"
            | "ISRC"
            | "PUBLISHER"
            | "LABEL"
            | "COPYRIGHT"
            | "COMMENT"
            | "DESCRIPTION"
            | "MUSICBRAINZ_ALBUMID"
            | "MUSICBRAINZ RELEASE ID"
            | "MUSICBRAINZ_TRACKID"
            | "MUSICBRAINZ_RELEASETRACKID"
            | "MUSICBRAINZ TRACK ID"
            | "MUSICBRAINZ_RECORDINGID"
            | "MUSICBRAINZ RECORDING ID"
            | "MUSICBRAINZ_ARTISTID"
            | "MUSICBRAINZ ARTIST ID"
            | "MUSICBRAINZ_ALBUMARTISTID"
            | "MUSICBRAINZ ALBUM ARTIST ID"
            | "MUSICBRAINZ_RELEASEGROUPID"
            | "MUSICBRAINZ RELEASE GROUP ID"
    )
}

fn extra_key_name(key: &str) -> String {
    normalize_meta_key(key)
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

pub fn normalize_meta_key(key: &str) -> String {
    key.trim().to_ascii_uppercase()
}

fn is_valid_store_id(store_id: &str) -> bool {
    store_id.len() == 32 && store_id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn atomic_temp_path(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("metabase.xml");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.with_file_name(format!(".{}.{}.{}.tmp", name, std::process::id(), nanos))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tag {
    Open { name: String, attrs: BTreeMap<String, String> },
    Close { name: String },
    SelfClose { name: String, attrs: BTreeMap<String, String> },
}

fn iter_tags(text: &str) -> Result<Vec<Tag>, DvdaMetabaseError> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"<!--") {
            let close = find_after(bytes, i + 4, b"-->").ok_or_else(|| {
                DvdaMetabaseError::Malformed(format!("unterminated comment at offset {}", i))
            })?;
            i = close + 3;
            continue;
        }
        if bytes[i..].starts_with(b"<?") {
            let close = find_after(bytes, i + 2, b"?>").ok_or_else(|| {
                DvdaMetabaseError::Malformed(format!("unterminated PI at offset {}", i))
            })?;
            i = close + 2;
            continue;
        }
        if bytes[i..].starts_with(b"<!") {
            let close = find_byte(bytes, i + 2, b'>').ok_or_else(|| {
                DvdaMetabaseError::Malformed(format!("unterminated declaration at offset {}", i))
            })?;
            i = close + 1;
            continue;
        }

        let close = find_tag_end(bytes, i + 1).ok_or_else(|| {
            DvdaMetabaseError::Malformed(format!("unterminated tag at offset {}", i))
        })?;
        let raw = std::str::from_utf8(&bytes[i + 1..close])
            .map_err(|e| DvdaMetabaseError::Malformed(format!("invalid UTF-8 tag: {}", e)))?
            .trim();
        if raw.is_empty() {
            return Err(DvdaMetabaseError::Malformed(format!("empty tag at offset {}", i)));
        }
        if let Some(stripped) = raw.strip_prefix('/') {
            out.push(Tag::Close { name: stripped.trim().to_ascii_lowercase() });
            i = close + 1;
            continue;
        }
        let self_close = raw.ends_with('/');
        let body = if self_close { raw[..raw.len() - 1].trim_end() } else { raw };
        let (name, attrs) = parse_tag_body(body)?;
        if self_close {
            out.push(Tag::SelfClose { name, attrs });
        } else {
            out.push(Tag::Open { name, attrs });
        }
        i = close + 1;
    }
    Ok(out)
}

fn parse_tag_body(body: &str) -> Result<(String, BTreeMap<String, String>), DvdaMetabaseError> {
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i == 0 {
        return Err(DvdaMetabaseError::Malformed("missing tag name".to_string()));
    }
    let name = body[..i].to_ascii_lowercase();
    let mut attrs = BTreeMap::new();
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let key = body[key_start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            return Err(DvdaMetabaseError::Malformed(format!("attribute {} missing '='", key)));
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'\"' {
            return Err(DvdaMetabaseError::Malformed(format!(
                "attribute {} must use double quotes",
                key
            )));
        }
        i += 1;
        let value_start = i;
        while i < bytes.len() && bytes[i] != b'\"' {
            i += 1;
        }
        if i >= bytes.len() {
            return Err(DvdaMetabaseError::Malformed(format!(
                "attribute {} has unterminated value",
                key
            )));
        }
        let raw_value = &body[value_start..i];
        i += 1;
        attrs.insert(key, decode_xml_entities(raw_value));
    }
    Ok((name, attrs))
}

fn find_after(haystack: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    haystack[start..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| start + p)
}

fn find_byte(haystack: &[u8], start: usize, byte: u8) -> Option<usize> {
    haystack[start..].iter().position(|b| *b == byte).map(|p| start + p)
}

fn find_tag_end(bytes: &[u8], mut i: usize) -> Option<usize> {
    let mut in_quote = false;
    while i < bytes.len() {
        match bytes[i] {
            b'\"' => in_quote = !in_quote,
            b'>' if !in_quote => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

fn decode_xml_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find('&') {
        out.push_str(&rest[..idx]);
        rest = &rest[idx + 1..];
        if let Some(semi) = rest.find(';') {
            let entity = &rest[..semi];
            match entity {
                "amp" => out.push('&'),
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "quot" => out.push('"'),
                "apos" => out.push('\''),
                _ if entity.starts_with("#x") => {
                    if let Ok(code) = u32::from_str_radix(&entity[2..], 16) {
                        if let Some(ch) = char::from_u32(code) {
                            out.push(ch);
                        } else {
                            out.push('&');
                            out.push_str(entity);
                            out.push(';');
                        }
                    } else {
                        out.push('&');
                        out.push_str(entity);
                        out.push(';');
                    }
                }
                _ if entity.starts_with('#') => {
                    if let Ok(code) = entity[1..].parse::<u32>() {
                        if let Some(ch) = char::from_u32(code) {
                            out.push(ch);
                        } else {
                            out.push('&');
                            out.push_str(entity);
                            out.push(';');
                        }
                    } else {
                        out.push('&');
                        out.push_str(entity);
                        out.push(';');
                    }
                }
                _ => {
                    out.push('&');
                    out.push_str(entity);
                    out.push(';');
                }
            }
            rest = &rest[semi + 1..];
        } else {
            out.push('&');
            break;
        }
    }
    out.push_str(rest);
    out
}

fn md5_hex_upper(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
        5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee,
        0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
        0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
        0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
        0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
        0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
        0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
        0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
        0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
        0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05,
        0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
        0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
        0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ];

    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(input.len() + 72);
    msg.extend_from_slice(input);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            let start = i * 4;
            *word = u32::from_le_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }

        let mut a = a0;
        let mut b = b0;
        let mut c = c0;
        let mut d = d0;

        for i in 0..64 {
            let (f, g) = if i < 16 {
                ((b & c) | ((!b) & d), i)
            } else if i < 32 {
                ((d & b) | ((!d) & c), (5 * i + 1) % 16)
            } else if i < 48 {
                (b ^ c ^ d, (3 * i + 5) % 16)
            } else {
                (c ^ (b | (!d)), (7 * i) % 16)
            };
            let next = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[i])
                    .wrapping_add(m[g])
                    .rotate_left(S[i]),
            );
            a = d;
            d = c;
            c = b;
            b = next;
        }

        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut digest = [0u8; 16];
    digest[0..4].copy_from_slice(&a0.to_le_bytes());
    digest[4..8].copy_from_slice(&b0.to_le_bytes());
    digest[8..12].copy_from_slice(&c0.to_le_bytes());
    digest[12..16].copy_from_slice(&d0.to_le_bytes());

    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(32);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORE: &str = "0123456789ABCDEF0123456789ABCDEF";

    #[test]
    fn parse_reads_store_and_tracks() {
        let xml = format!(
            r#"<?xml version="1.0"?><root><store id="{}" type="DVD" version="1.1"><track id="1.2.3"><meta name="title" value="Song"/><meta name="ALBUM ARTIST" value="Artist"/></track></store></root>"#,
            STORE
        );
        let m = parse_metabase_str(&xml).expect("parse");
        assert_eq!(m.store_id, STORE);
        assert_eq!(m.tracks.len(), 1);
        assert_eq!(m.tracks[0].id, "1.2.3");
        assert_eq!(m.tracks[0].meta.get("title").unwrap(), "Song");
        assert_eq!(m.tracks[0].meta.get("ALBUM ARTIST").unwrap(), "Artist");
        assert_eq!(track_value(Some(&m), "1.2.3", &["TITLE"]).as_deref(), Some("Song"));
    }

    #[test]
    fn reject_non_dvd_store() {
        let err = parse_metabase_str(r#"<root><store id="0123456789ABCDEF0123456789ABCDEF" type="SACD"/></root>"#)
            .unwrap_err();
        assert_eq!(err, DvdaMetabaseError::NotMetabase);
    }

    #[test]
    fn round_trip_preserves_escaped_values() {
        let mut meta = BTreeMap::new();
        meta.insert("TITLE".to_string(), "A & <B> \"C\"".to_string());
        let m = DvdaMetabase {
            store_id: STORE.to_string(),
            tracks: vec![DvdaMetabaseTrack { id: "1.1.1".to_string(), meta }],
        };
        let xml = serialize_metabase(&m);
        assert!(xml.contains("A &amp; &lt;B&gt; &quot;C&quot;"));
        let parsed = parse_metabase_str(&xml).expect("parse serialized");
        assert_eq!(parsed, m);
    }

    #[test]
    fn compute_store_id_matches_known_md5_vector() {
        assert_eq!(
            compute_store_id_from_audio_ts_ifo(b"abc"),
            "900150983CD24FB0D6963F7D28E17F72"
        );
    }

    #[test]
    fn pts_to_cd_sectors_rounds_to_nearest_frame() {
        assert_eq!(pts_durations_to_cd_sectors(&[90_000, 180_000]), vec![150, 225, 375]);
        assert_eq!(pts_durations_to_cd_sectors(&[1_200]), vec![150, 151]);
    }

    #[test]
    fn catalog_path_requires_hex_store_id() {
        assert!(expected_catalog_path("not-a-store-id").is_none());
        assert!(expected_catalog_path(STORE).unwrap().ends_with("0123456789ABCDEF0123456789ABCDEF.xml"));
    }

    #[test]
    fn album_value_for_track_ids_is_scoped_to_requested_group() {
        let mut g1a = BTreeMap::new();
        g1a.insert("ALBUM".to_string(), "Surround Album".to_string());
        let mut g1b = BTreeMap::new();
        g1b.insert("ALBUM".to_string(), "Surround Album".to_string());
        let mut g3a = BTreeMap::new();
        g3a.insert("ALBUM".to_string(), "Stereo Album".to_string());
        let metabase = DvdaMetabase {
            store_id: STORE.to_string(),
            tracks: vec![
                DvdaMetabaseTrack { id: "1.1.1".to_string(), meta: g1a },
                DvdaMetabaseTrack { id: "1.1.2".to_string(), meta: g1b },
                DvdaMetabaseTrack { id: "2.1.1".to_string(), meta: g3a },
            ],
        };

        assert_eq!(
            album_value(Some(&metabase), &["ALBUM"]),
            None,
            "global lookup must refuse mixed group-level values",
        );
        assert_eq!(
            album_value_for_track_ids(
                Some(&metabase),
                &["1.1.1".to_string(), "1.1.2".to_string()],
                &["ALBUM"],
            )
            .as_deref(),
            Some("Surround Album"),
        );
        assert_eq!(
            album_value_for_track_ids(Some(&metabase), &["2.1.1".to_string()], &["ALBUM"])
                .as_deref(),
            Some("Stereo Album"),
        );
    }

    #[test]
    fn album_value_for_track_ids_refuses_mixed_values_within_group() {
        let mut a = BTreeMap::new();
        a.insert("ALBUM".to_string(), "First".to_string());
        let mut b = BTreeMap::new();
        b.insert("ALBUM".to_string(), "Second".to_string());
        let metabase = DvdaMetabase {
            store_id: STORE.to_string(),
            tracks: vec![
                DvdaMetabaseTrack { id: "1.1.1".to_string(), meta: a },
                DvdaMetabaseTrack { id: "1.1.2".to_string(), meta: b },
            ],
        };

        assert_eq!(
            album_value_for_track_ids(
                Some(&metabase),
                &["1.1.1".to_string(), "1.1.2".to_string()],
                &["ALBUM"],
            ),
            None,
        );
    }

    #[test]
    fn parse_write_preserves_imported_meta_key_case() {
        let xml = format!(
            r#"<?xml version="1.0"?><root><store id="{}" type="DVD" version="1.1"><track id="1.2.3"><meta name="dvda_title" value="2"/><meta name="title" value="Song"/></track></store></root>"#,
            STORE
        );
        let parsed = parse_metabase_str(&xml).expect("parse");
        assert!(parsed.tracks[0].meta.contains_key("dvda_title"));
        assert!(parsed.tracks[0].meta.contains_key("title"));
        let serialized = serialize_metabase(&parsed);
        assert!(serialized.contains("name=\"dvda_title\""));
        assert!(serialized.contains("name=\"title\""));
        assert_eq!(track_value(Some(&parsed), "1.2.3", &["TITLE"]).as_deref(), Some("Song"));
    }

    #[test]
    fn resolved_track_metadata_maps_metabase_tags_to_pipeline_shape() {
        let mut meta = BTreeMap::new();
        meta.insert("TITLE".to_string(), "Song".to_string());
        meta.insert("ARTIST".to_string(), "Performer".to_string());
        meta.insert("ALBUM ARTIST".to_string(), "Album Artist".to_string());
        meta.insert("DATE".to_string(), "1985".to_string());
        meta.insert("TRACKNUMBER".to_string(), "3/9".to_string());
        meta.insert("TOTALTRACKS".to_string(), "9".to_string());
        meta.insert("MUSICBRAINZ_TRACKID".to_string(), "mb-recording".to_string());
        meta.insert("MUSICBRAINZ_RELEASETRACKID".to_string(), "mb-release-track".to_string());
        meta.insert("dvda_titleset".to_string(), "1".to_string());
        let metabase = DvdaMetabase {
            store_id: STORE.to_string(),
            tracks: vec![DvdaMetabaseTrack {
                id: "1.2.3".to_string(),
                meta,
            }],
        };

        let resolved = resolved_track_metadata(Some(&metabase), "1.2.3", 1, 1);
        assert_eq!(resolved.title.as_deref(), Some("Song"));
        assert_eq!(resolved.artist.as_deref(), Some("Performer"));
        assert_eq!(resolved.album_artist.as_deref(), Some("Album Artist"));
        assert_eq!(resolved.date.as_deref(), Some("1985"));
        assert_eq!(resolved.track_number, Some(3));
        assert_eq!(resolved.total_tracks, Some(9));
        assert_eq!(resolved.musicbrainz_recording_id.as_deref(), Some("mb-recording"));
        assert_eq!(
            resolved.musicbrainz_release_track_id.as_deref(),
            Some("mb-release-track")
        );
        assert_eq!(
            resolved.extra.get("dvda_metabase_dvda_titleset").map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn musicbrainz_trackid_resolves_as_recording_id_not_release_track_id() {
        let mut meta = BTreeMap::new();
        meta.insert("MUSICBRAINZ_TRACKID".to_string(), "recording-only".to_string());
        let metabase = DvdaMetabase {
            store_id: STORE.to_string(),
            tracks: vec![DvdaMetabaseTrack {
                id: "1.2.3".to_string(),
                meta,
            }],
        };

        let resolved = resolved_track_metadata(Some(&metabase), "1.2.3", 1, 1);
        assert_eq!(
            resolved.musicbrainz_recording_id.as_deref(),
            Some("recording-only")
        );
        assert_eq!(resolved.musicbrainz_release_track_id, None);
    }

}
