use std::collections::BTreeMap;
use std::path::Path;

use crate::convert::pipeline::{
    parse_private_stream_1_packets_with_mode, probe_mlp_major_sync, DvdaSubHeaderMode,
    DvdaSubstreamKind,
};
use crate::tui::dvda::sector::AobSectorReader;
use crate::tui::dvda::{
    channel_assignment, parse_dvda_volume, AudioAttributes, AudioChapter, AudioTitle,
    ChannelAssignment, DirectoryDvdaVolume, DvdaDisc, DvdaGroup, DvdaVolume, IsoUdfDvdaVolume,
    SamgTrack, SamgZone, TitleRef, TitleRefKind, TitleSet,
};

use super::dvda_mapper::map_dvda_disc_with_metabase;
use super::model::DiscContents;
use super::model::{AobProbeResult, AudioPresentationFormat, FormatProvenance};
use crate::tui::dvda_metabase::{self, DvdaMetabase};

const DVDA_AOB_FORMAT_PROBE_SECTORS: u32 = 512;
const DVDA_AOB_FORMAT_PROBE_MIN_SECTORS: u32 = 8;


/// Authored source context for an AOB probe.
///
/// The bytes may come from a backing ATS AOB inventory while the authored group
/// still comes from an AOB-less cross-ATS title set. Downmix detection must use
/// the authored origin, not only the physical file that supplied the bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AobProbeOrigin {
    pub authored_cross_ats: bool,
    pub source_title_set_nr: u8,
    pub backing_title_set_nr: u8,
}

impl AobProbeOrigin {
    pub const fn local(title_set_nr: u8) -> Self {
        Self {
            authored_cross_ats: false,
            source_title_set_nr: title_set_nr,
            backing_title_set_nr: title_set_nr,
        }
    }

    pub const fn cross_ats(source_title_set_nr: u8, backing_title_set_nr: u8) -> Self {
        Self {
            authored_cross_ats: true,
            source_title_set_nr,
            backing_title_set_nr,
        }
    }
}

/// AOB probe result plus packet-level evidence gathered before format facts were resolved.
///
/// High-rate MLP streams can begin far from the next major-sync frame. In that case
/// the packet scanner can prove that MLP packets are present even when it cannot yet
/// report sample rate, depth, or channel layout.
pub struct AobProbeOutcome {
    pub result: Option<AobProbeResult>,
    pub saw_mlp_packets: bool,
    pub saw_lpcm_packets: bool,
    pub scanned_sectors: u32,
    pub origin: AobProbeOrigin,
}


/// Sector translation selected for an AOB-less source ATS that borrows audio
/// sectors from a backing ATS with real AOB files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrossAtsAobSectorTranslation {
    /// Raw chapter sectors already address the backing ATS AOB inventory.
    Identity,
    /// Raw chapter sectors must be translated through source/resolved
    /// disc-absolute bases before addressing the backing ATS AOB inventory.
    CrossAtsAob {
        source_disc_absolute_base: u32,
        resolved_disc_absolute_base: u32,
    },
}

/// Shared cross-ATS AOB resolution used by both the materializer and the
/// browse/disc-info probe path. Keeping the candidate selection here prevents
/// those paths from choosing different backing ATS inventories for the same
/// AOB-less source ATS.
#[derive(Clone, Debug)]
pub struct CrossAtsAobResolution<'a> {
    pub source_title_set_nr: u8,
    pub resolved_title_set: &'a TitleSet,
    pub source_disc_absolute_base: u32,
    pub resolved_disc_absolute_base: u32,
    pub sector_translation: CrossAtsAobSectorTranslation,
}

struct CrossAtsAobCandidate<'a> {
    resolution: CrossAtsAobResolution<'a>,
}

