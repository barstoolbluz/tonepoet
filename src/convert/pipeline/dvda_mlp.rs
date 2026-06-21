#![forbid(unsafe_code)]

//! Lightweight validation for raw DVD-Audio MLP elementary streams.
//!
//! This is not a replacement for ffmpeg's decoder. It catches malformed access
//! unit boundaries, missing major-sync metadata, unexpected TrueHD payloads, and
//! IFO/source fact mismatches before the decode step runs.

use super::dvda_channel_layout::layout_for_assignment_code;

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

pub const MLP_STREAM_TYPE: u8 = 0xBB;
pub const TRUEHD_STREAM_TYPE: u8 = 0xBA;

const MLP_MAJOR_SYNC_OFFSET: usize = 4;
const MLP_MAJOR_SYNC_MIN_BYTES: usize = 28;
const MLP_FRAME_SIZE_MASK: u16 = 0x0FFF;
const MLP_SYNC_PREFIX: [u8; 3] = [0xF8, 0x72, 0x6F];

const MLP_QUANTS: [u32; 16] = [16, 20, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];


#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MlpStreamExpectation {
    pub sample_rate: Option<u32>,
    pub channel_count: Option<u32>,
    pub bit_depth: Option<u32>,
    pub group1_sample_rate: Option<u32>,
    pub group2_sample_rate: Option<u32>,
    pub group1_bit_depth: Option<u32>,
    pub group2_bit_depth: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MlpStreamInspection {
    pub payload_bytes: u64,
    pub frame_count: u64,
    pub major_sync_frame_count: u64,
    pub min_frame_bytes: Option<usize>,
    pub max_frame_bytes: Option<usize>,
    pub first_major_sync: Option<MlpMajorSyncInfo>,
    /// Present when fixture/prefix inspection stopped at a declared access unit
    /// that extends beyond the supplied byte slice. Full-track inspection keeps
    /// this disabled and treats the same condition as an error.
    pub trailing_partial_frame_bytes: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MlpInspectOptions {
    pub allow_trailing_partial_frame: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MlpMajorSyncInfo {
    pub stream_type: u8,
    pub group1_bits: u32,
    pub group2_bits: u32,
    pub group1_sample_rate: u32,
    pub group2_sample_rate: u32,
    pub channel_arrangement: u32,
    pub channel_count: u32,
    pub access_unit_size: u32,
    pub is_vbr: bool,
    pub peak_bitrate: u32,
    pub num_substreams: u32,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum MlpInspectError {
    Io(io::Error),
    Empty,
    PartialFrameHeader { offset: u64 },
    ZeroFrameLength { offset: u64 },
    FrameLengthPastEof {
        offset: u64,
        declared_bytes: usize,
        bytes_read: usize,
    },
    #[allow(dead_code)]
    FrameTooShortForMajorSync { offset: u64, frame_bytes: usize },
    MajorSyncTooShort { offset: u64, available: usize },
    MajorSyncBitRead { offset: u64, field: &'static str },
    UnexpectedStreamType { offset: u64, stream_type: u8 },
    UnsupportedMajorSyncValue {
        offset: u64,
        field: &'static str,
        value: u32,
    },
    MajorSyncChanged {
        offset: u64,
        field: &'static str,
        first: u32,
        later: u32,
    },
    ParityCheckFailed { offset: u64 },
    NoMajorSync,
    SampleRateMismatch { expected: u32, actual: u32 },
    ChannelCountMismatch { expected: u32, actual: u32 },
    BitDepthMismatch { expected: u32, actual: u32 },
    GroupSampleRateMismatch { group: u8, expected: u32, actual: u32 },
    GroupBitDepthMismatch { group: u8, expected: u32, actual: u32 },
}

impl fmt::Display for MlpInspectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "failed to inspect DVD-Audio MLP payload: {err}"),
            Self::Empty => write!(f, "DVD-Audio MLP payload is empty"),
            Self::PartialFrameHeader { offset } => write!(
                f,
                "DVD-Audio MLP payload ended with a partial access-unit header at byte {offset}"
            ),
            Self::ZeroFrameLength { offset } => write!(
                f,
                "DVD-Audio MLP access unit at byte {offset} declares a zero length"
            ),
            Self::FrameLengthPastEof {
                offset,
                declared_bytes,
                bytes_read,
            } => write!(
                f,
                "DVD-Audio MLP access unit at byte {offset} declares {declared_bytes} bytes, but only {bytes_read} bytes were readable"
            ),
            Self::FrameTooShortForMajorSync { offset, frame_bytes } => write!(
                f,
                "DVD-Audio MLP major-sync marker at byte {offset} appears in a frame with only {frame_bytes} bytes"
            ),
            Self::MajorSyncTooShort { offset, available } => write!(
                f,
                "DVD-Audio MLP major-sync header at byte {offset} has only {available} bytes"
            ),
            Self::MajorSyncBitRead { offset, field } => write!(
                f,
                "DVD-Audio MLP major-sync header at byte {offset} ended while reading {field}"
            ),
            Self::UnexpectedStreamType { offset, stream_type } => write!(
                f,
                "DVD-Audio MLP major-sync header at byte {offset} has stream type 0x{stream_type:02X}; expected DVD-Audio MLP 0x{MLP_STREAM_TYPE:02X}"
            ),
            Self::UnsupportedMajorSyncValue { offset, field, value } => write!(
                f,
                "DVD-Audio MLP major-sync header at byte {offset} reports unsupported {field} value {value}"
            ),
            Self::MajorSyncChanged {
                offset,
                field,
                first,
                later,
            } => write!(
                f,
                "DVD-Audio MLP major-sync {field} changed at byte {offset}: first {first}, later {later}"
            ),
            Self::ParityCheckFailed { offset } => write!(
                f,
                "DVD-Audio MLP access-unit parity check failed at byte {offset}"
            ),
            Self::NoMajorSync => write!(f, "DVD-Audio MLP payload contains no major-sync frame"),
            Self::SampleRateMismatch { expected, actual } => write!(
                f,
                "DVD-Audio MLP sample rate mismatch: IFO expected {expected} Hz, MLP major-sync reports {actual} Hz"
            ),
            Self::ChannelCountMismatch { expected, actual } => write!(
                f,
                "DVD-Audio MLP channel count mismatch: IFO expected {expected}, MLP major-sync reports {actual}"
            ),
            Self::BitDepthMismatch { expected, actual } => write!(
                f,
                "DVD-Audio MLP bit-depth mismatch: IFO expected {expected}, MLP major-sync reports {actual}"
            ),
            Self::GroupSampleRateMismatch { group, expected, actual } => write!(
                f,
                "DVD-Audio MLP group {group} sample-rate mismatch: IFO expected {expected} Hz, MLP major-sync reports {actual} Hz"
            ),
            Self::GroupBitDepthMismatch { group, expected, actual } => write!(
                f,
                "DVD-Audio MLP group {group} bit-depth mismatch: IFO expected {expected}, MLP major-sync reports {actual}"
            ),
        }
    }
}

