//! Independent exact Wave64 structural validation.
//!
//! The validator deliberately does not delegate container authority to an audio
//! decoder. It reads only the Wave64 root, chunk headers, format metadata, fact
//! metadata, and alignment padding; audio payload bytes are never buffered.

use std::fmt;
use std::io::{Read, Seek, SeekFrom};

const W64_RIFF_GUID: [u8; 16] = *b"riff.\x91\xcf\x11\xa5\xd6\x28\xdb\x04\xc1\0\0";
const W64_WAVE_GUID: [u8; 16] = *b"wave\xf3\xac\xd3\x11\x8c\xd1\0\xc0O\x8e\xdb\x8a";
const W64_FMT_GUID: [u8; 16] = *b"fmt \xf3\xac\xd3\x11\x8c\xd1\0\xc0O\x8e\xdb\x8a";
const W64_FACT_GUID: [u8; 16] = *b"fact\xf3\xac\xd3\x11\x8c\xd1\0\xc0O\x8e\xdb\x8a";
const W64_DATA_GUID: [u8; 16] = *b"data\xf3\xac\xd3\x11\x8c\xd1\0\xc0O\x8e\xdb\x8a";
const KSDATAFORMAT_SUBTYPE_PCM: [u8; 16] = [
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00,
    0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: [u8; 16] = [
    0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00,
    0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];
const ROOT_HEADER_BYTES: u64 = 40;
const CHUNK_HEADER_BYTES: u64 = 24;

/// PCM representation expected in an exact Wave64 carrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum W64SampleEncoding {
    /// Signed linear PCM.
    SignedInteger,
    /// IEEE floating-point PCM.
    FloatingPoint,
}

impl W64SampleEncoding {
    /// Stable evidence key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::SignedInteger => "signed_integer",
            Self::FloatingPoint => "floating_point",
        }
    }
}

/// Exact PCM facts supplied by the planner or an independent source authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct W64PcmExpectation {
    /// Samples per second.
    pub sample_rate_hz: u32,
    /// Interleaved channel count.
    pub channels: u16,
    /// Stored bits per sample.
    pub bits_per_sample: u16,
    /// Exact samples per channel.
    pub sample_frames: u64,
    /// Integer or floating-point encoding.
    pub encoding: W64SampleEncoding,
}

/// PCM format facts that can be validated before an exact frame authority exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct W64PcmFormatExpectation {
    /// Samples per second.
    pub sample_rate_hz: u32,
    /// Interleaved channel count.
    pub channels: u16,
    /// Stored bits per sample.
    pub bits_per_sample: u16,
    /// Integer or floating-point encoding.
    pub encoding: W64SampleEncoding,
}

impl From<W64PcmExpectation> for W64PcmFormatExpectation {
    fn from(expected: W64PcmExpectation) -> Self {
        Self {
            sample_rate_hz: expected.sample_rate_hz,
            channels: expected.channels,
            bits_per_sample: expected.bits_per_sample,
            encoding: expected.encoding,
        }
    }
}

/// Independently parsed exact Wave64 structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct W64ExactStructure {
    /// Physical file length.
    pub physical_file_bytes: u64,
    /// Root-declared file extent.
    pub declared_file_bytes: u64,
    /// Number of traversed chunks.
    pub chunk_count: u32,
    /// Byte offset of the unique format chunk.
    pub format_chunk_offset: u64,
    /// Byte offset of the optional/required fact chunk.
    pub fact_chunk_offset: Option<u64>,
    /// Byte offset of the unique data chunk.
    pub data_chunk_offset: u64,
    /// Declared data payload bytes, excluding the 24-byte chunk header.
    pub declared_data_bytes: u64,
    /// Exact sample frames derived from the data extent and block alignment.
    pub sample_frames: u64,
    /// Total validated zero alignment padding bytes.
    pub alignment_padding_bytes: u64,
}