/// Resolve a source ATS with no AOB files to the single backing ATS whose AOB
/// inventory covers every chapter range after either disc-absolute or identity
/// translation.
///
/// Returns `Ok(None)` when no backing ATS fits. Returns `Err` when more than one
/// backing ATS fits, because falling through to an ISO absolute-sector read can
/// mask an ambiguous or stale disc model.
pub fn resolve_cross_ats_backing_aob_title_set<'a>(
    disc: &'a DvdaDisc,
    source_title_set_nr: u8,
    title: &AudioTitle,
    source_disc_absolute_base: u32,
) -> Result<Option<CrossAtsAobResolution<'a>>, String> {
    let mut candidates = Vec::new();

    for candidate in &disc.title_sets {
        if candidate.number == source_title_set_nr || !title_set_has_existing_aobs(candidate) {
            continue;
        }

        let Some(resolved_disc_absolute_base) = title_set_disc_absolute_base(disc, candidate) else {
            continue;
        };

        let Some(sector_translation) = title_ranges_fit_cross_ats_aobs(
            title,
            source_disc_absolute_base,
            resolved_disc_absolute_base,
            &candidate.aobs,
        ) else {
            continue;
        };

        candidates.push(CrossAtsAobCandidate {
            resolution: CrossAtsAobResolution {
                source_title_set_nr,
                resolved_title_set: candidate,
                source_disc_absolute_base,
                resolved_disc_absolute_base,
                sector_translation,
            },
        });
    }

    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.into_iter().next().map(|candidate| candidate.resolution)),
        _ => {
            let alternatives = candidates
                .iter()
                .map(|candidate| candidate.resolution.resolved_title_set.number.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "DVD-Audio ATS {source_title_set_nr} has no AOB files and maps into multiple backing ATS AOB inventories: {alternatives}"
            ))
        }
    }
}

fn title_ranges_fit_cross_ats_aobs(
    title: &AudioTitle,
    source_disc_absolute_base: u32,
    resolved_disc_absolute_base: u32,
    aobs: &[crate::tui::dvda::AobFileEntry],
) -> Option<CrossAtsAobSectorTranslation> {
    let translated = CrossAtsAobSectorTranslation::CrossAtsAob {
        source_disc_absolute_base,
        resolved_disc_absolute_base,
    };
    if title_ranges_fit_aobs_using_translation(title, aobs, translated) {
        return Some(translated);
    }

    if title_ranges_fit_aobs_using_translation(
        title,
        aobs,
        CrossAtsAobSectorTranslation::Identity,
    ) {
        return Some(CrossAtsAobSectorTranslation::Identity);
    }

    None
}

fn title_ranges_fit_aobs_using_translation(
    title: &AudioTitle,
    aobs: &[crate::tui::dvda::AobFileEntry],
    translation: CrossAtsAobSectorTranslation,
) -> bool {
    if title.chapters.is_empty() {
        return false;
    }

    for chapter in &title.chapters {
        if chapter.sector_ranges.is_empty() {
            return false;
        }
        for range in &chapter.sector_ranges {
            let Some((first, last)) = translate_cross_ats_aob_range(
                range.first,
                range.last,
                translation,
            ) else {
                return false;
            };
            if !aob_entries_cover_range(first, last, aobs) {
                return false;
            }
        }
    }

    true
}

pub fn translate_cross_ats_aob_range(
    first: u32,
    last: u32,
    translation: CrossAtsAobSectorTranslation,
) -> Option<(u32, u32)> {
    if last < first {
        return None;
    }

    match translation {
        CrossAtsAobSectorTranslation::Identity => Some((first, last)),
        CrossAtsAobSectorTranslation::CrossAtsAob {
            source_disc_absolute_base,
            resolved_disc_absolute_base,
        } => {
            let first = translate_cross_ats_sector(
                first,
                source_disc_absolute_base,
                resolved_disc_absolute_base,
            )?;
            let last = translate_cross_ats_sector(
                last,
                source_disc_absolute_base,
                resolved_disc_absolute_base,
            )?;
            (last >= first).then_some((first, last))
        }
    }
}