impl MlpInspectError {
    #[must_use]
    pub fn is_audio_fact_mismatch(&self) -> bool {
        matches!(
            self,
            Self::SampleRateMismatch { .. }
                | Self::ChannelCountMismatch { .. }
                | Self::BitDepthMismatch { .. }
                | Self::GroupSampleRateMismatch { .. }
                | Self::GroupBitDepthMismatch { .. }
                | Self::UnsupportedMajorSyncValue { field: "channel_arrangement", .. }
        )
    }
}

impl std::error::Error for MlpInspectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for MlpInspectError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

pub fn inspect_mlp_file(
    path: &Path,
    expectation: MlpStreamExpectation,
) -> Result<MlpStreamInspection, MlpInspectError> {
    inspect_mlp_file_with_options(path, expectation, MlpInspectOptions::default())
}

pub fn inspect_mlp_file_with_options(
    path: &Path,
    expectation: MlpStreamExpectation,
    options: MlpInspectOptions,
) -> Result<MlpStreamInspection, MlpInspectError> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    inspect_mlp_reader(&mut reader, expectation, options)
}

/// Probe raw MLP payload bytes (from the first sector of a track) for a major
/// sync header. Scans through access unit frames within the payload until a
/// major sync is found. Returns format info if found, `None` if the payload
/// is too short or lacks a major sync. Intended for lightweight disc-info probing.
pub fn probe_mlp_major_sync(mlp_payload: &[u8]) -> Option<MlpMajorSyncInfo> {
    let mut offset = 0usize;
    while offset + 2 <= mlp_payload.len() {
        let frame = &mlp_payload[offset..];
        if frame_has_major_sync(frame) {
            return parse_mlp_major_sync(&frame[MLP_MAJOR_SYNC_OFFSET..], offset as u64).ok();
        }
        // Advance to the next access unit frame using the 2-byte length word.
        let raw = u16::from_be_bytes([frame[0], frame[1]]);
        let frame_len = usize::from(raw & MLP_FRAME_SIZE_MASK) * 2;
        if frame_len == 0 {
            break;
        }
        offset += frame_len;
    }
    None
}

#[cfg(test)]
fn inspect_mlp_bytes_with_options(
    bytes: &[u8],
    expectation: MlpStreamExpectation,
    options: MlpInspectOptions,
) -> Result<MlpStreamInspection, MlpInspectError> {
    let mut reader = bytes;
    inspect_mlp_reader(&mut reader, expectation, options)
}

