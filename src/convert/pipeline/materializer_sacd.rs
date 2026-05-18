//! PR 9 - SACD ISO materializer.
//!
//! This stage parses a SACD ISO table of contents into per-track `SacdTrack`
//! references. It intentionally does not decode DSD audio; `realize_track`
//! performs extraction later through the in-process `sacd-rs` crate.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::errors::{MaterializeError, SourceDetectError};
use super::reporter::PipelineReporter;
use super::stages::Materializer;
use super::tool::ToolRunner;
use super::types::*;
use crate::tui::sacd::{
    detect_sacd_iso, parse_sacd_iso, AreaInfo, AreaKind, DetectionResult, Genre, PlayTime,
    SacdError, SacdMetadata, TrackEntry, SACD_FRAME_RATE, SACD_SAMPLE_RATE_HZ,
};
use crate::tui::sacd_sidecar::{find_sidecar_for_iso, parse_sidecar, SidecarTrack};

pub struct SacdIsoMaterializer;

#[async_trait]
impl Materializer for SacdIsoMaterializer {
    async fn materialize(
        &self,
        req: &PipelineRequest,
        staging: &StagingDir,
        _runner: &dyn ToolRunner,
        _reporter: Option<&dyn PipelineReporter>,
        _tool_paths: &HashMap<String, PathBuf>,
        cancel: &CancellationToken,
    ) -> Result<PreparedSource, MaterializeError> {
        std::fs::create_dir_all(&staging.root)?;
        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }

        let explicit_sacd_request = explicit_sacd_requested(req);
        let metadata = parse_sacd_iso(&req.container)
            .map_err(|err| sacd_error_to_materialize(err, explicit_sacd_request))?;

        // Sidecar XML is the primary metadata source (has track titles,
        // artist, album from MusicBrainz). TOC provides structural data
        // (sector ranges, durations) and fallback text when no sidecar.
        let sidecar_tracks = load_sidecar_tracks(&req.container, req.source.sacd_area);

        let requested_area = requested_sacd_area(req.source.sacd_area);
        let area = sacd_area_info(&metadata, requested_area).ok_or_else(|| {
            MaterializeError::InvalidTrackSelection(format!(
                "requested SACD area {} is not present in {}",
                sacd_area_label(requested_area),
                req.container.display()
            ))
        })?;

        if area.tracks.is_empty() {
            return Err(MaterializeError::Parse(format!(
                "SACD area {} contains no tracks",
                sacd_area_label(requested_area)
            )));
        }

        let mut tracks = Vec::with_capacity(area.tracks.len());
        for (idx, entry) in area.tracks.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(MaterializeError::Cancelled);
            }
            if entry.length_lsn == 0 {
                return Err(MaterializeError::Parse(format!(
                    "SACD track {} has zero sector length",
                    idx + 1
                )));
            }

            let source_ordinal = (idx + 1) as u32;
            let track_number = u32::from(area.header.track_offset) + source_ordinal;
            tracks.push(PreparedTrack {
                id: TrackId {
                    source_ordinal,
                    disc_number: disc_number(&metadata),
                    track_number,
                },
                source_ref: TrackSourceRef::SacdTrack {
                    iso: req.container.clone(),
                    track_index: idx as u32,
                    area: requested_area,
                },
                metadata: track_metadata(
                    entry,
                    &metadata,
                    area,
                    requested_area,
                    track_number,
                    sidecar_tracks.get(idx),
                ),
                // The encoded artifact will usually be PCM FLAC/MP3/AAC/etc.,
                // not DSD64. Leave this unset so merge validation probes the
                // real encoded output instead of comparing against 2.8224 MHz
                // SACD source-domain sample counts.
                expected_samples: None,
                sample_rate: SACD_SAMPLE_RATE_HZ,
            });
        }

        let tracks = apply_track_selection(tracks, &req.source.track_selection)?;
        let album_metadata = album_metadata(
            &metadata,
            area,
            requested_area,
            tracks.len() as u32,
            sidecar_tracks.first(),
        );
        let mut tool_versions = BTreeMap::new();
        tool_versions.insert("sacd-rs".to_string(), "in-process".to_string());

        Ok(PreparedSource {
            container: req.container.clone(),
            kind: SourceKind::SacdIso,
            tracks,
            album_metadata,
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SacdIso,
                source_sha256: None,
                tool_versions,
                extracted_at: chrono::Utc::now(),
            },
        })
    }
}