pub fn translate_cross_ats_aob_sector(
    sector: u32,
    translation: CrossAtsAobSectorTranslation,
) -> Option<u32> {
    translate_cross_ats_aob_range(sector, sector, translation).map(|(sector, _)| sector)
}

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
    if std::fs::metadata(path)
        .map(|m| m.len() < 32 * 2048)
        .unwrap_or(true)
    {
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

    let loaded_metabase = match dvda_metabase::load_metabase(volume, path) {
        Ok(metabase) => metabase,
        Err(e) => {
            log::warn!(
                "DVD-Audio metabase load failed for '{}': {}",
                path.display(),
                e
            );
            None
        }
    };

    let mut probes = BTreeMap::new();
    for group in &disc.groups {
        if let Some(probe) = probe_group_aob_format_with_path(volume, &disc, group, Some(path)) {
            probes.insert(group.group_nr, probe);
        }
    }

    Ok(map_dvda_disc_with_metabase(
        &disc,
        &probes,
        loaded_metabase.as_ref().map(|loaded| &loaded.metabase),
        path,
    ))
}

fn samg_sector_block_count(track: &SamgTrack) -> u64 {
    if track.abs_last_sector < track.abs_first_sector {
        0
    } else {
        u64::from(track.abs_last_sector) - u64::from(track.abs_first_sector) + 1
    }
}

fn chapter_sector_span(chapter: &AudioChapter) -> Option<(u32, u32, u64)> {
    let first = chapter
        .sector_ranges
        .iter()
        .map(|range| range.first)
        .min()?;
    let last = chapter.sector_ranges.iter().map(|range| range.last).max()?;
    let blocks = chapter
        .sector_ranges
        .iter()
        .map(|range| u64::from(range.block_count()))
        .sum();
    Some((first, last, blocks))
}

fn samg_disc_absolute_base_for_title(disc: &DvdaDisc, title: &AudioTitle) -> Option<u32> {
    let samg = disc.samg.as_ref()?;
    if title.chapters.is_empty() {
        return None;
    }
    let chapter_spans: Option<Vec<_>> = title.chapters.iter().map(chapter_sector_span).collect();
    let chapter_spans = chapter_spans?;

    let mut tracks_by_group: BTreeMap<u8, Vec<&SamgTrack>> = BTreeMap::new();
    for track in samg
        .tracks
        .iter()
        .filter(|track| matches!(track.zone, SamgZone::Vob))
    {
        tracks_by_group
            .entry(track.group_nr)
            .or_default()
            .push(track);
    }

    for tracks in tracks_by_group.values_mut() {
        tracks.sort_by_key(|track| (track.track_nr, track.ordinal));
        if tracks.len() != chapter_spans.len() {
            continue;
        }
        let mut base: Option<u32> = None;
        let mut matched = true;
        for (track, (chapter_first, chapter_last, chapter_blocks)) in
            tracks.iter().zip(chapter_spans.iter().copied())
        {
            if samg_sector_block_count(track) != chapter_blocks {
                matched = false;
                break;
            }
            let Some(candidate_base) = track.abs_first_sector.checked_sub(chapter_first) else {
                matched = false;
                break;
            };
            let Some(expected_last) = candidate_base.checked_add(chapter_last) else {
                matched = false;
                break;
            };
            if track.abs_last_sector != expected_last {
                matched = false;
                break;
            }
            if let Some(existing_base) = base {
                if existing_base != candidate_base {
                    matched = false;
                    break;
                }
            } else {
                base = Some(candidate_base);
            }
        }
        if matched {
            return base;
        }
    }

    None
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
/// prefer a SAMG VOB-derived disc-absolute base, fall back to AMG/AOTT + ATSI
/// metadata, and read directly from the ISO file.
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

    probe_title_chapter_aob_format_with_path(
        volume,
        disc,
        group,
        title_ref,
        title_set,
        title,
        chapter,
        source_path,
    )
}

