use std::path::Path;

use crate::tui::sacd::{AreaKind, SacdMetadata};
use crate::tui::sacd_sidecar::SidecarMetadata;

use super::diagnostics::{DiagnosticSeverity, DiagnosticScope, DiscDiagnostic};
use super::labels;
use super::model::*;

/// DSD64 sample rate in Hz.
const SACD_SAMPLE_RATE_HZ: u32 = 2_822_400;

/// Build a unified `DiscContents` from a parsed SACD ISO.
///
/// Sidecar metadata (if available) provides higher-quality track titles
/// and album metadata. The mapper does not perform I/O — the caller loads
/// the sidecar and passes it in.
pub fn map_sacd_disc(
    metadata: &SacdMetadata,
    sidecar: Option<&SidecarMetadata>,
    source_path: &Path,
) -> DiscContents {
    let file_stem = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    // Album metadata: sidecar first, TOC fallback
    let sidecar_first_track = sidecar
        .and_then(|s| s.tracks.first());
    let album_title = sidecar_first_track
        .and_then(|t| t.meta.get("ALBUM"))
        .cloned()
        .or_else(|| metadata.album_title().map(|s| s.to_string()));
    let label = labels::disc_label(
        album_title.as_deref(),
        "",
        file_stem,
        DiscFormat::Sacd,
    );
    let album_artist = sidecar_first_track
        .and_then(|t| t.meta.get("ARTIST"))
        .cloned()
        .or_else(|| metadata.album_artist().map(|s| s.to_string()));
    let genre = sidecar_first_track
        .and_then(|t| t.meta.get("GENRE"))
        .cloned()
        .or_else(|| {
            metadata.master_toc.disc_genres.first()
                .or(metadata.master_toc.album_genres.first())
                .map(|g| g.name().to_string())
        });
    let year = sidecar_first_track
        .and_then(|t| t.meta.get("DATE"))
        .cloned()
        .or_else(|| {
            metadata.master_toc.disc_date.as_ref()
                .map(|d| d.year.to_string())
        });

    // Retain distinct tag authorities, even though presentation labels prefer
    // sidecar text. A user may reverse the precedence when extracting.
    let toc = &metadata.master_toc;
    let catalog = if !toc.disc_catalog_number.is_empty() { &toc.disc_catalog_number }
        else { &toc.album_catalog_number };
    let publisher = metadata.master_text.as_ref()
        .and_then(|text| text.disc_publisher.as_ref().or(text.album_publisher.as_ref()))
        .filter(|value| !value.trim().is_empty());
    let authored_album_tags = DiscExtractTagFields {
        catalog: (!catalog.trim().is_empty()).then(|| catalog.clone()),
        publisher: publisher.cloned(),
        album: metadata.album_title().map(str::to_string),
        artist: metadata.album_artist().map(str::to_string),
        genre: metadata.master_toc.disc_genres.first()
            .or(metadata.master_toc.album_genres.first()).map(|genre|genre.name().to_string()),
        year: metadata.master_toc.disc_date.as_ref().map(|date|date.year.to_string()),
        ..Default::default()
    };
    let sidecar_album_tags = DiscExtractTagFields {
        catalog: sidecar_first_track.and_then(|track|track.meta.get("CATALOGNUMBER")
            .or_else(||track.meta.get("CATALOG"))).cloned(),
        publisher: sidecar_first_track.and_then(|track|track.meta.get("PUBLISHER")
            .or_else(||track.meta.get("LABEL"))).cloned(),
        album: sidecar_first_track.and_then(|track|track.meta.get("ALBUM")).cloned(),
        artist: sidecar_first_track.and_then(|track|track.meta.get("ARTIST")).cloned(),
        genre: sidecar_first_track.and_then(|track|track.meta.get("GENRE")).cloned(),
        year: sidecar_first_track.and_then(|track|track.meta.get("DATE")).cloned(),
        ..Default::default()
    };
    let mut presentations = Vec::new();
    let mut diagnostics = Vec::new();
    let mut details = Vec::new();
    if !catalog.is_empty() { details.push(format!("Catalog {catalog}")); }
    if let Some(publisher) = publisher {
        if !publisher.trim().is_empty() { details.push(format!("Publisher {publisher}")); }
    }
    if let Some(date) = toc.disc_date {
        let text = if date.month == 0 { date.year.to_string() }
            else if date.day == 0 { format!("{:04}-{:02}",date.year,date.month) }
            else { format!("{:04}-{:02}-{:02}",date.year,date.month,date.day) };
        details.push(format!("Date {text}"));
    }
    if toc.album_set_size > 1 && toc.album_sequence_number > 0 {
        details.push(format!("Disc {} of {}",toc.album_sequence_number,toc.album_set_size));
    }
    if toc.disc_type_hybrid { details.push("Hybrid".into()); }
    let sacd_header_details = (!details.is_empty()).then(|| details.join(" · "));

    // Map stereo area
    if let Some(area) = &metadata.stereo {
        let mut pres = map_area(area, AreaKind::Stereo, sidecar, 1, &mut diagnostics);
        pres.extract.sacd_header_details = sacd_header_details.clone();
        pres.extract.authored_album_tags = authored_album_tags.clone();
        pres.extract.sidecar_album_tags = sidecar_album_tags.clone();
        pres.album_title = album_title.clone();
        pres.album_artist = album_artist.clone();
        pres.genre = genre.clone();
        pres.year = year.clone();
        presentations.push(pres);
    }

    // Map multichannel area
    if let Some(area) = &metadata.multi_channel {
        let mut pres = map_area(area, AreaKind::MultiChannel, sidecar, 2, &mut diagnostics);
        pres.extract.sacd_header_details = sacd_header_details.clone();
        pres.extract.authored_album_tags = authored_album_tags.clone();
        pres.extract.sidecar_album_tags = sidecar_album_tags.clone();
        pres.album_title = album_title.clone();
        pres.album_artist = album_artist.clone();
        pres.genre = genre.clone();
        pres.year = year.clone();
        presentations.push(pres);
    }

    DiscContents {
        format: DiscFormat::Sacd,
        label,
        source_path: source_path.to_path_buf(),
        presentations,
        suppressed: Vec::new(),
        copy_protection: CopyProtectionSummary {
            description: "None".to_string(),
        },
        diagnostics,
        album_title,
        album_artist,
        genre,
        year,
    }
}