fn inspect_mlp_reader<R: Read>(
    reader: &mut R,
    expectation: MlpStreamExpectation,
    options: MlpInspectOptions,
) -> Result<MlpStreamInspection, MlpInspectError> {
    let mut header = [0_u8; 2];
    let mut offset = 0_u64;
    let mut inspection = MlpStreamInspection::default();
    let mut known_num_substreams = None;

    loop {
        match reader.read(&mut header[..1])? {
            0 => break,
            1 => {}
            _ => unreachable!(),
        }
        if let Err(err) = reader.read_exact(&mut header[1..]) {
            if err.kind() == io::ErrorKind::UnexpectedEof {
                if options.allow_trailing_partial_frame {
                    inspection.trailing_partial_frame_bytes = Some(1);
                    break;
                }
                return Err(MlpInspectError::PartialFrameHeader { offset });
            }
            return Err(MlpInspectError::Io(err));
        }

        let frame_len = mlp_frame_length(header, offset)?;
        let mut frame = vec![0_u8; frame_len];
        frame[..2].copy_from_slice(&header);
        let bytes_after_header = read_exact_count(reader, &mut frame[2..])?;
        if bytes_after_header != frame_len - 2 {
            if options.allow_trailing_partial_frame {
                inspection.trailing_partial_frame_bytes = Some(bytes_after_header + 2);
                break;
            }
            return Err(MlpInspectError::FrameLengthPastEof {
                offset,
                declared_bytes: frame_len,
                bytes_read: bytes_after_header + 2,
            });
        }

        inspection.payload_bytes = inspection.payload_bytes.saturating_add(frame_len as u64);
        inspection.frame_count = inspection.frame_count.saturating_add(1);
        inspection.min_frame_bytes = Some(
            inspection
                .min_frame_bytes
                .map_or(frame_len, |current| current.min(frame_len)),
        );
        inspection.max_frame_bytes = Some(
            inspection
                .max_frame_bytes
                .map_or(frame_len, |current| current.max(frame_len)),
        );

        if frame_has_major_sync(&frame) {
            let info = parse_mlp_major_sync(&frame[MLP_MAJOR_SYNC_OFFSET..], offset + MLP_MAJOR_SYNC_OFFSET as u64)?;
            compare_repeated_major_sync(&inspection.first_major_sync, info, offset + MLP_MAJOR_SYNC_OFFSET as u64)?;
            known_num_substreams = Some(info.num_substreams);
            inspection.major_sync_frame_count = inspection.major_sync_frame_count.saturating_add(1);
            if inspection.first_major_sync.is_none() {
                inspection.first_major_sync = Some(info);
            }
        } else if let Some(num_substreams) = known_num_substreams {
            validate_mlp_frame_parity(&frame, num_substreams, offset)?;
        }

        offset = offset.saturating_add(frame_len as u64);
    }

    if inspection.frame_count == 0 {
        return Err(MlpInspectError::Empty);
    }

    let major_sync = inspection.first_major_sync.ok_or(MlpInspectError::NoMajorSync)?;
    validate_expected_facts(major_sync, expectation)?;
    Ok(inspection)
}

fn read_exact_count<R: Read>(reader: &mut R, mut out: &mut [u8]) -> Result<usize, MlpInspectError> {
    let mut total = 0_usize;
    while !out.is_empty() {
        match reader.read(out) {
            Ok(0) => return Ok(total),
            Ok(n) => {
                total += n;
                let rest = out;
                out = &mut rest[n..];
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(MlpInspectError::Io(err)),
        }
    }
    Ok(total)
}

fn mlp_frame_length(header: [u8; 2], offset: u64) -> Result<usize, MlpInspectError> {
    let raw = u16::from_be_bytes(header);
    let frame_len = usize::from(raw & MLP_FRAME_SIZE_MASK) * 2;
    if frame_len == 0 {
        return Err(MlpInspectError::ZeroFrameLength { offset });
    }
    if frame_len < 2 {
        return Err(MlpInspectError::FrameLengthPastEof {
            offset,
            declared_bytes: frame_len,
            bytes_read: 2,
        });
    }
    Ok(frame_len)
}

fn frame_has_major_sync(frame: &[u8]) -> bool {
    match frame.get(MLP_MAJOR_SYNC_OFFSET..MLP_MAJOR_SYNC_OFFSET + 4) {
        Some(sync) => sync[..3] == MLP_SYNC_PREFIX && (sync[3] & 0xFE) == TRUEHD_STREAM_TYPE,
        None => false,
    }
}

fn parse_mlp_major_sync(bytes: &[u8], offset: u64) -> Result<MlpMajorSyncInfo, MlpInspectError> {
    if bytes.len() < MLP_MAJOR_SYNC_MIN_BYTES {
        return Err(MlpInspectError::MajorSyncTooShort {
            offset,
            available: bytes.len(),
        });
    }
    if bytes[..3] != MLP_SYNC_PREFIX {
        return Err(MlpInspectError::UnsupportedMajorSyncValue {
            offset,
            field: "sync",
            value: u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]),
        });
    }

    let stream_type = bytes[3];
    if stream_type != MLP_STREAM_TYPE {
        return Err(MlpInspectError::UnexpectedStreamType { offset, stream_type });
    }

    let mut bits = BitReader::new(bytes);
    bits.skip(32)
        .ok_or(MlpInspectError::MajorSyncBitRead { offset, field: "sync" })?;
    let group1_quant = bits
        .read(4)
        .ok_or(MlpInspectError::MajorSyncBitRead { offset, field: "group1_bits" })?;
    let group2_quant = bits
        .read(4)
        .ok_or(MlpInspectError::MajorSyncBitRead { offset, field: "group2_bits" })?;
    let group1_bits = MLP_QUANTS[group1_quant as usize];
    let group2_bits = MLP_QUANTS[group2_quant as usize];
    if group1_bits == 0 {
        return Err(MlpInspectError::UnsupportedMajorSyncValue {
            offset,
            field: "group1_bits",
            value: group1_quant,
        });
    }

    let group1_rate_bits = bits
        .read(4)
        .ok_or(MlpInspectError::MajorSyncBitRead { offset, field: "group1_samplerate" })?;
    let group2_rate_bits = bits
        .read(4)
        .ok_or(MlpInspectError::MajorSyncBitRead { offset, field: "group2_samplerate" })?;
    let group1_sample_rate = mlp_sample_rate(group1_rate_bits);
    let group2_sample_rate = mlp_sample_rate(group2_rate_bits);
    if group1_sample_rate == 0 {
        return Err(MlpInspectError::UnsupportedMajorSyncValue {
            offset,
            field: "group1_samplerate",
            value: group1_rate_bits,
        });
    }

    bits.skip(11).ok_or(MlpInspectError::MajorSyncBitRead {
        offset,
        field: "reserved",
    })?;
    let channel_arrangement = bits
        .read(5)
        .ok_or(MlpInspectError::MajorSyncBitRead { offset, field: "channel_arrangement" })?;
    let channel_layout = u8::try_from(channel_arrangement)
        .ok()
        .and_then(layout_for_assignment_code)
        .ok_or(MlpInspectError::UnsupportedMajorSyncValue {
            offset,
            field: "channel_arrangement",
            value: channel_arrangement,
        })?;
    let channel_count = channel_layout.total_channel_count();
    if channel_layout.group2_channel_count() > 0 {
        if group2_bits == 0 {
            return Err(MlpInspectError::UnsupportedMajorSyncValue {
                offset,
                field: "group2_bits",
                value: group2_quant,
            });
        }
        if group2_sample_rate == 0 {
            return Err(MlpInspectError::UnsupportedMajorSyncValue {
                offset,
                field: "group2_samplerate",
                value: group2_rate_bits,
            });
        }
    }

    let access_unit_size = 40_u32 << (group1_rate_bits & 7);
    bits.skip(48).ok_or(MlpInspectError::MajorSyncBitRead {
        offset,
        field: "constant fields",
    })?;
    let is_vbr = bits
        .read(1)
        .ok_or(MlpInspectError::MajorSyncBitRead { offset, field: "is_vbr" })?
        != 0;
    let peak_bitrate_raw = bits
        .read(15)
        .ok_or(MlpInspectError::MajorSyncBitRead { offset, field: "peak_bitrate" })?;
    let peak_bitrate = ((peak_bitrate_raw * group1_sample_rate) + 8) >> 4;
    let num_substreams = bits
        .read(4)
        .ok_or(MlpInspectError::MajorSyncBitRead { offset, field: "num_substreams" })?;
    if num_substreams == 0 {
        return Err(MlpInspectError::UnsupportedMajorSyncValue {
            offset,
            field: "num_substreams",
            value: num_substreams,
        });
    }

    Ok(MlpMajorSyncInfo {
        stream_type,
        group1_bits,
        group2_bits,
        group1_sample_rate,
        group2_sample_rate,
        channel_arrangement,
        channel_count,
        access_unit_size,
        is_vbr,
        peak_bitrate,
        num_substreams,
    })
}