/// Probe a specific ATS title chapter to determine its actual stream codec and
/// audio format. Unlike `probe_group_aob_format_with_path`, this does not
/// assume that every track in a group has the same presentation.
pub fn probe_title_chapter_aob_format_with_path(
    volume: &dyn DvdaVolume,
    disc: &DvdaDisc,
    group: &DvdaGroup,
    title_ref: &TitleRef,
    title_set: &TitleSet,
    title: &AudioTitle,
    chapter: &AudioChapter,
    source_path: Option<&Path>,
) -> Option<AobProbeResult> {
    probe_title_chapter_aob_format_with_path_outcome(
        volume,
        disc,
        group,
        title_ref,
        title_set,
        title,
        chapter,
        source_path,
    )
    .and_then(|outcome| outcome.result)
}

/// Probe a specific ATS title chapter and retain packet-level evidence for callers
/// that can inherit stream facts from another track in the same group.
pub fn probe_title_chapter_aob_format_with_path_outcome(
    volume: &dyn DvdaVolume,
    disc: &DvdaDisc,
    group: &DvdaGroup,
    title_ref: &TitleRef,
    title_set: &TitleSet,
    title: &AudioTitle,
    chapter: &AudioChapter,
    source_path: Option<&Path>,
) -> Option<AobProbeOutcome> {
    probe_title_chapter_aob_format_with_path_outcome_with_origin(
        volume,
        disc,
        group,
        title_ref,
        title_set,
        title,
        chapter,
        source_path,
        AobProbeOrigin::local(title_set.number),
    )
}

/// Probe a chapter using an explicit authored-origin context. This is used when
/// the physical bytes come from a resolved backing ATS but the authored title is
/// still cross-ATS.
#[allow(clippy::too_many_arguments)]
pub fn probe_title_chapter_aob_format_with_path_outcome_with_origin(
    volume: &dyn DvdaVolume,
    disc: &DvdaDisc,
    group: &DvdaGroup,
    title_ref: &TitleRef,
    title_set: &TitleSet,
    title: &AudioTitle,
    chapter: &AudioChapter,
    source_path: Option<&Path>,
    origin: AobProbeOrigin,
) -> Option<AobProbeOutcome> {
    let first_sector = chapter.first_sector()?;

    // Try reading from this title set's own AOBs. Read a bounded 512-sector
    // prefix (~1 MiB), with smaller fallbacks for short or unusual AOBs.
    let reader = AobSectorReader::new(volume, &title_set.aobs);
    let from_aob = reader
        .read_blocks(first_sector, DVDA_AOB_FORMAT_PROBE_SECTORS)
        .or_else(|_| reader.read_blocks(first_sector, DVDA_AOB_FORMAT_PROBE_MIN_SECTORS))
        .or_else(|_| reader.read_blocks(first_sector, 1))
        .ok();

    let mut probe_origin = origin;
    let mut demux_mode = DvdaSubHeaderMode::DvdAudio;
    let sector_data = if let Some(data) = from_aob {
        data
    } else if let Some(read) = read_cross_ats_aob_probe_prefix(
        volume,
        disc,
        group,
        title_ref,
        title_set,
        title,
        chapter,
    ) {
        probe_origin = AobProbeOrigin::cross_ats(title_set.number, read.backing_title_set_nr);
        read.data
    } else {
        // Title set has no usable AOBs. Prefer SAMG VOB absolute sectors when
        // they correlate exactly with the ATS chapter sector ranges; otherwise
        // fall back to AMG/AOTT + ATSI metadata and read directly from the ISO.
        let iso_path = source_path?;
        let samg_disc_absolute_base = samg_disc_absolute_base_for_title(disc, title);
        if samg_disc_absolute_base.is_some() {
            demux_mode = DvdaSubHeaderMode::DvdVideo;
        }
        let disc_absolute_base = samg_disc_absolute_base.or_else(|| {
            title_set_disc_absolute_base_for_group(disc, group, title_ref, title_set)
        })?;
        let disc_lba = disc_absolute_base.checked_add(first_sector)?;
        probe_origin = AobProbeOrigin::cross_ats(title_set.number, title_set.number);
        log::debug!(
            "DVD-Audio AOB probe: cross-ATS ISO read for group {} track {}: disc_absolute_base={} first_sector={} disc_lba={} probe_sectors={}",
            group.group_nr, chapter.track_nr, disc_absolute_base, first_sector, disc_lba, DVDA_AOB_FORMAT_PROBE_SECTORS
        );
        read_iso_sector_prefix(iso_path, disc_lba, DVDA_AOB_FORMAT_PROBE_SECTORS)?
    };

    Some(probe_aob_sector_data_with_evidence(
        disc,
        group,
        probe_origin,
        demux_mode,
        &sector_data,
    ))
}