/// Route healthy SACDs into the SACD materializer.
///
/// A generic `.iso` file is not enough by itself. If the cheap detector finds
/// no SACD Master TOC magic, route to SACD only when the request carries an
/// explicit SACD selection. That keeps ordinary data/DVD ISOs in the existing
/// unknown-source path while still allowing deliberate SACD attempts to surface
/// parser errors such as `MaterializeError::Encrypted`.
pub(crate) fn is_sacd_iso_candidate(req: &PipelineRequest) -> Result<bool, SourceDetectError> {
    let detection = detect_sacd_iso(&req.container);
    Ok(is_sacd_detection_positive(detection)
        || (explicit_sacd_requested(req) && has_extension(&req.container, "iso")))
}

fn explicit_sacd_requested(req: &PipelineRequest) -> bool {
    req.source.sacd_area.is_some()
}

fn is_sacd_detection_positive(detection: DetectionResult) -> bool {
    matches!(
        detection,
        DetectionResult::HealthyAllRedundant | DetectionResult::HealthyPartialRedundant { .. }
    )
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(ext))
        .unwrap_or(false)
}

fn requested_sacd_area(area: Option<SacdArea>) -> SacdArea {
    area.unwrap_or(SacdArea::Stereo)
}

fn sacd_area_info(metadata: &SacdMetadata, area: SacdArea) -> Option<&AreaInfo> {
    match area {
        SacdArea::Stereo => metadata.stereo.as_ref(),
        SacdArea::MultiChannel => metadata.multi_channel.as_ref(),
    }
}

fn sacd_area_label(area: SacdArea) -> &'static str {
    match area {
        SacdArea::Stereo => "stereo",
        SacdArea::MultiChannel => "multichannel",
    }
}

fn sacd_area_kind_label(kind: AreaKind) -> &'static str {
    match kind {
        AreaKind::Stereo => "stereo",
        AreaKind::MultiChannel => "multichannel",
    }
}

fn disc_number(metadata: &SacdMetadata) -> Option<u32> {
    if metadata.master_toc.album_set_size > 1 {
        Some(u32::from(metadata.master_toc.album_sequence_number))
    } else {
        None
    }
}

