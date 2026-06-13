use std::collections::BTreeMap;
use std::path::Path;

use crate::tui::dvda::{CopyProtectionSource, DvdaDisc};
use crate::tui::dvda_metabase::{self, DvdaMetabase};

use super::diagnostics::{DiagnosticScope, DiagnosticSeverity, DiscDiagnostic};
use super::dvda_utils;
use super::labels;
use super::model::*;

/// Placeholder detection threshold in seconds.
const PLACEHOLDER_DURATION_THRESHOLD: f64 = 5.0;

/// Build a unified `DiscContents` from a parsed DVD-Audio disc.
///
/// `probes` maps group_nr to AOB probe results (may be empty if AOBs are
/// unavailable). The mapper is deterministic — all I/O (volume reading, AOB
/// probing) must happen before this call.
pub fn map_dvda_disc(
    disc: &DvdaDisc,
    probes: &BTreeMap<u8, AobProbeResult>,
    source_path: &Path,
) -> DiscContents {
    let loaded_metabase = match dvda_metabase::load_metabase_for_source_path(source_path) {
        Ok(metabase) => metabase,
        Err(e) => {
            log::warn!(
                "DVD-Audio metabase load failed for '{}': {}",
                source_path.display(),
                e
            );
            None
        }
    };

    map_dvda_disc_with_metabase(
        disc,
        probes,
        loaded_metabase.as_ref().map(|loaded| &loaded.metabase),
        source_path,
    )
}

pub fn map_dvda_disc_with_metabase(
    disc: &DvdaDisc,
    probes: &BTreeMap<u8, AobProbeResult>,
    metabase: Option<&DvdaMetabase>,
    source_path: &Path,
) -> DiscContents {
    let file_stem = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    // Disc-level album metadata: use whole-disc album_value. When groups have
    // different ALBUM values this returns None, which is correct — the disc-level
    // title is ambiguous. Per-presentation album metadata lives on DiscPresentation.
    let album_title = dvda_metabase::album_value(metabase, &["ALBUM"]);
    let album_artist = dvda_metabase::album_value(metabase, &["ALBUMARTIST", "ALBUM ARTIST", "ARTIST"]);
    let genre = dvda_metabase::album_value(metabase, &["GENRE"]);
    let year = dvda_metabase::album_value(metabase, &["DATE", "YEAR"]);

    let label = labels::disc_label(
        album_title.as_deref(),
        &disc.amg.provider_identifier,
        file_stem,
        DiscFormat::DvdAudio,
    );

    let copy_protection = map_copy_protection(&disc.copy_protection);

    let mut presentations = Vec::new();
    let mut suppressed = Vec::new();
    let mut diagnostics = Vec::new();

    for group in &disc.groups {
        let track_count = dvda_utils::group_track_count(disc, group);
        let duration = dvda_utils::group_duration_secs(disc, group);
        let group_id = PresentationId::DvdAudioGroup(group.group_nr);

        // Placeholder heuristic: AOTT-derived track count and duration
        if track_count == 0
            || (track_count == 1 && duration < PLACEHOLDER_DURATION_THRESHOLD)
        {
            suppressed.push(SuppressedPresentation {
                id: group_id.clone(),
                reason: format!(
                    "DVD-Audio placeholder: {} AOTT-derived track{}, {:.1}s duration",
                    track_count,
                    if track_count == 1 { "" } else { "s" },
                    duration,
                ),
                track_count,
                duration_secs: duration,
                native_detail: Some(format!("correlation={:?}", group.correlation)),
            });
            diagnostics.push(DiscDiagnostic {
                severity: DiagnosticSeverity::Warning,
                scope: DiagnosticScope::SuppressedCandidate,
                message: format!(
                    "Group {} suppressed as placeholder: {} track{}, {:.1}s",
                    group.group_nr,
                    track_count,
                    if track_count == 1 { "" } else { "s" },
                    duration,
                ),
            });
            continue;
        }

        // Resolve audio format: AOB probe (priority) → IFO/SAMG (fallback)
        let format = if let Some(probe) = probes.get(&group.group_nr) {
            let ch_label = probe.channel_label.clone();
            diagnostics.push(DiscDiagnostic {
                severity: DiagnosticSeverity::Info,
                scope: DiagnosticScope::Presentation(group_id.clone()),
                message: format!(
                    "Group {} format determined by AOB probe",
                    group.group_nr
                ),
            });
            AudioPresentationFormat {
                codec: Some(probe.codec.to_string()),
                sample_rate: Some(probe.sample_rate),
                bit_depth: Some(probe.bit_depth),
                channels: Some(probe.channels),
                channel_layout: Some(ch_label),
                lossless: true,
                provenance: FormatProvenance::AobProbe,
            }
        } else {
            let resolved = dvda_utils::resolve_group_format(disc, group);
            if resolved.provenance == FormatProvenance::Unknown {
                diagnostics.push(DiscDiagnostic {
                    severity: DiagnosticSeverity::Warning,
                    scope: DiagnosticScope::Presentation(group_id.clone()),
                    message: format!(
                        "Group {} format could not be determined from IFO or SAMG",
                        group.group_nr
                    ),
                });
            }
            resolved
        };

        let pres_label = labels::presentation_label(&format);
        let tracks = dvda_utils::build_dvda_tracks_with_metabase(disc, group, metabase);

        // Per-presentation album metadata scoped to this group's tracks.
        let group_track_ids = dvda_metabase::group_track_ids(disc, group);
        let pres_album_value = |keys: &[&str]| -> Option<String> {
            dvda_metabase::album_value_for_track_ids(metabase, &group_track_ids, keys)
        };

        presentations.push(DiscPresentation {
            id: group_id,
            label: pres_label,
            format,
            tracks,
            total_duration_secs: duration,
            album_title: pres_album_value(&["ALBUM"]),
            album_artist: pres_album_value(&["ALBUMARTIST", "ALBUM ARTIST", "ARTIST"]),
            genre: pres_album_value(&["GENRE"]),
            year: pres_album_value(&["DATE", "YEAR"]),
        });
    }

    DiscContents {
        format: DiscFormat::DvdAudio,
        label,
        source_path: source_path.to_path_buf(),
        presentations,
        suppressed,
        copy_protection,
        diagnostics,
        album_title,
        album_artist,
        genre,
        year,
    }
}

fn map_copy_protection(
    cp: &crate::tui::dvda::CopyProtectionInfo,
) -> CopyProtectionSummary {
    let description = match &cp.source {
        CopyProtectionSource::MkbPresence => {
            if cp.mkb_present {
                "MKB present (no AOB probe)"
            } else {
                "None"
            }
        }
        CopyProtectionSource::MkbPresentAobProbeReadable => "MKB present, AOBs readable",
        CopyProtectionSource::AobProbeNoMpegPs => {
            "MKB present, AOBs NOT readable (CPPM encrypted)"
        }
        CopyProtectionSource::AssumeDecryptedOverride => {
            "MKB present, assumed decrypted (override)"
        }
        CopyProtectionSource::NotDetected => "None",
    };
    CopyProtectionSummary {
        description: description.to_string(),
    }
}