struct CrossAtsAobProbeRead {
    data: Vec<u8>,
    backing_title_set_nr: u8,
}

fn read_cross_ats_aob_probe_prefix(
    volume: &dyn DvdaVolume,
    disc: &DvdaDisc,
    group: &DvdaGroup,
    title_ref: &TitleRef,
    source_title_set: &TitleSet,
    title: &AudioTitle,
    chapter: &AudioChapter,
) -> Option<CrossAtsAobProbeRead> {
    if title_set_has_existing_aobs(source_title_set) {
        return None;
    }

    let source_disc_absolute_base = samg_disc_absolute_base_for_title(disc, title).or_else(|| {
        title_set_disc_absolute_base_for_group(disc, group, title_ref, source_title_set)
    })?;

    let resolution = match resolve_cross_ats_backing_aob_title_set(
        disc,
        source_title_set.number,
        title,
        source_disc_absolute_base,
    ) {
        Ok(Some(resolution)) => resolution,
        Ok(None) => return None,
        Err(err) => {
            log::warn!("DVD-Audio AOB probe: {err}");
            return None;
        }
    };

    let first_sector = translate_cross_ats_aob_sector(
        chapter.first_sector()?,
        resolution.sector_translation,
    )?;
    let translation_label = match resolution.sector_translation {
        CrossAtsAobSectorTranslation::Identity => "identity",
        CrossAtsAobSectorTranslation::CrossAtsAob { .. } => "translated",
    };

    let reader = AobSectorReader::new(volume, &resolution.resolved_title_set.aobs);
    log::debug!(
        "DVD-Audio AOB probe: source ATS {} track {} reads backing ATS {} AOBs at sector {} using {} translation",
        source_title_set.number,
        chapter.track_nr,
        resolution.resolved_title_set.number,
        first_sector,
        translation_label
    );
    reader
        .read_blocks(first_sector, DVDA_AOB_FORMAT_PROBE_SECTORS)
        .or_else(|_| reader.read_blocks(first_sector, DVDA_AOB_FORMAT_PROBE_MIN_SECTORS))
        .or_else(|_| reader.read_blocks(first_sector, 1))
        .ok()
        .map(|data| CrossAtsAobProbeRead {
            data,
            backing_title_set_nr: resolution.resolved_title_set.number,
        })
}

fn title_set_has_existing_aobs(title_set: &TitleSet) -> bool {
    title_set.aobs.iter().any(|aob| aob.exists)
}

fn title_set_disc_absolute_base_for_group(
    disc: &DvdaDisc,
    group: &DvdaGroup,
    title_ref: &TitleRef,
    title_set: &TitleSet,
) -> Option<u32> {
    let aott_entry = disc
        .amg
        .audio_title_table
        .iter()
        .find(|entry| {
            entry.ordinal == u16::from(group.group_nr)
                && entry.title_set_nr == title_ref.title_set_nr
        })
        .or_else(|| {
            disc.amg
                .audio_title_table
                .iter()
                .find(|entry| entry.title_set_nr == title_ref.title_set_nr)
        })?;
    title_set_audio_vobs_disc_absolute_base(aott_entry, title_set)
}

fn title_set_disc_absolute_base(disc: &DvdaDisc, title_set: &TitleSet) -> Option<u32> {
    disc.amg
        .audio_title_table
        .iter()
        .filter(|entry| entry.title_set_nr == title_set.number)
        .filter_map(|entry| title_set_audio_vobs_disc_absolute_base(entry, title_set))
        .min()
}