fn album_metadata(
    metadata: &SacdMetadata,
    area: &AreaInfo,
    requested_area: SacdArea,
    total_tracks: u32,
    sidecar_first_track: Option<&SidecarTrack>,
) -> AlbumMetadata {
    let mut extra = BTreeMap::new();
    insert_nonempty(
        &mut extra,
        "sacd_area",
        sacd_area_label(requested_area).to_string(),
    );
    insert_nonempty(
        &mut extra,
        "sacd_area_kind",
        sacd_area_kind_label(area.header.kind).to_string(),
    );
    insert_nonempty(
        &mut extra,
        "sacd_album_catalog_number",
        metadata.master_toc.album_catalog_number.clone(),
    );
    insert_nonempty(
        &mut extra,
        "sacd_disc_catalog_number",
        metadata.master_toc.disc_catalog_number.clone(),
    );
    insert_nonempty(
        &mut extra,
        "sacd_disc_type_hybrid",
        metadata.master_toc.disc_type_hybrid.to_string(),
    );
    insert_nonempty(
        &mut extra,
        "sacd_channel_count",
        area.header.channel_count.to_string(),
    );
    insert_nonempty(
        &mut extra,
        "sacd_frame_format",
        format!("{:?}", area.header.frame_format),
    );
    insert_nonempty(
        &mut extra,
        "sacd_dst_encoded",
        area.header.frame_format.is_dst_encoded().to_string(),
    );
    if let Some(description) = &area.header.description {
        insert_nonempty(&mut extra, "sacd_area_description", description.clone());
    }
    if let Some(copyright) = &area.header.copyright {
        insert_nonempty(&mut extra, "sacd_area_copyright", copyright.clone());
    }

    let sc = |key: &str| sidecar_first_track.and_then(|t| t.meta.get(key)).cloned();

    AlbumMetadata {
        album: sc("ALBUM")
            .or_else(|| metadata.album_title().map(str::to_string))
            .or_else(|| area.header.description.clone()),
        album_artist: sc("ARTIST").or_else(|| metadata.album_artist().map(str::to_string)),
        genre: sc("GENRE").or_else(|| first_genre(metadata)),
        date: sc("DATE").or_else(|| format_disc_date(metadata.master_toc.disc_date)),
        total_tracks,
        total_discs: if metadata.master_toc.album_set_size > 1 {
            Some(u32::from(metadata.master_toc.album_set_size))
        } else {
            None
        },
        disc_number: disc_number(metadata),
        extra,
    }
}

fn track_metadata(
    entry: &TrackEntry,
    metadata: &SacdMetadata,
    area: &AreaInfo,
    requested_area: SacdArea,
    track_number: u32,
    sidecar: Option<&SidecarTrack>,
) -> TrackMetadata {
    let mut extra = BTreeMap::new();
    insert_nonempty(
        &mut extra,
        "sacd_area",
        sacd_area_label(requested_area).to_string(),
    );
    insert_nonempty(&mut extra, "sacd_start_lsn", entry.start_lsn.to_string());
    insert_nonempty(&mut extra, "sacd_length_lsn", entry.length_lsn.to_string());
    insert_nonempty(
        &mut extra,
        "sacd_start_frame",
        playtime_to_frame_count(entry.start_time).to_string(),
    );
    insert_nonempty(
        &mut extra,
        "sacd_duration_frames",
        playtime_to_frame_count(entry.duration).to_string(),
    );
    insert_nonempty(
        &mut extra,
        "sacd_channel_count",
        area.header.channel_count.to_string(),
    );
    insert_nonempty(
        &mut extra,
        "sacd_frame_format",
        format!("{:?}", area.header.frame_format),
    );
    insert_nonempty(
        &mut extra,
        "sacd_dst_encoded",
        area.header.frame_format.is_dst_encoded().to_string(),
    );

    if let Some(value) = &entry.text.songwriter {
        insert_nonempty(&mut extra, "sacd_songwriter", value.clone());
    }
    if let Some(value) = &entry.text.arranger {
        insert_nonempty(&mut extra, "sacd_arranger", value.clone());
    }
    if let Some(value) = &entry.text.extra_message {
        insert_nonempty(&mut extra, "sacd_extra_message", value.clone());
    }
    if let Some(value) = &entry.text.title_phonetic {
        insert_nonempty(&mut extra, "sacd_title_phonetic", value.clone());
    }
    if let Some(value) = &entry.text.performer_phonetic {
        insert_nonempty(&mut extra, "sacd_performer_phonetic", value.clone());
    }
    if let Some(value) = &entry.text.composer_phonetic {
        insert_nonempty(&mut extra, "sacd_composer_phonetic", value.clone());
    }

    // Sidecar is primary source for text metadata; TOC is fallback.
    let sc = |key: &str| sidecar.and_then(|t| t.meta.get(key)).cloned();

    TrackMetadata {
        title: sc("TITLE").or_else(|| entry.text.title.clone()),
        artist: sc("ARTIST")
            .or_else(|| entry.text.performer.clone())
            .or_else(|| metadata.album_artist().map(str::to_string)),
        album_artist: sc("ARTIST").or_else(|| metadata.album_artist().map(str::to_string)),
        composer: entry.text.composer.clone(),
        performer: sc("ARTIST").or_else(|| entry.text.performer.clone()),
        genre: sc("GENRE")
            .or_else(|| entry.genre.map(genre_to_string))
            .or_else(|| first_genre(metadata)),
        date: sc("DATE").or_else(|| format_disc_date(metadata.master_toc.disc_date)),
        track_number: Some(track_number),
        disc_number: disc_number(metadata),
        isrc: sc("ISRC").or_else(|| entry.isrc.clone()),
        publisher: None,
        copyright: area.header.copyright.clone(),
        comment: entry.text.message.clone(),
        pre_emphasis: false,
        extra: {
            // Preserve TOC ISRC in extra when sidecar overrides it.
            if let (Some(sidecar_isrc), Some(toc_isrc)) = (sc("ISRC"), &entry.isrc) {
                if sidecar_isrc != *toc_isrc {
                    insert_nonempty(&mut extra, "sacd_toc_isrc", toc_isrc.clone());
                }
            }
            // Carry sidecar-only fields into extra.
            if let Some(sidecar_track) = sidecar {
                for (key, value) in &sidecar_track.meta {
                    match key.as_str() {
                        // Already mapped to top-level fields.
                        "TITLE" | "ARTIST" | "GENRE" | "DATE" | "ISRC"
                        | "TRACKNUMBER" | "TOTALTRACKS" => {}
                        // Preserve everything else (MusicBrainz IDs, etc.).
                        _ => { insert_nonempty(&mut extra, &key.to_lowercase(), value.clone()); }
                    }
                }
            }
            extra
        },
    }
}