fn map_area(
    area: &crate::tui::sacd::AreaInfo,
    kind: AreaKind,
    sidecar: Option<&SidecarMetadata>,
    area_one_based: u8,
    diagnostics: &mut Vec<DiscDiagnostic>,
) -> DiscPresentation {
    let channel_count = area.header.channel_count;
    let channel_layout = sacd_channel_layout(channel_count);
    let is_dst = area.header.frame_format.is_dst_encoded();
    let total_duration = area.header.total_playtime.total_seconds();

    let area_id = match kind {
        AreaKind::Stereo => SacdAreaId::Stereo,
        AreaKind::MultiChannel => SacdAreaId::MultiChannel,
    };
    let pres_id = PresentationId::SacdArea(area_id);

    let format = AudioPresentationFormat {
        codec: Some("DSD".to_string()),
        sample_rate: Some(SACD_SAMPLE_RATE_HZ),
        bit_depth: Some(1),
        channels: Some(channel_count),
        channel_layout: Some(channel_layout.clone()),
        lossless: true,
        provenance: FormatProvenance::TocHeader,
    };

    // Build label: "DSD64 Stereo" or "DSD64 5.0 Multichannel"
    let label = format!("DSD64 {}", channel_layout);

    // Resolve sidecar tracks for this area
    let sidecar_tracks = sidecar.map(|s| s.tracks_for_area(area_one_based));

    let authored_track_tags: Vec<DiscExtractTagFields> = area.tracks.iter().map(|entry| {
        DiscExtractTagFields {
            title: entry.text.title.clone(),
            artist: entry.text.performer.clone(),
            performer: entry.text.performer.clone(),
            composer: entry.text.composer.clone(),
            genre: entry.genre.as_ref().map(|genre|genre.name().to_string()),
            ..Default::default()
        }
    }).collect();
    let sidecar_track_tags: Vec<DiscExtractTagFields> = area.tracks.iter().enumerate()
        .map(|(i,_)| {
            let metadata = sidecar_tracks.as_ref().and_then(|tracks| tracks.get(i));
            let get = |key| metadata.and_then(|track|track.meta.get(key)).cloned();
            DiscExtractTagFields {
                title: get("TITLE"), artist: get("ARTIST"),
                composer: get("COMPOSER"), performer: get("PERFORMER"),
                genre: get("GENRE"), year: get("DATE"),
                catalog: get("CATALOGNUMBER").or_else(||get("CATALOG")),
                publisher: get("PUBLISHER").or_else(||get("LABEL")),
                ..Default::default()
            }
        }).collect();
    // Map tracks
    let tracks: Vec<DiscTrack> = area
        .tracks
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            // Sidecar title/performer take priority over TOC
            let sidecar_track = sidecar_tracks
                .as_ref()
                .and_then(|st| st.get(i));
            let title = sidecar_track
                .and_then(|t| t.meta.get("TITLE"))
                .cloned()
                .or_else(|| entry.text.title.clone());
            let performer = sidecar_track
                .and_then(|t| t.meta.get("ARTIST"))
                .cloned()
                .or_else(|| entry.text.performer.clone());

            // Carry only fields that really exist in the SACD TOC/track text.
            // The selection window displays these for the focused track;
            // the DST flag stays available to the existing format selector.
            let mut details = Vec::new();
            if is_dst { details.push("DST encoded".to_string()); }
            if let Some(isrc) = &entry.isrc { details.push(format!("ISRC {isrc}")); }
            if let Some(composer) = &entry.text.composer { details.push(format!("Composer {composer}")); }
            if let Some(songwriter) = &entry.text.songwriter { details.push(format!("Songwriter {songwriter}")); }
            if let Some(arranger) = &entry.text.arranger { details.push(format!("Arranger {arranger}")); }
            if let Some(genre) = &entry.genre { details.push(format!("Genre {}", genre.name())); }
            let format_note = (!details.is_empty()).then(|| details.join(" · "));

            DiscTrack {
                number: (i + 1) as u32,
                title,
                performer,
                duration_secs: Some(entry.duration.total_seconds()),
                format_note,
            }
        })
        .collect();

    // Check for consistency issues
    let issue_count = area.consistency.issues.len();
    if issue_count > 0 {
        diagnostics.push(DiscDiagnostic {
            severity: DiagnosticSeverity::Warning,
            scope: DiagnosticScope::Presentation(pres_id.clone()),
            message: format!(
                "SACD {} area has {} TOC consistency issue{}",
                match kind {
                    AreaKind::Stereo => "stereo",
                    AreaKind::MultiChannel => "multichannel",
                },
                issue_count,
                if issue_count == 1 { "" } else { "s" },
            ),
        });
    }

    DiscPresentation {
        extract: PresentationExtractInfo { authored_track_tags, sidecar_track_tags, ..Default::default() },
        id: pres_id,
        label,
        format,
        tracks,
        total_duration_secs: total_duration,
        album_title: None,
        album_artist: None,
        genre: None,
        year: None,
    }
}

/// Derive a channel layout label from SACD channel count.
fn sacd_channel_layout(channel_count: u8) -> String {
    match channel_count {
        1 => "Mono".to_string(),
        2 => "Stereo".to_string(),
        5 => "5.0".to_string(),
        6 => "5.1".to_string(),
        n => format!("{}ch", n),
    }
}
