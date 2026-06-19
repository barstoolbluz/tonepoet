#![forbid(unsafe_code)]

use crate::tui::dvda::error::{DvdaError, Result};
use crate::tui::dvda::ifo::{amg::parse_amg, atsi::parse_atsi, samg::parse_samg};
use crate::tui::dvda::model::{
    groups_from_disc_parts, CopyProtectionInfo, CopyProtectionSource, DvdaDiagnostic, DvdaDisc,
    MAX_AUDIO_TITLESETS,
};
use crate::tui::dvda::sector::build_aob_inventory;
use crate::tui::dvda::volume::DvdaVolume;

pub fn parse_dvda_volume<V: DvdaVolume + ?Sized>(volume: &V) -> Result<DvdaDisc> {
    let mut diagnostics = Vec::new();
    let (amg_source, amg_bytes) = volume.read_with_backup("AUDIO_TS.IFO", "AUDIO_TS.BUP")?;
    let amg = parse_amg(&amg_bytes, &amg_source)?;

    let requested_ats = amg.audio_title_sets.min(MAX_AUDIO_TITLESETS);
    if amg.audio_title_sets > MAX_AUDIO_TITLESETS {
        diagnostics.push(DvdaDiagnostic::warn(
            "dvda.amg.too_many_audio_title_sets",
            format!("AMG declares {} audio title sets; capped at {}", amg.audio_title_sets, MAX_AUDIO_TITLESETS),
        ));
    }

    let mut title_sets = Vec::new();
    for title_set_nr in 1..=requested_ats {
        let ifo = format!("ATS_{title_set_nr:02}_0.IFO");
        let bup = format!("ATS_{title_set_nr:02}_0.BUP");
        let (source, bytes) = match volume.read_with_backup(&ifo, &bup) {
            Ok(found) => found,
            Err(DvdaError::MissingFile { .. }) => {
                diagnostics.push(DvdaDiagnostic::warn(
                    "dvda.atsi.missing",
                    format!("missing ATSI for title set {title_set_nr}"),
                ));
                continue;
            }
            Err(err) => return Err(err),
        };
        let aobs = build_aob_inventory(volume, title_set_nr)?;
        title_sets.push(parse_atsi(&bytes, &source, title_set_nr, aobs)?);
    }

    let samg = match volume.read_audio_ts_file("AUDIO_PP.IFO") {
        Ok(bytes) => {
            let parsed = parse_samg(&bytes, "AUDIO_PP.IFO")?;
            diagnostics.extend(parsed.diagnostics.iter().cloned());
            Some(parsed)
        },
        Err(DvdaError::MissingFile { .. }) => {
            diagnostics.push(DvdaDiagnostic::info("dvda.samg.absent", "AUDIO_PP.IFO is absent"));
            None
        }
        Err(err) => return Err(err),
    };

    let supplemental_video_ifo_present = volume.exists_audio_ts_file("AUDIO_SV.IFO");
    let mkb_present = volume.exists_audio_ts_file("DVDAUDIO.MKB");
    let copy_protection = CopyProtectionInfo {
        mkb_present,
        cppm_detected: mkb_present,
        source: if mkb_present { CopyProtectionSource::MkbPresence } else { CopyProtectionSource::NotDetected },
    };

    diagnostics.extend(cross_reference_diagnostics(&amg, &title_sets, samg.as_ref()));
    let groups = groups_from_disc_parts(&amg.audio_title_table, &title_sets, samg.as_ref());

    Ok(DvdaDisc {
        amg,
        title_sets,
        samg,
        groups,
        copy_protection,
        supplemental_video_ifo_present,
        diagnostics,
    })
}

fn cross_reference_diagnostics(
    amg: &crate::tui::dvda::model::AmgInfo,
    title_sets: &[crate::tui::dvda::model::TitleSet],
    samg: Option<&crate::tui::dvda::model::SamgInfo>,
) -> Vec<DvdaDiagnostic> {
    let mut out = Vec::new();
    for entry in &amg.audio_title_table {
        if !entry.playback_type.is_audio {
            continue;
        }
        let found = title_sets.iter().any(|ts| {
            ts.number == entry.title_set_nr && ts.titles.iter().any(|title| title.title_ordinal == entry.title_nr)
        });
        if !found {
            out.push(DvdaDiagnostic::warn(
                "dvda.aott.unresolved_title",
                format!(
                    "AOTT entry {} references ATS {:02} title {}, but ATSI did not contain that title",
                    entry.ordinal, entry.title_set_nr, entry.title_nr
                ),
            ));
        }
    }

    if let Some(samg) = samg {
        let atsi_track_count: usize = title_sets
            .iter()
            .flat_map(|ts| ts.titles.iter())
            .map(|title| title.chapters.len())
            .sum();
        if samg.tracks.len() < atsi_track_count {
            out.push(DvdaDiagnostic::warn(
                "dvda.samg.incomplete_relative_to_atsi",
                format!(
                    "SAMG has {} tracks while ATSI hierarchy has {}; retaining ATSI as authoritative",
                    samg.tracks.len(), atsi_track_count
                ),
            ));
        }
        for track in &samg.tracks {
            if track.abs_first_sector != track.abs_first_sector_dup {
                out.push(DvdaDiagnostic::warn(
                    "dvda.samg.duplicate_sector_mismatch",
                    format!(
                        "SAMG track {}.{} has abs_first_sector={} but duplicate={}",
                        track.group_nr, track.track_nr, track.abs_first_sector, track.abs_first_sector_dup
                    ),
                ));
            }
        }
    }

    out
}