fn first_genre(metadata: &SacdMetadata) -> Option<String> {
    metadata
        .master_toc
        .disc_genres
        .first()
        .or_else(|| metadata.master_toc.album_genres.first())
        .copied()
        .map(genre_to_string)
}

fn genre_to_string(genre: Genre) -> String {
    genre.name().to_string()
}

fn format_disc_date(date: Option<crate::tui::sacd::DiscDate>) -> Option<String> {
    let date = date?;
    if date.year == 0 {
        None
    } else if date.month == 0 {
        Some(format!("{:04}", date.year))
    } else if date.day == 0 {
        Some(format!("{:04}-{:02}", date.year, date.month))
    } else {
        Some(format!(
            "{:04}-{:02}-{:02}",
            date.year, date.month, date.day
        ))
    }
}

fn playtime_to_frame_count(time: PlayTime) -> u32 {
    u32::from(time.minutes) * 60 * SACD_FRAME_RATE
        + u32::from(time.seconds) * SACD_FRAME_RATE
        + u32::from(time.frames)
}

fn insert_nonempty(extra: &mut BTreeMap<String, String>, key: &str, value: String) {
    if !value.trim().is_empty() {
        extra.insert(key.to_string(), value);
    }
}

fn apply_track_selection(
    tracks: Vec<PreparedTrack>,
    selection: &TrackSelection,
) -> Result<Vec<PreparedTrack>, MaterializeError> {
    match selection {
        TrackSelection::All => Ok(tracks),
        TrackSelection::Range { start, end } => {
            if *start == 0 || *end == 0 || start > end {
                return Err(MaterializeError::InvalidTrackSelection(format!(
                    "invalid range {start}-{end}"
                )));
            }
            let max_ordinal = tracks.len() as u32;
            if *start > max_ordinal {
                return Err(MaterializeError::InvalidTrackSelection(format!(
                    "range start {start} exceeds track count {max_ordinal}"
                )));
            }
            Ok(tracks
                .into_iter()
                .filter(|track| {
                    track.id.source_ordinal >= *start && track.id.source_ordinal <= *end
                })
                .collect())
        }
        TrackSelection::Set(indices) => {
            if indices.is_empty() {
                return Err(MaterializeError::InvalidTrackSelection(
                    "empty track set".to_string(),
                ));
            }
            let max_ordinal = tracks.len() as u32;
            for &idx in indices {
                if idx == 0 || idx > max_ordinal {
                    return Err(MaterializeError::InvalidTrackSelection(format!(
                        "track {idx} outside valid range 1-{max_ordinal}"
                    )));
                }
            }
            Ok(tracks
                .into_iter()
                .filter(|track| indices.contains(&track.id.source_ordinal))
                .collect())
        }
    }
}

