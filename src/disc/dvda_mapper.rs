use std::collections::BTreeMap;
use std::path::Path;

use crate::tui::dvda::{CopyProtectionSource, DvdaDisc};

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
    let file_stem = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    let label = labels::disc_label(
        None,
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
        let tracks = dvda_utils::build_dvda_tracks(disc, group);

        presentations.push(DiscPresentation {
            id: group_id,
            label: pres_label,
            format,
            tracks,
            total_duration_secs: duration,
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
        album_title: None,
        album_artist: None,
        genre: None,
        year: None,
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
