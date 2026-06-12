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
    pub channel_assignment: u8,
    pub cci: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DvdaSubHeader {
    pub stream_id: u8,
    pub cyclic: u8,
    pub extra_header_length: u8,
    pub total_header_length: usize,
    pub cci: Option<u8>,
    pub pcm: Option<DvdaPcmSubHeader>,
}

impl DvdaSubHeader {
    #[must_use]
    pub const fn kind(self) -> DvdaSubstreamKind {
        DvdaSubstreamKind::from_stream_id(self.stream_id)
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

#[derive(Debug)]
pub enum DvdaDemuxError {
    SectorSize { actual: usize },
    MissingPackHeader,
    PackHeaderTruncated { stuffing: usize },
    PesPacketTruncated { offset: usize, length: usize },
    PrivateStreamHeaderTruncated { offset: usize, pes_end: usize },
    DvdaSubHeaderMissing { offset: usize, available: usize },
    DvdaSubHeaderTruncated {
        offset: usize,
        header_length: usize,
        available: usize,
    },
    MlpSubHeaderTooShort { offset: usize, extra_header_length: u8 },
    PcmSubHeaderTooShort { offset: usize, extra_header_length: u8 },
    UnexpectedSubstream { stream_id: u8 },
    PacketHandler(String),
    Write(io::Error),
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
        DvdaSubstreamKind::Pcm => Err(DvdaDemuxError::UnexpectedSubstream { stream_id: PCM_STREAM_ID }),
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

pub fn parse_private_stream_1_packets(sector: &[u8]) -> Result<Vec<DvdaPs1Packet<'_>>, DvdaDemuxError> {
    if sector.len() != DVD_SECTOR_SIZE {
        return Err(DvdaDemuxError::SectorSize { actual: sector.len() });
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
        let pes_end = offset.checked_add(6).and_then(|v| v.checked_add(pes_length)).ok_or(
            DvdaDemuxError::PesPacketTruncated {
                offset,
                length: pes_length,
            },
        )?;
        if pes_end > sector.len() {
            return Err(DvdaDemuxError::PesPacketTruncated {
                offset,
                length: pes_length,
            });
        }

        if stream_id == PRIVATE_STREAM_1 {
            packets.push(parse_private_stream_1_packet(sector, offset, pes_end)?);
        }

        offset = pes_end;
    }

    Ok(packets)
}

fn parse_private_stream_1_packet(
    sector: &[u8],
    pes_offset: usize,
    pes_end: usize,
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

    let sub_header = parse_sub_header(&sector[sub_header_offset..pes_end], sub_header_offset)?;
    let body_offset = sub_header_offset + sub_header.total_header_length;
    let payload = if body_offset < pes_end {
        &sector[body_offset..pes_end]
    } else {
        &[]
    };

    Ok(DvdaPs1Packet { sub_header, payload })
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
            stats.extra_header_length_change_count = stats
                .extra_header_length_change_count
                .saturating_add(1);
        }
        let expected_cyclic = previous.cyclic.wrapping_add(1);
        if sub_header.cyclic != previous.cyclic && sub_header.cyclic != expected_cyclic {
            stats.cyclic_discontinuity_count = stats
                .cyclic_discontinuity_count
                .saturating_add(1);
        }
    }

    match sub_header.kind() {
        DvdaSubstreamKind::Mlp => {
            stats.mlp_packets = stats.mlp_packets.saturating_add(1);
            stats.mlp_payload_bytes = stats.mlp_payload_bytes.saturating_add(payload_len as u64);
            if sub_header.extra_header_length != MLP_EXTRA_HEADER_LENGTH {
                stats.nonstandard_mlp_extra_header_packets = stats
                    .nonstandard_mlp_extra_header_packets
                    .saturating_add(1);
            }
        }
        DvdaSubstreamKind::Pcm => {
            stats.pcm_packets = stats.pcm_packets.saturating_add(1);
            stats.pcm_payload_bytes = stats.pcm_payload_bytes.saturating_add(payload_len as u64);
            if sub_header.extra_header_length != PCM_EXTRA_HEADER_LENGTH {
                stats.nonstandard_pcm_extra_header_packets = stats
                    .nonstandard_pcm_extra_header_packets
                    .saturating_add(1);
            }
            if let Some(pcm) = sub_header.pcm {
                if stats.first_pcm_sub_header.is_none() {
                    stats.first_pcm_sub_header = Some(pcm);
                }
                if let Some(previous) = stats.last_pcm_sub_header {
                    if pcm_format_without_pointer(previous) != pcm_format_without_pointer(pcm) {
                        stats.pcm_format_change_count = stats.pcm_format_change_count.saturating_add(1);
                    }
                }
                stats.last_pcm_sub_header = Some(pcm);
            }
        }
        DvdaSubstreamKind::Unknown(_) => {}
    }

