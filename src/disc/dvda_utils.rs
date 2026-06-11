use std::collections::BTreeMap;
use std::path::Path;

use crate::convert::pipeline::{
    parse_private_stream_1_packets, probe_mlp_major_sync, DvdaSubstreamKind,
};
use crate::tui::dvda::sector::AobSectorReader;
use crate::tui::dvda::{
    channel_assignment, parse_dvda_volume, AudioAttributes, ChannelAssignment,
    DirectoryDvdaVolume, DvdaDisc, DvdaGroup, DvdaVolume, IsoUdfDvdaVolume, TitleRefKind,
};

use super::dvda_mapper::map_dvda_disc;
use super::model::{AobProbeResult, AudioPresentationFormat, FormatProvenance};
use super::model::DiscContents;


/// Return true when `path` is a DVD-Audio ISO image.
///
/// This is a bounded classification helper for the browse scan path. It rejects
/// non-ISO and implausibly-small files before opening the DVD-Audio volume
/// abstraction and checking only the canonical `AUDIO_TS/AUDIO_TS.IFO` member.
/// It does not parse the disc or probe AOB audio streams.
pub fn is_dvda_iso(path: &Path) -> bool {
    let is_iso = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("iso"))
        .unwrap_or(false);
    if !is_iso || !path.is_file() {
        return false;
    }
    if std::fs::metadata(path).map(|m| m.len() < 32 * 2048).unwrap_or(true) {
        return false;
    }

    IsoUdfDvdaVolume::open(path)
        .and_then(|volume| volume.open_audio_ts_file("AUDIO_TS.IFO").map(|_| ()))
        .is_ok()
}

/// Return true when `path` is a filesystem DVD-Audio directory.
pub fn is_dvda_directory(path: &Path) -> bool {
    path.is_dir() && path.join("AUDIO_TS").join("AUDIO_TS.IFO").is_file()
}

/// Return true for any browsable DVD-Audio source supported by the parser.
pub fn is_dvda_source(path: &Path) -> bool {
    is_dvda_directory(path) || is_dvda_iso(path)
}

/// Parse a DVD-Audio ISO or directory and map it to `DiscContents`.
///
/// This is intentionally for the async `spawn_blocking` probe path, not browse
/// classification or rendering: it parses the disc and probes AOB sectors.
pub fn map_dvda_source(path: &Path) -> Result<DiscContents, String> {
    if is_dvda_directory(path) {
        let volume = DirectoryDvdaVolume::new(path);
        return map_dvda_volume(&volume, path);
    }

    let volume = IsoUdfDvdaVolume::open(path)
        .map_err(|e| format!("DVD-Audio ISO open failed for '{}': {e}", path.display()))?;
    map_dvda_volume(&volume, path)
}

fn map_dvda_volume(volume: &dyn DvdaVolume, path: &Path) -> Result<DiscContents, String> {
    let disc = parse_dvda_volume(volume)
        .map_err(|e| format!("DVD-Audio parse failed for '{}': {e}", path.display()))?;

    let mut probes = BTreeMap::new();
    for group in &disc.groups {
        if let Some(probe) = probe_group_aob_format(volume, &disc, group) {
            probes.insert(group.group_nr, probe);
        }
    }

    Ok(map_dvda_disc(&disc, &probes, path))
}