fn title_set_audio_vobs_disc_absolute_base(
    aott_entry: &crate::tui::dvda::AudioTitleTableEntry,
    title_set: &TitleSet,
) -> Option<u32> {
    if title_set.header.atsm_vobs != 0 {
        return Some(title_set.header.atsm_vobs);
    }

    aott_entry
        .atsi_mat_sector
        .checked_add(title_set.header.atstt_vobs)
}

#[allow(dead_code)]
fn chapter_first_sector_for_identity_translation(
    chapter: &AudioChapter,
    aobs: &[crate::tui::dvda::AobFileEntry],
) -> Option<u32> {
    chapter_first_sector_for_translation(chapter, aobs, |first, last| {
        (last >= first).then_some((first, last))
    })
}

#[allow(dead_code)]
fn chapter_first_sector_for_cross_ats_translation(
    chapter: &AudioChapter,
    source_disc_absolute_base: u32,
    resolved_disc_absolute_base: u32,
    aobs: &[crate::tui::dvda::AobFileEntry],
) -> Option<u32> {
    chapter_first_sector_for_translation(chapter, aobs, |first, last| {
        let first = translate_cross_ats_sector(first, source_disc_absolute_base, resolved_disc_absolute_base)?;
        let last = translate_cross_ats_sector(last, source_disc_absolute_base, resolved_disc_absolute_base)?;
        (last >= first).then_some((first, last))
    })
}

#[allow(dead_code)]
fn chapter_first_sector_for_translation<F>(
    chapter: &AudioChapter,
    aobs: &[crate::tui::dvda::AobFileEntry],
    mut translate: F,
) -> Option<u32>
where
    F: FnMut(u32, u32) -> Option<(u32, u32)>,
{
    let mut translated_first_sector = None;
    for range in &chapter.sector_ranges {
        let (first, last) = translate(range.first, range.last)?;
        if !aob_entries_cover_range(first, last, aobs) {
            return None;
        }
        if translated_first_sector.is_none() || Some(first) < translated_first_sector {
            translated_first_sector = Some(first);
        }
    }
    translated_first_sector
}

fn translate_cross_ats_sector(
    sector: u32,
    source_disc_absolute_base: u32,
    resolved_disc_absolute_base: u32,
) -> Option<u32> {
    let absolute = u64::from(source_disc_absolute_base).checked_add(u64::from(sector))?;
    let relative = absolute.checked_sub(u64::from(resolved_disc_absolute_base))?;
    u32::try_from(relative).ok()
}

fn aob_entries_cover_range(
    first: u32,
    last: u32,
    aobs: &[crate::tui::dvda::AobFileEntry],
) -> bool {
    if last < first {
        return false;
    }
    let mut cursor = first;
    while cursor <= last {
        let Some(aob) = aobs
            .iter()
            .filter(|aob| aob.exists)
            .find(|aob| aob.block_first <= cursor && cursor <= aob.block_last)
        else {
            return false;
        };
        if aob.block_last >= last {
            return true;
        }
        if aob.block_last == u32::MAX {
            return false;
        }
        cursor = aob.block_last + 1;
    }
    true
}

/// Probe one SAMG-only track by its own absolute sector span instead of using a
/// representative group track.
pub fn probe_samg_track_aob_format_with_path(
    disc: &DvdaDisc,
    group: &DvdaGroup,
    track: &SamgTrack,
    source_path: Option<&Path>,
) -> Option<AobProbeResult> {
    let iso_path = source_path?;
    let demux_mode = match track.zone {
        SamgZone::Aob => DvdaSubHeaderMode::DvdAudio,
        SamgZone::Vob => DvdaSubHeaderMode::DvdVideo,
    };
    let cross_ats = matches!(track.zone, SamgZone::Vob);
    let sector_data = read_iso_sector_prefix(
        iso_path,
        track.abs_first_sector,
        DVDA_AOB_FORMAT_PROBE_SECTORS,
    )?;

    probe_aob_sector_data(disc, group, cross_ats, demux_mode, &sector_data)
}

