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
        if let Some(probe) = probe_group_aob_format_with_path(volume, &disc, group, Some(path)) {
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
    probe_group_aob_format_with_path(volume, disc, group, None)
}

/// Probe with an optional source path for cross-ATS raw ISO sector reads.
/// When the title set has no AOB files and `source_path` points to an ISO,
/// compute the disc-absolute sector from AOTT + ATSI metadata and read
/// directly from the ISO file.
pub fn probe_group_aob_format_with_path(
    volume: &dyn DvdaVolume,
    disc: &DvdaDisc,
    group: &DvdaGroup,
    source_path: Option<&Path>,
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

    // Try reading from this title set's AOBs. Read up to 8 sectors so the
    // MLP major sync scan has enough data (some streams don't place the
    // major sync in the very first access unit frame).
    let reader = AobSectorReader::new(volume, &title_set.aobs);
    let from_aob = reader
        .read_blocks(first_sector, 8)
        .or_else(|_| reader.read_blocks(first_sector, 1))
        .ok();
    let cross_ats = from_aob.is_none();
    let sector_data = from_aob.or_else(|| {
            // Title set has no AOBs (e.g., ATS 2 on discs where all audio
            // lives within ATS 1's AOB files). Compute the disc-absolute
            // sector from AOTT atsi_mat_sector + ATSI atstt_vobs, and read
            // directly from the ISO.
            let iso_path = source_path?;
            if !iso_path.is_file() {
                return None;
            }
            let aott_entry = disc
                .amg
                .audio_title_table
                .iter()
                .find(|e| e.title_set_nr == title_ref.title_set_nr)?;
            let disc_lba = u64::from(aott_entry.atsi_mat_sector)
                + u64::from(title_set.header.atstt_vobs)
                + u64::from(first_sector);
            let byte_offset = disc_lba * 2048;
            use std::io::{Read, Seek, SeekFrom};
            let mut file = std::fs::File::open(iso_path).ok()?;
            let file_len = file.metadata().ok()?.len();
            if byte_offset + 2048 > file_len {
                return None;
            }
            file.seek(SeekFrom::Start(byte_offset)).ok()?;
            // Read up to 512 sectors (~1 MB) — the MLP major sync may not
            // appear in the first access unit of a cross-ATS stream.
            let probe_sectors = 512u64;
            let read_len = (probe_sectors * 2048).min(file_len - byte_offset) as usize;
            let mut buf = vec![0u8; read_len];
            file.read_exact(&mut buf).ok()?;
            Some(buf)
        })?;

    // Demux sectors and probe. For multi-sector reads (cross-ATS fallback),
    // demux each 2048-byte sector independently and concatenate MLP payloads
    // to find the major sync which may span multiple sectors.
    let sector_count = sector_data.len() / 2048;
    let mut codec_kind = None;
    let mut pcm_result = None;
    let mut mlp_payload = Vec::new();

    for i in 0..sector_count {
        let sector = &sector_data[i * 2048..(i + 1) * 2048];
        let packets = match parse_private_stream_1_packets(sector) {
            Ok(p) => p,
            Err(_) => continue,
        };
        for packet in &packets {
            match packet.sub_header.kind() {
                DvdaSubstreamKind::Pcm => {
                    if pcm_result.is_none() {
                        if let Some(pcm) = packet.sub_header.pcm.as_ref() {
                            if let (Some(rate), Some(bits)) = (pcm.group1_sample_rate, pcm.group1_bits) {
                                if let Some(ca) = channel_assignment(pcm.channel_assignment) {
                                    let ch_label = super::labels::channel_layout_label(
                                        pcm.channel_assignment,
                                        ca.group1_channels + ca.group2_channels,
                                    );
                                    pcm_result = Some(AobProbeResult {
                                        codec: "LPCM",
                                        sample_rate: rate,
                                        bit_depth: bits,
                                        channels: ca.group1_channels + ca.group2_channels,
                                        channel_assignment_code: pcm.channel_assignment,
                                        channel_label: ch_label,
                                        stereo_downmix_source_label: None,
                                    });
                                }
                            }
                        }
                    }
                    codec_kind = Some(DvdaSubstreamKind::Pcm);
                }
                DvdaSubstreamKind::Mlp => {
                    mlp_payload.extend_from_slice(packet.payload);
                    codec_kind = Some(DvdaSubstreamKind::Mlp);
                }
                _ => {}
            }
        }
    }

    match codec_kind? {
        DvdaSubstreamKind::Pcm => pcm_result,
        DvdaSubstreamKind::Mlp => {
            let info = probe_mlp_major_sync(&mlp_payload)?;
            let stereo_downmix_source_label =
                detect_stereo_downmix_source(disc, group, cross_ats, info.channel_count);
            let ch_label = if let Some(source_label) = stereo_downmix_source_label.as_deref() {
                format!("Stereo (derived from {})", source_label)
            } else {
                super::labels::channel_layout_label(
                    info.channel_arrangement as u8,
                    info.channel_count as u8,
                )
            };
            Some(AobProbeResult {
                codec: "MLP",
                sample_rate: info.group1_sample_rate,
                bit_depth: info.group1_bits,
                channels: info.channel_count as u8,
                channel_assignment_code: info.channel_arrangement as u8,
                channel_label: ch_label,
                stereo_downmix_source_label,
            })
        }
        _ => None,
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
                    performer: None,
                    duration_secs: Some(f64::from(ch.len_in_pts) / PTS_PER_SEC),
                    format_note: None,
                });
            }
        }
    }

    tracks
}

