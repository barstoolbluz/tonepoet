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

    let mut presentations = Vec::new();
    let mut diagnostics = Vec::new();

    // Map stereo area
    if let Some(area) = &metadata.stereo {
        let mut pres = map_area(area, AreaKind::Stereo, sidecar, 1, &mut diagnostics);
        pres.album_title = album_title.clone();
        pres.album_artist = album_artist.clone();
        pres.genre = genre.clone();
        pres.year = year.clone();
        presentations.push(pres);
    }

    // Map multichannel area
    if let Some(area) = &metadata.multi_channel {
        let mut pres = map_area(area, AreaKind::MultiChannel, sidecar, 2, &mut diagnostics);
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

            let format_note = if is_dst {
                Some("DST encoded".to_string())
            } else {
                None
            };

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