fn read_iso_sector_prefix(
    iso_path: &Path,
    first_sector: u32,
    max_sectors: u32,
) -> Option<Vec<u8>> {
    if !iso_path.is_file() {
        return None;
    }

    let byte_offset = u64::from(first_sector).checked_mul(2048)?;
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(iso_path).ok()?;
    let file_len = file.metadata().ok()?.len();
    if byte_offset + 2048 > file_len {
        return None;
    }
    file.seek(SeekFrom::Start(byte_offset)).ok()?;
    let read_len = (u64::from(max_sectors) * 2048).min(file_len - byte_offset) as usize;
    let mut buf = vec![0u8; read_len];
    file.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn probe_aob_sector_data(
    disc: &DvdaDisc,
    group: &DvdaGroup,
    cross_ats: bool,
    demux_mode: DvdaSubHeaderMode,
    sector_data: &[u8],
) -> Option<AobProbeResult> {
    let source_title_set_nr = group
        .title_refs
        .first()
        .map(|tr| tr.title_set_nr)
        .unwrap_or(0);
    let origin = if cross_ats {
        AobProbeOrigin::cross_ats(source_title_set_nr, 0)
    } else {
        AobProbeOrigin::local(source_title_set_nr)
    };
    probe_aob_sector_data_with_evidence(disc, group, origin, demux_mode, sector_data).result
}

fn probe_aob_sector_data_with_evidence(
    disc: &DvdaDisc,
    group: &DvdaGroup,
    origin: AobProbeOrigin,
    demux_mode: DvdaSubHeaderMode,
    sector_data: &[u8],
) -> AobProbeOutcome {
    // Demux sectors and probe. For multi-sector reads, demux each 2048-byte
    // sector independently and concatenate MLP payloads to find the major sync,
    // which may span multiple sectors.
    let sector_count = sector_data.len() / 2048;
    let mut codec_kind = None;
    let mut pcm_result = None;
    let mut mlp_payload = Vec::new();
    let mut saw_mlp_packets = false;
    let mut saw_lpcm_packets = false;

    for i in 0..sector_count {
        let sector = &sector_data[i * 2048..(i + 1) * 2048];
        let packets = match parse_private_stream_1_packets_with_mode(sector, demux_mode) {
            Ok(p) => p,
            Err(_) => continue,
        };
        for packet in &packets {
            match packet.sub_header.kind() {
                DvdaSubstreamKind::Pcm => {
                    saw_lpcm_packets = true;
                    if pcm_result.is_none() {
                        if let Some(pcm) = packet.sub_header.pcm.as_ref() {
                            let rate = pcm.group1_sample_rate.or(pcm.group2_sample_rate);
                            let bits = pcm.group1_bits.or(pcm.group2_bits);
                            if let (Some(rate), Some(bits)) = (rate, bits) {
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
                                        mlp_num_substreams: None,
                                    });
                                }
                            }
                        }
                    }
                    codec_kind = Some(DvdaSubstreamKind::Pcm);
                }
                DvdaSubstreamKind::Mlp => {
                    saw_mlp_packets = true;
                    mlp_payload.extend_from_slice(packet.payload);
                    codec_kind = Some(DvdaSubstreamKind::Mlp);
                }
                _ => {}
            }
        }
    }

    let result = match codec_kind {
        Some(DvdaSubstreamKind::Pcm) => pcm_result,
        Some(DvdaSubstreamKind::Mlp) => probe_mlp_major_sync(&mlp_payload).map(|info| {
            let stereo_downmix_source_label = detect_stereo_downmix_source(
                disc,
                group,
                origin.authored_cross_ats,
                info.channel_count,
            );
            let ch_label = if let Some(source_label) = stereo_downmix_source_label.as_deref() {
                format!("Stereo (derived from {})", source_label)
            } else {
                super::labels::channel_layout_label(
                    info.channel_arrangement as u8,
                    info.channel_count as u8,
                )
            };
            AobProbeResult {
                codec: "MLP",
                sample_rate: info.group1_sample_rate,
                bit_depth: info.group1_bits,
                channels: info.channel_count as u8,
                channel_assignment_code: info.channel_arrangement as u8,
                channel_label: ch_label,
                stereo_downmix_source_label,
                mlp_num_substreams: Some(info.num_substreams),
            }
        }),
        _ => None,
    };

    AobProbeOutcome {
        result,
        saw_mlp_packets,
        saw_lpcm_packets,
        scanned_sectors: u32::try_from(sector_count).unwrap_or(u32::MAX),
        origin,
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
    build_dvda_tracks_with_metabase(disc, group, None)
}