/// Probe the first AOB sector of a group's first track to determine the actual
/// codec (MLP vs LPCM) and audio format from the stream itself.
/// Returns `None` if AOBs are unavailable, the group has no title_refs, or
/// demuxing fails.
pub fn probe_group_aob_format(
    volume: &dyn DvdaVolume,
    disc: &DvdaDisc,
    group: &DvdaGroup,
) -> Option<AobProbeResult> {
    let title_ref = group.title_refs.first()?;
    let title_set = disc
        .title_sets
        .iter()
        .find(|ts| ts.number == title_ref.title_set_nr)?;
    let title = match title_ref.kind {
        TitleRefKind::AottTitleOrdinal => title_set
            .titles
            .iter()
            .find(|t| t.title_ordinal == title_ref.title_nr),
        TitleRefKind::AtsPgcTitleNr => title_set
            .titles
            .iter()
            .find(|t| t.title_nr == title_ref.title_nr),
    }?;
    let chapter = title.chapters.first()?;
    let first_sector = chapter.sector_ranges.first()?.first;

    let reader = AobSectorReader::new(volume, &title_set.aobs);
    let sector_data = reader.read_blocks(first_sector, 1).ok()?;

    let packets = parse_private_stream_1_packets(&sector_data).ok()?;
    let packet = packets.first()?;

    match packet.sub_header.kind() {
        DvdaSubstreamKind::Pcm => {
            let pcm = packet.sub_header.pcm.as_ref()?;
            let rate = pcm.group1_sample_rate?;
            let bits = pcm.group1_bits?;
            let ca = channel_assignment(pcm.channel_assignment)?;
            Some(AobProbeResult {
                codec: "LPCM",
                sample_rate: rate,
                bit_depth: bits,
                channels: ca.group1_channels + ca.group2_channels,
                channel_assignment_code: pcm.channel_assignment,
            })
        }
        DvdaSubstreamKind::Mlp => {
            let info = probe_mlp_major_sync(packet.payload)?;
            Some(AobProbeResult {
                codec: "MLP",
                sample_rate: info.group1_sample_rate,
                bit_depth: info.group1_bits,
                channels: info.channel_count as u8,
                channel_assignment_code: info.channel_arrangement as u8,
            })
        }
        DvdaSubstreamKind::Unknown(_) => None,
    }
}

/// Resolve track count for a DVD-Audio group. Uses three-tier fallback:
/// AOTT title_refs → SAMG tracks → AOTT table entry.
pub fn group_track_count(disc: &DvdaDisc, group: &DvdaGroup) -> usize {
    if !group.title_refs.is_empty() {
        let mut count = 0usize;
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
                TitleRefKind::AtsPgcTitleNr => {
                    ts.titles.iter().find(|t| t.title_nr == title_ref.title_nr)
                }
            };
            if let Some(t) = title {
                count += t.chapters.len();
            }
        }
        if count > 0 {
            return count;
        }
    }
    if !group.samg_tracks.is_empty() {
        return group.samg_tracks.len();
    }
    disc.amg
        .audio_title_table
        .iter()
        .find(|e| e.ordinal == u16::from(group.group_nr))
        .map(|e| usize::from(e.track_count))
        .unwrap_or(0)
}

/// Resolve total duration in seconds for a DVD-Audio group.
/// Sums PTS values from chapters, with SAMG fallback.
pub fn group_duration_secs(disc: &DvdaDisc, group: &DvdaGroup) -> f64 {
    const PTS_PER_SEC: f64 = 90_000.0;
    let mut total_pts: u64 = 0;

    if !group.title_refs.is_empty() {
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
                TitleRefKind::AtsPgcTitleNr => {
                    ts.titles.iter().find(|t| t.title_nr == title_ref.title_nr)
                }
            };
            if let Some(t) = title {
                for ch in &t.chapters {
                    total_pts += u64::from(ch.len_in_pts);
                }
            }
        }
        if total_pts > 0 {
            return total_pts as f64 / PTS_PER_SEC;
        }
    }

    if let Some(samg) = disc.samg.as_ref() {
        for samg_ref in &group.samg_tracks {
            if let Some(track) = samg.tracks.iter().find(|t| {
                t.ordinal == samg_ref.samg_ordinal
                    && t.group_nr == samg_ref.group_nr
                    && t.track_nr == samg_ref.track_nr
            }) {
                total_pts += u64::from(track.len_in_pts);
            }
        }
    }

    total_pts as f64 / PTS_PER_SEC
}