/// Detect whether a group is an authored stereo presentation of a multichannel
/// source. Returns `Some(source_layout_label)` (e.g., `Some("5.1")`) when the
/// evidence is strong, `None` otherwise.
///
/// The heuristic checks for the AOB-less-ATS pattern: the group's title set has
/// no AOB files, the resolved MLP stream is multichannel, and a sibling group
/// with matching track count and near-matching duration owns the AOBs.
fn detect_stereo_downmix_source(
    disc: &DvdaDisc,
    group: &DvdaGroup,
    cross_ats: bool,
    mlp_channel_count: u32,
) -> Option<String> {
    if !cross_ats || mlp_channel_count <= 2 {
        return None;
    }

    let my_tracks = group_track_count(disc, group);
    let my_duration = group_duration_secs(disc, group);
    if my_tracks == 0 {
        return None;
    }

    // Find a sibling group whose title set owns AOB files, with matching
    // track count and near-matching duration.
    for sibling in &disc.groups {
        if sibling.group_nr == group.group_nr {
            continue;
        }

        // Check sibling's title set has existing AOBs
        let sibling_has_aobs = sibling.title_refs.iter().any(|tr| {
            disc.title_sets
                .iter()
                .find(|ts| ts.number == tr.title_set_nr)
                .map(|ts| ts.aobs.iter().any(|a| a.exists))
                .unwrap_or(false)
        });
        if !sibling_has_aobs {
            continue;
        }

        let sib_tracks = group_track_count(disc, sibling);
        let sib_duration = group_duration_secs(disc, sibling);

        if sib_tracks != my_tracks || !durations_near_match(my_duration, sib_duration) {
            continue;
        }

        // Sibling matches. Get its channel layout from IFO audio_formats.
        for tr in &sibling.title_refs {
            if let Some(ts) = disc.title_sets.iter().find(|ts| ts.number == tr.title_set_nr) {
                let present: Vec<_> = ts.audio_formats.iter().filter(|a| a.present).collect();
                if let [attr] = present.as_slice() {
                    if let Some(ref ca) = attr.channel_assignment {
                        let total = ca.group1_channels + ca.group2_channels;
                        if total > 2 {
                            return Some(super::labels::channel_layout_label(ca.code, total));
                        }
                    }
                }
            }
        }

        // Sibling matched structurally but IFO format couldn't resolve.
        // Use the probed MLP channel count as fallback.
        return Some(super::labels::channel_layout_label(
            0xFF, // unknown code — falls back to "{N}ch"
            mlp_channel_count as u8,
        ));
    }

    None
}

fn durations_near_match(a: f64, b: f64) -> bool {
    let diff = (a - b).abs();
    let max_dur = a.max(b);
    diff <= max_dur * 0.01 || diff <= 30.0
}