/// Build per-track DiscTrack entries, preferring foo_input_dvda metabase tags
/// where the `{titleset}.{title}.{track}` id matches the parsed disc address.
pub fn build_dvda_tracks_with_metabase(
    disc: &DvdaDisc,
    group: &DvdaGroup,
    metabase: Option<&DvdaMetabase>,
) -> Vec<super::model::DiscTrack> {
    const PTS_PER_SEC: f64 = 90_000.0;
    let mut tracks = Vec::new();

    for addr in dvda_metabase::group_track_addrs(disc, group) {
        tracks.push(super::model::DiscTrack {
            number: u32::from(addr.track),
            title: dvda_metabase::track_value(metabase, &addr.id, &["TITLE"]),
            performer: dvda_metabase::track_value(metabase, &addr.id, &["ARTIST", "PERFORMER"]),
            duration_secs: Some(f64::from(addr.len_in_pts) / PTS_PER_SEC),
            format_note: None,
        });
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
            if let Some(ts) = disc
                .title_sets
                .iter()
                .find(|ts| ts.number == tr.title_set_nr)
            {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::dvda::{AobFileEntry, SectorRange};

    fn chapter_with_ranges(ranges: &[(u32, u32)]) -> AudioChapter {
        AudioChapter {
            track_nr: 1,
            track_type: 0,
            track_type_low_bits_candidate: 0,
            downmix_matrix: None,
            index_start: 1,
            first_pts: 0,
            len_in_pts: 90_000,
            sector_ranges: ranges
                .iter()
                .enumerate()
                .map(|(idx, (first, last))| SectorRange {
                    index_nr: u8::try_from(idx + 1).unwrap(),
                    first: *first,
                    last: *last,
                })
                .collect(),
        }
    }

    fn aob_entry(block_first: u32, block_last: u32) -> AobFileEntry {
        AobFileEntry {
            title_set_nr: 1,
            part_nr: 1,
            file_name: "ATS_01_1.AOB".to_string(),
            exists: true,
            byte_len: 2048 * u64::from(block_last - block_first + 1),
            block_first,
            block_last,
        }
    }

    #[test]
    fn cross_ats_disc_info_probe_prefers_identity_when_raw_ranges_fit_backing_aobs() {
        let chapter = chapter_with_ranges(&[(0, 48_190), (48_191, 86_926)]);
        let aobs = vec![aob_entry(0, 2_556_832)];

        assert_eq!(
            chapter_first_sector_for_cross_ats_translation(
                &chapter,
                2_576_316,
                12_239,
                &aobs,
            ),
            None
        );
        assert_eq!(
            chapter_first_sector_for_identity_translation(&chapter, &aobs),
            Some(0)
        );
    }

    #[test]
    fn cross_ats_disc_info_probe_accepts_translated_ranges_when_they_fit() {
        let chapter = chapter_with_ranges(&[(10, 20)]);
        let aobs = vec![aob_entry(110, 120)];

        assert_eq!(
            chapter_first_sector_for_cross_ats_translation(&chapter, 1_100, 1_000, &aobs),
            Some(110)
        );
        assert_eq!(chapter_first_sector_for_identity_translation(&chapter, &aobs), None);
    }
}
