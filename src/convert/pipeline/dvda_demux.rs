#![forbid(unsafe_code)]

//! DVD-Audio AOB MPEG Program Stream demuxing.
//!
//! DVD-Audio AOB sectors are 2048-byte MPEG-2 Program Stream packs. This
//! module strips MPEG-PS/PES framing and exposes DVD Private Stream 1 packets
//! with the DVD-Audio sub-header parsed. Callers can then route MLP payloads to
//! the MLP elementary-stream path or unpack LPCM payloads in-process.

use std::fmt;
use std::io::{self, Write};

pub const DVD_SECTOR_SIZE: usize = 2048;
pub const PACK_START_CODE: [u8; 4] = [0x00, 0x00, 0x01, 0xBA];
pub const PES_START_PREFIX: [u8; 3] = [0x00, 0x00, 0x01];
pub const PRIVATE_STREAM_1: u8 = 0xBD;
pub const PCM_STREAM_ID: u8 = 0xA0;
pub const MLP_STREAM_ID: u8 = 0xA1;
/// Canonical DVD-Audio MLP Private Stream 1 extra-header length observed in real AOB sectors.
/// The CCI byte remains at offset 8; byte 9 is currently treated as reserved/padding.
pub const MLP_EXTRA_HEADER_LENGTH: u8 = 6;
pub const PCM_EXTRA_HEADER_LENGTH: u8 = 9;
const MLP_ACCESS_UNIT_LENGTH_MASK: usize = 0x0fff;
const MLP_ACCESS_UNIT_LENGTH_BYTES_PER_WORD: usize = 2;
const MLP_MIN_ACCESS_UNIT_BYTES: usize = 4;
const MLP_MAX_ACCESS_UNIT_BYTES: usize = MLP_ACCESS_UNIT_LENGTH_MASK * MLP_ACCESS_UNIT_LENGTH_BYTES_PER_WORD;
pub const MLP_FIRST_ACCESS_UNIT_POINTER_PAYLOAD_BIAS: usize = 5;
const MLP_RESYNC_LOOKAHEAD_UNITS: usize = 2;
const MLP_MAJOR_SYNC_FBA: [u8; 4] = [0xF8, 0x72, 0x6F, 0xBA];
const MLP_MAJOR_SYNC_FBB: [u8; 4] = [0xF8, 0x72, 0x6F, 0xBB];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DvdaSubstreamKind {
    Pcm,
    Mlp,
    Unknown(u8),
}

impl DvdaSubstreamKind {
    #[must_use]
    pub const fn from_stream_id(stream_id: u8) -> Self {
        match stream_id {
            PCM_STREAM_ID => Self::Pcm,
            MLP_STREAM_ID => Self::Mlp,
            other => Self::Unknown(other),
        }
    }
}

const DVD_VIDEO_LPCM_SUBSTREAM_FIRST: u8 = 0xA0;
const DVD_VIDEO_LPCM_SUBSTREAM_LAST: u8 = 0xA7;