/// Exact Wave64 validation failure.
#[derive(Debug)]
pub enum W64ValidationError {
    /// I/O failed while reading structural metadata.
    Io(std::io::Error),
    /// Structural or semantic invariant failed.
    Invalid(String),
}

impl W64ValidationError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl fmt::Display for W64ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "Wave64 structural I/O failed: {error}"),
            Self::Invalid(message) => write!(formatter, "Wave64 structural validation failed: {message}"),
        }
    }
}

impl std::error::Error for W64ValidationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

impl From<std::io::Error> for W64ValidationError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone, Copy)]
struct ParsedFormat {
    encoding: W64SampleEncoding,
    channels: u16,
    sample_rate_hz: u32,
    byte_rate: u32,
    block_align: u16,
    bits_per_sample: u16,
    valid_bits_per_sample: u16,
    channel_mask: Option<u32>,
}

fn read_exact_at<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    buffer: &mut [u8],
) -> Result<(), W64ValidationError> {
    reader.seek(SeekFrom::Start(offset))?;
    reader.read_exact(buffer)?;
    Ok(())
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

fn checked_add(left: u64, right: u64, description: &str) -> Result<u64, W64ValidationError> {
    left.checked_add(right)
        .ok_or_else(|| W64ValidationError::invalid(format!("{description} overflowed u64")))
}

fn align_up_8(value: u64) -> Result<u64, W64ValidationError> {
    checked_add(value, 7, "8-byte alignment").map(|value| value & !7)
}

fn parse_format<R: Read + Seek>(
    reader: &mut R,
    payload_offset: u64,
    payload_bytes: u64,
) -> Result<ParsedFormat, W64ValidationError> {
    if payload_bytes < 16 {
        return Err(W64ValidationError::invalid(format!(
            "format chunk payload is {payload_bytes} bytes; at least 16 are required"
        )));
    }
    let read_len = usize::try_from(payload_bytes.min(40))
        .map_err(|_| W64ValidationError::invalid("format chunk length is not addressable"))?;
    let mut bytes = vec![0_u8; read_len];
    read_exact_at(reader, payload_offset, &mut bytes)?;

    let format_tag = le_u16(&bytes[0..2]);
    let channels = le_u16(&bytes[2..4]);
    let sample_rate_hz = le_u32(&bytes[4..8]);
    let byte_rate = le_u32(&bytes[8..12]);
    let block_align = le_u16(&bytes[12..14]);
    let bits_per_sample = le_u16(&bytes[14..16]);
    let (encoding, valid_bits_per_sample, channel_mask) = match format_tag {
        0x0001 => {
            if payload_bytes != 16 {
                return Err(W64ValidationError::invalid(format!(
                    "PCM format chunk payload is {payload_bytes} bytes; exactly 16 are required"
                )));
            }
            (W64SampleEncoding::SignedInteger, bits_per_sample, None)
        }
        0x0003 => {
            if payload_bytes != 16 {
                return Err(W64ValidationError::invalid(format!(
                    "IEEE-float format chunk payload is {payload_bytes} bytes; exactly 16 are required"
                )));
            }
            (W64SampleEncoding::FloatingPoint, bits_per_sample, None)
        }
        0xfffe => {
            if payload_bytes != 40 || bytes.len() < 40 {
                return Err(W64ValidationError::invalid(format!(
                    "WAVEFORMATEXTENSIBLE payload is {payload_bytes} bytes; exactly 40 are required"
                )));
            }
            let extension_bytes = le_u16(&bytes[16..18]);
            if extension_bytes != 22 {
                return Err(W64ValidationError::invalid(format!(
                    "WAVEFORMATEXTENSIBLE cbSize is {extension_bytes}; exactly 22 are required"
                )));
            }
            let valid_bits_per_sample = le_u16(&bytes[18..20]);
            if valid_bits_per_sample == 0 || valid_bits_per_sample > bits_per_sample {
                return Err(W64ValidationError::invalid(format!(
                    "WAVEFORMATEXTENSIBLE valid bits are {valid_bits_per_sample}, incompatible with {bits_per_sample} stored bits"
                )));
            }
            let channel_mask = le_u32(&bytes[20..24]);
            if channel_mask != 0 && channel_mask.count_ones() != u32::from(channels) {
                return Err(W64ValidationError::invalid(format!(
                    "WAVEFORMATEXTENSIBLE channel mask 0x{channel_mask:08x} has {} speakers for {channels} channels",
                    channel_mask.count_ones()
                )));
            }
            let subformat: [u8; 16] = bytes[24..40]
                .try_into()
                .map_err(|_| W64ValidationError::invalid("missing extensible subformat GUID"))?;
            let encoding = if subformat == KSDATAFORMAT_SUBTYPE_PCM {
                W64SampleEncoding::SignedInteger
            } else if subformat == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                W64SampleEncoding::FloatingPoint
            } else {
                return Err(W64ValidationError::invalid(format!(
                    "unsupported WAVEFORMATEXTENSIBLE subformat GUID {:02x?}",
                    subformat
                )));
            };
            (encoding, valid_bits_per_sample, Some(channel_mask))
        }
        other => {
            return Err(W64ValidationError::invalid(format!(
                "unsupported WAVE format tag 0x{other:04x}"
            )));
        }
    };

    Ok(ParsedFormat {
        encoding,
        channels,
        sample_rate_hz,
        byte_rate,
        block_align,
        bits_per_sample,
        valid_bits_per_sample,
        channel_mask,
    })
}