#[allow(dead_code)]
fn mlp_channel_count_for_arrangement(channel_arrangement: u32) -> Option<u32> {
    let code = u8::try_from(channel_arrangement).ok()?;
    layout_for_assignment_code(code).map(|layout| layout.total_channel_count())
}

fn compare_repeated_major_sync(
    first: &Option<MlpMajorSyncInfo>,
    later: MlpMajorSyncInfo,
    offset: u64,
) -> Result<(), MlpInspectError> {
    let Some(first) = first else {
        return Ok(());
    };

    compare_major_sync_field(offset, "group1_bits", first.group1_bits, later.group1_bits)?;
    compare_major_sync_field(
        offset,
        "group1_samplerate",
        first.group1_sample_rate,
        later.group1_sample_rate,
    )?;
    compare_major_sync_field(
        offset,
        "channel_arrangement",
        first.channel_arrangement,
        later.channel_arrangement,
    )?;
    compare_major_sync_field(offset, "channels", first.channel_count, later.channel_count)?;

    // Group 2 rate/depth fields are only meaningful when the channel
    // arrangement includes a second channel group. Some one-group streams carry
    // placeholder values in the group 2 major-sync slots; do not make those
    // ignored placeholders part of the validation contract.
    if major_sync_has_group2_channels(*first) || major_sync_has_group2_channels(later) {
        compare_major_sync_field(offset, "group2_bits", first.group2_bits, later.group2_bits)?;
        compare_major_sync_field(
            offset,
            "group2_samplerate",
            first.group2_sample_rate,
            later.group2_sample_rate,
        )?;
    }

    compare_major_sync_field(offset, "num_substreams", first.num_substreams, later.num_substreams)
}

fn major_sync_has_group2_channels(info: MlpMajorSyncInfo) -> bool {
    u8::try_from(info.channel_arrangement)
        .ok()
        .and_then(layout_for_assignment_code)
        .map_or(false, |layout| layout.group2_channel_count() > 0)
}

fn compare_major_sync_field(
    offset: u64,
    field: &'static str,
    first: u32,
    later: u32,
) -> Result<(), MlpInspectError> {
    if first == later {
        Ok(())
    } else {
        Err(MlpInspectError::MajorSyncChanged {
            offset,
            field,
            first,
            later,
        })
    }
}