    stats.last_sub_header = Some(sub_header);
}

fn pcm_format_without_pointer(pcm: DvdaPcmSubHeader) -> (u8, u8, u8, u8, Option<u32>, Option<u32>, Option<u32>, Option<u32>, u8) {
    (
        pcm.group1_bits_code,
        pcm.group2_bits_code,
        pcm.group1_sample_rate_code,
        pcm.group2_sample_rate_code,
        pcm.group1_bits,
        pcm.group2_bits,
        pcm.group1_sample_rate,
        pcm.group2_sample_rate,
        pcm.channel_assignment,
    )
}

fn parse_sub_header(bytes: &[u8], offset: usize) -> Result<DvdaSubHeader, DvdaDemuxError> {
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

    let (cci, pcm) = match DvdaSubstreamKind::from_stream_id(stream_id) {
        DvdaSubstreamKind::Mlp => (bytes.get(8).copied(), None),
        DvdaSubstreamKind::Pcm => {
            if extra_header_length >= PCM_EXTRA_HEADER_LENGTH {
                let pcm = parse_pcm_sub_header(bytes);
                (Some(pcm.cci), Some(pcm))
            } else {
                (None, None)
            }
        }
        DvdaSubstreamKind::Unknown(_) => (None, None),
    };

    Ok(DvdaSubHeader {
        stream_id,
        cyclic,
        extra_header_length,
        total_header_length,
        cci,
        pcm,
    })
}