/// Parse and validate an exact PCM Wave64 file.
///
/// This function requires the root-declared extent to equal the physical file
/// extent, traverses every chunk exactly, validates alignment padding, rejects
/// duplicate required chunks, validates the complete PCM format contract, and
/// derives an exact frame count from the declared data extent.
fn validate_exact_w64_pcm_inner<R: Read + Seek>(
    reader: &mut R,
    expected: W64PcmFormatExpectation,
    expected_sample_frames: Option<u64>,
) -> Result<W64ExactStructure, W64ValidationError> {
    if expected.sample_rate_hz == 0 {
        return Err(W64ValidationError::invalid("expected sample rate must be non-zero"));
    }
    if expected.channels == 0 {
        return Err(W64ValidationError::invalid("expected channel count must be non-zero"));
    }
    match expected.encoding {
        W64SampleEncoding::SignedInteger
            if !matches!(expected.bits_per_sample, 16 | 24 | 32) =>
        {
            return Err(W64ValidationError::invalid(format!(
                "unsupported signed-integer PCM width {}",
                expected.bits_per_sample
            )));
        }
        W64SampleEncoding::FloatingPoint
            if !matches!(expected.bits_per_sample, 32 | 64) =>
        {
            return Err(W64ValidationError::invalid(format!(
                "unsupported floating-point PCM width {}",
                expected.bits_per_sample
            )));
        }
        _ => {}
    }

    let physical_file_bytes = reader.seek(SeekFrom::End(0))?;
    if physical_file_bytes < ROOT_HEADER_BYTES {
        return Err(W64ValidationError::invalid(format!(
            "physical file is {physical_file_bytes} bytes; root header requires {ROOT_HEADER_BYTES}"
        )));
    }

    let mut root = [0_u8; ROOT_HEADER_BYTES as usize];
    read_exact_at(reader, 0, &mut root)?;
    if root[0..16] != W64_RIFF_GUID {
        return Err(W64ValidationError::invalid("root RIFF GUID is not Wave64 RIFF"));
    }
    let declared_file_bytes = le_u64(&root[16..24]);
    if root[24..40] != W64_WAVE_GUID {
        return Err(W64ValidationError::invalid("root form GUID is not Wave64 WAVE"));
    }
    if declared_file_bytes != physical_file_bytes {
        return Err(W64ValidationError::invalid(format!(
            "root declares {declared_file_bytes} bytes but the physical file contains {physical_file_bytes} bytes"
        )));
    }

    let bytes_per_sample = u64::from(expected.bits_per_sample)
        .checked_add(7)
        .ok_or_else(|| W64ValidationError::invalid("bits-per-sample overflow"))?
        / 8;
    if bytes_per_sample == 0 || expected.bits_per_sample % 8 != 0 {
        return Err(W64ValidationError::invalid(format!(
            "expected bits per sample {} is not a supported whole-byte PCM width",
            expected.bits_per_sample
        )));
    }
    let expected_block_align_u64 = u64::from(expected.channels)
        .checked_mul(bytes_per_sample)
        .ok_or_else(|| W64ValidationError::invalid("expected block alignment overflow"))?;
    let expected_block_align = u16::try_from(expected_block_align_u64)
        .map_err(|_| W64ValidationError::invalid("expected block alignment exceeds u16"))?;
    let expected_byte_rate_u64 = u64::from(expected.sample_rate_hz)
        .checked_mul(expected_block_align_u64)
        .ok_or_else(|| W64ValidationError::invalid("expected byte rate overflow"))?;
    let expected_byte_rate = u32::try_from(expected_byte_rate_u64)
        .map_err(|_| W64ValidationError::invalid("expected byte rate exceeds u32"))?;
    let mut offset = ROOT_HEADER_BYTES;
    let mut chunk_count = 0_u32;
    let mut format: Option<(u64, ParsedFormat)> = None;
    let mut fact: Option<(u64, u64)> = None;
    let mut data: Option<(u64, u64)> = None;
    let mut alignment_padding_bytes = 0_u64;

    while offset < declared_file_bytes {
        if offset % 8 != 0 {
            return Err(W64ValidationError::invalid(format!(
                "chunk begins at unaligned offset {offset}"
            )));
        }
        let remaining = declared_file_bytes - offset;
        if remaining < CHUNK_HEADER_BYTES {
            return Err(W64ValidationError::invalid(format!(
                "{remaining} undeclared/truncated bytes remain at offset {offset}"
            )));
        }
        let mut header = [0_u8; CHUNK_HEADER_BYTES as usize];
        read_exact_at(reader, offset, &mut header)?;
        let guid: [u8; 16] = header[0..16]
            .try_into()
            .map_err(|_| W64ValidationError::invalid("chunk GUID is incomplete"))?;
        let declared_chunk_bytes = le_u64(&header[16..24]);
        if declared_chunk_bytes < CHUNK_HEADER_BYTES {
            return Err(W64ValidationError::invalid(format!(
                "chunk at offset {offset} declares {declared_chunk_bytes} bytes; at least {CHUNK_HEADER_BYTES} are required"
            )));
        }
        let chunk_end = checked_add(offset, declared_chunk_bytes, "chunk extent")?;
        if chunk_end > declared_file_bytes {
            return Err(W64ValidationError::invalid(format!(
                "chunk at offset {offset} ends at {chunk_end}, beyond declared/physical extent {declared_file_bytes}"
            )));
        }
        let payload_offset = checked_add(offset, CHUNK_HEADER_BYTES, "chunk payload offset")?;
        let payload_bytes = declared_chunk_bytes - CHUNK_HEADER_BYTES;
        chunk_count = chunk_count
            .checked_add(1)
            .ok_or_else(|| W64ValidationError::invalid("chunk count overflow"))?;

        if guid == W64_FMT_GUID {
            if format.is_some() {
                return Err(W64ValidationError::invalid("duplicate format chunk"));
            }
            format = Some((offset, parse_format(reader, payload_offset, payload_bytes)?));
        } else if guid == W64_FACT_GUID {
            if fact.is_some() {
                return Err(W64ValidationError::invalid("duplicate fact chunk"));
            }
            if payload_bytes != 8 {
                return Err(W64ValidationError::invalid(format!(
                    "fact chunk payload is {payload_bytes} bytes; exactly 8 are required"
                )));
            }
            let mut frames = [0_u8; 8];
            read_exact_at(reader, payload_offset, &mut frames)?;
            fact = Some((offset, le_u64(&frames)));
        } else if guid == W64_DATA_GUID {
            if data.is_some() {
                return Err(W64ValidationError::invalid("duplicate data chunk"));
            }
            data = Some((offset, payload_bytes));
        }

        if chunk_end == declared_file_bytes {
            offset = chunk_end;
            break;
        }
        let next_offset = align_up_8(chunk_end)?;
        if next_offset > declared_file_bytes {
            return Err(W64ValidationError::invalid(format!(
                "alignment after chunk at offset {offset} extends beyond the declared file"
            )));
        }
        let padding_bytes = next_offset - chunk_end;
        if padding_bytes > 0 {
            let mut padding = [0_u8; 7];
            let padding_len = usize::try_from(padding_bytes)
                .map_err(|_| W64ValidationError::invalid("alignment padding is not addressable"))?;
            read_exact_at(reader, chunk_end, &mut padding[..padding_len])?;
            if padding[..padding_len].iter().any(|byte| *byte != 0) {
                return Err(W64ValidationError::invalid(format!(
                    "non-zero alignment padding follows chunk at offset {offset}"
                )));
            }
            alignment_padding_bytes = checked_add(
                alignment_padding_bytes,
                padding_bytes,
                "alignment padding total",
            )?;
        }
        offset = next_offset;
    }

    if offset != declared_file_bytes {
        return Err(W64ValidationError::invalid(format!(
            "chunk traversal ended at {offset}, not exact extent {declared_file_bytes}"
        )));
    }
    let (format_chunk_offset, format) = format
        .ok_or_else(|| W64ValidationError::invalid("missing format chunk"))?;
    let (data_chunk_offset, declared_data_bytes) = data
        .ok_or_else(|| W64ValidationError::invalid("missing data chunk"))?;

    if format.encoding != expected.encoding {
        return Err(W64ValidationError::invalid(format!(
            "encoding is {}, expected {}",
            format.encoding.key(),
            expected.encoding.key()
        )));
    }
    if format.channels != expected.channels {
        return Err(W64ValidationError::invalid(format!(
            "channel count is {}, expected {}",
            format.channels, expected.channels
        )));
    }
    if format.sample_rate_hz != expected.sample_rate_hz {
        return Err(W64ValidationError::invalid(format!(
            "sample rate is {}, expected {}",
            format.sample_rate_hz, expected.sample_rate_hz
        )));
    }
    if format.bits_per_sample != expected.bits_per_sample {
        return Err(W64ValidationError::invalid(format!(
            "bits per sample is {}, expected {}",
            format.bits_per_sample, expected.bits_per_sample
        )));
    }
    if format.valid_bits_per_sample != expected.bits_per_sample {
        return Err(W64ValidationError::invalid(format!(
            "valid bits per sample is {}, expected exact {}",
            format.valid_bits_per_sample, expected.bits_per_sample
        )));
    }
    if let Some(channel_mask) = format.channel_mask {
        if channel_mask != 0 && channel_mask.count_ones() != u32::from(expected.channels) {
            return Err(W64ValidationError::invalid(format!(
                "channel mask 0x{channel_mask:08x} disagrees with {} channels",
                expected.channels
            )));
        }
    }
    if format.block_align != expected_block_align {
        return Err(W64ValidationError::invalid(format!(
            "block alignment is {}, expected {}",
            format.block_align, expected_block_align
        )));
    }
    if format.byte_rate != expected_byte_rate {
        return Err(W64ValidationError::invalid(format!(
            "byte rate is {}, expected {}",
            format.byte_rate, expected_byte_rate
        )));
    }
    if let Some(expected_sample_frames) = expected_sample_frames {
        let expected_data_bytes = expected_sample_frames
            .checked_mul(expected_block_align_u64)
            .ok_or_else(|| W64ValidationError::invalid("expected data extent overflow"))?;
        if declared_data_bytes != expected_data_bytes {
            return Err(W64ValidationError::invalid(format!(
                "data chunk declares {declared_data_bytes} payload bytes, expected {expected_data_bytes}"
            )));
        }
    }
    if declared_data_bytes % u64::from(format.block_align) != 0 {
        return Err(W64ValidationError::invalid(format!(
            "data payload {declared_data_bytes} is not divisible by block alignment {}",
            format.block_align
        )));
    }
    let sample_frames = declared_data_bytes / u64::from(format.block_align);
    if let Some(expected_sample_frames) = expected_sample_frames {
        if sample_frames != expected_sample_frames {
            return Err(W64ValidationError::invalid(format!(
                "data extent yields {sample_frames} frames, expected {expected_sample_frames}"
            )));
        }
    }

    let fact_chunk_offset = match (expected.encoding, fact) {
        (W64SampleEncoding::FloatingPoint, None) => {
            return Err(W64ValidationError::invalid(
                "floating-point Wave64 is missing its fact chunk",
            ));
        }
        (W64SampleEncoding::FloatingPoint, Some((fact_offset, fact_frames))) => {
            if fact_frames != sample_frames {
                return Err(W64ValidationError::invalid(format!(
                    "fact chunk declares {fact_frames} frames, data extent yields {sample_frames}"
                )));
            }
            Some(fact_offset)
        }
        (W64SampleEncoding::SignedInteger, Some((fact_offset, fact_frames))) => {
            if fact_frames != sample_frames {
                return Err(W64ValidationError::invalid(format!(
                    "integer fact chunk declares {fact_frames} frames, data extent yields {sample_frames}"
                )));
            }
            Some(fact_offset)
        }
        (W64SampleEncoding::SignedInteger, None) => None,
    };

    Ok(W64ExactStructure {
        physical_file_bytes,
        declared_file_bytes,
        chunk_count,
        format_chunk_offset,
        fact_chunk_offset,
        data_chunk_offset,
        declared_data_bytes,
        sample_frames,
        alignment_padding_bytes,
    })
}