#[must_use]
const fn is_dvd_video_lpcm_substream_id(stream_id: u8) -> bool {
    stream_id >= DVD_VIDEO_LPCM_SUBSTREAM_FIRST && stream_id <= DVD_VIDEO_LPCM_SUBSTREAM_LAST
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DvdaSubHeaderMode {
    /// DVD-Audio AOB Private Stream 1 layout: byte 3 is extra_header_length.
    DvdAudio,
    /// DVD-Video VOB LPCM layout: bytes 2..3 are the first access unit pointer
    /// and LPCM payload starts after the fixed 7-byte private header.
    DvdVideo,
}

impl Default for DvdaSubHeaderMode {
    fn default() -> Self {
        Self::DvdAudio
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DvdaPcmSubHeader {
    pub first_audio_frame: u16,
    pub group1_bits_code: u8,
    pub group2_bits_code: u8,
    pub group1_sample_rate_code: u8,
    pub group2_sample_rate_code: u8,
    pub group1_bits: Option<u32>,
    pub group2_bits: Option<u32>,
    pub group1_sample_rate: Option<u32>,
    pub group2_sample_rate: Option<u32>,
    pub channel_count: Option<u8>,
    pub channel_assignment: u8,
    pub cci: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DvdaSubHeader {
    pub stream_id: u8,
    pub cyclic: u8,
    pub extra_header_length: u8,
    pub total_header_length: usize,
    pub first_access_unit_pointer: Option<u16>,
    pub cci: Option<u8>,
    pub pcm: Option<DvdaPcmSubHeader>,
}

impl DvdaSubHeader {
    #[must_use]
    pub const fn kind(self) -> DvdaSubstreamKind {
        if self.pcm.is_some() {
            DvdaSubstreamKind::Pcm
        } else {
            DvdaSubstreamKind::from_stream_id(self.stream_id)
        }
    }

    #[must_use]
    pub fn mlp_first_access_unit_payload_offset(self) -> Option<usize> {
        if !matches!(self.kind(), DvdaSubstreamKind::Mlp) {
            return None;
        }
        usize::from(self.first_access_unit_pointer?)
            .checked_sub(MLP_FIRST_ACCESS_UNIT_POINTER_PAYLOAD_BIAS)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DvdaPs1Packet<'a> {
    pub sub_header: DvdaSubHeader,
    pub payload: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DvdaDemuxStats {
    pub sectors_seen: u64,
    pub private_stream_1_packets: u64,
    pub mlp_packets: u64,
    pub pcm_packets: u64,
    pub mlp_payload_bytes: u64,
    pub pcm_payload_bytes: u64,
    pub first_sub_header: Option<DvdaSubHeader>,
    pub last_sub_header: Option<DvdaSubHeader>,
    pub first_pcm_sub_header: Option<DvdaPcmSubHeader>,
    pub last_pcm_sub_header: Option<DvdaPcmSubHeader>,
    pub pcm_format_change_count: u64,
    pub cci_change_count: u64,
    pub cyclic_discontinuity_count: u64,
    pub extra_header_length_change_count: u64,
    pub nonstandard_mlp_extra_header_packets: u64,
    pub nonstandard_pcm_extra_header_packets: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MlpReassemblyMode {
    Strict,
    Tolerant {
        max_resync_bytes: usize,
        max_resync_events: usize,
    },
}

impl Default for MlpReassemblyMode {
    fn default() -> Self {
        Self::Strict
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MlpStartConfidence {
    MajorSync,
    ConsecutiveAccessUnits,
}

impl MlpStartConfidence {
    const fn label(self) -> &'static str {
        match self {
            Self::MajorSync => "MajorSync",
            Self::ConsecutiveAccessUnits => "ConsecutiveAccessUnits",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VerifiedMlpStart {
    offset: usize,
    declared_len: usize,
    confidence: MlpStartConfidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MlpAccessUnitReassemblyStats {
    pub packets_seen: u64,
    pub access_units: u64,
    pub input_payload_bytes: u64,
    pub framed_bytes: u64,
    pub padding_bytes: u64,
    pub leading_fragment_bytes: u64,
    pub carry_bytes_max: usize,
    pub trailing_fragment_bytes: u64,
    pub resync_events: u64,
    pub resync_bytes: u64,
    pub first_resync_offset: Option<u64>,
    pub first_resync_reason: Option<&'static str>,
}

pub struct MlpAccessUnitReassembler<W: Write> {
    writer: W,
    pending: Vec<u8>,
    mode: MlpReassemblyMode,
    stream_started: bool,
    absolute_input_bytes: u64,
    stats: MlpAccessUnitReassemblyStats,
}

impl<W: Write> MlpAccessUnitReassembler<W> {
    pub fn new(writer: W) -> Self {
        Self::new_with_mode(writer, MlpReassemblyMode::Strict)
    }

    pub fn new_with_mode(writer: W, mode: MlpReassemblyMode) -> Self {
        Self {
            writer,
            pending: Vec::new(),
            mode,
            stream_started: false,
            absolute_input_bytes: 0,
            stats: MlpAccessUnitReassemblyStats::default(),
        }
    }

    pub fn push_packet(
        &mut self,
        sub_header: DvdaSubHeader,
        payload: &[u8],
    ) -> Result<(), DvdaDemuxError> {
        self.stats.packets_seen = self.stats.packets_seen.saturating_add(1);
        self.stats.input_payload_bytes = self
            .stats
            .input_payload_bytes
            .saturating_add(payload.len() as u64);
        let packet_input_offset = self.absolute_input_bytes;
        self.absolute_input_bytes = self
            .absolute_input_bytes
            .saturating_add(payload.len() as u64);

        let payload = self.trim_initial_packet_fragment(sub_header, payload, packet_input_offset)?;
        self.push_payload_after_accounting(payload)
    }

    #[allow(dead_code)]
    pub fn push_payload(&mut self, payload: &[u8]) -> Result<(), DvdaDemuxError> {
        self.stats.input_payload_bytes = self
            .stats
            .input_payload_bytes
            .saturating_add(payload.len() as u64);
        self.absolute_input_bytes = self
            .absolute_input_bytes
            .saturating_add(payload.len() as u64);
        if !payload.is_empty() {
            self.stream_started = true;
        }
        self.push_payload_after_accounting(payload)
    }

    pub fn finish(&mut self) -> Result<(), DvdaDemuxError> {
        self.drain_complete_access_units()?;
        if self.pending.is_empty() {
            self.writer.flush().map_err(DvdaDemuxError::Write)?;
            return Ok(());
        }

        if self.pending.iter().all(|byte| *byte == 0) {
            self.stats.padding_bytes = self
                .stats
                .padding_bytes
                .saturating_add(self.pending.len() as u64);
            self.pending.clear();
            self.writer.flush().map_err(DvdaDemuxError::Write)?;
            return Ok(());
        }

        // Track-end trailing fragments are expected for DVD-Audio tracks cut from
        // a continuous MLP bitstream.  The track window ends mid-access-unit; the
        // remaining bytes are a partial AU tail, not corruption.
        log::debug!(
            "MLP reassembly dropping {} trailing fragment byte(s) at end of track",
            self.pending.len()
        );
        self.stats.trailing_fragment_bytes = self.pending.len() as u64;
        self.pending.clear();
        self.writer.flush().map_err(DvdaDemuxError::Write)?;
        Ok(())
    }

    pub const fn stats(&self) -> MlpAccessUnitReassemblyStats {
        self.stats
    }

    pub fn into_inner(self) -> W {
        self.writer
    }

    fn trim_initial_packet_fragment<'a>(
        &mut self,
        sub_header: DvdaSubHeader,
        payload: &'a [u8],
        packet_input_offset: u64,
    ) -> Result<&'a [u8], DvdaDemuxError> {
        if self.stream_started || payload.is_empty() {
            return Ok(payload);
        }

        if let Some(first_access_unit_offset) = sub_header.mlp_first_access_unit_payload_offset() {
            if first_access_unit_offset > payload.len() {
                return Err(DvdaDemuxError::PacketHandler(format!(
                    "MLP first access-unit pointer {} maps to payload offset {}, beyond {} payload byte(s)",
                    sub_header.first_access_unit_pointer.unwrap_or_default(),
                    first_access_unit_offset,
                    payload.len()
                )));
            }

            let skipped = &payload[..first_access_unit_offset];
            if skipped.iter().all(|byte| *byte == 0) {
                self.stats.padding_bytes = self
                    .stats
                    .padding_bytes
                    .saturating_add(skipped.len() as u64);
            } else {
                self.stats.leading_fragment_bytes = self
                    .stats
                    .leading_fragment_bytes
                    .saturating_add(skipped.len() as u64);
            }
            self.stream_started = true;
            return Ok(&payload[first_access_unit_offset..]);
        }

        if payload.iter().all(|byte| *byte == 0) {
            self.stats.padding_bytes = self
                .stats
                .padding_bytes
                .saturating_add(payload.len() as u64);
            return Ok(&payload[payload.len()..]);
        }

        if verified_mlp_start_at(payload, 0).is_some() {
            self.stream_started = true;
            return Ok(payload);
        }

        Err(DvdaDemuxError::PacketHandler(format!(
            "first MLP packet at payload byte {} has no usable first access-unit pointer and does not start at a verified access unit",
            packet_input_offset
        )))
    }

    fn push_payload_after_accounting(&mut self, payload: &[u8]) -> Result<(), DvdaDemuxError> {
        self.pending.extend_from_slice(payload);
        self.note_carry_bytes();
        self.drain_complete_access_units()
    }

    fn drain_complete_access_units(&mut self) -> Result<(), DvdaDemuxError> {
        loop {
            self.note_carry_bytes();
            if self.pending.is_empty() {
                return Ok(());
            }

            if self.pending.len() < 2 {
                return Ok(());
            }

            if self.pending.iter().all(|byte| *byte == 0) {
                self.stats.padding_bytes = self
                    .stats
                    .padding_bytes
                    .saturating_add(self.pending.len() as u64);
                self.pending.clear();
                continue;
            }

            let Some(access_unit_bytes) = mlp_declared_access_unit_bytes(&self.pending[..2]) else {
                self.resync_or_fail("invalid or zero MLP access-unit length")?;
                continue;
            };

            if self.pending.len() < access_unit_bytes {
                return Ok(());
            }

            let after = &self.pending[access_unit_bytes..];
            if after.len() >= 2
                && !after.iter().all(|byte| *byte == 0)
                && mlp_declared_access_unit_bytes(&after[..2]).is_none()
            {
                self.resync_or_fail("MLP access-unit length did not land on the next boundary")?;
                continue;
            }

            self.writer
                .write_all(&self.pending[..access_unit_bytes])
                .map_err(DvdaDemuxError::Write)?;
            self.pending.drain(..access_unit_bytes);
            self.stats.access_units = self.stats.access_units.saturating_add(1);
            self.stats.framed_bytes = self
                .stats
                .framed_bytes
                .saturating_add(access_unit_bytes as u64);
        }
    }

    fn resync_or_fail(&mut self, reason: &'static str) -> Result<(), DvdaDemuxError> {
        let Some(start) = find_next_verified_mlp_access_unit_start(&self.pending[1..]) else {
            return Err(DvdaDemuxError::PacketHandler(format!(
                "MLP reassembly could not recover after {reason}; no verified access-unit start found in {} pending byte(s)",
                self.pending.len()
            )));
        };

        let discard = start.offset + 1;
        let skipped = &self.pending[..discard];
        if skipped.iter().all(|byte| *byte == 0) {
            self.stats.padding_bytes = self.stats.padding_bytes.saturating_add(discard as u64);
            self.pending.drain(..discard);
            return Ok(());
        }

        match self.mode {
            MlpReassemblyMode::Strict => Err(DvdaDemuxError::MlpStrictResyncRejected {
                input_byte: self
                    .absolute_input_bytes
                    .saturating_sub(self.pending.len() as u64),
                skipped_bytes: discard,
                confidence: start.confidence.label(),
                declared_len: start.declared_len,
                reason,
            }),
            MlpReassemblyMode::Tolerant {
                max_resync_bytes,
                max_resync_events,
            } => {
                let next_resync_bytes = self.stats.resync_bytes.saturating_add(discard as u64);
                let next_resync_events = self.stats.resync_events.saturating_add(1);
                if next_resync_bytes > max_resync_bytes as u64
                    || next_resync_events > max_resync_events as u64
                {
                    return Err(DvdaDemuxError::PacketHandler(format!(
                        "MLP reassembly resync limit exceeded: events={} bytes={} limits events={} bytes={} ({reason})",
                        next_resync_events,
                        next_resync_bytes,
                        max_resync_events,
                        max_resync_bytes
                    )));
                }

                if self.stats.first_resync_offset.is_none() {
                    self.stats.first_resync_offset = Some(
                        self.absolute_input_bytes
                            .saturating_sub(self.pending.len() as u64),
                    );
                    self.stats.first_resync_reason = Some(reason);
                }
                self.stats.resync_bytes = next_resync_bytes;
                self.stats.resync_events = next_resync_events;
                self.pending.drain(..discard);
                Ok(())
            }
        }
    }

    fn note_carry_bytes(&mut self) {
        self.stats.carry_bytes_max = self.stats.carry_bytes_max.max(self.pending.len());
    }
}

fn mlp_declared_access_unit_bytes(prefix: &[u8]) -> Option<usize> {
    if prefix.len() < 2 {
        return None;
    }
    let words = (u16::from_be_bytes([prefix[0], prefix[1]]) as usize) & MLP_ACCESS_UNIT_LENGTH_MASK;
    let bytes = words.checked_mul(MLP_ACCESS_UNIT_LENGTH_BYTES_PER_WORD)?;
    if (MLP_MIN_ACCESS_UNIT_BYTES..=MLP_MAX_ACCESS_UNIT_BYTES).contains(&bytes) {
        Some(bytes)
    } else {
        None
    }
}

fn find_next_verified_mlp_access_unit_start(bytes: &[u8]) -> Option<VerifiedMlpStart> {
    bytes
        .windows(2)
        .enumerate()
        .find_map(|(offset, _)| verified_mlp_start_at(bytes, offset))
}

fn verified_mlp_start_at(bytes: &[u8], offset: usize) -> Option<VerifiedMlpStart> {
    if offset + 2 > bytes.len() {
        return None;
    }
    let declared_len = mlp_declared_access_unit_bytes(&bytes[offset..offset + 2])?;
    if offset + declared_len > bytes.len() {
        return None;
    }

    let candidate = &bytes[offset..offset + declared_len];
    let after = &bytes[offset + declared_len..];
    if mlp_access_unit_has_major_sync(candidate)
        && (after.is_empty()
            || after.iter().all(|byte| *byte == 0)
            || verified_following_access_unit(after))
    {
        return Some(VerifiedMlpStart {
            offset,
            declared_len,
            confidence: MlpStartConfidence::MajorSync,
        });
    }

    if count_consecutive_mlp_access_units(&bytes[offset..], MLP_RESYNC_LOOKAHEAD_UNITS)
        >= MLP_RESYNC_LOOKAHEAD_UNITS
    {
        return Some(VerifiedMlpStart {
            offset,
            declared_len,
            confidence: MlpStartConfidence::ConsecutiveAccessUnits,
        });
    }

    None
}

fn verified_following_access_unit(bytes: &[u8]) -> bool {
    let Some(len) = mlp_declared_access_unit_bytes(bytes) else {
        return false;
    };
    len <= bytes.len()
}

fn count_consecutive_mlp_access_units(mut bytes: &[u8], max_units: usize) -> usize {
    let mut units = 0;
    while units < max_units {
        let Some(len) = mlp_declared_access_unit_bytes(bytes) else {
            break;
        };
        if len > bytes.len() {
            break;
        }
        units += 1;
        bytes = &bytes[len..];
    }
    units
}

fn mlp_access_unit_has_major_sync(access_unit: &[u8]) -> bool {
    access_unit.len() >= 8
        && (access_unit[4..8] == MLP_MAJOR_SYNC_FBA || access_unit[4..8] == MLP_MAJOR_SYNC_FBB)
}

#[derive(Debug)]
pub enum DvdaDemuxError {
    SectorSize {
        actual: usize,
    },
    MissingPackHeader,
    PackHeaderTruncated {
        stuffing: usize,
    },
    PesPacketTruncated {
        offset: usize,
        length: usize,
    },
    PrivateStreamHeaderTruncated {
        offset: usize,
        pes_end: usize,
    },
    DvdaSubHeaderMissing {
        offset: usize,
        available: usize,
    },
    DvdaSubHeaderTruncated {
        offset: usize,
        header_length: usize,
        available: usize,
    },
    MlpSubHeaderTooShort {
        offset: usize,
        extra_header_length: u8,
    },
    PcmSubHeaderTooShort {
        offset: usize,
        extra_header_length: u8,
    },
    UnexpectedSubstream {
        stream_id: u8,
    },
    PacketHandler(String),
    MlpStrictResyncRejected {
        input_byte: u64,
        skipped_bytes: usize,
        confidence: &'static str,
        declared_len: usize,
        reason: &'static str,
    },
    Write(io::Error),
}

impl DvdaDemuxError {
    pub const fn is_mlp_strict_resync_rejection(&self) -> bool {
        matches!(self, Self::MlpStrictResyncRejected { .. })
    }
}

impl fmt::Display for DvdaDemuxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SectorSize { actual } => write!(
                f,
                "DVD-Audio sector must be {DVD_SECTOR_SIZE} bytes, got {actual} bytes"
            ),
            Self::MissingPackHeader => write!(f, "DVD-Audio sector does not start with MPEG-PS pack header"),
            Self::PackHeaderTruncated { stuffing } => write!(
                f,
                "MPEG-PS pack header stuffing length {stuffing} exceeds the sector boundary"
            ),
            Self::PesPacketTruncated { offset, length } => write!(
                f,
                "PES packet at byte {offset} declares {length} payload bytes beyond the sector boundary"
            ),
            Self::PrivateStreamHeaderTruncated { offset, pes_end } => write!(
                f,
                "Private Stream 1 PES header at byte {offset} extends beyond PES end byte {pes_end}"
            ),
            Self::DvdaSubHeaderMissing { offset, available } => write!(
                f,
                "DVD-Audio sub-header missing at byte {offset}; only {available} bytes remain in PES payload"
            ),
            Self::DvdaSubHeaderTruncated {
                offset,
                header_length,
                available,
            } => write!(
                f,
                "DVD-Audio sub-header at byte {offset} declares {header_length} bytes, but only {available} bytes remain"
            ),
            Self::MlpSubHeaderTooShort {
                offset,
                extra_header_length,
            } => write!(
                f,
                "MLP sub-header at byte {offset} declares extra_header_length {extra_header_length}, expected at least {MLP_EXTRA_HEADER_LENGTH}"
            ),
            Self::PcmSubHeaderTooShort {
                offset,
                extra_header_length,
            } => write!(
                f,
                "LPCM sub-header at byte {offset} declares extra_header_length {extra_header_length}, expected at least {PCM_EXTRA_HEADER_LENGTH}"
            ),
            Self::UnexpectedSubstream { stream_id } => write!(
                f,
                "unexpected DVD-Audio Private Stream 1 substream id 0x{stream_id:02X}; expected MLP 0x{MLP_STREAM_ID:02X} or LPCM 0x{PCM_STREAM_ID:02X}"
            ),
            Self::PacketHandler(err) => write!(f, "DVD-Audio Private Stream 1 packet handler failed: {err}"),
            Self::MlpStrictResyncRejected {
                input_byte,
                skipped_bytes,
                confidence,
                declared_len,
                reason,
            } => write!(
                f,
                "MLP reassembly strict mode rejected nonzero resync at input byte {input_byte}: skipped {skipped_bytes} byte(s) before {confidence} candidate with declared length {declared_len} ({reason})"
            ),
            Self::Write(err) => write!(f, "failed to write demuxed DVD-Audio payload: {err}"),
        }
    }
}

impl std::error::Error for DvdaDemuxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Write(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for DvdaDemuxError {
    fn from(err: io::Error) -> Self {
        Self::Write(err)
    }
}

#[allow(dead_code)]
pub fn extract_mlp_from_sector<W: Write>(
    sector: &[u8],
    out: &mut W,
    stats: &mut DvdaDemuxStats,
) -> Result<(), DvdaDemuxError> {
    let mut pending = Vec::new();
    demux_private_stream_1_packets(sector, stats, |packet| match packet.sub_header.kind() {
        DvdaSubstreamKind::Mlp => {
            pending.extend_from_slice(packet.payload);
            Ok(())
        }
        DvdaSubstreamKind::Pcm => Err(DvdaDemuxError::UnexpectedSubstream {
            stream_id: PCM_STREAM_ID,
        }),
        DvdaSubstreamKind::Unknown(_) => Ok(()),
    })?;
    out.write_all(&pending).map_err(DvdaDemuxError::Write)
}

#[allow(dead_code)]
pub fn demux_private_stream_1_packets<F>(
    sector: &[u8],
    stats: &mut DvdaDemuxStats,
    mut on_packet: F,
) -> Result<(), DvdaDemuxError>
where
    F: FnMut(DvdaPs1Packet<'_>) -> Result<(), DvdaDemuxError>,
{
    let packets = parse_private_stream_1_packets(sector)?;

    stats.sectors_seen = stats.sectors_seen.saturating_add(1);
    for packet in packets {
        stats.private_stream_1_packets = stats.private_stream_1_packets.saturating_add(1);
        record_sub_header(stats, packet.sub_header, packet.payload.len());
        on_packet(packet)?;
    }

    Ok(())
}

pub fn record_private_stream_1_packets(stats: &mut DvdaDemuxStats, packets: &[DvdaPs1Packet<'_>]) {
    stats.sectors_seen = stats.sectors_seen.saturating_add(1);
    for packet in packets {
        stats.private_stream_1_packets = stats.private_stream_1_packets.saturating_add(1);
        record_sub_header(stats, packet.sub_header, packet.payload.len());
    }
}

pub fn parse_private_stream_1_packets(
    sector: &[u8],
) -> Result<Vec<DvdaPs1Packet<'_>>, DvdaDemuxError> {
    parse_private_stream_1_packets_with_mode(sector, DvdaSubHeaderMode::DvdAudio)
}

pub fn parse_private_stream_1_packets_with_mode(
    sector: &[u8],
    mode: DvdaSubHeaderMode,
) -> Result<Vec<DvdaPs1Packet<'_>>, DvdaDemuxError> {
    if sector.len() != DVD_SECTOR_SIZE {
        return Err(DvdaDemuxError::SectorSize {
            actual: sector.len(),
        });
    }
    if sector[..PACK_START_CODE.len()] != PACK_START_CODE {
        return Err(DvdaDemuxError::MissingPackHeader);
    }

    let stuffing = usize::from(sector[13] & 0x07);
    let mut offset = 14usize
        .checked_add(stuffing)
        .ok_or(DvdaDemuxError::PackHeaderTruncated { stuffing })?;
    if offset > sector.len() {
        return Err(DvdaDemuxError::PackHeaderTruncated { stuffing });
    }

    let mut packets = Vec::new();
    while offset + 6 <= sector.len() {
        if sector[offset..offset + PES_START_PREFIX.len()] != PES_START_PREFIX {
            break;
        }

        let stream_id = sector[offset + 3];
        let pes_length = u16::from_be_bytes([sector[offset + 4], sector[offset + 5]]) as usize;
        let pes_end = offset
            .checked_add(6)
            .and_then(|v| v.checked_add(pes_length))
            .ok_or(DvdaDemuxError::PesPacketTruncated {
                offset,
                length: pes_length,
            })?;
        if pes_end > sector.len() {
            return Err(DvdaDemuxError::PesPacketTruncated {
                offset,
                length: pes_length,
            });
        }

        if stream_id == PRIVATE_STREAM_1 {
            packets.push(parse_private_stream_1_packet(
                sector, offset, pes_end, mode,
            )?);
        }

        offset = pes_end;
    }

    Ok(packets)
}

fn parse_private_stream_1_packet(
    sector: &[u8],
    pes_offset: usize,
    pes_end: usize,
    mode: DvdaSubHeaderMode,
) -> Result<DvdaPs1Packet<'_>, DvdaDemuxError> {
    if pes_offset + 9 > pes_end {
        return Err(DvdaDemuxError::PrivateStreamHeaderTruncated {
            offset: pes_offset,
            pes_end,
        });
    }

    let pes_header_data_length = usize::from(sector[pes_offset + 8]);
    let sub_header_offset = pes_offset
        .checked_add(9)
        .and_then(|v| v.checked_add(pes_header_data_length))
        .ok_or(DvdaDemuxError::PrivateStreamHeaderTruncated {
            offset: pes_offset,
            pes_end,
        })?;
    if sub_header_offset > pes_end {
        return Err(DvdaDemuxError::PrivateStreamHeaderTruncated {
            offset: pes_offset,
            pes_end,
        });
    }

    let available = pes_end - sub_header_offset;
    if available < 4 {
        return Err(DvdaDemuxError::DvdaSubHeaderMissing {
            offset: sub_header_offset,
            available,
        });
    }

    let sub_header =
        parse_sub_header(&sector[sub_header_offset..pes_end], sub_header_offset, mode)?;
    let body_offset = sub_header_offset + sub_header.total_header_length;
    let payload = if body_offset < pes_end {
        &sector[body_offset..pes_end]
    } else {
        &[]
    };

    Ok(DvdaPs1Packet {
        sub_header,
        payload,
    })
}

fn record_sub_header(stats: &mut DvdaDemuxStats, sub_header: DvdaSubHeader, payload_len: usize) {
    if matches!(sub_header.kind(), DvdaSubstreamKind::Unknown(_)) {
        return;
    }

    if stats.first_sub_header.is_none() {
        stats.first_sub_header = Some(sub_header);
    }

    if let Some(previous) = stats.last_sub_header {
        if previous.cci != sub_header.cci {
            stats.cci_change_count = stats.cci_change_count.saturating_add(1);
        }
        if previous.extra_header_length != sub_header.extra_header_length {
            stats.extra_header_length_change_count =
                stats.extra_header_length_change_count.saturating_add(1);
        }
        let expected_cyclic = previous.cyclic.wrapping_add(1);
        if sub_header.cyclic != previous.cyclic && sub_header.cyclic != expected_cyclic {
            stats.cyclic_discontinuity_count = stats.cyclic_discontinuity_count.saturating_add(1);
        }
    }

    match sub_header.kind() {
        DvdaSubstreamKind::Mlp => {
            stats.mlp_packets = stats.mlp_packets.saturating_add(1);
            stats.mlp_payload_bytes = stats.mlp_payload_bytes.saturating_add(payload_len as u64);
            if sub_header.extra_header_length != MLP_EXTRA_HEADER_LENGTH {
                stats.nonstandard_mlp_extra_header_packets =
                    stats.nonstandard_mlp_extra_header_packets.saturating_add(1);
            }
        }
        DvdaSubstreamKind::Pcm => {
            stats.pcm_packets = stats.pcm_packets.saturating_add(1);
            stats.pcm_payload_bytes = stats.pcm_payload_bytes.saturating_add(payload_len as u64);
            if sub_header.extra_header_length != PCM_EXTRA_HEADER_LENGTH {
                stats.nonstandard_pcm_extra_header_packets =
                    stats.nonstandard_pcm_extra_header_packets.saturating_add(1);
            }
            if let Some(pcm) = sub_header.pcm {
                if stats.first_pcm_sub_header.is_none() {
                    stats.first_pcm_sub_header = Some(pcm);
                }
                if let Some(previous) = stats.last_pcm_sub_header {
                    if pcm_format_without_pointer(previous) != pcm_format_without_pointer(pcm) {
                        stats.pcm_format_change_count =
                            stats.pcm_format_change_count.saturating_add(1);
                    }
                }
                stats.last_pcm_sub_header = Some(pcm);
            }
        }
        DvdaSubstreamKind::Unknown(_) => {}
    }

    stats.last_sub_header = Some(sub_header);
}

fn pcm_format_without_pointer(
    pcm: DvdaPcmSubHeader,
) -> (
    u8,
    u8,
    u8,
    u8,
    Option<u32>,
    Option<u32>,
    Option<u32>,
    Option<u32>,
    Option<u8>,
    u8,
) {
    (
        pcm.group1_bits_code,
        pcm.group2_bits_code,
        pcm.group1_sample_rate_code,
        pcm.group2_sample_rate_code,
        pcm.group1_bits,
        pcm.group2_bits,
        pcm.group1_sample_rate,
        pcm.group2_sample_rate,
        pcm.channel_count,
        pcm.channel_assignment,
    )
}

fn parse_sub_header(
    bytes: &[u8],
    offset: usize,
    mode: DvdaSubHeaderMode,
) -> Result<DvdaSubHeader, DvdaDemuxError> {
    match mode {
        DvdaSubHeaderMode::DvdAudio => parse_dvd_audio_sub_header(bytes, offset),
        DvdaSubHeaderMode::DvdVideo => parse_dvd_video_sub_header(bytes, offset),
    }
}

fn parse_dvd_audio_sub_header(
    bytes: &[u8],
    offset: usize,
) -> Result<DvdaSubHeader, DvdaDemuxError> {
    if bytes.len() < 4 {
        return Err(DvdaDemuxError::DvdaSubHeaderMissing {
            offset,
            available: bytes.len(),
        });
    }

    let stream_id = bytes[0];
    let cyclic = bytes[1];
    let extra_header_length = bytes[3];
    let total_header_length = 4usize + usize::from(extra_header_length);

    if total_header_length > bytes.len() {
        return Err(DvdaDemuxError::DvdaSubHeaderTruncated {
            offset,
            header_length: total_header_length,
            available: bytes.len(),
        });
    }

    let (first_access_unit_pointer, cci, pcm) = match DvdaSubstreamKind::from_stream_id(stream_id) {
        DvdaSubstreamKind::Mlp => (
            (extra_header_length >= 2).then(|| u16::from_be_bytes([bytes[4], bytes[5]])),
            bytes.get(8).copied(),
            None,
        ),
        DvdaSubstreamKind::Pcm => {
            if extra_header_length >= PCM_EXTRA_HEADER_LENGTH {
                let pcm = parse_dvd_audio_pcm_sub_header(bytes);
                (None, Some(pcm.cci), Some(pcm))
            } else {
                (None, None, None)
            }
        }
        DvdaSubstreamKind::Unknown(_) => (None, None, None),
    };

    Ok(DvdaSubHeader {
        stream_id,
        cyclic,
        extra_header_length,
        total_header_length,
        first_access_unit_pointer,
        cci,
        pcm,
    })
}

fn parse_dvd_video_sub_header(
    bytes: &[u8],
    offset: usize,
) -> Result<DvdaSubHeader, DvdaDemuxError> {
    const DVD_VIDEO_LPCM_TOTAL_HEADER_LENGTH: usize = 7;
    const DVD_VIDEO_LPCM_EXTRA_HEADER_LENGTH: u8 = 3;

    if bytes.len() < DVD_VIDEO_LPCM_TOTAL_HEADER_LENGTH {
        return Err(DvdaDemuxError::DvdaSubHeaderMissing {
            offset,
            available: bytes.len(),
        });
    }

    let stream_id = bytes[0];
    let cyclic = bytes[1];
    let (cci, pcm) = if is_dvd_video_lpcm_substream_id(stream_id) {
        if let Some(pcm) = parse_dvd_video_pcm_sub_header(bytes) {
            (Some(pcm.cci), Some(pcm))
        } else {
            (None, None)
        }
    } else {
        // DVD-Video mode is only selected for VOB LPCM evidence. Unknown
        // sub-stream IDs remain parseable for diagnostics/filtering, but we do
        // not reinterpret bytes[3] as a DVD-Audio extra-header length here.
        (None, None)
    };

    Ok(DvdaSubHeader {
        stream_id,
        cyclic,
        extra_header_length: DVD_VIDEO_LPCM_EXTRA_HEADER_LENGTH,
        total_header_length: DVD_VIDEO_LPCM_TOTAL_HEADER_LENGTH,
        first_access_unit_pointer: None,
        cci,
        pcm,
    })
}

fn parse_dvd_audio_pcm_sub_header(bytes: &[u8]) -> DvdaPcmSubHeader {
    let first_audio_frame = u16::from_be_bytes([bytes[4], bytes[5]]);
    let bits_byte = bytes[7];
    let rate_byte = bytes[8];
    let group1_bits_code = bits_byte & 0x0f;
    let group2_bits_code = bits_byte >> 4;
    let group1_sample_rate_code = rate_byte & 0x0f;
    let group2_sample_rate_code = rate_byte >> 4;

    DvdaPcmSubHeader {
        first_audio_frame,
        group1_bits_code,
        group2_bits_code,
        group1_sample_rate_code,
        group2_sample_rate_code,
        group1_bits: decode_pcm_bits_code(group1_bits_code),
        group2_bits: decode_pcm_bits_code(group2_bits_code),
        group1_sample_rate: decode_pcm_sample_rate_code(group1_sample_rate_code),
        group2_sample_rate: decode_pcm_sample_rate_code(group2_sample_rate_code),
        channel_count: None,
        channel_assignment: bytes[10],
        cci: bytes[12],
    }
}

fn parse_dvd_video_pcm_sub_header(bytes: &[u8]) -> Option<DvdaPcmSubHeader> {
    if bytes.len() < 7 {
        return None;
    }

    let first_access_unit_pointer = u16::from_be_bytes([bytes[2], bytes[3]]);
    let format = bytes[5];
    let bits_code = format >> 6;
    let sample_rate_code = (format >> 4) & 0x03;
    let channel_count = (format & 0x07).saturating_add(1);
    let group1_bits = decode_pcm_bits_code(bits_code)?;
    let group1_sample_rate = decode_dvd_video_lpcm_sample_rate_code(sample_rate_code)?;

    Some(DvdaPcmSubHeader {
        first_audio_frame: first_access_unit_pointer,
        group1_bits_code: bits_code,
        group2_bits_code: 0,
        group1_sample_rate_code: sample_rate_code,
        group2_sample_rate_code: 0,
        group1_bits: Some(group1_bits),
        group2_bits: None,
        group1_sample_rate: Some(group1_sample_rate),
        group2_sample_rate: None,
        channel_count: Some(channel_count),
        // DVD-Video LPCM encodes channel count directly in the packet format
        // byte. Do not reject 7/8-channel packets just because there is no
        // DVD-Audio channel-assignment value for them.
        channel_assignment: dvd_video_lpcm_channel_assignment_for_count(channel_count),
        cci: bytes[6],
    })
}

fn dvd_video_lpcm_channel_assignment_for_count(channel_count: u8) -> u8 {
    match channel_count {
        1 => 0,
        2 => 1,
        3 => 7,
        4 => 3,
        5 => 10,
        6 => 12,
        // Neutral compatibility value. DVD-Video probing and realization use
        // channel_count, not this DVD-Audio-oriented assignment field.
        7 | 8 => 0,
        _ => 0,
    }
}

#[must_use]
pub const fn decode_pcm_bits_code(code: u8) -> Option<u32> {
    match code {
        0 => Some(16),
        1 => Some(20),
        2 => Some(24),
        _ => None,
    }
}

#[must_use]
pub const fn decode_dvd_video_lpcm_sample_rate_code(code: u8) -> Option<u32> {
    match code {
        0x0 => Some(48_000),
        0x1 => Some(96_000),
        _ => None,
    }
}

#[must_use]
pub const fn decode_pcm_sample_rate_code(code: u8) -> Option<u32> {
    match code {
        0x0 => Some(48_000),
        0x1 => Some(96_000),
        0x2 => Some(192_000),
        0x8 => Some(44_100),
        0x9 => Some(88_200),
        0xA => Some(176_400),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sector_with_private_stream(
        substream_id: u8,
        payload: &[u8],
        stuffing: u8,
    ) -> [u8; DVD_SECTOR_SIZE] {
        let mut sector = [0_u8; DVD_SECTOR_SIZE];
        sector[..4].copy_from_slice(&PACK_START_CODE);
        sector[13] = stuffing & 0x07;

        let pes_offset = 14 + usize::from(stuffing & 0x07);
        sector[pes_offset..pes_offset + 4].copy_from_slice(&[0x00, 0x00, 0x01, PRIVATE_STREAM_1]);
        sector[pes_offset + 6] = 0x80;
        sector[pes_offset + 7] = 0x80;
        sector[pes_offset + 8] = 0;

        let sub_header = match substream_id {
            MLP_STREAM_ID => mlp_sub_header_with(0, MLP_EXTRA_HEADER_LENGTH, 0),
            PCM_STREAM_ID => vec![
                PCM_STREAM_ID,
                0,
                0,
                PCM_EXTRA_HEADER_LENGTH,
                0,
                0,
                0,
                0x22,
                0x22,
                0,
                0,
                0,
                0,
            ],
            other => vec![other, 0, 0, 5, 0, 0, 0, 0, 0],
        };

        let pes_payload_len = 3 + sub_header.len() + payload.len();
        sector[pes_offset + 4..pes_offset + 6]
            .copy_from_slice(&(pes_payload_len as u16).to_be_bytes());

        let sub_offset = pes_offset + 9;
        sector[sub_offset..sub_offset + sub_header.len()].copy_from_slice(&sub_header);
        let payload_offset = sub_offset + sub_header.len();
        sector[payload_offset..payload_offset + payload.len()].copy_from_slice(payload);
        sector
    }

    fn new_pack_sector() -> [u8; DVD_SECTOR_SIZE] {
        let mut sector = [0_u8; DVD_SECTOR_SIZE];
        sector[..4].copy_from_slice(&PACK_START_CODE);
        sector
    }

    fn mlp_sub_header_with(cyclic: u8, extra_header_length: u8, cci: u8) -> Vec<u8> {
        let mut sub_header = vec![MLP_STREAM_ID, cyclic, 0, extra_header_length];
        sub_header.resize(4 + usize::from(extra_header_length), 0);
        if sub_header.len() > 5 {
            let pointer = (MLP_FIRST_ACCESS_UNIT_POINTER_PAYLOAD_BIAS as u16).to_be_bytes();
            sub_header[4] = pointer[0];
            sub_header[5] = pointer[1];
        }
        if sub_header.len() > 8 {
            sub_header[8] = cci;
        }
        sub_header
    }

    fn pcm_sub_header_with(extra_header_length: u8) -> Vec<u8> {
        let mut sub_header = vec![PCM_STREAM_ID, 0, 0, extra_header_length];
        sub_header.resize(4 + usize::from(extra_header_length), 0);
        sub_header[7] = 0x22;
        sub_header[8] = 0x22;
        sub_header[10] = 0;
        sub_header[12] = 0;
        sub_header
    }

    fn dvd_video_pcm_sub_header_with(first_access_unit_pointer: u16, format_byte: u8) -> Vec<u8> {
        let [fau_hi, fau_lo] = first_access_unit_pointer.to_be_bytes();
        vec![PCM_STREAM_ID, 0x04, fau_hi, fau_lo, 0x03, format_byte, 0x7F]
    }

    fn write_private_stream_packet(
        sector: &mut [u8; DVD_SECTOR_SIZE],
        offset: usize,
        sub_header: &[u8],
        payload: &[u8],
    ) -> usize {
        let mut body = Vec::with_capacity(sub_header.len() + payload.len());
        body.extend_from_slice(sub_header);
        body.extend_from_slice(payload);
        write_pes_packet(sector, offset, PRIVATE_STREAM_1, &[], &body)
    }

    fn write_pes_packet(
        sector: &mut [u8; DVD_SECTOR_SIZE],
        offset: usize,
        stream_id: u8,
        pes_header_data: &[u8],
        body: &[u8],
    ) -> usize {
        assert!(pes_header_data.len() <= u8::MAX as usize);
        let pes_length = 3 + pes_header_data.len() + body.len();
        assert!(pes_length <= u16::MAX as usize);
        assert!(offset + 6 + pes_length <= DVD_SECTOR_SIZE);

        sector[offset..offset + 4].copy_from_slice(&[0x00, 0x00, 0x01, stream_id]);
        sector[offset + 4..offset + 6].copy_from_slice(&(pes_length as u16).to_be_bytes());
        sector[offset + 6] = 0x80;
        sector[offset + 7] = 0x80;
        sector[offset + 8] = pes_header_data.len() as u8;
        let header_offset = offset + 9;
        sector[header_offset..header_offset + pes_header_data.len()]
            .copy_from_slice(pes_header_data);
        let body_offset = header_offset + pes_header_data.len();
        sector[body_offset..body_offset + body.len()].copy_from_slice(body);
        offset + 6 + pes_length
    }

    fn write_stream_payload(
        sector: &mut [u8; DVD_SECTOR_SIZE],
        offset: usize,
        stream_id: u8,
        payload: &[u8],
    ) -> usize {
        assert!(payload.len() <= u16::MAX as usize);
        assert!(offset + 6 + payload.len() <= DVD_SECTOR_SIZE);
        sector[offset..offset + 4].copy_from_slice(&[0x00, 0x00, 0x01, stream_id]);
        sector[offset + 4..offset + 6].copy_from_slice(&(payload.len() as u16).to_be_bytes());
        sector[offset + 6..offset + 6 + payload.len()].copy_from_slice(payload);
        offset + 6 + payload.len()
    }

    struct AobFixture {
        name: &'static str,
        first_cyclic: u8,
        payload_bytes: u64,
    }

    const AOB_MLP_FIXTURES: &[AobFixture] = &[
        AobFixture {
            name: "ap_eye_in_the_sky_first_16_sectors.bin",
            first_cyclic: 0,
            payload_bytes: 32_059,
        },
        AobFixture {
            name: "ap_friendly_card_first_16_sectors.bin",
            first_cyclic: 0,
            payload_bytes: 32_059,
        },
        AobFixture {
            name: "ap_i_robot_first_16_sectors.bin",
            first_cyclic: 0,
            payload_bytes: 32_059,
        },
        AobFixture {
            name: "hdad2009_first_16_sectors.bin",
            first_cyclic: 0,
            payload_bytes: 32_059,
        },
        AobFixture {
            name: "hawks_and_doves_first_16_sectors.bin",
            first_cyclic: 32,
            payload_bytes: 32_059,
        },
        AobFixture {
            name: "mgletsgetiton_first_16_sectors.bin",
            first_cyclic: 32,
            payload_bytes: 32_059,
        },
        AobFixture {
            name: "talking_heads_77_first_16_sectors.bin",
            first_cyclic: 32,
            payload_bytes: 32_059,
        },
    ];

    #[test]
    fn demuxes_real_mlp_aob_sector_fixtures() {
        let fixture_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dvda_aob_samples");
        for fixture in AOB_MLP_FIXTURES {
            let path = fixture_root.join(fixture.name);
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
            assert_eq!(
                bytes.len(),
                16 * DVD_SECTOR_SIZE,
                "{} must contain exactly 16 sectors",
                fixture.name
            );

            let mut stats = DvdaDemuxStats::default();
            let mut payload = Vec::new();
            let mut cyclic_values = Vec::new();
            for sector in bytes.chunks_exact(DVD_SECTOR_SIZE) {
                demux_private_stream_1_packets(sector, &mut stats, |packet| {
                    assert_eq!(
                        packet.sub_header.kind(),
                        DvdaSubstreamKind::Mlp,
                        "{} should contain only MLP DVD-A substreams",
                        fixture.name
                    );
                    cyclic_values.push(packet.sub_header.cyclic);
                    payload.extend_from_slice(packet.payload);
                    Ok(())
                })
                .unwrap_or_else(|err| panic!("{} failed to demux: {err}", fixture.name));
            }

            assert_eq!(stats.sectors_seen, 16, "{} sector count", fixture.name);
            assert_eq!(
                stats.private_stream_1_packets, 16,
                "{} PS1 packet count",
                fixture.name
            );
            assert_eq!(stats.mlp_packets, 16, "{} MLP packet count", fixture.name);
            assert_eq!(stats.pcm_packets, 0, "{} LPCM packet count", fixture.name);
            assert_eq!(
                stats.mlp_payload_bytes, fixture.payload_bytes,
                "{} payload bytes",
                fixture.name
            );
            assert_eq!(
                payload.len() as u64,
                fixture.payload_bytes,
                "{} payload vector length",
                fixture.name
            );
            assert_eq!(
                stats.nonstandard_mlp_extra_header_packets, 0,
                "{} should use canonical real-disc MLP extra headers",
                fixture.name
            );
            assert_eq!(
                stats.extra_header_length_change_count, 0,
                "{} should have stable MLP extra headers",
                fixture.name
            );
            assert_eq!(
                stats.cyclic_discontinuity_count, 0,
                "{} should have contiguous cyclic counters",
                fixture.name
            );
            assert_eq!(
                stats
                    .first_sub_header
                    .expect("first header")
                    .extra_header_length,
                MLP_EXTRA_HEADER_LENGTH
            );
            assert_eq!(
                stats
                    .first_sub_header
                    .expect("first header")
                    .total_header_length,
                4 + usize::from(MLP_EXTRA_HEADER_LENGTH)
            );
            assert_eq!(
                cyclic_values.first().copied(),
                Some(fixture.first_cyclic),
                "{} first cyclic",
                fixture.name
            );
            for pair in cyclic_values.windows(2) {
                assert_eq!(
                    pair[1],
                    pair[0].wrapping_add(1),
                    "{} cyclic sequence",
                    fixture.name
                );
            }
        }
    }


    fn mlp_access_unit(byte_len: usize, marker: u8) -> Vec<u8> {
        assert!(byte_len >= MLP_MIN_ACCESS_UNIT_BYTES);
        assert_eq!(byte_len % 2, 0);
        let words = (byte_len / MLP_ACCESS_UNIT_LENGTH_BYTES_PER_WORD) as u16;
        let mut out = vec![0_u8; byte_len];
        let encoded = words & MLP_ACCESS_UNIT_LENGTH_MASK as u16;
        out[0] = ((encoded >> 8) & 0x0f) as u8;
        out[1] = (encoded & 0xff) as u8;
        out[2] = marker;
        out[3] = marker.wrapping_add(1);
        if byte_len >= 8 {
            out[4..8].copy_from_slice(&[0xF8, 0x72, 0x6F, 0xBA]);
        }
        out
    }

    #[test]
    fn mlp_reassembler_writes_access_units_and_drops_packet_padding() {
        let first = mlp_access_unit(8, 0x10);
        let second = mlp_access_unit(10, 0x20);
        let mut first_packet = Vec::new();
        first_packet.extend_from_slice(&first);
        first_packet.extend_from_slice(&[0, 0, 0, 0]);
        let mut second_packet = Vec::new();
        second_packet.extend_from_slice(&second);
        second_packet.extend_from_slice(&[0, 0]);

        let mut out = Vec::new();
        let mut reassembler = MlpAccessUnitReassembler::new(&mut out);
        reassembler
            .push_payload(&first_packet)
            .expect("first packet should reassemble");
        reassembler
            .push_payload(&second_packet)
            .expect("second packet should reassemble");
        reassembler.finish().expect("zero padding tail should finish");
        let stats = reassembler.stats();
        drop(reassembler);

        let mut expected = Vec::new();
        expected.extend_from_slice(&first);
        expected.extend_from_slice(&second);
        assert_eq!(out, expected);
        assert_eq!(stats.access_units, 2);
        assert_eq!(stats.framed_bytes, expected.len() as u64);
        assert_eq!(stats.padding_bytes, 6);
        assert_eq!(stats.resync_bytes, 0);
    }

    #[test]
    fn mlp_reassembler_carries_access_unit_across_packet_boundaries() {
        let frame = mlp_access_unit(12, 0x33);
        let mut out = Vec::new();
        let mut reassembler = MlpAccessUnitReassembler::new(&mut out);
        reassembler.push_payload(&frame[..5]).expect("prefix should buffer");
        assert_eq!(reassembler.stats().access_units, 0);
        reassembler.push_payload(&frame[5..]).expect("suffix should complete frame");
        reassembler.finish().expect("complete frame should finish");
        drop(reassembler);

        assert_eq!(out, frame);
    }

    #[test]
    fn mlp_reassembler_accepts_non_padding_tail_as_trailing_fragment() {
        let frame = mlp_access_unit(8, 0x44);
        let mut out = Vec::new();
        let mut reassembler = MlpAccessUnitReassembler::new(&mut out);
        reassembler.push_payload(&frame).expect("frame should reassemble");
        reassembler.push_payload(&[0x12, 0x34, 0x56]).expect("tail buffers until finish");
        reassembler
            .finish()
            .expect("trailing fragment at track end should succeed");
        assert_eq!(reassembler.stats().trailing_fragment_bytes, 3);
        assert_eq!(reassembler.stats().access_units, 1);
    }


    #[test]
    fn mlp_reassembler_strict_rejects_nonzero_resync() {
        let first = mlp_access_unit(8, 0x55);
        let second = mlp_access_unit(8, 0x66);
        let mut payload = vec![0, 0, 0x7E];
        payload.extend_from_slice(&first);
        payload.extend_from_slice(&second);

        let mut out = Vec::new();
        let mut reassembler = MlpAccessUnitReassembler::new(&mut out);
        let err = reassembler
            .push_payload(&payload)
            .expect_err("strict mode should reject nonzero resync");
        assert!(format!("{err}").contains("strict mode rejected nonzero resync"));
    }

    #[test]
    fn mlp_reassembler_tolerant_resync_is_bounded_and_reported() {
        let first = mlp_access_unit(8, 0x77);
        let second = mlp_access_unit(8, 0x88);
        let mut payload = vec![0, 0, 0x7E];
        payload.extend_from_slice(&first);
        payload.extend_from_slice(&second);

        let mut out = Vec::new();
        let mut reassembler = MlpAccessUnitReassembler::new_with_mode(
            &mut out,
            MlpReassemblyMode::Tolerant {
                max_resync_bytes: 3,
                max_resync_events: 1,
            },
        );
        reassembler
            .push_payload(&payload)
            .expect("bounded tolerant resync should recover");
        reassembler.finish().expect("recovered stream should finish");
        let stats = reassembler.stats();
        drop(reassembler);

        let mut expected = Vec::new();
        expected.extend_from_slice(&first);
        expected.extend_from_slice(&second);
        assert_eq!(out, expected);
        assert_eq!(stats.resync_events, 1);
        assert_eq!(stats.resync_bytes, 3);
        assert_eq!(stats.first_resync_reason, Some("invalid or zero MLP access-unit length"));
    }

    #[test]
    fn mlp_reassembler_rejects_false_positive_length_resync() {
        let payload = [0, 0, 0x7E, 0x00, 0x04, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];

        let mut strict_out = Vec::new();
        let mut strict = MlpAccessUnitReassembler::new(&mut strict_out);
        let strict_err = strict
            .push_payload(&payload)
            .expect_err("strict mode should reject unsupported candidate");
        assert!(format!("{strict_err}").contains("no verified access-unit start"));

        let mut tolerant_out = Vec::new();
        let mut tolerant = MlpAccessUnitReassembler::new_with_mode(
            &mut tolerant_out,
            MlpReassemblyMode::Tolerant {
                max_resync_bytes: 16,
                max_resync_events: 1,
            },
        );
        let tolerant_err = tolerant
            .push_payload(&payload)
            .expect_err("tolerant mode also needs supporting frame evidence");
        assert!(format!("{tolerant_err}").contains("no verified access-unit start"));
    }

    #[test]
    fn mlp_reassembler_counts_trailing_fragment_bytes() {
        let mut out = Vec::new();
        let mut reassembler = MlpAccessUnitReassembler::new(&mut out);
        reassembler
            .push_payload(&[0x01, 0x00, 0xAA, 0xBB, 0xCC])
            .expect("oversized declared length buffers until end of stream");
        reassembler
            .finish()
            .expect("trailing fragment at track end should succeed");
        assert_eq!(reassembler.stats().trailing_fragment_bytes, 5);
    }

    #[test]
    fn mlp_reassembler_is_independent_of_packet_split_points() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&mlp_access_unit(8, 0x91));
        stream.extend_from_slice(&mlp_access_unit(10, 0x92));
        stream.extend_from_slice(&mlp_access_unit(12, 0x93));

        for split in 0..=stream.len() {
            let mut out = Vec::new();
            let mut reassembler = MlpAccessUnitReassembler::new(&mut out);
            reassembler
                .push_payload(&stream[..split])
                .expect("prefix should reassemble or carry");
            reassembler
                .push_payload(&stream[split..])
                .expect("suffix should reassemble or carry");
            reassembler.finish().expect("complete stream should finish");
            drop(reassembler);
            assert_eq!(out, stream, "split at {split}");
        }
    }

    fn reassemble_aob_fixture_fragment(
        name: &str,
    ) -> (Vec<u8>, MlpAccessUnitReassemblyStats, DvdaDemuxStats) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dvda_mlp_fixture")
            .join(name);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        assert_eq!(bytes.len() % DVD_SECTOR_SIZE, 0, "fixture must contain whole sectors");

        let mut demux_stats = DvdaDemuxStats::default();
        let mut out = Vec::new();
        let mut reassembler = MlpAccessUnitReassembler::new(&mut out);
        for sector in bytes.chunks_exact(DVD_SECTOR_SIZE) {
            let packets = parse_private_stream_1_packets(sector)
                .unwrap_or_else(|err| panic!("{} failed to parse: {err}", path.display()));
            record_private_stream_1_packets(&mut demux_stats, &packets);
            for packet in packets {
                if matches!(packet.sub_header.kind(), DvdaSubstreamKind::Mlp) {
                    reassembler
                        .push_packet(packet.sub_header, packet.payload)
                        .unwrap_or_else(|err| panic!("{} failed to reassemble: {err}", path.display()));
                }
            }
        }
        let reassembly_stats = reassembler.stats();
        drop(reassembler);
        (out, reassembly_stats, demux_stats)
    }

    #[test]
    fn bowie_first_100_sector_fixture_reassembles_without_resync() {
        let (out, reassembly, demux) =
            reassemble_aob_fixture_fragment("aob_sectors_0_99.bin");

        assert_eq!(demux.sectors_seen, 100);
        assert_eq!(demux.mlp_packets, 100);
        assert_eq!(demux.mlp_payload_bytes, 200_479);
        assert_eq!(reassembly.packets_seen, 100);
        assert_eq!(reassembly.input_payload_bytes, 200_479);
        assert_eq!(reassembly.access_units, 704);
        assert_eq!(reassembly.framed_bytes, 200_350);
        assert_eq!(out.len(), 200_350);
        assert_eq!(reassembly.leading_fragment_bytes, 0);
        assert_eq!(reassembly.padding_bytes, 0);
        assert_eq!(reassembly.resync_events, 0);
        assert_eq!(reassembly.resync_bytes, 0);
        assert_eq!(&out[4..8], &MLP_MAJOR_SYNC_FBB);
    }

    #[test]
    fn bowie_midstream_fixture_uses_packet_pointer_without_resync() {
        let (out, reassembly, demux) =
            reassemble_aob_fixture_fragment("aob_sectors_48191_48210.bin");

        assert_eq!(demux.sectors_seen, 20);
        assert_eq!(demux.mlp_packets, 20);
        assert_eq!(demux.mlp_payload_bytes, 40_100);
        assert_eq!(reassembly.packets_seen, 20);
        assert_eq!(reassembly.input_payload_bytes, 40_100);
        assert_eq!(reassembly.leading_fragment_bytes, 366);
        assert_eq!(reassembly.access_units, 80);
        assert_eq!(reassembly.framed_bytes, 39_694);
        assert_eq!(out.len(), 39_694);
        assert_eq!(reassembly.resync_events, 0);
        assert_eq!(reassembly.resync_bytes, 0);
    }

    #[test]
    fn extracts_raw_mlp_payload_from_private_stream_1() {
        let payload = [0xF8, 0x72, 0x6F, 0xBA, 0x01, 0x02, 0x03];
        let sector = sector_with_private_stream(MLP_STREAM_ID, &payload, 0);
        let mut out = Vec::new();
        let mut stats = DvdaDemuxStats::default();

        extract_mlp_from_sector(&sector, &mut out, &mut stats).expect("MLP payload should demux");

        assert_eq!(out, payload);
        assert_eq!(stats.sectors_seen, 1);
        assert_eq!(stats.private_stream_1_packets, 1);
        assert_eq!(stats.mlp_packets, 1);
        assert_eq!(stats.mlp_payload_bytes, payload.len() as u64);
        assert_eq!(
            stats.first_sub_header.expect("sub-header").stream_id,
            MLP_STREAM_ID
        );
        assert_eq!(
            stats.last_sub_header.expect("last sub-header").stream_id,
            MLP_STREAM_ID
        );
        assert_eq!(stats.nonstandard_mlp_extra_header_packets, 0);
    }

    #[test]
    fn exposes_lpcm_packets_and_sub_header_facts() {
        let payload = [0x11, 0x22, 0x33, 0x44];
        let sector = sector_with_private_stream(PCM_STREAM_ID, &payload, 0);
        let mut stats = DvdaDemuxStats::default();
        let mut seen = Vec::new();

        demux_private_stream_1_packets(&sector, &mut stats, |packet| {
            assert_eq!(packet.sub_header.stream_id, PCM_STREAM_ID);
            assert_eq!(packet.payload, payload);
            let pcm = packet.sub_header.pcm.expect("PCM sub-header");
            assert_eq!(pcm.group1_bits, Some(24));
            assert_eq!(pcm.group1_sample_rate, Some(192_000));
            seen.extend_from_slice(packet.payload);
            Ok(())
        })
        .expect("LPCM packet should demux");

        assert_eq!(seen, payload);
        assert_eq!(stats.pcm_packets, 1);
        assert_eq!(stats.pcm_payload_bytes, payload.len() as u64);
        assert_eq!(
            stats.first_pcm_sub_header.expect("pcm").channel_assignment,
            0
        );
    }

    #[test]
    fn tracks_packet_header_consistency_diagnostics() {
        let first = sector_with_private_stream(MLP_STREAM_ID, &[0x01], 0);
        let mut second = sector_with_private_stream(MLP_STREAM_ID, &[0x02], 0);
        let sub_header_offset = 14 + 9;
        second[sub_header_offset + 1] = 9;
        second[sub_header_offset + 3] = MLP_EXTRA_HEADER_LENGTH + 1;

        let mut out = Vec::new();
        let mut stats = DvdaDemuxStats::default();
        extract_mlp_from_sector(&first, &mut out, &mut stats).expect("first packet");
        extract_mlp_from_sector(&second, &mut out, &mut stats).expect("second packet");

        assert_eq!(stats.extra_header_length_change_count, 1);
        assert_eq!(stats.nonstandard_mlp_extra_header_packets, 1);
        assert_eq!(stats.cyclic_discontinuity_count, 1);
    }

    #[test]
    fn honors_pack_header_stuffing() {
        let payload = [0xAA, 0xBB, 0xCC];
        let sector = sector_with_private_stream(MLP_STREAM_ID, &payload, 7);
        let mut out = Vec::new();
        let mut stats = DvdaDemuxStats::default();

        extract_mlp_from_sector(&sector, &mut out, &mut stats).expect("stuffed pack should demux");

        assert_eq!(out, payload);
    }

    #[test]
    fn mlp_extractor_rejects_lpcm_payloads() {
        let sector = sector_with_private_stream(PCM_STREAM_ID, &[1, 2, 3], 0);
        let mut out = Vec::new();
        let mut stats = DvdaDemuxStats::default();

        let err = extract_mlp_from_sector(&sector, &mut out, &mut stats)
            .expect_err("MLP-only extractor rejects LPCM");

        assert!(matches!(
            err,
            DvdaDemuxError::UnexpectedSubstream {
                stream_id: PCM_STREAM_ID
            }
        ));
    }

    #[test]
    fn demuxes_multiple_pes_packets_in_one_sector() {
        let mut sector = new_pack_sector();
        let mut offset = 14;
        offset = write_stream_payload(&mut sector, offset, 0xBE, &[0xAA, 0xBB]);
        offset = write_private_stream_packet(
            &mut sector,
            offset,
            &mlp_sub_header_with(0, MLP_EXTRA_HEADER_LENGTH, 0),
            &[0x01, 0x02],
        );
        offset = write_pes_packet(&mut sector, offset, 0xE0, &[0x11, 0x22], &[0x33, 0x44]);
        let _end = write_private_stream_packet(
            &mut sector,
            offset,
            &mlp_sub_header_with(1, MLP_EXTRA_HEADER_LENGTH, 0),
            &[0x03, 0x04, 0x05],
        );

        let mut out = Vec::new();
        let mut stats = DvdaDemuxStats::default();
        extract_mlp_from_sector(&sector, &mut out, &mut stats)
            .expect("multi-PES sector should demux");

        assert_eq!(out, vec![0x01, 0x02, 0x03, 0x04, 0x05]);
        assert_eq!(stats.private_stream_1_packets, 2);
        assert_eq!(stats.mlp_packets, 2);
        assert_eq!(stats.mlp_payload_bytes, 5);
    }

    #[test]
    fn demuxes_system_header_before_private_stream_1() {
        let mut sector = new_pack_sector();
        let mut offset = 14;
        offset = write_stream_payload(&mut sector, offset, 0xBB, &[0x00, 0x01, 0x02, 0x03]);
        let _end = write_private_stream_packet(
            &mut sector,
            offset,
            &mlp_sub_header_with(0, MLP_EXTRA_HEADER_LENGTH, 0),
            &[0xC0, 0xFF, 0xEE],
        );

        let mut out = Vec::new();
        let mut stats = DvdaDemuxStats::default();
        extract_mlp_from_sector(&sector, &mut out, &mut stats)
            .expect("system header should be skipped");

        assert_eq!(out, vec![0xC0, 0xFF, 0xEE]);
        assert_eq!(stats.private_stream_1_packets, 1);
        assert_eq!(stats.mlp_packets, 1);
    }

    #[test]
    fn demuxes_multiple_private_stream_1_packets_in_one_sector() {
        let mut sector = new_pack_sector();
        let mut offset = 14;
        for (cyclic, payload) in [
            (0_u8, &[0x10_u8][..]),
            (1_u8, &[0x20, 0x21][..]),
            (2_u8, &[0x30][..]),
        ] {
            offset = write_private_stream_packet(
                &mut sector,
                offset,
                &mlp_sub_header_with(cyclic, MLP_EXTRA_HEADER_LENGTH, 0),
                payload,
            );
        }

        let mut out = Vec::new();
        let mut stats = DvdaDemuxStats::default();
        extract_mlp_from_sector(&sector, &mut out, &mut stats)
            .expect("all PS1 packets should demux");

        assert_eq!(out, vec![0x10, 0x20, 0x21, 0x30]);
        assert_eq!(stats.private_stream_1_packets, 3);
        assert_eq!(stats.mlp_packets, 3);
        assert_eq!(stats.cyclic_discontinuity_count, 0);
    }

    #[test]
    fn rejects_private_stream_with_malformed_pes_header_length() {
        let mut sector = new_pack_sector();
        let offset = 14;
        sector[offset..offset + 4].copy_from_slice(&[0x00, 0x00, 0x01, PRIVATE_STREAM_1]);
        sector[offset + 4..offset + 6].copy_from_slice(&4_u16.to_be_bytes());
        sector[offset + 6] = 0x80;
        sector[offset + 7] = 0x80;
        sector[offset + 8] = 8;
        sector[offset + 9] = MLP_STREAM_ID;

        let err = extract_mlp_from_sector(&sector, &mut Vec::new(), &mut DvdaDemuxStats::default())
            .expect_err("PES header length that points beyond PES end should fail");

        assert!(matches!(
            err,
            DvdaDemuxError::PrivateStreamHeaderTruncated { .. }
        ));
    }

    #[test]
    fn parses_dvd_video_lpcm_fixed_header_with_variable_fau_pointer() {
        let mut sector = new_pack_sector();
        write_private_stream_packet(
            &mut sector,
            14,
            &dvd_video_pcm_sub_header_with(0x0058, 0x81),
            &[0x55, 0x66],
        );

        let packets =
            parse_private_stream_1_packets_with_mode(&sector, DvdaSubHeaderMode::DvdVideo)
                .expect("DVD-Video LPCM packet should demux with DVD-Video mode");
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].payload, &[0x55, 0x66]);
        let header = packets[0].sub_header;
        assert_eq!(header.extra_header_length, 3);
        assert_eq!(header.total_header_length, 7);
        let pcm = header.pcm.expect("DVD-Video LPCM fields should parse");
        assert_eq!(pcm.first_audio_frame, 0x0058);
        assert_eq!(pcm.group1_bits, Some(24));
        assert_eq!(pcm.group1_sample_rate, Some(48_000));
        assert_eq!(pcm.channel_count, Some(2));
        assert_eq!(pcm.channel_assignment, 1);
        assert_eq!(pcm.cci, 0x7F);
    }

    #[test]
    fn dvd_video_lpcm_mode_ignores_byte_three_as_length() {
        let mut sector = new_pack_sector();
        write_private_stream_packet(
            &mut sector,
            14,
            &dvd_video_pcm_sub_header_with(0x0190, 0x91),
            &[0xAA],
        );

        let packets =
            parse_private_stream_1_packets_with_mode(&sector, DvdaSubHeaderMode::DvdVideo)
                .expect("DVD-Video LPCM packet should not treat FAU low byte as header length");
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].payload, &[0xAA]);
        let header = packets[0].sub_header;
        assert_eq!(header.extra_header_length, 3);
        assert_eq!(header.total_header_length, 7);
        let pcm = header.pcm.expect("DVD-Video LPCM fields should parse");
        assert_eq!(pcm.first_audio_frame, 0x0190);
        assert_eq!(pcm.group1_bits, Some(24));
        assert_eq!(pcm.group1_sample_rate, Some(96_000));
        assert_eq!(pcm.channel_count, Some(2));
        assert_eq!(pcm.channel_assignment, 1);
    }

    #[test]
    fn dvd_video_lpcm_parses_seven_and_eight_channel_counts() {
        for (channels, format_byte) in [(7_u8, 0x86_u8), (8_u8, 0x87_u8)] {
            let mut sector = new_pack_sector();
            write_private_stream_packet(
                &mut sector,
                14,
                &dvd_video_pcm_sub_header_with(0x0058, format_byte),
                &[0x55],
            );

            let packets =
                parse_private_stream_1_packets_with_mode(&sector, DvdaSubHeaderMode::DvdVideo)
                    .expect("DVD-Video LPCM packet should parse");

            assert_eq!(packets.len(), 1);
            let header = packets[0].sub_header;
            assert_eq!(header.kind(), DvdaSubstreamKind::Pcm);
            let pcm = header.pcm.expect("pcm");
            assert_eq!(pcm.group1_bits, Some(24));
            assert_eq!(pcm.group1_sample_rate, Some(48_000));
            assert_eq!(pcm.channel_count, Some(channels));
        }
    }

    #[test]
    fn dvd_video_lpcm_mode_treats_substream_a1_with_seven_and_eight_channels_as_pcm() {
        for (channels, format_byte) in [(7_u8, 0x86_u8), (8_u8, 0x87_u8)] {
            let mut sector = new_pack_sector();
            let mut sub_header = dvd_video_pcm_sub_header_with(0x0058, format_byte);
            sub_header[0] = 0xA1;
            write_private_stream_packet(&mut sector, 14, &sub_header, &[0x55]);

            let packets =
                parse_private_stream_1_packets_with_mode(&sector, DvdaSubHeaderMode::DvdVideo)
                    .expect("DVD-Video LPCM packet should parse for substream A1");

            assert_eq!(packets.len(), 1);
            assert_eq!(packets[0].sub_header.stream_id, 0xA1);
            assert_eq!(packets[0].sub_header.kind(), DvdaSubstreamKind::Pcm);
            assert_eq!(packets[0].sub_header.pcm.expect("pcm").channel_count, Some(channels));
        }
    }

    #[test]
    fn dvd_video_lpcm_mode_treats_substream_a1_as_pcm() {
        let mut sector = new_pack_sector();
        write_private_stream_packet(
            &mut sector,
            14,
            &dvd_video_pcm_sub_header_with(0x0058, 0x91),
            &[0x55],
        );
        let offset = 14;
        let payload_len = 3 + dvd_video_pcm_sub_header_with(0x0058, 0x91).len() + 1;
        sector[offset + 4..offset + 6].copy_from_slice(&(payload_len as u16).to_be_bytes());
        let sub_header_offset = offset + 9;
        sector[sub_header_offset] = 0xA1;

        let packets =
            parse_private_stream_1_packets_with_mode(&sector, DvdaSubHeaderMode::DvdVideo)
                .expect("DVD-Video LPCM packet should parse for substream A1");
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].sub_header.stream_id, 0xA1);
        assert_eq!(packets[0].sub_header.kind(), DvdaSubstreamKind::Pcm);
        assert_eq!(packets[0].sub_header.pcm.expect("pcm").channel_count, Some(2));
    }

    #[test]
    fn dvd_audio_mode_does_not_guess_dvd_video_lpcm_from_payload() {
        let mut sector = new_pack_sector();
        write_private_stream_packet(
            &mut sector,
            14,
            &dvd_video_pcm_sub_header_with(0x0058, 0x81),
            &[0x55, 0x66],
        );

        let err = parse_private_stream_1_packets(&sector).expect_err(
            "default DVD-Audio mode must keep interpreting byte 3 as extra_header_length",
        );
        assert!(matches!(err, DvdaDemuxError::DvdaSubHeaderTruncated { .. }));
    }

    #[test]
    fn handles_sub_header_extra_length_variants() {
        let mut mlp_sector = new_pack_sector();
        write_private_stream_packet(
            &mut mlp_sector,
            14,
            &mlp_sub_header_with(0, MLP_EXTRA_HEADER_LENGTH + 1, 0x5A),
            &[0x99],
        );
        let mut mlp_out = Vec::new();
        let mut mlp_stats = DvdaDemuxStats::default();
        extract_mlp_from_sector(&mlp_sector, &mut mlp_out, &mut mlp_stats)
            .expect("MLP sub-header with longer extra header should demux");
        assert_eq!(mlp_out, vec![0x99]);
        assert_eq!(mlp_stats.nonstandard_mlp_extra_header_packets, 1);
        assert_eq!(
            mlp_stats
                .first_sub_header
                .expect("header")
                .total_header_length,
            4 + usize::from(MLP_EXTRA_HEADER_LENGTH + 1)
        );
        assert_eq!(mlp_stats.first_sub_header.expect("header").cci, Some(0x5A));

        let mut pcm_sector = new_pack_sector();
        write_private_stream_packet(
            &mut pcm_sector,
            14,
            &pcm_sub_header_with(PCM_EXTRA_HEADER_LENGTH + 1),
            &[0x55, 0x66],
        );
        let mut pcm_stats = DvdaDemuxStats::default();
        let mut pcm_payload = Vec::new();
        demux_private_stream_1_packets(&pcm_sector, &mut pcm_stats, |packet| {
            pcm_payload.extend_from_slice(packet.payload);
            Ok(())
        })
        .expect("PCM sub-header with longer extra header should demux");
        assert_eq!(pcm_payload, vec![0x55, 0x66]);
        assert_eq!(pcm_stats.nonstandard_pcm_extra_header_packets, 1);
        assert_eq!(
            pcm_stats
                .first_sub_header
                .expect("header")
                .total_header_length,
            14
        );
    }

    #[test]
    fn mlp_extractor_skips_unknown_dvda_substream_by_default() {
        let mut sector = new_pack_sector();
        let offset =
            write_private_stream_packet(&mut sector, 14, &[0xA7, 3, 0, 2, 0, 0], &[0xDE, 0xAD]);
        write_private_stream_packet(
            &mut sector,
            offset,
            &mlp_sub_header_with(0, MLP_EXTRA_HEADER_LENGTH, 0),
            &[0xF8, 0x72],
        );

        let mut out = Vec::new();
        let mut stats = DvdaDemuxStats::default();
        extract_mlp_from_sector(&sector, &mut out, &mut stats)
            .expect("unknown DVD-A substreams should be diagnostics-first by default");

        assert_eq!(out, vec![0xF8, 0x72]);
        assert_eq!(stats.private_stream_1_packets, 2);
        assert_eq!(stats.mlp_packets, 1);
        assert_eq!(
            stats
                .first_sub_header
                .expect("first recognized header")
                .stream_id,
            MLP_STREAM_ID
        );
    }

    #[test]
    fn mlp_uses_declared_short_extra_header_without_rejecting_sector() {
        let mut sector = new_pack_sector();
        write_private_stream_packet(
            &mut sector,
            14,
            &mlp_sub_header_with(0, 0, 0),
            &[0xA5, 0x5A],
        );

        let mut out = Vec::new();
        let mut stats = DvdaDemuxStats::default();
        extract_mlp_from_sector(&sector, &mut out, &mut stats)
            .expect("short MLP extra_header_length should be recorded as nonstandard, not rejected by the parser");

        assert_eq!(out, vec![0xA5, 0x5A]);
        let header = stats.first_sub_header.expect("header");
        assert_eq!(header.total_header_length, 4);
        assert_eq!(header.cci, None);
        assert_eq!(stats.nonstandard_mlp_extra_header_packets, 1);
    }

    #[test]
    fn demuxes_zero_payload_private_stream_sector() {
        let mut sector = new_pack_sector();
        write_private_stream_packet(
            &mut sector,
            14,
            &mlp_sub_header_with(0, MLP_EXTRA_HEADER_LENGTH, 0),
            &[],
        );

        let mut out = Vec::new();
        let mut stats = DvdaDemuxStats::default();
        extract_mlp_from_sector(&sector, &mut out, &mut stats)
            .expect("zero-payload MLP packet should demux");

        assert!(out.is_empty());
        assert_eq!(stats.private_stream_1_packets, 1);
        assert_eq!(stats.mlp_packets, 1);
        assert_eq!(stats.mlp_payload_bytes, 0);
    }

    #[test]
    fn ignores_padding_bytes_after_last_pes_packet() {
        let mut sector = new_pack_sector();
        let end = write_private_stream_packet(
            &mut sector,
            14,
            &mlp_sub_header_with(0, MLP_EXTRA_HEADER_LENGTH, 0),
            &[0xDE, 0xAD],
        );
        sector[end..end + 16].fill(0xFF);

        let mut out = Vec::new();
        let mut stats = DvdaDemuxStats::default();
        extract_mlp_from_sector(&sector, &mut out, &mut stats)
            .expect("trailing padding should be ignored");

        assert_eq!(out, vec![0xDE, 0xAD]);
        assert_eq!(stats.private_stream_1_packets, 1);
    }

    #[test]
    fn rejects_truncated_pes_packet() {
        let mut sector = [0_u8; DVD_SECTOR_SIZE];
        sector[..4].copy_from_slice(&PACK_START_CODE);
        let offset = 14;
        sector[offset..offset + 4].copy_from_slice(&[0, 0, 1, PRIVATE_STREAM_1]);
        sector[offset + 4..offset + 6].copy_from_slice(&u16::MAX.to_be_bytes());

        let err = extract_mlp_from_sector(&sector, &mut Vec::new(), &mut DvdaDemuxStats::default())
            .expect_err("oversize PES packet should fail");

        assert!(matches!(err, DvdaDemuxError::PesPacketTruncated { .. }));
    }

    #[test]
    fn ignores_non_private_stream_packets() {
        let mut sector = [0_u8; DVD_SECTOR_SIZE];
        sector[..4].copy_from_slice(&PACK_START_CODE);
        let offset = 14;
        sector[offset..offset + 4].copy_from_slice(&[0, 0, 1, 0xBE]);
        sector[offset + 4..offset + 6].copy_from_slice(&3_u16.to_be_bytes());
        sector[offset + 6..offset + 9].copy_from_slice(&[1, 2, 3]);

        let mut out = Vec::new();
        let mut stats = DvdaDemuxStats::default();
        extract_mlp_from_sector(&sector, &mut out, &mut stats)
            .expect("padding stream should be skipped");

        assert!(out.is_empty());
        assert_eq!(stats.private_stream_1_packets, 0);
    }
    #[test]
    fn malformed_sector_after_valid_packet_is_sector_atomic() {
        let mut sector = new_pack_sector();
        let offset = write_private_stream_packet(
            &mut sector,
            14,
            &mlp_sub_header_with(0, MLP_EXTRA_HEADER_LENGTH, 0),
            &[0xAA, 0xBB],
        );
        sector[offset..offset + 4].copy_from_slice(&[0x00, 0x00, 0x01, PRIVATE_STREAM_1]);
        sector[offset + 4..offset + 6].copy_from_slice(&4_u16.to_be_bytes());
        sector[offset + 6] = 0x80;
        sector[offset + 7] = 0x80;
        sector[offset + 8] = 8;

        let mut callbacks = 0_u32;
        let mut emitted = Vec::new();
        let err =
            demux_private_stream_1_packets(&sector, &mut DvdaDemuxStats::default(), |packet| {
                callbacks += 1;
                emitted.extend_from_slice(packet.payload);
                Ok(())
            })
            .expect_err("later malformed PES should fail the whole sector before callbacks run");

        assert!(matches!(
            err,
            DvdaDemuxError::PrivateStreamHeaderTruncated { .. }
        ));
        assert_eq!(callbacks, 0);
        assert!(emitted.is_empty());
    }

    #[test]
    fn mlp_extractor_does_not_write_partial_output_on_later_semantic_rejection() {
        let mut sector = new_pack_sector();
        let offset = write_private_stream_packet(
            &mut sector,
            14,
            &mlp_sub_header_with(0, MLP_EXTRA_HEADER_LENGTH, 0),
            &[0xAA, 0xBB],
        );
        write_private_stream_packet(
            &mut sector,
            offset,
            &pcm_sub_header_with(PCM_EXTRA_HEADER_LENGTH),
            &[0xCC],
        );

        let mut out = Vec::new();
        let err = extract_mlp_from_sector(&sector, &mut out, &mut DvdaDemuxStats::default())
            .expect_err("MLP-only extractor should reject mixed LPCM sector atomically");

        assert!(matches!(
            err,
            DvdaDemuxError::UnexpectedSubstream {
                stream_id: PCM_STREAM_ID
            }
        ));
        assert!(out.is_empty());
    }

    fn fuzz_next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn fuzz_byte(state: &mut u64) -> u8 {
        (fuzz_next(state) >> 24) as u8
    }

    fn fill_random_sector(seed: u64) -> [u8; DVD_SECTOR_SIZE] {
        let mut state = seed | 1;
        let mut sector = [0_u8; DVD_SECTOR_SIZE];
        for byte in &mut sector {
            *byte = fuzz_byte(&mut state);
        }
        if seed % 3 != 0 {
            sector[..4].copy_from_slice(&PACK_START_CODE);
            sector[13] &= 0x07;
        }
        if seed % 5 == 0 {
            let offset = 14 + usize::from(sector[13] & 0x07);
            if offset + 12 < DVD_SECTOR_SIZE {
                sector[offset..offset + 4].copy_from_slice(&[0x00, 0x00, 0x01, PRIVATE_STREAM_1]);
                let length = u16::from_be_bytes([fuzz_byte(&mut state), fuzz_byte(&mut state)]);
                sector[offset + 4..offset + 6].copy_from_slice(&length.to_be_bytes());
            }
        }
        sector
    }

    fn structured_fuzz_sector(seed: u64) -> [u8; DVD_SECTOR_SIZE] {
        let mut state = seed | 1;
        let mut sector = new_pack_sector();
        sector[13] = fuzz_byte(&mut state) & 0x07;
        let mut offset = 14 + usize::from(sector[13] & 0x07);
        let packet_count = usize::from(fuzz_byte(&mut state) % 10);

        for _ in 0..packet_count {
            if offset + 6 >= DVD_SECTOR_SIZE {
                break;
            }
            let stream_id = match fuzz_byte(&mut state) % 5 {
                0 | 1 => PRIVATE_STREAM_1,
                2 => 0xBE,
                3 => 0xBB,
                _ => 0xE0,
            };
            sector[offset..offset + 4].copy_from_slice(&[0x00, 0x00, 0x01, stream_id]);

            let remaining_payload_capacity = DVD_SECTOR_SIZE - offset - 6;
            let choose_truncated = fuzz_byte(&mut state) % 11 == 0;
            let payload_len = if choose_truncated {
                remaining_payload_capacity.saturating_add(usize::from(fuzz_byte(&mut state)) + 1)
            } else {
                usize::from(fuzz_byte(&mut state)) % (remaining_payload_capacity.min(96) + 1)
            };
            let encoded_len = payload_len.min(u16::MAX as usize);
            sector[offset + 4..offset + 6].copy_from_slice(&(encoded_len as u16).to_be_bytes());

            if encoded_len > remaining_payload_capacity {
                break;
            }

            if stream_id == PRIVATE_STREAM_1 && encoded_len >= 3 {
                sector[offset + 6] = 0x80;
                sector[offset + 7] = 0x80;
                let header_data_len = if fuzz_byte(&mut state) % 7 == 0 {
                    (encoded_len as u8).saturating_add(8)
                } else {
                    fuzz_byte(&mut state) % ((encoded_len - 2).min(u8::MAX as usize) as u8 + 1)
                };
                sector[offset + 8] = header_data_len;
                let body_offset = offset + 9 + usize::from(header_data_len);
                if body_offset < offset + 6 + encoded_len {
                    let stream = match fuzz_byte(&mut state) % 4 {
                        0 | 1 => MLP_STREAM_ID,
                        2 => PCM_STREAM_ID,
                        _ => fuzz_byte(&mut state),
                    };
                    sector[body_offset] = stream;
                    if body_offset + 3 < offset + 6 + encoded_len {
                        sector[body_offset + 1] = fuzz_byte(&mut state);
                        sector[body_offset + 2] = 0;
                        sector[body_offset + 3] = match stream {
                            MLP_STREAM_ID => fuzz_byte(&mut state) % 8,
                            PCM_STREAM_ID => fuzz_byte(&mut state) % 13,
                            _ => fuzz_byte(&mut state) % 8,
                        };
                    }
                }
            } else {
                let payload_start = offset + 6;
                let payload_end = payload_start + encoded_len;
                for byte in &mut sector[payload_start..payload_end] {
                    *byte = fuzz_byte(&mut state);
                }
            }

            offset += 6 + encoded_len;
            if fuzz_byte(&mut state) % 9 == 0 {
                break;
            }
        }

        sector
    }

    fn assert_demux_no_panic_and_atomic_on_parse_error(sector: &[u8]) {
        let mut stats = DvdaDemuxStats::default();
        let mut callback_count = 0_u32;
        let mut emitted = Vec::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            demux_private_stream_1_packets(sector, &mut stats, |packet| {
                callback_count += 1;
                emitted.extend_from_slice(packet.payload);
                Ok(())
            })
        }));

        assert!(
            result.is_ok(),
            "demuxer panicked while parsing fuzzed sector"
        );
        if result.expect("checked panic").is_err() {
            assert_eq!(
                callback_count, 0,
                "callback ran before a parse error was reported"
            );
            assert!(
                emitted.is_empty(),
                "payload was emitted before a parse error was reported"
            );
        }
    }

    #[test]
    fn random_sector_fuzz_does_not_panic_or_emit_on_parse_error() {
        for seed in 0..4096_u64 {
            let sector = fill_random_sector(
                0xDADA_0000_0000_0000 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15),
            );
            assert_demux_no_panic_and_atomic_on_parse_error(&sector);
        }
    }

    #[test]
    fn structured_pes_fuzz_does_not_panic_or_emit_on_parse_error() {
        for seed in 0..4096_u64 {
            let sector = structured_fuzz_sector(
                0xA0B0_C0D0_E0F0_0000 ^ seed.wrapping_mul(0xD1B5_4A32_D192_ED03),
            );
            assert_demux_no_panic_and_atomic_on_parse_error(&sector);
        }
    }
}