fn parse_pcm_sub_header(bytes: &[u8]) -> DvdaPcmSubHeader {
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
        channel_assignment: bytes[10],
        cci: bytes[12],
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

    fn sector_with_private_stream(substream_id: u8, payload: &[u8], stuffing: u8) -> [u8; DVD_SECTOR_SIZE] {
        let mut sector = [0_u8; DVD_SECTOR_SIZE];
        sector[..4].copy_from_slice(&PACK_START_CODE);
        sector[13] = stuffing & 0x07;

        let pes_offset = 14 + usize::from(stuffing & 0x07);
        sector[pes_offset..pes_offset + 4]
            .copy_from_slice(&[0x00, 0x00, 0x01, PRIVATE_STREAM_1]);
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
        sector[pes_offset + 4..pes_offset + 6].copy_from_slice(&(pes_payload_len as u16).to_be_bytes());

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
        sector[header_offset..header_offset + pes_header_data.len()].copy_from_slice(pes_header_data);
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
        AobFixture { name: "ap_eye_in_the_sky_first_16_sectors.bin", first_cyclic: 0, payload_bytes: 32_059 },
        AobFixture { name: "ap_friendly_card_first_16_sectors.bin", first_cyclic: 0, payload_bytes: 32_059 },
        AobFixture { name: "ap_i_robot_first_16_sectors.bin", first_cyclic: 0, payload_bytes: 32_059 },
        AobFixture { name: "hdad2009_first_16_sectors.bin", first_cyclic: 0, payload_bytes: 32_059 },
        AobFixture { name: "hawks_and_doves_first_16_sectors.bin", first_cyclic: 32, payload_bytes: 32_059 },
        AobFixture { name: "mgletsgetiton_first_16_sectors.bin", first_cyclic: 32, payload_bytes: 32_059 },
        AobFixture { name: "talking_heads_77_first_16_sectors.bin", first_cyclic: 32, payload_bytes: 32_059 },
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
            assert_eq!(stats.private_stream_1_packets, 16, "{} PS1 packet count", fixture.name);
            assert_eq!(stats.mlp_packets, 16, "{} MLP packet count", fixture.name);
            assert_eq!(stats.pcm_packets, 0, "{} LPCM packet count", fixture.name);
            assert_eq!(stats.mlp_payload_bytes, fixture.payload_bytes, "{} payload bytes", fixture.name);
            assert_eq!(payload.len() as u64, fixture.payload_bytes, "{} payload vector length", fixture.name);
            assert_eq!(stats.nonstandard_mlp_extra_header_packets, 0, "{} should use canonical real-disc MLP extra headers", fixture.name);
            assert_eq!(stats.extra_header_length_change_count, 0, "{} should have stable MLP extra headers", fixture.name);
            assert_eq!(stats.cyclic_discontinuity_count, 0, "{} should have contiguous cyclic counters", fixture.name);
            assert_eq!(stats.first_sub_header.expect("first header").extra_header_length, MLP_EXTRA_HEADER_LENGTH);
            assert_eq!(stats.first_sub_header.expect("first header").total_header_length, 4 + usize::from(MLP_EXTRA_HEADER_LENGTH));
            assert_eq!(cyclic_values.first().copied(), Some(fixture.first_cyclic), "{} first cyclic", fixture.name);
            for pair in cyclic_values.windows(2) {
                assert_eq!(pair[1], pair[0].wrapping_add(1), "{} cyclic sequence", fixture.name);
            }
        }
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
        assert_eq!(stats.first_sub_header.expect("sub-header").stream_id, MLP_STREAM_ID);
        assert_eq!(stats.last_sub_header.expect("last sub-header").stream_id, MLP_STREAM_ID);
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
        assert_eq!(stats.first_pcm_sub_header.expect("pcm").channel_assignment, 0);
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

        let err = extract_mlp_from_sector(&sector, &mut out, &mut stats).expect_err("MLP-only extractor rejects LPCM");

        assert!(matches!(err, DvdaDemuxError::UnexpectedSubstream { stream_id: PCM_STREAM_ID }));
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
        extract_mlp_from_sector(&sector, &mut out, &mut stats).expect("multi-PES sector should demux");

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
        extract_mlp_from_sector(&sector, &mut out, &mut stats).expect("system header should be skipped");

        assert_eq!(out, vec![0xC0, 0xFF, 0xEE]);
        assert_eq!(stats.private_stream_1_packets, 1);
        assert_eq!(stats.mlp_packets, 1);
    }

    #[test]
    fn demuxes_multiple_private_stream_1_packets_in_one_sector() {
        let mut sector = new_pack_sector();
        let mut offset = 14;
        for (cyclic, payload) in [(0_u8, &[0x10_u8][..]), (1_u8, &[0x20, 0x21][..]), (2_u8, &[0x30][..])] {
            offset = write_private_stream_packet(
                &mut sector,
                offset,
                &mlp_sub_header_with(cyclic, MLP_EXTRA_HEADER_LENGTH, 0),
                payload,
            );
        }

        let mut out = Vec::new();
        let mut stats = DvdaDemuxStats::default();
        extract_mlp_from_sector(&sector, &mut out, &mut stats).expect("all PS1 packets should demux");

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

        assert!(matches!(err, DvdaDemuxError::PrivateStreamHeaderTruncated { .. }));
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
        assert_eq!(mlp_stats.first_sub_header.expect("header").total_header_length, 4 + usize::from(MLP_EXTRA_HEADER_LENGTH + 1));
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
        assert_eq!(pcm_stats.first_sub_header.expect("header").total_header_length, 14);
    }


    #[test]
    fn mlp_extractor_skips_unknown_dvda_substream_by_default() {
        let mut sector = new_pack_sector();
        let offset = write_private_stream_packet(
            &mut sector,
            14,
            &[0xA7, 3, 0, 2, 0, 0],
            &[0xDE, 0xAD],
        );
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
        assert_eq!(stats.first_sub_header.expect("first recognized header").stream_id, MLP_STREAM_ID);
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
        extract_mlp_from_sector(&sector, &mut out, &mut stats).expect("zero-payload MLP packet should demux");

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
        extract_mlp_from_sector(&sector, &mut out, &mut stats).expect("trailing padding should be ignored");

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
        extract_mlp_from_sector(&sector, &mut out, &mut stats).expect("padding stream should be skipped");

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
        let err = demux_private_stream_1_packets(&sector, &mut DvdaDemuxStats::default(), |packet| {
            callbacks += 1;
            emitted.extend_from_slice(packet.payload);
            Ok(())
        })
        .expect_err("later malformed PES should fail the whole sector before callbacks run");

        assert!(matches!(err, DvdaDemuxError::PrivateStreamHeaderTruncated { .. }));
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
        write_private_stream_packet(&mut sector, offset, &pcm_sub_header_with(PCM_EXTRA_HEADER_LENGTH), &[0xCC]);

        let mut out = Vec::new();
        let err = extract_mlp_from_sector(&sector, &mut out, &mut DvdaDemuxStats::default())
            .expect_err("MLP-only extractor should reject mixed LPCM sector atomically");

        assert!(matches!(err, DvdaDemuxError::UnexpectedSubstream { stream_id: PCM_STREAM_ID }));
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

        assert!(result.is_ok(), "demuxer panicked while parsing fuzzed sector");
        if result.expect("checked panic").is_err() {
            assert_eq!(callback_count, 0, "callback ran before a parse error was reported");
            assert!(emitted.is_empty(), "payload was emitted before a parse error was reported");
        }
    }

    #[test]
    fn random_sector_fuzz_does_not_panic_or_emit_on_parse_error() {
        for seed in 0..4096_u64 {
            let sector = fill_random_sector(0xDADA_0000_0000_0000 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            assert_demux_no_panic_and_atomic_on_parse_error(&sector);
        }
    }

    #[test]
    fn structured_pes_fuzz_does_not_panic_or_emit_on_parse_error() {
        for seed in 0..4096_u64 {
            let sector = structured_fuzz_sector(0xA0B0_C0D0_E0F0_0000 ^ seed.wrapping_mul(0xD1B5_4A32_D192_ED03));
            assert_demux_no_panic_and_atomic_on_parse_error(&sector);
        }
    }

}