/// Inspect an exact PCM Wave64 carrier and derive its frame count from its
/// independently validated data extent.
pub fn inspect_exact_w64_pcm<R: Read + Seek>(
    reader: &mut R,
    expected: W64PcmFormatExpectation,
) -> Result<W64ExactStructure, W64ValidationError> {
    validate_exact_w64_pcm_inner(reader, expected, None)
}

/// Validate an exact PCM Wave64 carrier against an externally supplied exact
/// frame count.
pub fn validate_exact_w64_pcm<R: Read + Seek>(
    reader: &mut R,
    expected: W64PcmExpectation,
) -> Result<W64ExactStructure, W64ValidationError> {
    validate_exact_w64_pcm_inner(reader, expected.into(), Some(expected.sample_frames))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn push_chunk(file: &mut Vec<u8>, guid: [u8; 16], payload: &[u8], pad: bool) {
        file.extend_from_slice(&guid);
        file.extend_from_slice(&(CHUNK_HEADER_BYTES + payload.len() as u64).to_le_bytes());
        file.extend_from_slice(payload);
        if pad {
            while file.len() % 8 != 0 {
                file.push(0);
            }
        }
    }

    fn direct_format(expectation: W64PcmExpectation) -> Vec<u8> {
        let bytes_per_sample = u64::from(expectation.bits_per_sample / 8);
        let block_align = u64::from(expectation.channels) * bytes_per_sample;
        let byte_rate = u32::try_from(u64::from(expectation.sample_rate_hz) * block_align)
            .expect("test byte rate fits u32");
        let block_align = u16::try_from(block_align).expect("test block align fits u16");
        let mut format = Vec::with_capacity(16);
        let tag = match expectation.encoding {
            W64SampleEncoding::SignedInteger => 1_u16,
            W64SampleEncoding::FloatingPoint => 3_u16,
        };
        format.extend_from_slice(&tag.to_le_bytes());
        format.extend_from_slice(&expectation.channels.to_le_bytes());
        format.extend_from_slice(&expectation.sample_rate_hz.to_le_bytes());
        format.extend_from_slice(&byte_rate.to_le_bytes());
        format.extend_from_slice(&block_align.to_le_bytes());
        format.extend_from_slice(&expectation.bits_per_sample.to_le_bytes());
        format
    }

    fn fixture(expectation: W64PcmExpectation, fact: bool) -> Vec<u8> {
        let bytes_per_sample = u64::from(expectation.bits_per_sample / 8);
        let block_align = u64::from(expectation.channels) * bytes_per_sample;
        let format = direct_format(expectation);

        let mut file = Vec::from(W64_RIFF_GUID);
        file.extend_from_slice(&0_u64.to_le_bytes());
        file.extend_from_slice(&W64_WAVE_GUID);
        push_chunk(&mut file, W64_FMT_GUID, &format, true);
        if fact {
            push_chunk(
                &mut file,
                W64_FACT_GUID,
                &expectation.sample_frames.to_le_bytes(),
                true,
            );
        }
        let payload_bytes = usize::try_from(expectation.sample_frames * block_align)
            .expect("test payload fits usize");
        push_chunk(&mut file, W64_DATA_GUID, &vec![0_u8; payload_bytes], false);
        let file_len = file.len() as u64;
        file[16..24].copy_from_slice(&file_len.to_le_bytes());
        file
    }

    #[test]
    fn accepts_exact_integer_with_unaligned_final_data() {
        let expected = W64PcmExpectation {
            sample_rate_hz: 44_100,
            channels: 1,
            bits_per_sample: 24,
            sample_frames: 3,
            encoding: W64SampleEncoding::SignedInteger,
        };
        let bytes = fixture(expected, false);
        assert_ne!(bytes.len() % 8, 0);
        let parsed = validate_exact_w64_pcm(&mut Cursor::new(bytes), expected).unwrap();
        assert_eq!(parsed.sample_frames, 3);
        assert_eq!(parsed.declared_data_bytes, 9);
    }

    #[test]
    fn accepts_exact_float_with_fact() {
        let expected = W64PcmExpectation {
            sample_rate_hz: 88_200,
            channels: 2,
            bits_per_sample: 64,
            sample_frames: 4,
            encoding: W64SampleEncoding::FloatingPoint,
        };
        let parsed = validate_exact_w64_pcm(&mut Cursor::new(fixture(expected, true)), expected)
            .unwrap();
        assert_eq!(parsed.sample_frames, 4);
        assert!(parsed.fact_chunk_offset.is_some());
    }

    #[test]
    fn rejects_exact_frame_count_mismatch() {
        let actual = W64PcmExpectation {
            sample_rate_hz: 96_000,
            channels: 2,
            bits_per_sample: 24,
            sample_frames: 4,
            encoding: W64SampleEncoding::SignedInteger,
        };
        let expected = W64PcmExpectation {
            sample_frames: 5,
            ..actual
        };
        let error = validate_exact_w64_pcm(&mut Cursor::new(fixture(actual, false)), expected)
            .unwrap_err();
        assert!(error.to_string().contains("expected 30"));
    }

    #[test]
    fn rejects_float_without_fact_chunk() {
        let expected = W64PcmExpectation {
            sample_rate_hz: 48_000,
            channels: 1,
            bits_per_sample: 32,
            sample_frames: 4,
            encoding: W64SampleEncoding::FloatingPoint,
        };
        let error = validate_exact_w64_pcm(&mut Cursor::new(fixture(expected, false)), expected)
            .unwrap_err();
        assert!(error.to_string().contains("missing its fact chunk"));
    }

    #[test]
    fn rejects_zero_channel_expectation_without_panicking() {
        let expected = W64PcmExpectation {
            sample_rate_hz: 48_000,
            channels: 0,
            bits_per_sample: 24,
            sample_frames: 4,
            encoding: W64SampleEncoding::SignedInteger,
        };
        let error = validate_exact_w64_pcm(&mut Cursor::new(Vec::<u8>::new()), expected)
            .unwrap_err();
        assert!(error.to_string().contains("channel count must be non-zero"));
    }

    #[test]
    fn rejects_false_root_extent() {
        let expected = W64PcmExpectation {
            sample_rate_hz: 88_200,
            channels: 1,
            bits_per_sample: 64,
            sample_frames: 4,
            encoding: W64SampleEncoding::FloatingPoint,
        };
        let mut bytes = fixture(expected, true);
        bytes[16..24].copy_from_slice(&136_u64.to_le_bytes());
        let error = validate_exact_w64_pcm(&mut Cursor::new(bytes), expected).unwrap_err();
        assert!(error.to_string().contains("physical file"));
    }

    #[test]
    fn rejects_false_data_extent() {
        let expected = W64PcmExpectation {
            sample_rate_hz: 88_200,
            channels: 1,
            bits_per_sample: 32,
            sample_frames: 4,
            encoding: W64SampleEncoding::FloatingPoint,
        };
        let mut bytes = fixture(expected, true);
        let data_offset = bytes
            .windows(16)
            .position(|window| window == W64_DATA_GUID)
            .unwrap();
        bytes[data_offset + 16..data_offset + 24].copy_from_slice(&24_u64.to_le_bytes());
        let error = validate_exact_w64_pcm(&mut Cursor::new(bytes), expected).unwrap_err();
        assert!(
            error.to_string().contains("data chunk")
                || error.to_string().contains("chunk traversal")
                || error.to_string().contains("undeclared/truncated")
        );
    }

    #[test]
    fn rejects_nonzero_alignment_padding() {
        let expected = W64PcmExpectation {
            sample_rate_hz: 44_100,
            channels: 1,
            bits_per_sample: 24,
            sample_frames: 4,
            encoding: W64SampleEncoding::SignedInteger,
        };
        let original = fixture(expected, false);
        let mut bytes = original[..80].to_vec();
        let unknown_guid = *b"junk\xf3\xac\xd3\x11\x8c\xd1\0\xc0O\x8e\xdb\x8a";
        push_chunk(&mut bytes, unknown_guid, &[0], true);
        let padding_index = bytes.len() - 1;
        bytes[padding_index] = 1;
        bytes.extend_from_slice(&original[80..]);
        let file_len = bytes.len() as u64;
        bytes[16..24].copy_from_slice(&file_len.to_le_bytes());
        let error = validate_exact_w64_pcm(&mut Cursor::new(bytes), expected).unwrap_err();
        assert!(error.to_string().contains("non-zero alignment padding"));
    }

    #[test]
    fn rejects_duplicate_data_chunk() {
        let expected = W64PcmExpectation {
            sample_rate_hz: 48_000,
            channels: 1,
            bits_per_sample: 24,
            sample_frames: 1,
            encoding: W64SampleEncoding::SignedInteger,
        };
        let mut bytes = fixture(expected, false);
        while bytes.len() % 8 != 0 {
            bytes.push(0);
        }
        push_chunk(&mut bytes, W64_DATA_GUID, &[], false);
        let file_len = bytes.len() as u64;
        bytes[16..24].copy_from_slice(&file_len.to_le_bytes());
        let error = validate_exact_w64_pcm(&mut Cursor::new(bytes), expected).unwrap_err();
        assert!(error.to_string().contains("duplicate data chunk"));
    }

    #[test]
    fn rejects_undeclared_trailing_bytes() {
        let expected = W64PcmExpectation {
            sample_rate_hz: 48_000,
            channels: 1,
            bits_per_sample: 24,
            sample_frames: 1,
            encoding: W64SampleEncoding::SignedInteger,
        };
        let mut bytes = fixture(expected, false);
        bytes.push(0);
        let error = validate_exact_w64_pcm(&mut Cursor::new(bytes), expected).unwrap_err();
        assert!(error.to_string().contains("physical file"));
    }
}
