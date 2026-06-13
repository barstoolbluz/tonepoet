#![forbid(unsafe_code)]

use std::collections::BTreeMap;

pub const DVD_BLOCK_SIZE: u64 = 2048;
pub const MAX_AUDIO_TITLESETS: u8 = 99;
pub const MAX_AOB_PARTS: u8 = 9;
pub const MISSING_AOB_VIRTUAL_BYTES: u64 = (1024 * 1024 - 32) * 1024;
pub const DOWNMIX_SOURCE_CHANNELS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DvdaDisc {
    pub amg: AmgInfo,
    pub title_sets: Vec<TitleSet>,
    pub samg: Option<SamgInfo>,
    pub groups: Vec<DvdaGroup>,
    pub copy_protection: CopyProtectionInfo,
    pub supplemental_video_ifo_present: bool,
    pub diagnostics: Vec<DvdaDiagnostic>,
}

impl DvdaDisc {
    pub fn title_count(&self) -> usize {
        self.title_sets.iter().map(|ts| ts.titles.len()).sum()
    }

    pub fn track_count_from_atsi(&self) -> usize {
        self.title_sets
            .iter()
            .flat_map(|ts| ts.titles.iter())
            .map(|title| title.chapters.len())
            .sum()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyProtectionInfo {
    pub mkb_present: bool,
    pub cppm_detected: bool,
    pub source: CopyProtectionSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyProtectionSource {
    /// `DVDAUDIO.MKB` is present, but no AOB readability probe has refined the result.
    MkbPresence,
    /// `DVDAUDIO.MKB` is present and an AOB probe found readable MPEG-PS data.
    MkbPresentAobProbeReadable,
    /// `DVDAUDIO.MKB` is present and an AOB probe did not find readable MPEG-PS data.
    AobProbeNoMpegPs,
    /// User or caller explicitly declared the AOB data already decrypted.
    AssumeDecryptedOverride,
    NotDetected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DvdaDiagnostic {
    pub severity: DiagnosticSeverity,
    pub code: &'static str,
    pub message: String,
}

impl DvdaDiagnostic {
    pub fn warn(code: &'static str, message: impl Into<String>) -> Self {
        Self { severity: DiagnosticSeverity::Warning, code, message: message.into() }
    }

    pub fn info(code: &'static str, message: impl Into<String>) -> Self {
        Self { severity: DiagnosticSeverity::Info, code, message: message.into() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Info,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmgInfo {
    pub source_file: String,
    pub last_sector: u32,
    pub ifo_last_sector: u32,
    pub specification_version: u8,
    pub category: u32,
    pub nr_of_volumes: u16,
    pub this_volume_nr: u16,
    pub disc_side: u8,
    pub audio_title_sets: u8,
    pub video_title_sets: u8,
    pub provider_identifier: String,
    pub position_code: u64,
    pub ifo_last_byte: u32,
    pub first_play_pgc: u32,
    pub pointers: AmgPointers,
    pub audio_title_table: Vec<AudioTitleTableEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AmgPointers {
    pub amg_asvs: u32,
    pub amgm_vobs: u32,
    pub att_srpt: u32,
    pub aott_srpt: u32,
    pub amgm_pgci_ut: u32,
    pub ats_atrt: u32,
    pub txtdt_mgi: u32,
    pub amgm_c_adt: u32,
    pub amgm_vobu_admap: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioTitleTableEntry {
    pub ordinal: u16,
    pub playback_type: AudioPlaybackType,
    pub track_count: u8,
    pub len_in_pts: u32,
    pub title_set_nr: u8,
    pub title_nr: u8,
    pub atsi_mat_sector: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioPlaybackType {
    pub is_audio: bool,
    pub type_ext: u8,
    pub title_set_nr: u8,
    pub raw: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleSet {
    pub number: u8,
    pub source_file: String,
    pub kind: TitleSetKind,
    pub header: AtsiHeader,
    /// Effective byte offset used to parse `audio_pgcit_t`. The reference
    /// foobar implementation seeks to byte 0x800; this field lets fixtures
    /// prove whether the header pointer and the reference location agree.
    pub audio_pgcit_offset: usize,
    pub audio_formats: Vec<AudioAttributes>,
    pub downmix_matrices: Vec<DownmixMatrix>,
    pub aobs: Vec<AobFileEntry>,
    pub aobs_last_sector: Option<u32>,
    pub titles: Vec<AudioTitle>,
    pub diagnostics: Vec<DvdaDiagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TitleSetKind {
    Audio,
    Video,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtsiHeader {
    pub ats_last_sector: u32,
    pub atsi_last_sector: u32,
    pub specification_version: u8,
    pub category: u32,
    pub atsm_vobs: u32,
    pub atstt_vobs: u32,
    pub ats_ptt_srpt: u32,
    pub ats_pgcit: u32,
    pub ats_c_adt: u32,
    pub ats_vobu_admap: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AobFileEntry {
    pub title_set_nr: u8,
    pub part_nr: u8,
    pub file_name: String,
    pub exists: bool,
    pub byte_len: u64,
    pub block_first: u32,
    pub block_last: u32,
}

impl AobFileEntry {
    pub fn contains(&self, block: u32) -> bool {
        self.exists && block >= self.block_first && block <= self.block_last
    }

    pub fn block_count(&self) -> u32 {
        if self.block_last < self.block_first {
            0
        } else {
            self.block_last - self.block_first + 1
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioAttributes {
    pub format_index: u8,
    pub present: bool,
    pub audio_type_raw: u16,
    pub channel_format: ChannelFormat,
    pub channel_assignment: Option<ChannelAssignment>,
    /// The IFO entry names channel layout and PCM/MLP-compatible group rates/depths.
    /// The exact stream coding is left as Unknown until AOB packet parsing exists.
    pub coding: AudioCoding,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioCoding {
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelFormat {
    pub group1_bits: Option<u8>,
    pub group2_bits: Option<u8>,
    pub group1_sample_rate: Option<u32>,
    pub group2_sample_rate: Option<u32>,
    pub assignment_code: u8,
    pub raw: [u8; 3],
}

impl ChannelFormat {
    pub fn total_channels(&self, assignment: Option<&ChannelAssignment>) -> Option<u8> {
        assignment.map(|a| a.group1_channels + a.group2_channels)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelAssignment {
    pub code: u8,
    pub group1: &'static [&'static str],
    pub group2: &'static [&'static str],
    pub group1_channels: u8,
    pub group2_channels: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownmixMatrix {
    pub index: u8,
    pub raw: [u8; 18],
    pub phase: DownmixPhase,
    pub channels: Vec<DownmixChannelCoefficients>,
}

impl DownmixMatrix {
    pub fn source_channel(&self, source_channel: u8) -> Option<&DownmixChannelCoefficients> {
        self.channels.iter().find(|entry| entry.source_channel == source_channel)
    }

    pub fn left_coefficient(&self, source_channel: u8) -> Option<&DownmixCoefficient> {
        self.source_channel(source_channel).map(|entry| &entry.left)
    }

    pub fn right_coefficient(&self, source_channel: u8) -> Option<&DownmixCoefficient> {
        self.source_channel(source_channel).map(|entry| &entry.right)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownmixPhase {
    pub left_mask: u8,
    pub right_mask: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownmixChannelCoefficients {
    pub source_channel: u8,
    pub left: DownmixCoefficient,
    pub right: DownmixCoefficient,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownmixCoefficient {
    pub raw: u8,
    pub inverse_phase: bool,
}

impl DownmixCoefficient {
    /// Decode the reference attenuation law used by foo_input_dvda.
    /// `None` means coefficient 255, which represents no contribution.
    pub fn attenuation_db(&self) -> Option<f64> {
        if self.raw < 200 {
            Some(-0.2007 * f64::from(self.raw))
        } else if self.raw < 255 {
            Some(-(2.0 * 0.2007 * f64::from(self.raw - 200) + 0.2007 * 200.0))
        } else {
            None
        }
    }

    /// Linear coefficient including phase inversion. This is a helper only;
    /// Phase 1 does not perform downmix DSP.
    pub fn linear_gain(&self) -> Option<f64> {
        let gain = 10.0_f64.powf(self.attenuation_db()? / 20.0);
        Some(if self.inverse_phase { -gain } else { gain })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioTitle {
    pub title_set_nr: u8,
    /// Raw `ats_title_idx_t.title_nr` byte (PGC identifier, often 0x81, 0x82...).
    /// Use `title_ordinal` for AOTT resolution and human-facing numbering.
    pub title_nr: u8,
    /// 1-based ordinal position within the ATS. This is what the AOTT
    /// `audio_title_info_t.title_nr` field references.
    pub title_ordinal: u8,
    pub title_table_offset: u32,
    /// Uniform low three bits observed in this title's raw `track_type` bytes,
    /// when all chapters share the same value. This is a diagnostic hint only:
    /// real discs have shown that these bits do not reliably identify an ATS
    /// audio-format table entry.
    pub uniform_track_type_low_bits_candidate: Option<u8>,
    /// Distinct low-three-bit values observed in raw `track_type` bytes, in
    /// first-seen order. These values are intentionally not named audio-format
    /// indices; Phase 3 must identify the stream format from AOB packet data.
    pub track_type_low_bits_candidates: Vec<u8>,
    pub track_count_declared: u8,
    pub index_count_declared: u8,
    pub len_in_pts: u32,
    pub chapters: Vec<AudioChapter>,
}

/// User-visible DVD-Audio track/chapter record as represented by ATSI.
///
/// The reference implementation names this object `dvda_track_t`. The project
/// brief uses `AudioChapter`; the fields below preserve the foo_input_dvda
/// track semantics while keeping the brief's public type name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioChapter {
    pub track_nr: u8,
    /// Raw ATS track-type byte. Keep this byte as parser evidence only. Its low
    /// three bits are exposed below as a candidate diagnostic value, not as a
    /// decoded audio-format table index.
    pub track_type: u8,
    /// Low three bits of `track_type`, retained as a provisional diagnostic hint.
    /// Do not use this as an audio-format selector; Phase 3 must prove format
    /// identity from AOB packet headers.
    pub track_type_low_bits_candidate: u8,
    pub downmix_matrix: Option<u8>,
    pub index_start: u8,
    pub first_pts: u32,
    pub len_in_pts: u32,
    pub sector_ranges: Vec<SectorRange>,
}

pub type AudioTrack = AudioChapter;

impl AudioChapter {
    pub fn first_sector(&self) -> Option<u32> {
        self.sector_ranges.iter().map(|r| r.first).min()
    }

    pub fn last_sector(&self) -> Option<u32> {
        self.sector_ranges.iter().map(|r| r.last).max()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectorRange {
    pub index_nr: u8,
    pub first: u32,
    pub last: u32,
}

impl SectorRange {
    pub fn block_count(&self) -> u32 {
        if self.last < self.first { 0 } else { self.last - self.first + 1 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SamgInfo {
    pub source_file: String,
    pub specification_version: u8,
    pub track_count_declared: u16,
    pub tracks: Vec<SamgTrack>,
    /// Total bytes supplied by the volume backend. Full SAMG files are expected
    /// to be 128 KiB: eight 16 KiB copies. Short synthetic/unit fixtures are
    /// accepted but reported through diagnostics.
    pub raw_len: usize,
    pub expected_len: usize,
    pub copy_size: usize,
    pub copy_count: u8,
    pub repeated_copies_valid: bool,
    pub copy_validations: Vec<SamgCopyValidation>,
    pub diagnostics: Vec<DvdaDiagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SamgCopyValidation {
    pub copy_index: u8,
    pub byte_start: usize,
    pub matches_first_copy: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SamgTrack {
    pub ordinal: u16,
    pub group_nr: u8,
    pub track_nr: u8,
    pub first_pts: u32,
    pub len_in_pts: u32,
    pub zone: SamgZone,
    pub flags: u8,
    pub channel_format: ChannelFormat,
    pub channel_assignment: Option<ChannelAssignment>,
    pub abs_first_sector: u32,
    pub abs_first_sector_dup: u32,
    pub abs_last_sector: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SamgZone {
    Aob,
    Vob,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DvdaGroup {
    pub group_nr: u8,
    pub title_refs: Vec<TitleRef>,
    pub samg_tracks: Vec<SamgTrackRef>,
    pub correlation: GroupCorrelation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TitleRefKind {
    /// A title reference from the AMG AOTT table. The reference value is an
    /// AOTT/ATSI title ordinal, not a raw ATS PGC title number.
    AottTitleOrdinal,
    /// A fallback title reference synthesized from ATSI data when the AMG AOTT
    /// table is absent. The reference value is the raw ATS PGC title number.
    AtsPgcTitleNr,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TitleRef {
    pub title_set_nr: u8,
    pub title_nr: u8,
    pub kind: TitleRefKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SamgTrackRef {
    pub samg_ordinal: u16,
    pub group_nr: u8,
    pub track_nr: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupCorrelation {
    FromAmgAott,
    FromAtsiFallback,
    /// A PGC title that exists in an ATS PGCIT but is not referenced by any
    /// `is_audio` AOTT entry. Common for stereo presentations on discs where
    /// multichannel and stereo share the same ATS AOBs (e.g., Talking Heads
    /// Rhino DVD-Audio reissues). IFO audio-format facts may not describe
    /// this title accurately.
    OrphanPgcTitle,
    SamgOnly,
    MixedAmgAndSamg,
}

pub fn channel_assignment(code: u8) -> Option<ChannelAssignment> {
    let (g1, g2): (&'static [&'static str], &'static [&'static str]) = match code {
        0 => (&["C"], &[]),
        1 => (&["L", "R"], &[]),
        2 => (&["L", "R"], &["S"]),
        3 => (&["L", "R"], &["Ls", "Rs"]),
        4 => (&["L", "R"], &["LFE"]),
        5 => (&["L", "R"], &["LFE", "S"]),
        6 => (&["L", "R"], &["LFE", "Ls", "Rs"]),
        7 => (&["L", "R"], &["C"]),
        8 => (&["L", "R"], &["C", "S"]),
        9 => (&["L", "R"], &["C", "Ls", "Rs"]),
        10 => (&["L", "R"], &["C", "LFE"]),
        11 => (&["L", "R"], &["C", "LFE", "S"]),
        12 => (&["L", "R"], &["C", "LFE", "Ls", "Rs"]),
        13 => (&["L", "R", "C"], &["S"]),
        14 => (&["L", "R", "C"], &["Ls", "Rs"]),
        15 => (&["L", "R", "C"], &["LFE"]),
        16 => (&["L", "R", "C"], &["LFE", "S"]),
        17 => (&["L", "R", "C"], &["LFE", "Ls", "Rs"]),
        18 => (&["L", "R", "Ls", "Rs"], &["LFE"]),
        19 => (&["L", "R", "Ls", "Rs"], &["C"]),
        20 => (&["L", "R", "Ls", "Rs"], &["C", "LFE"]),
        _ => return None,
    };
    Some(ChannelAssignment {
        code,
        group1: g1,
        group2: g2,
        group1_channels: g1.len() as u8,
        group2_channels: g2.len() as u8,
    })
}

pub fn bit_depth_from_code(code: u8) -> Option<u8> {
    match code {
        0 => Some(16),
        1 => Some(20),
        2 => Some(24),
        _ => None,
    }
}

pub fn sample_rate_from_code(code: u8) -> Option<u32> {
    let base = if (code & 0x08) == 0 { 48_000 } else { 44_100 };
    match code & 0x07 {
        0 => Some(base),
        1 => Some(base * 2),
        2 => Some(base * 4),
        _ => None,
    }
}


/// Return the low three bits of an ATS track-type byte as a diagnostic hint.
///
/// Some earlier parser notes treated these bits as an ATS audio-format table
/// index. Real discs contradict that assumption: the value can stay at zero
/// while the title uses another format entry. Keep this helper deliberately
/// named as a candidate value so callers do not treat it as decode authority.
pub fn track_type_low_bits_candidate(track_type: u8) -> u8 {
    track_type & 0x07
}

pub fn parse_channel_format(raw: [u8; 3]) -> ChannelFormat {
    let group1_bits_code = raw[0] >> 4;
    let group2_bits_code = raw[0] & 0x0f;
    let group1_freq_code = raw[1] >> 4;
    let group2_freq_code = raw[1] & 0x0f;
    let assignment_code = raw[2];
    ChannelFormat {
        group1_bits: bit_depth_from_code(group1_bits_code),
        group2_bits: bit_depth_from_code(group2_bits_code),
        group1_sample_rate: sample_rate_from_code(group1_freq_code),
        group2_sample_rate: sample_rate_from_code(group2_freq_code),
        assignment_code,
        raw,
    }
}

pub(crate) fn groups_from_disc_parts(
    aott: &[AudioTitleTableEntry],
    title_sets: &[TitleSet],
    samg: Option<&SamgInfo>,
) -> Vec<DvdaGroup> {
    let mut groups: BTreeMap<u8, DvdaGroup> = BTreeMap::new();

    if !aott.is_empty() {
        for entry in aott.iter().filter(|entry| entry.playback_type.is_audio) {
            let group_nr = entry.ordinal.min(u8::MAX as u16) as u8;
            let group = groups.entry(group_nr).or_insert_with(|| DvdaGroup {
                group_nr,
                title_refs: Vec::new(),
                samg_tracks: Vec::new(),
                correlation: GroupCorrelation::FromAmgAott,
            });
            let title_ref = TitleRef {
                title_set_nr: entry.title_set_nr,
                title_nr: entry.title_nr,
                kind: TitleRefKind::AottTitleOrdinal,
            };
            if !group.title_refs.contains(&title_ref) {
                group.title_refs.push(title_ref);
            }
        }
    } else {
        let mut ordinal: u8 = 1;
        for ts in title_sets {
            for title in &ts.titles {
                groups.insert(ordinal, DvdaGroup {
                    group_nr: ordinal,
                    title_refs: vec![TitleRef {
                        title_set_nr: ts.number,
                        title_nr: title.title_nr,
                        kind: TitleRefKind::AtsPgcTitleNr,
                    }],
                    samg_tracks: Vec::new(),
                    correlation: GroupCorrelation::FromAtsiFallback,
                });
                ordinal = ordinal.saturating_add(1);
            }
        }
    }

    // Surface orphan PGC titles: titles that exist in an ATS PGCIT but
    // aren't referenced by any AOTT audio entry. Common on DVD-Audio discs
    // where the stereo presentation is a second title in ATS 1 (e.g., all
    // Talking Heads Rhino DVD-Audio reissues).
    if !aott.is_empty() {
        let mut next_group_nr = groups.keys().copied().max().unwrap_or(0).saturating_add(1);
        for ts in title_sets {
            for title in &ts.titles {
                let already_referenced = groups.values().any(|group| {
                    group.title_refs.iter().any(|tr| {
                        tr.title_set_nr == ts.number && match tr.kind {
                            TitleRefKind::AottTitleOrdinal => tr.title_nr == title.title_ordinal,
                            TitleRefKind::AtsPgcTitleNr => tr.title_nr == title.title_nr,
                        }
                    })
                });
                if !already_referenced {
                    groups.insert(next_group_nr, DvdaGroup {
                        group_nr: next_group_nr,
                        title_refs: vec![TitleRef {
                            title_set_nr: ts.number,
                            title_nr: title.title_ordinal,
                            kind: TitleRefKind::AottTitleOrdinal,
                        }],
                        samg_tracks: Vec::new(),
                        correlation: GroupCorrelation::OrphanPgcTitle,
                    });
                    next_group_nr = next_group_nr.saturating_add(1);
                }
            }
        }
    }

    if let Some(samg) = samg {
        for track in &samg.tracks {
            let group = groups.entry(track.group_nr).or_insert_with(|| DvdaGroup {
                group_nr: track.group_nr,
                title_refs: Vec::new(),
                samg_tracks: Vec::new(),
                correlation: GroupCorrelation::SamgOnly,
            });
            group.samg_tracks.push(SamgTrackRef {
                samg_ordinal: track.ordinal,
                group_nr: track.group_nr,
                track_nr: track.track_nr,
            });
            if !group.title_refs.is_empty() {
                group.correlation = GroupCorrelation::MixedAmgAndSamg;
            }
        }
    }

    groups.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::track_type_low_bits_candidate;

    #[test]
    fn track_type_low_bits_are_exposed_only_as_candidate_bits() {
        assert_eq!(track_type_low_bits_candidate(0x00), 0);
        assert_eq!(track_type_low_bits_candidate(0xa5), 5);
    }
}