/// Resolve structured audio format for a DVD-Audio group from IFO/SAMG metadata.
/// Returns format fields suitable for building an AudioPresentationFormat.
/// AOB probe results take priority when available — call this as the fallback.
pub fn resolve_group_format(disc: &DvdaDisc, group: &DvdaGroup) -> AudioPresentationFormat {
    let mut rates: Vec<u32> = Vec::new();
    let mut depths: Vec<u32> = Vec::new();
    let mut best_assignment: Option<&ChannelAssignment> = None;
    let mut provenance = FormatProvenance::Unknown;

    // Walk title_refs → title_set → audio_formats
    for title_ref in &group.title_refs {
        let Some(ts) = disc
            .title_sets
            .iter()
            .find(|ts| ts.number == title_ref.title_set_nr)
        else {
            continue;
        };
        let present: Vec<&AudioAttributes> =
            ts.audio_formats.iter().filter(|a| a.present).collect();
        if let [attr] = present.as_slice() {
            let cf = &attr.channel_format;
            if let Some(r) = cf.group1_sample_rate.or(cf.group2_sample_rate) {
                if !rates.contains(&r) {
                    rates.push(r);
                }
            }
            if let Some(d) = cf.group1_bits.or(cf.group2_bits) {
                let d32 = u32::from(d);
                if !depths.contains(&d32) {
                    depths.push(d32);
                }
            }
            if let Some(ref ca) = attr.channel_assignment {
                best_assignment = Some(ca);
            }
            provenance = FormatProvenance::IfoAttributes;
        }
    }

    // SAMG fallback when title_refs didn't resolve format info
    if rates.is_empty() && depths.is_empty() && best_assignment.is_none() {
        if let Some(samg) = disc.samg.as_ref() {
            for samg_ref in &group.samg_tracks {
                if let Some(track) = samg.tracks.iter().find(|t| {
                    t.ordinal == samg_ref.samg_ordinal
                        && t.group_nr == samg_ref.group_nr
                        && t.track_nr == samg_ref.track_nr
                }) {
                    let cf = &track.channel_format;
                    if let Some(r) = cf.group1_sample_rate.or(cf.group2_sample_rate) {
                        if !rates.contains(&r) {
                            rates.push(r);
                        }
                    }
                    if let Some(d) = cf.group1_bits.or(cf.group2_bits) {
                        let d32 = u32::from(d);
                        if !depths.contains(&d32) {
                            depths.push(d32);
                        }
                    }
                    if let Some(ref ca) = track.channel_assignment {
                        best_assignment = Some(ca);
                    }
                    provenance = FormatProvenance::Samg;
                }
            }
        }
    }

    let sample_rate = if rates.len() == 1 {
        Some(rates[0])
    } else {
        None
    };
    let bit_depth = if depths.len() == 1 {
        Some(depths[0])
    } else {
        None
    };
    let (channels, channel_layout) = if let Some(ca) = best_assignment {
        let total = ca.group1_channels + ca.group2_channels;
        let label = super::labels::channel_layout_label(ca.code, total);
        (Some(total), Some(label))
    } else {
        (None, None)
    };

    AudioPresentationFormat {
        codec: None,
        sample_rate,
        bit_depth,
        channels,
        channel_layout,
        lossless: true,
        provenance,
    }
}

/// Build per-track DiscTrack entries for a DVD-Audio group.
pub fn build_dvda_tracks(disc: &DvdaDisc, group: &DvdaGroup) -> Vec<super::model::DiscTrack> {
    const PTS_PER_SEC: f64 = 90_000.0;
    let mut tracks = Vec::new();

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
            TitleRefKind::AtsPgcTitleNr => {
                ts.titles.iter().find(|t| t.title_nr == title_ref.title_nr)
            }
        };
        if let Some(t) = title {
            for ch in &t.chapters {
                tracks.push(super::model::DiscTrack {
                    number: u32::from(ch.track_nr),
                    title: None,
                    duration_secs: Some(f64::from(ch.len_in_pts) / PTS_PER_SEC),
                    format_note: None,
                });
            }
        }
    }

    tracks
}