fn sacd_error_to_materialize(err: SacdError, explicit_sacd_request: bool) -> MaterializeError {
    match err {
        SacdError::NotSacdIso if explicit_sacd_request => MaterializeError::Encrypted,
        SacdError::NotSacdIso => MaterializeError::Parse(err.to_string()),
        SacdError::Malformed(message) if looks_encrypted(&message) => MaterializeError::Encrypted,
        SacdError::Malformed(message) => MaterializeError::Parse(message),
        SacdError::TooSmall { .. } => MaterializeError::Parse(err.to_string()),
        SacdError::Io(message) => MaterializeError::Extraction(message),
    }
}

fn looks_encrypted(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("encrypted") || lower.contains("scrambled") || lower.contains("cipher")
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::path::PathBuf;

    pub(crate) fn default_area_for_test(area: Option<SacdArea>) -> SacdArea {
        requested_sacd_area(area)
    }

    pub(crate) fn sacd_expected_samples_for_test() -> Option<u64> {
        None
    }

    pub(crate) fn detection_positive_for_test(detection: DetectionResult) -> bool {
        is_sacd_detection_positive(detection)
    }

    pub(crate) fn explicit_sacd_for_test(req: &PipelineRequest) -> bool {
        explicit_sacd_requested(req)
    }

    pub(crate) fn encrypted_mapping_for_test(
        err: SacdError,
        explicit_sacd_request: bool,
    ) -> MaterializeError {
        sacd_error_to_materialize(err, explicit_sacd_request)
    }

    pub(crate) fn selection_ordinals_for_test(
        track_count: u32,
        selection: TrackSelection,
    ) -> Result<Vec<u32>, MaterializeError> {
        let tracks = (1..=track_count)
            .map(|ordinal| PreparedTrack {
                id: TrackId {
                    source_ordinal: ordinal,
                    disc_number: None,
                    track_number: ordinal,
                },
                source_ref: TrackSourceRef::SacdTrack {
                    iso: PathBuf::from("disc.iso"),
                    track_index: ordinal - 1,
                    area: SacdArea::Stereo,
                },
                metadata: TrackMetadata::default(),
                expected_samples: None,
                sample_rate: SACD_SAMPLE_RATE_HZ,
            })
            .collect();
        apply_track_selection(tracks, &selection).map(|tracks| {
            tracks
                .into_iter()
                .map(|track| track.id.source_ordinal)
                .collect()
        })
    }
}

/// Load sidecar tracks for the requested area, if a sidecar XML exists.
/// Returns an empty vec if no sidecar is found or parsing fails.
fn load_sidecar_tracks(iso: &Path, sacd_area: Option<SacdArea>) -> Vec<SidecarTrack> {
    let sidecar_path = match find_sidecar_for_iso(iso) {
        Some(p) => p,
        None => return Vec::new(),
    };
    let sidecar = match parse_sidecar(&sidecar_path) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("SACD sidecar at {} could not be parsed: {:?}", sidecar_path.display(), e);
            return Vec::new();
        }
    };
    let area_index = match sacd_area.unwrap_or(SacdArea::Stereo) {
        SacdArea::Stereo => 1_u8,
        SacdArea::MultiChannel => 2_u8,
    };
    sidecar.tracks_for_area(area_index).into_iter().cloned().collect()
}