fn validate_mlp_frame_parity(
    frame: &[u8],
    num_substreams: u32,
    offset: u64,
) -> Result<(), MlpInspectError> {
    let mut parity = 0_u8;
    let mut pos = 0_usize;
    for idx in 0..=num_substreams {
        if pos + 2 > frame.len() {
            return Err(MlpInspectError::FrameLengthPastEof {
                offset,
                declared_bytes: pos + 2,
                bytes_read: frame.len(),
            });
        }
        parity ^= frame[pos];
        parity ^= frame[pos + 1];
        let has_extended_header = idx == 0 || (frame[pos] & 0x80) != 0;
        pos += 2;
        if has_extended_header {
            if pos + 2 > frame.len() {
                return Err(MlpInspectError::FrameLengthPastEof {
                    offset,
                    declared_bytes: pos + 2,
                    bytes_read: frame.len(),
                });
            }
            parity ^= frame[pos];
            parity ^= frame[pos + 1];
            pos += 2;
        }
    }

    if (((parity >> 4) ^ parity) & 0x0F) == 0x0F {
        Ok(())
    } else {
        Err(MlpInspectError::ParityCheckFailed { offset })
    }
}

fn validate_expected_facts(
    info: MlpMajorSyncInfo,
    expectation: MlpStreamExpectation,
) -> Result<(), MlpInspectError> {
    warn_if_expected_fact_differs(
        "sample rate",
        expectation.sample_rate,
        Some(info.group1_sample_rate),
        "IFO expected {expected} Hz, MLP major-sync reports {actual} Hz; using MLP major-sync",
    );
    warn_if_expected_fact_differs(
        "channel count",
        expectation.channel_count,
        Some(info.channel_count),
        "IFO expected {expected} channels, MLP major-sync reports {actual}; using MLP major-sync",
    );
    warn_if_expected_fact_differs(
        "bit depth",
        expectation.bit_depth,
        Some(info.group1_bits),
        "IFO expected {expected}-bit, MLP major-sync reports {actual}-bit; using MLP major-sync",
    );
    warn_if_expected_fact_differs(
        "group 1 sample rate",
        expectation.group1_sample_rate,
        Some(info.group1_sample_rate),
        "IFO expected group 1 {expected} Hz, MLP major-sync reports {actual} Hz; using MLP major-sync",
    );
    warn_if_expected_fact_differs(
        "group 2 sample rate",
        expectation.group2_sample_rate,
        Some(info.group2_sample_rate),
        "IFO expected group 2 {expected} Hz, MLP major-sync reports {actual} Hz; using MLP major-sync",
    );
    warn_if_expected_fact_differs(
        "group 1 bit depth",
        expectation.group1_bit_depth,
        Some(info.group1_bits),
        "IFO expected group 1 {expected}-bit, MLP major-sync reports {actual}-bit; using MLP major-sync",
    );
    warn_if_expected_fact_differs(
        "group 2 bit depth",
        expectation.group2_bit_depth,
        Some(info.group2_bits),
        "IFO expected group 2 {expected}-bit, MLP major-sync reports {actual}-bit; using MLP major-sync",
    );

    Ok(())
}

fn warn_if_expected_fact_differs(
    field: &'static str,
    expected: Option<u32>,
    actual: Option<u32>,
    detail: &'static str,
) {
    let (Some(expected), Some(actual)) = (expected, actual) else {
        return;
    };
    if expected == actual {
        return;
    }

    log::warn!(
        "DVD-Audio MLP {field} metadata mismatch: {}",
        detail
            .replace("{expected}", &expected.to_string())
            .replace("{actual}", &actual.to_string())
    );
}

fn mlp_sample_rate(value: u32) -> u32 {
    if value == 0x0F {
        0
    } else if (value & 8) != 0 {
        44_100_u32 << (value & 7)
    } else {
        48_000_u32 << (value & 7)
    }
}

struct BitReader<'a> {
    bytes: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, bit_pos: 0 }
    }

    fn read(&mut self, count: usize) -> Option<u32> {
        if count > 32 || self.bit_pos.checked_add(count)? > self.bytes.len() * 8 {
            return None;
        }
        let mut value = 0_u32;
        for _ in 0..count {
            let byte = self.bytes[self.bit_pos / 8];
            let shift = 7 - (self.bit_pos % 8);
            value = (value << 1) | u32::from((byte >> shift) & 1);
            self.bit_pos += 1;
        }
        Some(value)
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        if self.bit_pos.checked_add(count)? > self.bytes.len() * 8 {
            return None;
        }
        self.bit_pos += count;
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::dvda_demux::{
        demux_private_stream_1_packets, DvdaDemuxStats, DvdaSubstreamKind, DVD_SECTOR_SIZE,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn inspects_major_sync_frame_and_validates_expected_facts() {
        let path = write_temp_mlp(&[major_sync_frame(40, 2, 2, 1, 1)]);

        let inspection = inspect_mlp_file(
            &path,
            MlpStreamExpectation {
                sample_rate: Some(192_000),
                channel_count: Some(2),
                bit_depth: Some(24),
                group1_sample_rate: Some(192_000),
                group1_bit_depth: Some(24),
                ..MlpStreamExpectation::default()
            },
        )
        .expect("MLP frame inspection should pass");

        assert_eq!(inspection.frame_count, 1);
        assert_eq!(inspection.major_sync_frame_count, 1);
        let info = inspection.first_major_sync.expect("major sync info");
        assert_eq!(info.group1_sample_rate, 192_000);
        assert_eq!(info.channel_count, 2);
        assert_eq!(info.group1_bits, 24);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn accepts_expected_sample_rate_mismatch_as_advisory() {
        let path = write_temp_mlp(&[major_sync_frame(40, 2, 2, 1, 1)]);

        let inspection = inspect_mlp_file(
            &path,
            MlpStreamExpectation {
                sample_rate: Some(96_000),
                channel_count: Some(2),
                bit_depth: Some(24),
                ..MlpStreamExpectation::default()
            },
        )
        .expect("IFO/MLP sample-rate mismatch should be advisory");

        assert_eq!(
            inspection
                .first_major_sync
                .expect("major sync info")
                .group1_sample_rate,
            192_000
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn accepts_expected_group_bit_depth_mismatch_as_advisory() {
        let path = write_temp_mlp(&[major_sync_frame(40, 2, 2, 1, 1)]);

        let inspection = inspect_mlp_file(
            &path,
            MlpStreamExpectation {
                group1_bit_depth: Some(20),
                ..MlpStreamExpectation::default()
            },
        )
        .expect("IFO/MLP group bit-depth mismatch should be advisory");

        assert_eq!(
            inspection
                .first_major_sync
                .expect("major sync info")
                .group1_bits,
            24
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_invalid_group2_bit_depth_when_arrangement_has_group2_channels() {
        let path = write_temp_mlp(&[major_sync_frame_with_group2(40, 2, 2, 3, 0, 2, 1)]);

        let err = inspect_mlp_file(&path, MlpStreamExpectation::default())
            .expect_err("invalid group 2 bit depth should fail when group 2 channels exist");

        assert!(
            matches!(
                err,
                MlpInspectError::UnsupportedMajorSyncValue {
                    field: "group2_bits",
                    value: 3,
                    ..
                }
            ),
            "unexpected error: {err:?}"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_invalid_group2_sample_rate_when_arrangement_has_group2_channels() {
        let path = write_temp_mlp(&[major_sync_frame_with_group2(40, 2, 2, 1, 15, 2, 1)]);

        let err = inspect_mlp_file(&path, MlpStreamExpectation::default())
            .expect_err("invalid group 2 sample rate should fail when group 2 channels exist");

        assert!(
            matches!(
                err,
                MlpInspectError::UnsupportedMajorSyncValue {
                    field: "group2_samplerate",
                    value: 15,
                    ..
                }
            ),
            "unexpected error: {err:?}"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn ignores_invalid_group2_placeholders_when_arrangement_has_one_group() {
        let path = write_temp_mlp(&[major_sync_frame_with_group2(40, 2, 2, 3, 15, 1, 1)]);

        let inspection = inspect_mlp_file(&path, MlpStreamExpectation::default())
            .expect("group 2 placeholders should be ignored for one-group arrangements");

        assert_eq!(inspection.first_major_sync.expect("major sync").channel_arrangement, 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_repeated_major_sync_group2_sample_rate_change() {
        let path = write_temp_mlp(&[
            major_sync_frame_with_group2(40, 2, 2, 1, 0, 2, 1),
            major_sync_frame_with_group2(40, 2, 2, 1, 1, 2, 1),
        ]);

        let err = inspect_mlp_file(&path, MlpStreamExpectation::default())
            .expect_err("changed group 2 rate should fail consistency validation");

        assert!(
            matches!(
                err,
                MlpInspectError::MajorSyncChanged {
                    field: "group2_samplerate",
                    first: 48_000,
                    later: 96_000,
                    ..
                }
            ),
            "unexpected error: {err:?}"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_repeated_major_sync_group2_bit_depth_change() {
        let path = write_temp_mlp(&[
            major_sync_frame_with_group2(40, 2, 2, 1, 0, 2, 1),
            major_sync_frame_with_group2(40, 2, 2, 0, 0, 2, 1),
        ]);

        let err = inspect_mlp_file(&path, MlpStreamExpectation::default())
            .expect_err("changed group 2 bit depth should fail consistency validation");

        assert!(
            matches!(
                err,
                MlpInspectError::MajorSyncChanged {
                    field: "group2_bits",
                    first: 20,
                    later: 16,
                    ..
                }
            ),
            "unexpected error: {err:?}"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_repeated_major_sync_channel_arrangement_change_even_when_total_channels_match() {
        let path = write_temp_mlp(&[
            major_sync_frame_with_group2(40, 2, 2, 1, 0, 2, 1),
            major_sync_frame_with_group2(40, 2, 2, 1, 0, 4, 1),
        ]);

        let err = inspect_mlp_file(&path, MlpStreamExpectation::default())
            .expect_err("changed channel arrangement should fail consistency validation");

        assert!(
            matches!(
                err,
                MlpInspectError::MajorSyncChanged {
                    field: "channel_arrangement",
                    first: 2,
                    later: 4,
                    ..
                }
            ),
            "unexpected error: {err:?}"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_frame_length_past_eof() {
        let mut frame = major_sync_frame(40, 2, 2, 1, 1);
        frame.truncate(12);
        let path = write_temp_mlp(&[frame]);

        let err = inspect_mlp_file(&path, MlpStreamExpectation::default())
            .expect_err("truncated frame should fail");

        assert!(matches!(err, MlpInspectError::FrameLengthPastEof { .. }));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_truehd_major_sync_in_dvd_audio_mlp_payload() {
        let mut frame = major_sync_frame(40, 2, 2, 1, 1);
        frame[7] = TRUEHD_STREAM_TYPE;
        let path = write_temp_mlp(&[frame]);

        let err = inspect_mlp_file(&path, MlpStreamExpectation::default())
            .expect_err("TrueHD stream type should fail for DVD-Audio MLP extraction");

        assert!(matches!(err, MlpInspectError::UnexpectedStreamType { .. }));
        let _ = fs::remove_file(path);
    }


    #[test]
    fn real_aob_fixtures_validate_mlp_access_unit_parsing() {
        struct FixtureExpectation {
            name: &'static str,
            sample_rate: u32,
            channels: u32,
            arrangement: u32,
            frames: u64,
            major_sync_frames: u64,
            trailing_partial_bytes: usize,
        }

        let expectations = [
            FixtureExpectation { name: "ap_eye_in_the_sky_first_16_sectors.bin", sample_rate: 192_000, channels: 2, arrangement: 1, frames: 62, major_sync_frames: 8, trailing_partial_bytes: 179 },
            FixtureExpectation { name: "ap_friendly_card_first_16_sectors.bin", sample_rate: 192_000, channels: 2, arrangement: 1, frames: 61, major_sync_frames: 8, trailing_partial_bytes: 481 },
            FixtureExpectation { name: "ap_i_robot_first_16_sectors.bin", sample_rate: 192_000, channels: 2, arrangement: 1, frames: 60, major_sync_frames: 8, trailing_partial_bytes: 19 },
            FixtureExpectation { name: "hdad2009_first_16_sectors.bin", sample_rate: 192_000, channels: 2, arrangement: 1, frames: 58, major_sync_frames: 8, trailing_partial_bytes: 473 },
            FixtureExpectation { name: "hawks_and_doves_first_16_sectors.bin", sample_rate: 176_400, channels: 2, arrangement: 1, frames: 100, major_sync_frames: 13, trailing_partial_bytes: 245 },
            FixtureExpectation { name: "mgletsgetiton_first_16_sectors.bin", sample_rate: 96_000, channels: 5, arrangement: 19, frames: 49, major_sync_frames: 2, trailing_partial_bytes: 549 },
            FixtureExpectation { name: "talking_heads_77_first_16_sectors.bin", sample_rate: 96_000, channels: 6, arrangement: 20, frames: 427, major_sync_frames: 54, trailing_partial_bytes: 215 },
        ];

        for fixture in expectations {
            let payload = demux_fixture_payload(fixture.name);
            let inspection = inspect_mlp_bytes_with_options(
                &payload,
                MlpStreamExpectation {
                    sample_rate: Some(fixture.sample_rate),
                    channel_count: Some(fixture.channels),
                    bit_depth: Some(24),
                    group1_sample_rate: Some(fixture.sample_rate),
                    group1_bit_depth: Some(24),
                    ..MlpStreamExpectation::default()
                },
                MlpInspectOptions { allow_trailing_partial_frame: true },
            )
            .unwrap_or_else(|err| panic!("{} prefix inspection failed: {err}", fixture.name));

            assert_eq!(inspection.frame_count, fixture.frames, "{} complete frame count", fixture.name);
            assert_eq!(inspection.major_sync_frame_count, fixture.major_sync_frames, "{} major-sync count", fixture.name);
            assert_eq!(
                inspection.trailing_partial_frame_bytes,
                Some(fixture.trailing_partial_bytes),
                "{} expected a truncated final access unit because fixtures are fixed 16-sector windows",
                fixture.name
            );

            let info = inspection.first_major_sync.expect("major sync info");
            assert_eq!(info.stream_type, MLP_STREAM_TYPE, "{} stream type", fixture.name);
            assert_eq!(info.group1_bits, 24, "{} group 1 depth", fixture.name);
            assert_eq!(info.group1_sample_rate, fixture.sample_rate, "{} sample rate", fixture.name);
            assert_eq!(info.channel_count, fixture.channels, "{} channel count", fixture.name);
            assert_eq!(info.channel_arrangement, fixture.arrangement, "{} channel arrangement", fixture.name);
        }
    }

    #[test]
    fn strict_full_payload_inspection_rejects_truncated_sector_fixture_windows() {
        let payload = demux_fixture_payload("ap_eye_in_the_sky_first_16_sectors.bin");
        let err = inspect_mlp_bytes_with_options(
            &payload,
            MlpStreamExpectation::default(),
            MlpInspectOptions::default(),
        )
        .expect_err("fixed 16-sector windows should not be accepted as complete MLP files");

        assert!(matches!(err, MlpInspectError::FrameLengthPastEof { .. }));
    }

    #[test]
    fn dvd_audio_mlp_channel_arrangements_match_foo_input_dvda_reference_counts() {
        // foo_input_dvda audio_stream_info_t::mlppcm_table has 21 DVD-Audio
        // MLP/PCM channel-arrangement entries. Keep the MLP major-sync mapping
        // tied to the shared DVD-A layout table rather than a separate count table.
        let expected_group_counts = [
            (1, 0),
            (2, 0),
            (2, 1),
            (2, 2),
            (2, 1),
            (2, 2),
            (2, 3),
            (2, 1),
            (2, 2),
            (2, 3),
            (2, 2),
            (2, 3),
            (2, 4),
            (3, 1),
            (3, 2),
            (3, 1),
            (3, 2),
            (3, 3),
            (4, 1),
            (4, 1),
            (4, 2),
        ];

        for (code, (group1, group2)) in expected_group_counts.iter().copied().enumerate() {
            let layout = layout_for_assignment_code(code as u8).expect("valid DVD-A MLP/PCM assignment");
            assert_eq!(layout.group1_channel_count(), group1, "group 1 channels for assignment {code}");
            assert_eq!(layout.group2_channel_count(), group2, "group 2 channels for assignment {code}");
            assert_eq!(
                mlp_channel_count_for_arrangement(code as u32),
                Some(group1 + group2),
                "MLP major-sync channel count for assignment {code}"
            );
        }
    }

    #[test]
    fn rejects_reserved_dvd_audio_mlp_channel_arrangements() {
        for arrangement in 21..32 {
            let path = write_temp_mlp(&[major_sync_frame(40, 2, 2, arrangement, 1)]);
            let err = inspect_mlp_file(&path, MlpStreamExpectation::default())
                .expect_err("reserved channel arrangement should fail");
            assert!(
                matches!(
                    err,
                    MlpInspectError::UnsupportedMajorSyncValue {
                        field: "channel_arrangement",
                        value,
                        ..
                    } if value == arrangement
                ),
                "unexpected error for arrangement {arrangement}: {err:?}"
            );
            let _ = fs::remove_file(path);
        }
    }


    fn demux_fixture_payload(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dvda_aob_samples")
            .join(name);
        let bytes = fs::read(&path).unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let mut payload = Vec::new();
        let mut stats = DvdaDemuxStats::default();
        for sector in bytes.chunks_exact(DVD_SECTOR_SIZE) {
            demux_private_stream_1_packets(sector, &mut stats, |packet| {
                assert_eq!(packet.sub_header.kind(), DvdaSubstreamKind::Mlp, "{name} should contain only MLP");
                payload.extend_from_slice(packet.payload);
                Ok(())
            })
            .unwrap_or_else(|err| panic!("{name} failed to demux: {err}"));
        }
        assert_eq!(stats.mlp_packets, 16, "{name} MLP packet count");
        payload
    }

    fn major_sync_frame(
        frame_len: usize,
        group1_quant: u32,
        group1_rate_bits: u32,
        channel_arrangement: u32,
        num_substreams: u32,
    ) -> Vec<u8> {
        assert!(frame_len >= 40 && frame_len % 2 == 0);
        let mut frame = vec![0_u8; frame_len];
        let words = (frame_len / 2) as u16;
        frame[..2].copy_from_slice(&(words & MLP_FRAME_SIZE_MASK).to_be_bytes());
        frame[MLP_MAJOR_SYNC_OFFSET..MLP_MAJOR_SYNC_OFFSET + 4]
            .copy_from_slice(&[0xF8, 0x72, 0x6F, MLP_STREAM_TYPE]);
        set_bits(&mut frame[MLP_MAJOR_SYNC_OFFSET..], 32, 4, group1_quant);
        set_bits(&mut frame[MLP_MAJOR_SYNC_OFFSET..], 36, 4, 0);
        set_bits(&mut frame[MLP_MAJOR_SYNC_OFFSET..], 40, 4, group1_rate_bits);
        set_bits(&mut frame[MLP_MAJOR_SYNC_OFFSET..], 44, 4, 0);
        set_bits(&mut frame[MLP_MAJOR_SYNC_OFFSET..], 59, 5, channel_arrangement);
        set_bits(&mut frame[MLP_MAJOR_SYNC_OFFSET..], 112, 1, 1);
        set_bits(&mut frame[MLP_MAJOR_SYNC_OFFSET..], 113, 15, 256);
        set_bits(&mut frame[MLP_MAJOR_SYNC_OFFSET..], 128, 4, num_substreams);
        frame
    }

    fn major_sync_frame_with_group2(
        frame_len: usize,
        group1_quant: u32,
        group1_rate_bits: u32,
        group2_quant: u32,
        group2_rate_bits: u32,
        channel_arrangement: u32,
        num_substreams: u32,
    ) -> Vec<u8> {
        let mut frame = major_sync_frame(
            frame_len,
            group1_quant,
            group1_rate_bits,
            channel_arrangement,
            num_substreams,
        );
        set_bits(&mut frame[MLP_MAJOR_SYNC_OFFSET..], 36, 4, group2_quant);
        set_bits(&mut frame[MLP_MAJOR_SYNC_OFFSET..], 44, 4, group2_rate_bits);
        frame
    }

    fn set_bits(bytes: &mut [u8], bit_pos: usize, count: usize, value: u32) {
        for i in 0..count {
            let bit = ((value >> (count - 1 - i)) & 1) as u8;
            let pos = bit_pos + i;
            let byte = pos / 8;
            let shift = 7 - (pos % 8);
            bytes[byte] &= !(1_u8 << shift);
            bytes[byte] |= bit << shift;
        }
    }

    fn write_temp_mlp(frames: &[Vec<u8>]) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "dvda_mlp_inspect_{}_{}.mlp",
            std::process::id(),
            unique
        ));
        let mut bytes = Vec::new();
        for frame in frames {
            bytes.extend_from_slice(frame);
        }
        fs::write(&path, bytes).expect("write MLP test fixture");
        path
    }
}
