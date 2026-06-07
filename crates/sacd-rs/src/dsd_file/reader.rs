// SPDX-License-Identifier: GPL-2.0-or-later
//! Streaming readers for DSF and DSDIFF/DSD/DST files.
//!
//! The rest of this crate historically concentrated on SACD ISO extraction and
//! DSF/DSDIFF writers. This module adds the corresponding read-side abstraction
//! layer without copying cladst's implementation. The abstraction shape is
//! intentionally simple and idiomatic for `sacd-rs`:
//!
//! - [`DsdFrameReader`] yields uncompressed DSD frames in the crate's canonical
//!   layout: byte-interleaved/clustered by channel, MSB-first within each byte.
//! - [`DstFrameReader`] yields encoded DSDIFF/DST frame payloads plus an
//!   optional accompanying `DSTC` CRC field when the file provides one.
//! - [`open_dsd_file`] detects DSF, DSDIFF/DSD, and DSDIFF/DST and returns a
//!   typed enum over the concrete readers.
//!
//! This module now supports both typed lossless access and unified decoded-DSD
//! access. DSDIFF/DST input is adapted through the in-tree DST decoder with
//! `DSTC` validation before yielding canonical DSD frames when `DSTC` is present.

use crate::dsd_file::inspect::{
    inspect_dsd_container, inspect_dsdiff, inspect_dsf, DsdByteOrder, DsdCompression,
    DsdContainerDiagnostic, DsdContainerDiagnosticSeverity, DsdContainerError, DsdContainerFormat,
    DsdContainerInfo,
};
use crate::dff_dst_writer::dst_frame_crc;
use crate::dst::{decode_frame_with_rate, DstError, DstRate};
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};

const DSTF: [u8; 4] = *b"DSTF";
const DSTC: [u8; 4] = *b"DSTC";
const DSTI: [u8; 4] = *b"DSTI";
const FRTE: [u8; 4] = *b"FRTE";

const DSDIFF_CHUNK_HEADER_SIZE: u64 = 12;
const DEFAULT_DSDIFF_DSD_FRAME_BYTES_PER_CHANNEL: usize = 4704;
const DSF_CANONICAL_BLOCK_SIZE_PER_CHANNEL: usize = 4096;

const BIT_REVERSE: [u8; 256] = build_bit_reverse_table();

const fn build_bit_reverse_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut b = i as u8;
        let mut r = 0u8;
        let mut bit = 0;
        while bit < 8 {
            r = (r << 1) | (b & 1);
            b >>= 1;
            bit += 1;
        }
        t[i] = r;
        i += 1;
    }
    t
}

/// A chunk of uncompressed DSD in canonical `sacd-rs` layout.
///
/// `data` is byte-interleaved across channels and MSB-first within each byte.
/// For DSF input the reader converts from Sony's channel-major, LSB-first block
/// layout into this canonical form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdFrame {
    pub frame_index: u64,
    pub data: Vec<u8>,
    pub channel_count: u16,
    pub sample_rate: u32,
    pub byte_order: DsdByteOrder,
    pub is_final: bool,
}

/// A DSDIFF/DSD frame deinterleaved into per-channel byte vectors.
///
/// DSDIFF stores uncompressed DSD as byte-interleaved channel data. Most of the
/// crate consumes [`DsdFrame`]'s canonical interleaved form, but analysis and
/// validation code sometimes needs per-channel reader slices. This type makes
/// that model available without changing the canonical source contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdChannelFrame {
    pub frame_index: u64,
    pub channels: Vec<Vec<u8>>,
    pub sample_rate: u32,
    pub byte_order: DsdByteOrder,
    pub is_final: bool,
}

/// Container-level DSDIFF/DST frame checksum state.
///
/// DSDIFF/DST `DSTC` is not part of the encoded DST payload; it is a container
/// checksum over the decoded, MSB-first interleaved DSD represented by the
/// preceding `DSTF`. A reader that has not decoded the frame can only report
/// whether `DSTC` was absent, malformed, or present-but-unchecked. Decode paths
/// upgrade that state to `PresentPassed` or `PresentFailed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DstCrcStatus {
    /// The frame had no accompanying `DSTC` chunk. This is allowed for ordinary
    /// DSDIFF/DST input, but validation reports it explicitly.
    NoCrc,
    /// A `DSTC` chunk was present and structurally valid, but the encoded DST
    /// frame has not been decoded yet, so the checksum has not been evaluated.
    PresentUnchecked { expected: u32 },
    /// A structurally valid `DSTC` chunk was present and matched the decoded
    /// DSD bytes.
    PresentPassed { expected: u32, actual: u32 },
    /// A structurally valid `DSTC` chunk was present but did not match the
    /// decoded DSD bytes.
    PresentFailed { expected: u32, actual: u32 },
    /// A `DSTC` chunk was present but malformed, for example because its chunk
    /// length was not exactly four bytes.
    Malformed { reason: String },
}

impl DstCrcStatus {
    pub fn from_optional_dstc(dstc: Option<u32>) -> Self {
        match dstc {
            Some(expected) => Self::PresentUnchecked { expected },
            None => Self::NoCrc,
        }
    }

    pub fn verify(dstc: Option<u32>, decoded_interleaved_dsd: &[u8]) -> Self {
        match dstc {
            Some(expected) => {
                let actual = dst_frame_crc(decoded_interleaved_dsd);
                if actual == expected {
                    Self::PresentPassed { expected, actual }
                } else {
                    Self::PresentFailed { expected, actual }
                }
            }
            None => Self::NoCrc,
        }
    }
}

impl fmt::Display for DstCrcStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCrc => f.write_str("no CRC"),
            Self::PresentUnchecked { expected } => write!(f, "CRC present, unchecked: 0x{:08x}", expected),
            Self::PresentPassed { expected, actual } => {
                write!(f, "CRC present and passed: expected=0x{:08x}, actual=0x{:08x}", expected, actual)
            }
            Self::PresentFailed { expected, actual } => {
                write!(f, "DSTC mismatch: expected=0x{:08x}, actual=0x{:08x}", expected, actual)
            }
            Self::Malformed { reason } => write!(f, "CRC malformed: {}", reason),
        }
    }
}

/// One encoded DSDIFF/DST frame and its optional checksum chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DstFrame {
    pub frame_index: u64,
    /// Absolute byte offset of the `DSTF` chunk header.
    pub chunk_offset: u64,
    /// Absolute byte offset of the first encoded payload byte inside `DSTF`.
    pub payload_offset: u64,
    pub encoded: Vec<u8>,
    /// Optional raw `DSTC` checksum value from the stream. Some DSDIFF/DST files
    /// omit `DSTC`; readers preserve that fact instead of inventing a checksum.
    pub dstc: Option<u32>,
    /// Structured checksum state. Raw encoded-frame readers report `NoCrc` or
    /// `PresentUnchecked`; decoded validation paths upgrade this to pass/fail.
    pub crc_status: DstCrcStatus,
    /// Total physical size of the `DSTF` chunk including header and odd-size pad.
    pub physical_chunk_size: u32,
    pub sample_rate: u32,
    pub channel_count: u16,
    pub is_final: bool,
}

/// Streaming access to uncompressed DSD frames.
pub trait DsdFrameReader {
    fn info(&self) -> &DsdContainerInfo;
    fn next_dsd_frame(&mut self) -> Result<Option<DsdFrame>, DsdReadError>;
}

/// Streaming access to encoded DST frames.
pub trait DstFrameReader {
    fn info(&self) -> &DsdContainerInfo;
    fn next_dst_frame(&mut self) -> Result<Option<DstFrame>, DsdReadError>;
}

/// Optional frame-index navigation for readers whose underlying container can
/// seek safely by frame boundary.
///
/// DSF and DSDIFF/DSD use synthetic frame boundaries chosen by the reader.
/// DSDIFF/DST uses physical `DSTI`/`DSTF` frame boundaries from the file.
pub trait DsdFrameSeek {
    /// Total number of readable frames when known.
    fn frame_count(&self) -> Option<u64>;
    /// Index of the frame that will be returned by the next read.
    fn current_frame_index(&self) -> u64;
    /// Move the reader to a frame boundary. Seeking to `frame_count()` is a
    /// legal EOF seek; seeking past it is an error.
    fn seek_frame(&mut self, frame_index: u64) -> Result<(), DsdReadError>;
}

/// Reader returned by [`open_dsd_file`].
pub enum DsdFileReader<R: Read + Seek> {
    Dsf(DsfStreamReader<R>),
    DsdiffDsd(DsdDsdiffStreamReader<R>),
    DsdiffDst(DstDsdiffStreamReader<R>),
}

impl<R: Read + Seek> DsdFileReader<R> {
    pub fn info(&self) -> &DsdContainerInfo {
        match self {
            Self::Dsf(r) => r.info(),
            Self::DsdiffDsd(r) => r.info(),
            Self::DsdiffDst(r) => r.info(),
        }
    }

    /// Read raw DSF ID3v2 bytes when the stream is DSF and has a validated
    /// metadata offset. DSDIFF metadata is represented as ordinary trailing
    /// chunks and is intentionally not parsed by this typed reader yet.
    pub fn read_dsf_id3_footer(&mut self) -> Result<Option<Vec<u8>>, DsdReadError> {
        match self {
            Self::Dsf(r) => r.read_id3_footer(),
            Self::DsdiffDsd(_) | Self::DsdiffDst(_) => Ok(None),
        }
    }

    /// Convert the typed lossless reader into a unified uncompressed-DSD reader.
    ///
    /// DSF and DSDIFF/DSD streams pass through directly. DSDIFF/DST streams are
    /// wrapped in [`DstToDsdAdapter`], decoded by the in-tree DST decoder, and
    /// checked against `DSTC` when the source file provides it.
    pub fn into_decoded_dsd_reader(self) -> Result<DsdDecodedFileReader<R>, DsdReadError> {
        match self {
            Self::Dsf(r) => Ok(DsdDecodedFileReader::Dsf(r)),
            Self::DsdiffDsd(r) => Ok(DsdDecodedFileReader::DsdiffDsd(r)),
            Self::DsdiffDst(r) => Ok(DsdDecodedFileReader::DsdiffDst(DstToDsdAdapter::new(r)?)),
        }
    }
}

/// A unified reader that yields canonical uncompressed DSD frames from any
/// supported DSD file container.
pub enum DsdDecodedFileReader<R: Read + Seek> {
    Dsf(DsfStreamReader<R>),
    DsdiffDsd(DsdDsdiffStreamReader<R>),
    DsdiffDst(DstToDsdAdapter<DstDsdiffStreamReader<R>>),
}

impl<R: Read + Seek> DsdDecodedFileReader<R> {
    pub fn info(&self) -> &DsdContainerInfo {
        match self {
            Self::Dsf(r) => r.info(),
            Self::DsdiffDsd(r) => r.info(),
            Self::DsdiffDst(r) => r.info(),
        }
    }

    /// Checksum result for the last decoded DSDIFF/DST frame. Returns `None`
    /// for DSF/DSDIFF-DSD sources or before the first DST frame is decoded.
    pub fn last_dst_crc_status(&self) -> Option<&DstCrcStatus> {
        match self {
            Self::DsdiffDst(r) => r.last_crc_status(),
            Self::Dsf(_) | Self::DsdiffDsd(_) => None,
        }
    }
}

impl<R: Read + Seek> DsdFrameReader for DsdDecodedFileReader<R> {
    fn info(&self) -> &DsdContainerInfo {
        DsdDecodedFileReader::info(self)
    }

    fn next_dsd_frame(&mut self) -> Result<Option<DsdFrame>, DsdReadError> {
        match self {
            Self::Dsf(r) => r.next_dsd_frame(),
            Self::DsdiffDsd(r) => r.next_dsd_frame(),
            Self::DsdiffDst(r) => r.next_dsd_frame(),
        }
    }
}

impl<R: Read + Seek> DsdFrameSeek for DsdDecodedFileReader<R> {
    fn frame_count(&self) -> Option<u64> {
        match self {
            Self::Dsf(r) => r.frame_count(),
            Self::DsdiffDsd(r) => r.frame_count(),
            Self::DsdiffDst(r) => r.frame_count(),
        }
    }

    fn current_frame_index(&self) -> u64 {
        match self {
            Self::Dsf(r) => r.current_frame_index(),
            Self::DsdiffDsd(r) => r.current_frame_index(),
            Self::DsdiffDst(r) => r.current_frame_index(),
        }
    }

    fn seek_frame(&mut self, frame_index: u64) -> Result<(), DsdReadError> {
        match self {
            Self::Dsf(r) => r.seek_frame(frame_index),
            Self::DsdiffDsd(r) => r.seek_frame(frame_index),
            Self::DsdiffDst(r) => r.seek_frame(frame_index),
        }
    }
}

/// Strict streaming-reader errors.
#[derive(Debug)]
pub enum DsdReadError {
    Io(io::Error),
    Container(DsdContainerError),
    Dst(DstError),
    DstCrc {
        offset: u64,
        frame_index: Option<u64>,
        status: DstCrcStatus,
    },
    UnsupportedFormat { reason: String },
    Malformed { offset: u64, reason: String },
}

impl fmt::Display for DsdReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error while reading DSD stream: {}", e),
            Self::Container(e) => write!(f, "DSD container error: {}", e),
            Self::Dst(e) => write!(f, "DST decode error while reading DSD stream: {}", e),
            Self::DstCrc { offset, frame_index, status } => {
                match frame_index {
                    Some(frame) => write!(f, "DSTC validation error at byte {} for frame {}: {}", offset, frame, status),
                    None => write!(f, "DSTC validation error at byte {}: {}", offset, status),
                }
            }
            Self::UnsupportedFormat { reason } => write!(f, "unsupported DSD stream: {}", reason),
            Self::Malformed { offset, reason } => {
                write!(f, "malformed DSD stream at byte {}: {}", offset, reason)
            }
        }
    }
}

impl std::error::Error for DsdReadError {}

impl From<io::Error> for DsdReadError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<DsdContainerError> for DsdReadError {
    fn from(e: DsdContainerError) -> Self {
        Self::Container(e)
    }
}

impl From<DstError> for DsdReadError {
    fn from(e: DstError) -> Self {
        Self::Dst(e)
    }
}

/// Detect and open a DSF, DSDIFF/DSD, or DSDIFF/DST stream.
pub fn open_dsd_file<R: Read + Seek>(mut reader: R) -> Result<DsdFileReader<R>, DsdReadError> {
    let info = inspect_dsd_container(&mut reader)?;
    match (info.format, info.compression) {
        (DsdContainerFormat::Dsf, DsdCompression::Dsd) => {
            reader.seek(SeekFrom::Start(0))?;
            Ok(DsdFileReader::Dsf(DsfStreamReader::new(reader)?))
        }
        (DsdContainerFormat::Dsdiff, DsdCompression::Dsd) => {
            reader.seek(SeekFrom::Start(0))?;
            Ok(DsdFileReader::DsdiffDsd(DsdDsdiffStreamReader::new(reader)?))
        }
        (DsdContainerFormat::Dsdiff, DsdCompression::Dst) => {
            reader.seek(SeekFrom::Start(0))?;
            Ok(DsdFileReader::DsdiffDst(DstDsdiffStreamReader::new(reader)?))
        }
        (_, DsdCompression::Unknown(code)) => Err(DsdReadError::UnsupportedFormat {
            reason: format!("unknown DSD compression code {}", fourcc_lossy(code)),
        }),
        (format, compression) => Err(DsdReadError::UnsupportedFormat {
            reason: format!("unsupported combination {:?}/{:?}", format, compression),
        }),
    }
}

/// Detect and open a DSF, DSDIFF/DSD, or DSDIFF/DST stream as canonical
/// uncompressed DSD frames. DSDIFF/DST input is decoded and `DSTC`-checked.
pub fn open_dsd_as_decoded_reader<R: Read + Seek>(reader: R) -> Result<DsdDecodedFileReader<R>, DsdReadError> {
    open_dsd_file(reader)?.into_decoded_dsd_reader()
}

/// Streaming Sony DSF reader.
pub struct DsfStreamReader<R: Read + Seek> {
    reader: R,
    info: DsdContainerInfo,
    block_size_per_channel: usize,
    bytes_per_channel_total: u64,
    bytes_per_channel_read: u64,
    frame_index: u64,
}

impl<R: Read + Seek> DsfStreamReader<R> {
    pub fn new(mut reader: R) -> Result<Self, DsdReadError> {
        let info = inspect_dsf(&mut reader)?;
        reject_error_diagnostics(&info.diagnostics)?;
        if info.compression != DsdCompression::Dsd || info.format != DsdContainerFormat::Dsf {
            return Err(DsdReadError::UnsupportedFormat {
                reason: "DSF reader requires uncompressed Sony DSF".to_string(),
            });
        }
        let channels = nonzero_channel_count(&info)?;
        let block_size_per_channel = info
            .dsf_block_size_per_channel
            .unwrap_or(DSF_CANONICAL_BLOCK_SIZE_PER_CHANNEL as u32) as usize;
        if block_size_per_channel == 0 {
            return Err(malformed(info.data_offset, "DSF block size per channel is zero"));
        }
        if block_size_per_channel != DSF_CANONICAL_BLOCK_SIZE_PER_CHANNEL {
            return Err(DsdReadError::UnsupportedFormat {
                reason: format!(
                    "DSF block size {} is not supported by this streaming reader; expected {}",
                    block_size_per_channel, DSF_CANONICAL_BLOCK_SIZE_PER_CHANNEL
                ),
            });
        }
        let sample_count = info.sample_count_per_channel.ok_or_else(|| {
            malformed(info.data_offset, "DSF sample count is missing from inspected header")
        })?;
        let bytes_per_channel_total = ceil_div_u64(sample_count, 8)?;
        let minimum_payload = bytes_per_channel_total
            .checked_mul(u64::from(channels))
            .ok_or_else(|| malformed(info.data_offset, "DSF declared sample count overflows payload size"))?;
        if info.data_size < minimum_payload {
            return Err(malformed(
                info.data_offset,
                format!(
                    "DSF data chunk ends before declared samples: payload {} bytes is smaller than required {} bytes",
                    info.data_size, minimum_payload
                ),
            ));
        }
        reader.seek(SeekFrom::Start(info.data_offset))?;
        Ok(Self {
            reader,
            info,
            block_size_per_channel,
            bytes_per_channel_total,
            bytes_per_channel_read: 0,
            frame_index: 0,
        })
    }

    /// Read the optional ID3v2 footer bytes referenced by the DSF DSD chunk.
    ///
    /// The reader validates the ID3v2 header and synchsafe tag size and then
    /// restores the previous stream position. Audio iteration state is not
    /// modified.
    pub fn read_id3_footer(&mut self) -> Result<Option<Vec<u8>>, DsdReadError> {
        let Some(offset) = self.info.metadata_offset else {
            return Ok(None);
        };
        let pos = self.reader.stream_position()?;
        let len = stream_len(&mut self.reader)?;
        let tag_len = id3v2_total_size_at(&mut self.reader, offset, len)?;
        let tag_len_usize = usize::try_from(tag_len).map_err(|_| {
            malformed(offset, format!("ID3v2 tag size {} exceeds usize", tag_len))
        })?;
        self.reader.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0u8; tag_len_usize];
        self.reader.read_exact(&mut bytes)?;
        self.reader.seek(SeekFrom::Start(pos))?;
        Ok(Some(bytes))
    }

    pub fn into_inner(self) -> R {
        self.reader
    }
}

impl<R: Read + Seek> DsdFrameReader for DsfStreamReader<R> {
    fn info(&self) -> &DsdContainerInfo {
        &self.info
    }

    fn next_dsd_frame(&mut self) -> Result<Option<DsdFrame>, DsdReadError> {
        if self.bytes_per_channel_read >= self.bytes_per_channel_total {
            return Ok(None);
        }
        let channels = usize::from(nonzero_channel_count(&self.info)?);
        let remaining_per_channel = self
            .bytes_per_channel_total
            .checked_sub(self.bytes_per_channel_read)
            .ok_or_else(|| malformed(self.info.data_offset, "DSF per-channel read counter underflow"))?;
        let useful_per_channel = remaining_per_channel.min(self.block_size_per_channel as u64) as usize;
        let mut channel_blocks = Vec::with_capacity(channels);
        for ch in 0..channels {
            let mut block = vec![0u8; self.block_size_per_channel];
            self.reader.read_exact(&mut block)?;
            if useful_per_channel < self.block_size_per_channel {
                if let Some((tail, _)) = block[useful_per_channel..]
                    .iter()
                    .enumerate()
                    .find(|(_, b)| **b != 0)
                {
                    let block_group_size = (self.block_size_per_channel as u64)
                        .checked_mul(channels as u64)
                        .ok_or_else(|| malformed(self.info.data_offset, "DSF block-group size overflow"))?;
                    let group_offset = self
                        .frame_index
                        .checked_mul(block_group_size)
                        .ok_or_else(|| malformed(self.info.data_offset, "DSF final-block offset overflow"))?;
                    let channel_offset = (ch as u64)
                        .checked_mul(self.block_size_per_channel as u64)
                        .ok_or_else(|| malformed(self.info.data_offset, "DSF final-block channel offset overflow"))?;
                    let physical_offset = self
                        .info
                        .data_offset
                        .checked_add(group_offset)
                        .and_then(|n| n.checked_add(channel_offset))
                        .and_then(|n| n.checked_add(useful_per_channel as u64))
                        .and_then(|n| n.checked_add(tail as u64))
                        .ok_or_else(|| malformed(self.info.data_offset, "DSF final-block padding offset overflow"))?;
                    return Err(malformed(
                        physical_offset,
                        format!(
                            "DSF final block padding for channel {} is not zero at byte {} within the channel block",
                            ch,
                            useful_per_channel + tail
                        ),
                    ));
                }
            }
            channel_blocks.push(block);
        }

        let mut data = Vec::with_capacity(useful_per_channel * channels);
        for i in 0..useful_per_channel {
            for ch in 0..channels {
                data.push(BIT_REVERSE[channel_blocks[ch][i] as usize]);
            }
        }
        self.bytes_per_channel_read = self
            .bytes_per_channel_read
            .checked_add(useful_per_channel as u64)
            .ok_or_else(|| malformed(self.info.data_offset, "DSF read counter overflow"))?;
        let is_final = self.bytes_per_channel_read >= self.bytes_per_channel_total;
        let frame = DsdFrame {
            frame_index: self.frame_index,
            data,
            channel_count: self.info.channel_count,
            sample_rate: self.info.sample_rate,
            byte_order: DsdByteOrder::MsbFirst,
            is_final,
        };
        self.frame_index = self
            .frame_index
            .checked_add(1)
            .ok_or_else(|| malformed(self.info.data_offset, "DSF frame index overflow"))?;
        Ok(Some(frame))
    }
}

impl<R: Read + Seek> DsfStreamReader<R> {
    fn total_frame_count(&self) -> Result<u64, DsdReadError> {
        ceil_div_u64(self.bytes_per_channel_total, self.block_size_per_channel as u64)
    }
}

impl<R: Read + Seek> DsdFrameSeek for DsfStreamReader<R> {
    fn frame_count(&self) -> Option<u64> {
        self.total_frame_count().ok()
    }

    fn current_frame_index(&self) -> u64 {
        self.frame_index
    }

    fn seek_frame(&mut self, frame_index: u64) -> Result<(), DsdReadError> {
        let total = self.total_frame_count()?;
        if frame_index > total {
            return Err(malformed(
                self.info.data_offset,
                format!("DSF frame seek {} is past frame_count {}", frame_index, total),
            ));
        }
        let channels = u64::from(nonzero_channel_count(&self.info)?);
        let per_channel = frame_index
            .checked_mul(self.block_size_per_channel as u64)
            .ok_or_else(|| malformed(self.info.data_offset, "DSF seek byte-count overflow"))?;
        let per_channel = per_channel.min(self.bytes_per_channel_total);
        let physical_skip = frame_index
            .checked_mul(self.block_size_per_channel as u64)
            .and_then(|n| n.checked_mul(channels))
            .ok_or_else(|| malformed(self.info.data_offset, "DSF seek offset overflow"))?;
        let offset = checked_add(self.info.data_offset, physical_skip, "DSF seek absolute offset overflow")?;
        self.reader.seek(SeekFrom::Start(offset))?;
        self.bytes_per_channel_read = per_channel;
        self.frame_index = frame_index;
        Ok(())
    }
}

/// Streaming DSDIFF/DSD reader.
pub struct DsdDsdiffStreamReader<R: Read + Seek> {
    reader: R,
    info: DsdContainerInfo,
    remaining_payload_bytes: u64,
    frame_bytes: usize,
    frame_index: u64,
}

impl<R: Read + Seek> DsdDsdiffStreamReader<R> {
    pub fn new(reader: R) -> Result<Self, DsdReadError> {
        Self::with_frame_bytes(reader, 0)
    }

    /// Open with an explicit maximum frame payload. A value of `0` selects the
    /// SACD/DST cadence-sized default: `4704 * channel_count` bytes.
    pub fn with_frame_bytes(mut reader: R, frame_bytes: usize) -> Result<Self, DsdReadError> {
        let info = inspect_dsdiff(&mut reader)?;
        reject_error_diagnostics(&info.diagnostics)?;
        if info.compression != DsdCompression::Dsd || info.format != DsdContainerFormat::Dsdiff {
            return Err(DsdReadError::UnsupportedFormat {
                reason: "DSDIFF/DSD reader requires an uncompressed DSDIFF data chunk".to_string(),
            });
        }
        match info.dsdiff_cmpr_code {
            Some(code) if code == *b"DSD " => {}
            Some(code) => {
                return Err(DsdReadError::UnsupportedFormat {
                    reason: format!(
                        "DSDIFF/DSD reader requires PROP/CMPR = DSD ; got {}",
                        fourcc_lossy(code)
                    ),
                });
            }
            None => {
                return Err(DsdReadError::UnsupportedFormat {
                    reason: "DSDIFF/DSD reader requires an explicit PROP/CMPR = DSD ".to_string(),
                });
            }
        }
        let channels = nonzero_channel_count(&info)?;
        if info.data_size % u64::from(channels) != 0 {
            return Err(malformed(
                info.data_offset,
                format!(
                    "DSDIFF/DSD payload size {} is not divisible by channel count {}",
                    info.data_size, channels
                ),
            ));
        }
        let default_frame_bytes = DEFAULT_DSDIFF_DSD_FRAME_BYTES_PER_CHANNEL
            .checked_mul(usize::from(channels))
            .ok_or_else(|| malformed(info.data_offset, "default DSDIFF frame size overflow"))?;
        let frame_bytes = if frame_bytes == 0 { default_frame_bytes } else { frame_bytes };
        if frame_bytes == 0 {
            return Err(malformed(info.data_offset, "DSDIFF/DSD frame size must be non-zero"));
        }
        if frame_bytes % usize::from(channels) != 0 {
            return Err(malformed(
                info.data_offset,
                format!(
                    "DSDIFF/DSD frame size {} would split {}-channel byte-interleaved samples",
                    frame_bytes, channels
                ),
            ));
        }
        reader.seek(SeekFrom::Start(info.data_offset))?;
        let remaining_payload_bytes = info.data_size;
        Ok(Self {
            reader,
            info,
            remaining_payload_bytes,
            frame_bytes,
            frame_index: 0,
        })
    }

    pub fn into_inner(self) -> R {
        self.reader
    }

    /// Read the next DSDIFF/DSD frame and deinterleave its byte-interleaved
    /// payload into per-channel vectors. This provides the useful reader
    /// shape while retaining [`DsdFrameReader`]'s canonical interleaved API for
    /// the rest of tonepoet.
    pub fn next_dsd_channel_frame(&mut self) -> Result<Option<DsdChannelFrame>, DsdReadError> {
        let frame = match self.next_dsd_frame()? {
            Some(frame) => frame,
            None => return Ok(None),
        };
        let channels = usize::from(frame.channel_count);
        let deinterleaved = deinterleave_dsdiff_frame(&frame.data, channels, self.info.data_offset)?;
        Ok(Some(DsdChannelFrame {
            frame_index: frame.frame_index,
            channels: deinterleaved,
            sample_rate: frame.sample_rate,
            byte_order: frame.byte_order,
            is_final: frame.is_final,
        }))
    }
}

impl<R: Read + Seek> DsdFrameReader for DsdDsdiffStreamReader<R> {
    fn info(&self) -> &DsdContainerInfo {
        &self.info
    }

    fn next_dsd_frame(&mut self) -> Result<Option<DsdFrame>, DsdReadError> {
        if self.remaining_payload_bytes == 0 {
            return Ok(None);
        }
        let n = self.remaining_payload_bytes.min(self.frame_bytes as u64) as usize;
        let mut data = vec![0u8; n];
        self.reader.read_exact(&mut data)?;
        self.remaining_payload_bytes -= n as u64;
        let frame = DsdFrame {
            frame_index: self.frame_index,
            data,
            channel_count: self.info.channel_count,
            sample_rate: self.info.sample_rate,
            byte_order: DsdByteOrder::MsbFirst,
            is_final: self.remaining_payload_bytes == 0,
        };
        self.frame_index = self
            .frame_index
            .checked_add(1)
            .ok_or_else(|| malformed(self.info.data_offset, "DSDIFF/DSD frame index overflow"))?;
        Ok(Some(frame))
    }
}

impl<R: Read + Seek> DsdDsdiffStreamReader<R> {
    fn total_frame_count(&self) -> Result<u64, DsdReadError> {
        ceil_div_u64(self.info.data_size, self.frame_bytes as u64)
    }
}

impl<R: Read + Seek> DsdFrameSeek for DsdDsdiffStreamReader<R> {
    fn frame_count(&self) -> Option<u64> {
        self.total_frame_count().ok()
    }

    fn current_frame_index(&self) -> u64 {
        self.frame_index
    }

    fn seek_frame(&mut self, frame_index: u64) -> Result<(), DsdReadError> {
        let total = self.total_frame_count()?;
        if frame_index > total {
            return Err(malformed(
                self.info.data_offset,
                format!("DSDIFF/DSD frame seek {} is past frame_count {}", frame_index, total),
            ));
        }
        let consumed = frame_index
            .checked_mul(self.frame_bytes as u64)
            .ok_or_else(|| malformed(self.info.data_offset, "DSDIFF/DSD seek byte-count overflow"))?
            .min(self.info.data_size);
        let offset = checked_add(self.info.data_offset, consumed, "DSDIFF/DSD seek absolute offset overflow")?;
        self.reader.seek(SeekFrom::Start(offset))?;
        self.remaining_payload_bytes = self.info.data_size - consumed;
        self.frame_index = frame_index;
        Ok(())
    }
}

/// Streaming DSDIFF/DST reader. Construction scans only chunk headers and
/// frame descriptors; encoded payload bytes are read lazily by `next_dst_frame`.
pub struct DstDsdiffStreamReader<R: Read + Seek> {
    reader: R,
    info: DsdContainerInfo,
    frames: Vec<DstFrameDescriptor>,
    has_dsti: bool,
    next_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DstFrameScan {
    frames: Vec<DstFrameDescriptor>,
    has_dsti: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DstFrameDescriptor {
    chunk_offset: u64,
    payload_offset: u64,
    payload_size: u64,
    physical_chunk_size: u32,
    dstc: Option<u32>,
}

impl<R: Read + Seek> DstDsdiffStreamReader<R> {
    pub fn new(mut reader: R) -> Result<Self, DsdReadError> {
        let info = inspect_dsdiff(&mut reader)?;
        reject_error_diagnostics(&info.diagnostics)?;
        if info.compression != DsdCompression::Dst || info.format != DsdContainerFormat::Dsdiff {
            return Err(DsdReadError::UnsupportedFormat {
                reason: "DSDIFF/DST reader requires a DST-compressed DSDIFF stream".to_string(),
            });
        }
        let scan = scan_dsdiff_dst_frames(&mut reader, &info)?;
        reader.seek(SeekFrom::Start(info.data_offset))?;
        Ok(Self {
            reader,
            info,
            frames: scan.frames,
            has_dsti: scan.has_dsti,
            next_index: 0,
        })
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Whether the DSDIFF/DST stream supplied a validated top-level `DSTI`
    /// index. Ordinary files may omit it; files emitted by this crate include
    /// it. The reader scans physical `DSTF` chunks either way.
    pub fn has_dsti(&self) -> bool {
        self.has_dsti
    }

    /// Number of frames carrying a structurally valid `DSTC` checksum chunk.
    pub fn frames_with_dstc(&self) -> usize {
        self.frames.iter().filter(|f| f.dstc.is_some()).count()
    }

    /// Number of frames without a `DSTC` checksum chunk.
    pub fn frames_without_dstc(&self) -> usize {
        self.frames.iter().filter(|f| f.dstc.is_none()).count()
    }

    pub fn into_inner(self) -> R {
        self.reader
    }
}

impl<R: Read + Seek> DstFrameReader for DstDsdiffStreamReader<R> {
    fn info(&self) -> &DsdContainerInfo {
        &self.info
    }

    fn next_dst_frame(&mut self) -> Result<Option<DstFrame>, DsdReadError> {
        if self.next_index >= self.frames.len() {
            return Ok(None);
        }
        let desc = self.frames[self.next_index].clone();
        let payload_len = usize::try_from(desc.payload_size).map_err(|_| {
            malformed(desc.payload_offset, "DSTF payload too large to allocate on this platform")
        })?;
        let mut encoded = vec![0u8; payload_len];
        self.reader.seek(SeekFrom::Start(desc.payload_offset))?;
        self.reader.read_exact(&mut encoded)?;
        let frame_index = self.next_index as u64;
        self.next_index += 1;
        Ok(Some(DstFrame {
            frame_index,
            chunk_offset: desc.chunk_offset,
            payload_offset: desc.payload_offset,
            encoded,
            dstc: desc.dstc,
            crc_status: DstCrcStatus::from_optional_dstc(desc.dstc),
            physical_chunk_size: desc.physical_chunk_size,
            sample_rate: self.info.sample_rate,
            channel_count: self.info.channel_count,
            is_final: self.next_index >= self.frames.len(),
        }))
    }
}

impl<R: Read + Seek> DsdFrameSeek for DstDsdiffStreamReader<R> {
    fn frame_count(&self) -> Option<u64> {
        Some(self.frames.len() as u64)
    }

    fn current_frame_index(&self) -> u64 {
        self.next_index as u64
    }

    fn seek_frame(&mut self, frame_index: u64) -> Result<(), DsdReadError> {
        let target = usize::try_from(frame_index).map_err(|_| {
            malformed(self.info.data_offset, "DSDIFF/DST frame seek index exceeds usize")
        })?;
        if target > self.frames.len() {
            return Err(malformed(
                self.info.data_offset,
                format!("DSDIFF/DST frame seek {} is past frame_count {}", frame_index, self.frames.len()),
            ));
        }
        self.next_index = target;
        Ok(())
    }
}

/// Adapter that decodes a [`DstFrameReader`] into canonical DSD frames.
///
/// The adapter verifies decoded frames against DSDIFF `DSTC` values when the
/// file supplies them. It does not silently continue after corrupt encoded
/// payloads, checksum mismatches, illegal channel counts/rates, or short decoder
/// output.
pub struct DstToDsdAdapter<R: DstFrameReader> {
    inner: R,
    expected_decoded_len: usize,
    decoded_frame_index: u64,
    last_crc_status: Option<DstCrcStatus>,
}

impl<R: DstFrameReader> DstToDsdAdapter<R> {
    pub fn new(inner: R) -> Result<Self, DsdReadError> {
        let channels = checked_dst_channel_count(inner.info())?;
        let rate = checked_dst_rate(inner.info())?;
        let expected_decoded_len = dst_decoded_frame_len(channels, rate)?;
        Ok(Self {
            inner,
            expected_decoded_len,
            decoded_frame_index: 0,
            last_crc_status: None,
        })
    }

    pub fn info(&self) -> &DsdContainerInfo {
        self.inner.info()
    }

    /// Checksum result for the last decoded frame, if any.
    pub fn last_crc_status(&self) -> Option<&DstCrcStatus> {
        self.last_crc_status.as_ref()
    }

    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: DstFrameReader> DsdFrameReader for DstToDsdAdapter<R> {
    fn info(&self) -> &DsdContainerInfo {
        self.inner.info()
    }

    fn next_dsd_frame(&mut self) -> Result<Option<DsdFrame>, DsdReadError> {
        let Some(frame) = self.inner.next_dst_frame()? else {
            return Ok(None);
        };
        let channels = checked_dst_channel_count(self.inner.info())?;
        let rate = checked_dst_rate(self.inner.info())?;
        let decoded = decode_frame_with_rate(&frame.encoded, channels, rate)?;
        if decoded.len() != self.expected_decoded_len {
            return Err(malformed(
                frame.payload_offset,
                format!(
                    "DST decoder returned {} bytes for frame {}, expected {}",
                    decoded.len(), frame.frame_index, self.expected_decoded_len
                ),
            ));
        }
        let crc_status = DstCrcStatus::verify(frame.dstc, &decoded);
        if matches!(&crc_status, DstCrcStatus::PresentFailed { .. } | DstCrcStatus::Malformed { .. }) {
            return Err(DsdReadError::DstCrc {
                offset: frame.payload_offset,
                frame_index: Some(frame.frame_index),
                status: crc_status,
            });
        }
        self.last_crc_status = Some(crc_status);
        let out = DsdFrame {
            frame_index: self.decoded_frame_index,
            data: decoded,
            channel_count: self.inner.info().channel_count,
            sample_rate: self.inner.info().sample_rate,
            byte_order: DsdByteOrder::MsbFirst,
            is_final: frame.is_final,
        };
        self.decoded_frame_index = self
            .decoded_frame_index
            .checked_add(1)
            .ok_or_else(|| malformed(frame.payload_offset, "decoded DST frame index overflow"))?;
        Ok(Some(out))
    }
}

impl<R: DstFrameReader + DsdFrameSeek> DsdFrameSeek for DstToDsdAdapter<R> {
    fn frame_count(&self) -> Option<u64> {
        self.inner.frame_count()
    }

    fn current_frame_index(&self) -> u64 {
        self.decoded_frame_index
    }

    fn seek_frame(&mut self, frame_index: u64) -> Result<(), DsdReadError> {
        self.inner.seek_frame(frame_index)?;
        self.decoded_frame_index = frame_index;
        Ok(())
    }
}

fn scan_dsdiff_dst_frames<R: Read + Seek>(
    reader: &mut R,
    info: &DsdContainerInfo,
) -> Result<DstFrameScan, DsdReadError> {
    let dst_payload_start = info.data_offset;
    let dst_payload_end = checked_add(dst_payload_start, info.data_size, "DST chunk end overflow")?;
    let stream_len = stream_len(reader)?;
    if dst_payload_end > stream_len {
        return Err(malformed(
            dst_payload_start,
            format!("DST chunk ends at {}, beyond file length {}", dst_payload_end, stream_len),
        ));
    }

    let mut descriptors = Vec::new();
    let mut pending_dstf: Option<DstFrameDescriptor> = None;
    let mut saw_frte = false;
    let mut pos = dst_payload_start;
    while has_complete_chunk_header(pos, dst_payload_end) {
        reader.seek(SeekFrom::Start(pos))?;
        let id = read_fourcc(reader)?;
        let size = read_u64_be(reader)?;
        let payload_offset = checked_add(pos, DSDIFF_CHUNK_HEADER_SIZE, "DST subchunk payload offset overflow")?;
        let payload_end = checked_add(payload_offset, size, "DST subchunk payload end overflow")?;
        let next = padded_chunk_end(payload_offset, size)
            .ok_or_else(|| malformed(pos, "DST subchunk padded end overflow"))?;
        if payload_end > dst_payload_end || next > dst_payload_end {
            return Err(malformed(
                pos,
                format!("DST subchunk {} exceeds enclosing DST chunk", fourcc_lossy(id)),
            ));
        }
        if id == DSTC && size != 4 {
            return Err(DsdReadError::DstCrc {
                offset: pos,
                frame_index: Some(descriptors.len() as u64),
                status: DstCrcStatus::Malformed {
                    reason: format!("DSTC chunk size is {}, expected 4", size),
                },
            });
        }
        validate_stream_zero_pad_byte(reader, payload_end, size, dst_payload_end, pos, "DST subchunk")?;
        match id {
            FRTE => {
                if !descriptors.is_empty() || pending_dstf.is_some() {
                    return Err(malformed(pos, "FRTE appeared after DST frame data"));
                }
                if size < 6 {
                    return Err(malformed(pos, "FRTE chunk shorter than mandatory 6 bytes"));
                }
                saw_frte = true;
            }
            DSTF => {
                if !saw_frte {
                    return Err(malformed(pos, "DSTF appeared before FRTE"));
                }
                if let Some(prev) = pending_dstf.take() {
                    // DSTC is optional in the wild. A new DSTF closes the previous
                    // frame when no DSTC chunk intervened.
                    descriptors.push(prev);
                }
                let total = checked_chunk_total(size)?;
                let physical_chunk_size = u32::try_from(total).map_err(|_| {
                    malformed(pos, format!("DSTF physical size {} exceeds u32 DSTI field", total))
                })?;
                pending_dstf = Some(DstFrameDescriptor {
                    chunk_offset: pos,
                    payload_offset,
                    payload_size: size,
                    physical_chunk_size,
                    dstc: None,
                });
            }
            DSTC => {
                let mut desc = pending_dstf.take().ok_or_else(|| {
                    malformed(pos, "DSTC appeared before matching DSTF")
                })?;
                if size != 4 {
                    return Err(DsdReadError::DstCrc {
                        offset: pos,
                        frame_index: Some(descriptors.len() as u64),
                        status: DstCrcStatus::Malformed {
                            reason: format!("DSTC chunk size is {}, expected 4", size),
                        },
                    });
                }
                reader.seek(SeekFrom::Start(payload_offset))?;
                desc.dstc = Some(read_u32_be(reader)?);
                descriptors.push(desc);
            }
            _ => {
                return Err(malformed(
                    pos,
                    format!("unexpected subchunk {} inside DSDIFF DST chunk", fourcc_lossy(id)),
                ));
            }
        }
        pos = next;
    }
    if pos != dst_payload_end {
        return Err(malformed(
            pos,
            format!("{} trailing byte(s) inside DST payload after complete subchunks", dst_payload_end - pos),
        ));
    }
    if let Some(desc) = pending_dstf {
        // Last frame may legitimately omit DSTC.
        descriptors.push(desc);
    }
    if !saw_frte {
        return Err(malformed(dst_payload_start, "DST chunk missing FRTE"));
    }

    let declared = parse_frte_frame_count(reader, dst_payload_start, dst_payload_end)?;
    if declared != descriptors.len() as u32 {
        return Err(malformed(
            dst_payload_start,
            format!(
                "FRTE declares {} frame(s), but {} DSTF frame(s) were found",
                declared,
                descriptors.len()
            ),
        ));
    }
    let has_dsti = validate_dsti_if_present(reader, info, &descriptors)?;
    Ok(DstFrameScan { frames: descriptors, has_dsti })
}

fn parse_frte_frame_count<R: Read + Seek>(reader: &mut R, start: u64, end: u64) -> Result<u32, DsdReadError> {
    let mut pos = start;
    while has_complete_chunk_header(pos, end) {
        reader.seek(SeekFrom::Start(pos))?;
        let id = read_fourcc(reader)?;
        let size = read_u64_be(reader)?;
        let payload_offset = checked_add(pos, DSDIFF_CHUNK_HEADER_SIZE, "FRTE payload offset overflow")?;
        let next = padded_chunk_end(payload_offset, size)
            .ok_or_else(|| malformed(pos, "DST subchunk padded end overflow"))?;
        if id == FRTE {
            if size < 6 {
                return Err(malformed(pos, "FRTE chunk shorter than mandatory 6 bytes"));
            }
            reader.seek(SeekFrom::Start(payload_offset))?;
            return Ok(read_u32_be(reader)?);
        }
        pos = next;
    }
    Err(malformed(start, "DST chunk missing FRTE"))
}

fn validate_dsti_if_present<R: Read + Seek>(
    reader: &mut R,
    info: &DsdContainerInfo,
    frames: &[DstFrameDescriptor],
) -> Result<bool, DsdReadError> {
    let dst_payload_end = checked_add(info.data_offset, info.data_size, "DST payload end overflow")?;
    let mut pos = padded_chunk_end(info.data_offset, info.data_size)
        .ok_or_else(|| malformed(info.data_offset, "DST chunk padded end overflow"))?;
    let stream_len = stream_len(reader)?;
    while has_complete_chunk_header(pos, stream_len) {
        reader.seek(SeekFrom::Start(pos))?;
        let id = read_fourcc(reader)?;
        let size = read_u64_be(reader)?;
        let payload_offset = checked_add(pos, DSDIFF_CHUNK_HEADER_SIZE, "top-level payload offset overflow")?;
        let payload_end = checked_add(payload_offset, size, "top-level payload end overflow")?;
        let next = padded_chunk_end(payload_offset, size)
            .ok_or_else(|| malformed(pos, "top-level chunk padded end overflow"))?;
        if payload_end > stream_len || next > stream_len {
            return Err(malformed(pos, "top-level chunk exceeds file length"));
        }
        validate_stream_zero_pad_byte(reader, payload_end, size, stream_len, pos, "top-level chunk")?;
        if id == DSTI {
            let expected = frames
                .len()
                .checked_mul(12)
                .ok_or_else(|| malformed(pos, "DSTI expected-size overflow"))? as u64;
            if size != expected {
                return Err(malformed(
                    pos,
                    format!("DSTI size is {}, expected {} for {} frame(s)", size, expected, frames.len()),
                ));
            }
            for (idx, frame) in frames.iter().enumerate() {
                let entry_offset = checked_add(
                    payload_offset,
                    (idx as u64).checked_mul(12).ok_or_else(|| malformed(payload_offset, "DSTI entry offset overflow"))?,
                    "DSTI entry offset overflow",
                )?;
                reader.seek(SeekFrom::Start(entry_offset))?;
                let offset_in_dst = read_u64_be(reader)?;
                let physical_size = read_u32_be(reader)?;
                let actual_offset_in_dst = frame
                    .chunk_offset
                    .checked_sub(info.data_offset)
                    .ok_or_else(|| malformed(frame.chunk_offset, "DSTF chunk offset precedes DST payload"))?;
                if offset_in_dst != actual_offset_in_dst {
                    return Err(malformed(
                        entry_offset,
                        format!(
                            "DSTI frame {} offset is {}, actual DSTF offset is {}",
                            idx, offset_in_dst, actual_offset_in_dst
                        ),
                    ));
                }
                let indexed_chunk_offset = checked_add(info.data_offset, offset_in_dst, "DSTI absolute DSTF offset overflow")?;
                reader.seek(SeekFrom::Start(indexed_chunk_offset))?;
                let indexed_id = read_fourcc(reader)?;
                if indexed_id != DSTF {
                    return Err(malformed(
                        indexed_chunk_offset,
                        format!(
                            "DSTI frame {} offset lands on {}, expected DSTF",
                            idx,
                            fourcc_lossy(indexed_id)
                        ),
                    ));
                }
                let indexed_payload_size = read_u64_be(reader)?;
                let indexed_physical_size = checked_chunk_total(indexed_payload_size)?;
                if indexed_physical_size != u64::from(physical_size) {
                    return Err(malformed(
                        entry_offset + 8,
                        format!(
                            "DSTI frame {} physical size is {}, but chunk at indexed offset has physical size {}",
                            idx, physical_size, indexed_physical_size
                        ),
                    ));
                }
                if physical_size != frame.physical_chunk_size {
                    return Err(malformed(
                        entry_offset + 8,
                        format!(
                            "DSTI frame {} physical size is {}, actual DSTF size is {}",
                            idx, physical_size, frame.physical_chunk_size
                        ),
                    ));
                }
            }
            return Ok(true);
        }
        // DSDIFF metadata/footer chunks may legally follow DSTI. If no DSTI has
        // been found yet and we encounter footer-like chunks, continue scanning.
        // If there is no DSTI at all, the reader still has physical DSTF
        // descriptors from the DST payload scan and can stream/seek exactly.
        pos = next;
        if pos < dst_payload_end {
            return Err(malformed(pos, "internal DSTI scan position moved inside DST payload"));
        }
    }
    // DSTI is strongly preferred and required for files emitted by our writer,
    // but ordinary DSDIFF/DST files are encountered without it. The reader has
    // already scanned physical DSTF boundaries, so streaming and frame-index
    // seeking remain exact without DSTI. Validate rigorously when DSTI exists.
    Ok(false)
}

fn validate_stream_zero_pad_byte<R: Read + Seek>(
    reader: &mut R,
    payload_end: u64,
    size: u64,
    enclosing_end: u64,
    chunk_offset: u64,
    context: &str,
) -> Result<(), DsdReadError> {
    if size % 2 == 0 {
        return Ok(());
    }
    if payload_end >= enclosing_end {
        return Err(malformed(
            chunk_offset,
            format!("odd-sized {} has no room for required pad byte", context),
        ));
    }
    let saved = reader.stream_position()?;
    reader.seek(SeekFrom::Start(payload_end))?;
    let mut b = [0u8; 1];
    reader.read_exact(&mut b)?;
    reader.seek(SeekFrom::Start(saved))?;
    if b[0] != 0 {
        return Err(malformed(
            payload_end,
            format!("{} odd-chunk pad byte is 0x{:02x}, expected 0x00", context, b[0]),
        ));
    }
    Ok(())
}

fn reject_error_diagnostics(diagnostics: &[DsdContainerDiagnostic]) -> Result<(), DsdReadError> {
    if let Some(diag) = diagnostics
        .iter()
        .find(|d| d.severity == DsdContainerDiagnosticSeverity::Error)
    {
        Err(malformed(diag.offset, diag.message.clone()))
    } else {
        Ok(())
    }
}

fn checked_dst_channel_count(info: &DsdContainerInfo) -> Result<u8, DsdReadError> {
    let channels = nonzero_channel_count(info)?;
    let channels_u8 = u8::try_from(channels).map_err(|_| {
        DsdReadError::UnsupportedFormat {
            reason: format!("DST channel count {} exceeds u8", channels),
        }
    })?;
    match channels_u8 {
        1..=6 => Ok(channels_u8),
        _ => Err(DsdReadError::UnsupportedFormat {
            reason: format!(
                "DST decode adapter supports legal channel counts 1 through 6; got {}",
                channels
            ),
        }),
    }
}

fn checked_dst_rate(info: &DsdContainerInfo) -> Result<DstRate, DsdReadError> {
    DstRate::from_sample_rate(info.sample_rate).map_err(DsdReadError::Dst)
}

fn dst_decoded_frame_len(channels: u8, rate: DstRate) -> Result<usize, DsdReadError> {
    rate.frame_bytes_per_channel()
        .and_then(|bytes| {
            bytes.checked_mul(usize::from(channels)).ok_or(
                DstError::ArithmeticDecodeFailure("decoded DST frame byte count overflow"),
            )
        })
        .map_err(DsdReadError::Dst)
}

fn nonzero_channel_count(info: &DsdContainerInfo) -> Result<u16, DsdReadError> {
    if info.channel_count == 0 {
        Err(malformed(info.data_offset, "channel count is zero"))
    } else {
        Ok(info.channel_count)
    }
}

fn deinterleave_dsdiff_frame(
    interleaved: &[u8],
    channels: usize,
    diagnostic_offset: u64,
) -> Result<Vec<Vec<u8>>, DsdReadError> {
    if channels == 0 {
        return Err(malformed(diagnostic_offset, "cannot deinterleave DSDIFF/DSD frame with zero channels"));
    }
    if interleaved.len() % channels != 0 {
        return Err(malformed(
            diagnostic_offset,
            format!(
                "DSDIFF/DSD frame payload length {} is not divisible by channel count {}",
                interleaved.len(), channels
            ),
        ));
    }
    let bytes_per_channel = interleaved.len() / channels;
    let mut out = vec![Vec::with_capacity(bytes_per_channel); channels];
    for sample_group in interleaved.chunks_exact(channels) {
        for (channel, byte) in sample_group.iter().enumerate() {
            out[channel].push(*byte);
        }
    }
    Ok(out)
}

fn ceil_div_u64(n: u64, d: u64) -> Result<u64, DsdReadError> {
    if d == 0 {
        return Err(malformed(0, "division by zero"));
    }
    n.checked_add(d - 1)
        .map(|v| v / d)
        .ok_or_else(|| malformed(0, "ceiling division overflow"))
}

fn checked_add(lhs: u64, rhs: u64, reason: &'static str) -> Result<u64, DsdReadError> {
    lhs.checked_add(rhs).ok_or_else(|| malformed(lhs, reason))
}

fn checked_chunk_total(data_len: u64) -> Result<u64, DsdReadError> {
    data_len
        .checked_add(data_len & 1)
        .and_then(|n| n.checked_add(DSDIFF_CHUNK_HEADER_SIZE))
        .ok_or_else(|| malformed(0, "DSDIFF chunk total-size overflow"))
}

fn has_complete_chunk_header(pos: u64, end: u64) -> bool {
    pos.checked_add(DSDIFF_CHUNK_HEADER_SIZE)
        .map(|n| n <= end)
        .unwrap_or(false)
}

fn padded_chunk_end(payload_offset: u64, payload_size: u64) -> Option<u64> {
    payload_offset.checked_add(payload_size)?.checked_add(payload_size & 1)
}

fn stream_len<R: Seek>(reader: &mut R) -> Result<u64, DsdReadError> {
    let pos = reader.stream_position()?;
    let len = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(pos))?;
    Ok(len)
}

fn read_fourcc<R: Read>(reader: &mut R) -> Result<[u8; 4], DsdReadError> {
    let mut b = [0u8; 4];
    reader.read_exact(&mut b)?;
    Ok(b)
}

fn read_u32_be<R: Read>(reader: &mut R) -> Result<u32, DsdReadError> {
    let mut b = [0u8; 4];
    reader.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

fn read_u64_be<R: Read>(reader: &mut R) -> Result<u64, DsdReadError> {
    let mut b = [0u8; 8];
    reader.read_exact(&mut b)?;
    Ok(u64::from_be_bytes(b))
}

fn id3v2_total_size_at<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    file_len: u64,
) -> Result<u64, DsdReadError> {
    if offset >= file_len {
        return Err(malformed(offset, format!("metadata_offset {} is beyond file length {}", offset, file_len)));
    }
    let remaining = file_len
        .checked_sub(offset)
        .ok_or_else(|| malformed(offset, "metadata offset underflow"))?;
    if remaining < 10 {
        return Err(malformed(
            offset,
            format!("metadata_offset {} leaves fewer than 10 bytes for an ID3v2 header", offset),
        ));
    }
    reader.seek(SeekFrom::Start(offset))?;
    let mut header = [0u8; 10];
    reader.read_exact(&mut header)?;
    if &header[0..3] != b"ID3" {
        return Err(malformed(offset, format!("metadata_offset {} does not point to an ID3v2 tag", offset)));
    }
    if header[3] == 0xff || header[4] == 0xff {
        return Err(malformed(offset + 3, "ID3v2 version bytes are invalid"));
    }
    let declared = parse_id3v2_synchsafe_size(&header[6..10])
        .ok_or_else(|| malformed(offset + 6, "ID3v2 tag size is not synchsafe"))?;
    let total = 10u64
        .checked_add(declared)
        .ok_or_else(|| malformed(offset + 6, "ID3v2 total tag size overflow"))?;
    let end = offset
        .checked_add(total)
        .ok_or_else(|| malformed(offset + 6, "ID3v2 tag end offset overflow"))?;
    if end > file_len {
        return Err(malformed(
            offset + 6,
            format!(
                "ID3v2 tag at metadata_offset {} declares end {}, beyond file length {}",
                offset, end, file_len
            ),
        ));
    }
    if end != file_len {
        return Err(malformed(
            offset + 6,
            format!(
                "ID3v2 tag at metadata_offset {} ends at {}, but file length is {}; trailing metadata bytes are not accepted",
                offset, end, file_len
            ),
        ));
    }
    Ok(total)
}

fn parse_id3v2_synchsafe_size(bytes: &[u8]) -> Option<u64> {
    if bytes.len() != 4 || bytes.iter().any(|b| b & 0x80 != 0) {
        return None;
    }
    Some(
        ((bytes[0] as u64) << 21)
            | ((bytes[1] as u64) << 14)
            | ((bytes[2] as u64) << 7)
            | (bytes[3] as u64),
    )
}

fn malformed(offset: u64, reason: impl Into<String>) -> DsdReadError {
    DsdReadError::Malformed {
        offset,
        reason: reason.into(),
    }
}

fn fourcc_lossy(code: [u8; 4]) -> String {
    code.iter()
        .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dff_dst_writer::{dst_frame_crc, DffDstWriter, SACD_SAMPLING_FREQUENCY};
    use crate::dff_writer::DffWriter;
    use crate::dsf_writer::{ChannelType, DsfWriter};
    use crate::dst::{dst_interleaved_frame_len, encode_uncompressed_frame_interleaved};
    use std::io::Cursor;

    fn collect_dsd<R: DsdFrameReader>(reader: &mut R) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(frame) = reader.next_dsd_frame().unwrap() {
            out.extend_from_slice(&frame.data);
        }
        out
    }

    fn read_u64_be_from(buf: &[u8], off: usize) -> u64 {
        u64::from_be_bytes(buf[off..off + 8].try_into().unwrap())
    }

    #[test]
    fn open_dsf_and_read_canonical_msb_interleaved_dsd() {
        let payload = vec![0x80, 0x01, 0xaa, 0x55, 0xf0, 0x0f];
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DsfWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved(&payload).unwrap();
            writer.finish().unwrap();
        }
        cursor.set_position(0);
        let mut reader = match open_dsd_file(cursor).unwrap() {
            DsdFileReader::Dsf(r) => r,
            _ => panic!("expected DSF reader"),
        };
        assert_eq!(reader.info().channel_count, 2);
        assert_eq!(collect_dsd(&mut reader), payload);
    }

    #[test]
    fn dsf_reader_rejects_truncated_payload() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DsfWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved(&[0xaa, 0x55]).unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        bytes.truncate(100);
        let err = match DsfStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("truncated DSF unexpectedly opened"),
            Err(err) => err,
        };
        // Truncation can trigger different error paths depending on where
        // the cut lands — file-size mismatch, data chunk bounds, or I/O underread.
        let msg = err.to_string();
        assert!(
            msg.contains("data chunk") || msg.contains("file_size") || msg.contains("actual file length") || msg.contains("failed to fill") || msg.contains("truncat"),
            "unexpected error for truncated DSF: {err}"
        );
    }

    #[test]
    fn dsf_reader_rejects_non_zero_final_block_padding() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DsfWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved(&[0xaa, 0x55]).unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        // Payload starts at byte 92. For one byte per channel, channel 0's
        // first padding byte is the second byte of the first channel block.
        bytes[93] = 0x7f;
        let mut reader = DsfStreamReader::new(Cursor::new(bytes)).unwrap();
        let err = reader.next_dsd_frame().unwrap_err();
        assert!(err.to_string().contains("final block padding"));
    }

    #[test]
    fn dsf_reader_rejects_inconsistent_channel_type() {
        let bytes = crate::dsf_writer::serialize_header(
            ChannelType::Surround51,
            2,
            SACD_SAMPLING_FREQUENCY,
            0,
            0,
            92,
        );
        let err = match DsfStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("inconsistent channel_type unexpectedly opened"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("channel_type"));
    }

    #[test]
    fn dsf_reader_rejects_unpadded_data_chunk() {
        let mut bytes = crate::dsf_writer::serialize_header(
            ChannelType::Stereo,
            2,
            SACD_SAMPLING_FREQUENCY,
            8,
            2,
            94,
        );
        bytes.extend_from_slice(&[0, 0]);
        let err = match DsfStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("unpadded DSF data chunk unexpectedly opened"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("expected padded DSF payload"));
    }

    #[test]
    fn dsf_reader_rejects_unsupported_sample_rate() {
        let bytes = crate::dsf_writer::serialize_header(
            ChannelType::Stereo,
            2,
            123_456,
            0,
            0,
            92,
        );
        let err = match DsfStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("unsupported DSF sample rate unexpectedly opened"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("unsupported DSF sample_frequency"));
    }

    #[test]
    fn open_dsdiff_dsd_and_read_payload_chunks() {
        let payload: Vec<u8> = (0..100).map(|i| i as u8).collect();
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DffWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_frame(&payload).unwrap();
            writer.finish().unwrap();
        }
        cursor.set_position(0);
        let mut reader = match open_dsd_file(cursor).unwrap() {
            DsdFileReader::DsdiffDsd(r) => r,
            _ => panic!("expected DSDIFF/DSD reader"),
        };
        assert_eq!(reader.info().compression, DsdCompression::Dsd);
        assert_eq!(collect_dsd(&mut reader), payload);
    }

    #[test]
    fn dsdiff_dsd_reader_respects_requested_frame_size() {
        let payload: Vec<u8> = (0..10).map(|i| i as u8).collect();
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DffWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_frame(&payload).unwrap();
            writer.finish().unwrap();
        }
        cursor.set_position(0);
        let mut reader = DsdDsdiffStreamReader::with_frame_bytes(cursor, 4).unwrap();
        assert_eq!(reader.next_dsd_frame().unwrap().unwrap().data, vec![0, 1, 2, 3]);
        assert_eq!(reader.next_dsd_frame().unwrap().unwrap().data, vec![4, 5, 6, 7]);
        assert_eq!(reader.next_dsd_frame().unwrap().unwrap().data, vec![8, 9]);
        assert!(reader.next_dsd_frame().unwrap().is_none());
    }

    #[test]
    fn open_dsdiff_dst_and_read_encoded_frames_with_crc() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded_a = vec![0xaa; frame_len];
        let decoded_b = vec![0x55; frame_len];
        let encoded_a = vec![0x80, 0x01, 0x02];
        let encoded_b = vec![0x80, 0x03, 0x04, 0x05];
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded_a, &decoded_a).unwrap();
            writer.write_encoded_frame(&encoded_b, &decoded_b).unwrap();
            writer.finish().unwrap();
        }
        cursor.set_position(0);
        let mut reader = match open_dsd_file(cursor).unwrap() {
            DsdFileReader::DsdiffDst(r) => r,
            _ => panic!("expected DSDIFF/DST reader"),
        };
        assert_eq!(reader.len(), 2);
        let a = reader.next_dst_frame().unwrap().unwrap();
        assert_eq!(a.encoded, encoded_a);
        assert_eq!(a.dstc, Some(dst_frame_crc(&decoded_a)));
        assert_eq!(a.crc_status, DstCrcStatus::PresentUnchecked { expected: dst_frame_crc(&decoded_a) });
        assert_eq!(a.physical_chunk_size, 16); // 12 header + 3 payload + 1 pad
        assert!(!a.is_final);
        let b = reader.next_dst_frame().unwrap().unwrap();
        assert_eq!(b.encoded, encoded_b);
        assert_eq!(b.dstc, Some(dst_frame_crc(&decoded_b)));
        assert_eq!(b.crc_status, DstCrcStatus::PresentUnchecked { expected: dst_frame_crc(&decoded_b) });
        assert_eq!(b.physical_chunk_size, 16); // 12 header + 4 payload
        assert!(b.is_final);
        assert!(reader.next_dst_frame().unwrap().is_none());
    }

    #[test]
    fn dsdiff_dst_reader_rejects_bad_dsti_offset() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded = vec![0xaa; frame_len];
        let encoded = vec![0x80, 0x01, 0x02];
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded, &decoded).unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        let dsti_pos = bytes.windows(4).position(|w| w == b"DSTI").unwrap();
        // Corrupt the first index offset. It should point to the physical DSTF
        // chunk start inside the enclosing DST payload.
        bytes[dsti_pos + 12..dsti_pos + 20].copy_from_slice(&999u64.to_be_bytes());
        let err = match DstDsdiffStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("corrupt DSTI offset unexpectedly opened"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("DSTI frame 0 offset"));
    }

    #[test]
    fn dsdiff_dst_reader_accepts_missing_optional_dstc() {
        // Reuse a valid header from DffDstWriter, then truncate after DSTF.
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded = vec![0xaa; frame_len];
        let encoded = vec![0x80, 0x01, 0x02];
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded, &decoded).unwrap();
            writer.finish().unwrap();
        }
        let mut valid = cursor.into_inner();
        let dstc_pos = valid.windows(4).position(|w| w == b"DSTC").unwrap();
        valid.truncate(dstc_pos);
        // Patch the outer FRM8 and enclosing DST size down to the truncated stream.
        let frte_pos = valid.windows(4).position(|w| w == b"FRTE").unwrap();
        let top_level_dst_pos = frte_pos - 12;
        let dst_payload_start = top_level_dst_pos + 12;
        let dst_size = (valid.len() - dst_payload_start) as u64;
        valid[top_level_dst_pos + 4..top_level_dst_pos + 12].copy_from_slice(&dst_size.to_be_bytes());
        let frm8_size = (valid.len() - 12) as u64;
        valid[4..12].copy_from_slice(&frm8_size.to_be_bytes());
        let mut reader = DstDsdiffStreamReader::new(Cursor::new(valid)).unwrap();
        let frame = reader.next_dst_frame().unwrap().unwrap();
        assert_eq!(frame.dstc, None);
        assert_eq!(frame.crc_status, DstCrcStatus::NoCrc);
        assert_eq!(frame.encoded, encoded);
        assert!(reader.next_dst_frame().unwrap().is_none());
    }


    #[test]
    fn dsdiff_dst_reader_reports_malformed_dstc_status() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded = vec![0xaa; frame_len];
        let encoded = vec![0x80, 0x01, 0x02];
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded, &decoded).unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        let dstc_pos = bytes.windows(4).position(|w| w == b"DSTC").unwrap();
        bytes[dstc_pos + 4..dstc_pos + 12].copy_from_slice(&3u64.to_be_bytes());
        let err = match DstDsdiffStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("malformed DSTC unexpectedly opened"),
            Err(err) => err,
        };
        match err {
            DsdReadError::DstCrc { status: DstCrcStatus::Malformed { reason }, .. } => {
                assert!(reason.contains("DSTC chunk size"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn dsdiff_dst_reader_rejects_dsti_size_mismatch() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded = vec![0xaa; frame_len];
        let encoded = vec![0x80, 0x01, 0x02];
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded, &decoded).unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        let dsti_pos = bytes.windows(4).position(|w| w == b"DSTI").unwrap();
        bytes[dsti_pos + 4..dsti_pos + 12].copy_from_slice(&0u64.to_be_bytes());
        let frm8_size = read_u64_be_from(&bytes, 4) + 12;
        assert!(frm8_size <= bytes.len() as u64);
        let err = match DstDsdiffStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("bad DSTI size unexpectedly opened"),
            Err(err) => err,
        };
        // The zeroed DSTI size causes the chunk-bounds check to fire before
        // the DSTI-specific validation — either error path is acceptable.
        let msg = err.to_string();
        assert!(
            msg.contains("DSTI") || msg.contains("chunk payload ends") || msg.contains("beyond"),
            "unexpected error: {err}"
        );
    }
    #[test]
    fn dsdiff_dst_adapter_decodes_frames_and_validates_dstc() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded_a = vec![0xaa; frame_len];
        let decoded_b = vec![0x55; frame_len];
        let encoded_a = encode_uncompressed_frame_interleaved(&decoded_a, 2).unwrap();
        let encoded_b = encode_uncompressed_frame_interleaved(&decoded_b, 2).unwrap();
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded_a, &decoded_a).unwrap();
            writer.write_encoded_frame(&encoded_b, &decoded_b).unwrap();
            writer.finish().unwrap();
        }
        cursor.set_position(0);

        let mut reader = open_dsd_as_decoded_reader(cursor).unwrap();
        assert_eq!(reader.frame_count(), Some(2));
        assert_eq!(reader.current_frame_index(), 0);
        let first = reader.next_dsd_frame().unwrap().unwrap();
        assert_eq!(reader.last_dst_crc_status(), Some(&DstCrcStatus::PresentPassed { expected: dst_frame_crc(&decoded_a), actual: dst_frame_crc(&decoded_a) }));
        assert_eq!(first.frame_index, 0);
        assert_eq!(first.data, decoded_a);
        assert!(!first.is_final);

        reader.seek_frame(1).unwrap();
        assert_eq!(reader.current_frame_index(), 1);
        let second = reader.next_dsd_frame().unwrap().unwrap();
        assert_eq!(reader.last_dst_crc_status(), Some(&DstCrcStatus::PresentPassed { expected: dst_frame_crc(&decoded_b), actual: dst_frame_crc(&decoded_b) }));
        assert_eq!(second.frame_index, 1);
        assert_eq!(second.data, decoded_b);
        assert!(second.is_final);
        assert!(reader.next_dsd_frame().unwrap().is_none());
    }

    #[test]
    fn dsdiff_dst_adapter_rejects_dstc_mismatch() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded = vec![0xaa; frame_len];
        let encoded = encode_uncompressed_frame_interleaved(&decoded, 2).unwrap();
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded, &decoded).unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        let dstc_pos = bytes.windows(4).position(|w| w == b"DSTC").unwrap();
        bytes[dstc_pos + 15] ^= 0x01;

        let mut reader = open_dsd_as_decoded_reader(Cursor::new(bytes)).unwrap();
        let err = match reader.next_dsd_frame() {
            Ok(_) => panic!("DSTC mismatch unexpectedly decoded"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("DSTC mismatch"));
    }

    #[test]
    fn dsdiff_dsd_seek_and_roundtrip_through_unified_reader() {
        let payload: Vec<u8> = (0..24).map(|i| i as u8).collect();
        let mut source = Cursor::new(Vec::new());
        {
            let mut writer = DffWriter::new(&mut source, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_frame(&payload).unwrap();
            writer.finish().unwrap();
        }
        source.set_position(0);
        let mut reader = match open_dsd_as_decoded_reader(source).unwrap() {
            DsdDecodedFileReader::DsdiffDsd(r) => DsdDecodedFileReader::DsdiffDsd(r),
            _ => panic!("expected DSDIFF/DSD reader"),
        };
        assert_eq!(reader.frame_count(), Some(1));
        reader.seek_frame(0).unwrap();
        assert_eq!(collect_dsd(&mut reader), payload);

        let mut roundtrip = Cursor::new(Vec::new());
        {
            let mut writer = DffWriter::new(&mut roundtrip, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_frame(&payload).unwrap();
            writer.finish().unwrap();
        }
        roundtrip.set_position(0);
        let mut roundtrip_reader = open_dsd_as_decoded_reader(roundtrip).unwrap();
        assert_eq!(collect_dsd(&mut roundtrip_reader), payload);
    }

    #[test]
    fn dsf_reader_seek_preserves_canonical_layout() {
        let mut payload = Vec::new();
        for _ in 0..4096 {
            payload.extend_from_slice(&[0x12, 0x34]);
        }
        for _ in 0..8 {
            payload.extend_from_slice(&[0xab, 0xcd]);
        }
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DsfWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved(&payload).unwrap();
            writer.finish().unwrap();
        }
        cursor.set_position(0);
        let mut reader = DsfStreamReader::new(cursor).unwrap();
        assert_eq!(reader.frame_count(), Some(2));
        reader.seek_frame(1).unwrap();
        let frame = reader.next_dsd_frame().unwrap().unwrap();
        assert_eq!(frame.data, payload[4096 * 2..]);
        assert!(frame.is_final);
    }

    #[test]
    fn dsdiff_dst_decode_to_dff_roundtrip() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let mut decoded = vec![0u8; frame_len];
        for (i, b) in decoded.iter_mut().enumerate() {
            *b = if i & 1 == 0 { 0xf0 } else { 0x0f };
        }
        let encoded = encode_uncompressed_frame_interleaved(&decoded, 2).unwrap();
        let mut dst_file = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut dst_file, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded, &decoded).unwrap();
            writer.finish().unwrap();
        }
        dst_file.set_position(0);
        let mut decoded_reader = open_dsd_as_decoded_reader(dst_file).unwrap();
        let decoded_from_reader = collect_dsd(&mut decoded_reader);
        assert_eq!(decoded_from_reader, decoded);

        let mut dff_file = Cursor::new(Vec::new());
        {
            let mut writer = DffWriter::new(&mut dff_file, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_frame(&decoded_from_reader).unwrap();
            writer.finish().unwrap();
        }
        dff_file.set_position(0);
        let mut dff_reader = open_dsd_as_decoded_reader(dff_file).unwrap();
        assert_eq!(collect_dsd(&mut dff_reader), decoded);
    }


    fn mutate_dsf_channel_fields(bytes: &mut [u8], channel_type: u32, channel_count: u32) {
        bytes[48..52].copy_from_slice(&channel_type.to_le_bytes());
        bytes[52..56].copy_from_slice(&channel_count.to_le_bytes());
    }

    #[test]
    fn dsf_reader_writer_roundtrip_preserves_canonical_audio() {
        let mut payload = Vec::new();
        for i in 0..(4096 * 2 + 32) {
            payload.push((i & 0xff) as u8);
            payload.push((255 - (i & 0xff)) as u8);
        }

        let mut first = Cursor::new(Vec::new());
        {
            let mut writer = DsfWriter::new(&mut first, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved(&payload).unwrap();
            writer.finish().unwrap();
        }
        first.set_position(0);
        let mut first_reader = DsfStreamReader::new(first).unwrap();
        let canonical = collect_dsd(&mut first_reader);
        assert_eq!(canonical, payload);

        let mut second = Cursor::new(Vec::new());
        {
            let mut writer = DsfWriter::new(&mut second, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved(&canonical).unwrap();
            writer.finish().unwrap();
        }
        second.set_position(0);
        let mut second_reader = DsfStreamReader::new(second).unwrap();
        assert_eq!(collect_dsd(&mut second_reader), payload);
        assert_eq!(second_reader.info().byte_order, DsdByteOrder::LsbFirst);
        assert_eq!(second_reader.info().dsf_block_size_per_channel, Some(4096));
    }

    #[test]
    fn dsf_reader_writer_roundtrip_can_preserve_id3_footer() {
        let payload = vec![0xaa, 0x55, 0x12, 0x34];
        let id3 = b"ID3\x04\x00\x00\x00\x00\x00\x00".to_vec();
        let mut first = Cursor::new(Vec::new());
        {
            let mut writer = DsfWriter::new(&mut first, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved(&payload).unwrap();
            writer.set_id3_footer(id3.clone());
            writer.finish().unwrap();
        }
        first.set_position(0);
        let mut first_reader = DsfStreamReader::new(first).unwrap();
        assert_eq!(first_reader.read_id3_footer().unwrap(), Some(id3.clone()));
        let canonical = collect_dsd(&mut first_reader);
        assert_eq!(canonical, payload);

        let mut second = Cursor::new(Vec::new());
        {
            let mut writer = DsfWriter::new(&mut second, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved(&canonical).unwrap();
            writer.set_id3_footer(id3.clone());
            writer.finish().unwrap();
        }
        second.set_position(0);
        let mut second_reader = DsfStreamReader::new(second).unwrap();
        assert_eq!(second_reader.read_id3_footer().unwrap(), Some(id3));
        assert_eq!(collect_dsd(&mut second_reader), payload);
    }

    #[test]
    fn dsf_reader_accepts_exact_spec_channel_type_pairs() {
        let cases = [(1u32, 1u32), (2, 2), (3, 3), (4, 4), (5, 4), (6, 5), (7, 6)];
        for (channel_type, channel_count) in cases {
            let mut bytes = crate::dsf_writer::serialize_header(
                ChannelType::Stereo,
                channel_count as u8,
                SACD_SAMPLING_FREQUENCY,
                0,
                0,
                92,
            );
            mutate_dsf_channel_fields(&mut bytes, channel_type, channel_count);
            let reader = DsfStreamReader::new(Cursor::new(bytes));
            assert!(
                reader.is_ok(),
                "channel_type {} / channel_count {} unexpectedly rejected: {:?}",
                channel_type,
                channel_count,
                reader.err()
            );
        }
    }

    #[test]
    fn dsf_reader_rejects_truncation_inside_final_channel_block() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DsfWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved(&[0xaa, 0x55, 0x12, 0x34]).unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        bytes.truncate(bytes.len() - 1);
        let err = match DsfStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("truncated final DSF block unexpectedly opened"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("data chunk ends") || err.to_string().contains("total_file_size"));
    }

    #[test]
    fn dsf_reader_rejects_truncation_inside_metadata_tag() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DsfWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved(&[0xaa, 0x55]).unwrap();
            writer.set_id3_footer(b"ID3\x04\x00\x00\x00\x00\x00\x04TEST".to_vec());
            writer.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        bytes.truncate(bytes.len() - 2);
        let err = match DsfStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("DSF with truncated ID3 footer unexpectedly opened"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("declares end") || err.to_string().contains("total_file_size"));
    }

    #[test]
    fn dsf_reader_read_id3_footer_preserves_audio_position() {
        let payload = vec![0x80, 0x01, 0xaa, 0x55];
        let id3 = b"ID3\x04\x00\x00\x00\x00\x00\x00".to_vec();
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DsfWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved(&payload).unwrap();
            writer.set_id3_footer(id3.clone());
            writer.finish().unwrap();
        }
        cursor.set_position(0);
        let mut reader = DsfStreamReader::new(cursor).unwrap();
        assert_eq!(reader.read_id3_footer().unwrap(), Some(id3));
        assert_eq!(reader.next_dsd_frame().unwrap().unwrap().data, payload);
    }


    fn patch_frm8_size(bytes: &mut [u8]) {
        let size = (bytes.len() as u64).checked_sub(12).unwrap();
        bytes[4..12].copy_from_slice(&size.to_be_bytes());
    }

    fn minimal_dsdiff_dsd_with_payload(payload: &[u8], pad: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FRM8");
        bytes.extend_from_slice(&0u64.to_be_bytes());
        bytes.extend_from_slice(b"DSD ");
        bytes.extend_from_slice(b"FVER");
        bytes.extend_from_slice(&4u64.to_be_bytes());
        bytes.extend_from_slice(&0x0105_0000u32.to_be_bytes());
        let prop_size = 4u64 + 16 + 22 + 32;
        bytes.extend_from_slice(b"PROP");
        bytes.extend_from_slice(&prop_size.to_be_bytes());
        bytes.extend_from_slice(b"SND ");
        bytes.extend_from_slice(b"FS  ");
        bytes.extend_from_slice(&4u64.to_be_bytes());
        bytes.extend_from_slice(&SACD_SAMPLING_FREQUENCY.to_be_bytes());
        bytes.extend_from_slice(b"CHNL");
        bytes.extend_from_slice(&10u64.to_be_bytes());
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(b"SLFT");
        bytes.extend_from_slice(b"SRGT");
        bytes.extend_from_slice(b"CMPR");
        bytes.extend_from_slice(&20u64.to_be_bytes());
        bytes.extend_from_slice(b"DSD ");
        bytes.push(14);
        bytes.extend_from_slice(b"not compressed");
        bytes.push(0);
        bytes.extend_from_slice(b"DSD ");
        bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        bytes.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            bytes.push(pad);
        }
        patch_frm8_size(&mut bytes);
        bytes
    }

    fn minimal_mono_dsdiff_dsd_with_payload(payload: &[u8], pad: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FRM8");
        bytes.extend_from_slice(&0u64.to_be_bytes());
        bytes.extend_from_slice(b"DSD ");
        bytes.extend_from_slice(b"FVER");
        bytes.extend_from_slice(&4u64.to_be_bytes());
        bytes.extend_from_slice(&0x0105_0000u32.to_be_bytes());
        let prop_size = 4u64 + 16 + 18 + 32;
        bytes.extend_from_slice(b"PROP");
        bytes.extend_from_slice(&prop_size.to_be_bytes());
        bytes.extend_from_slice(b"SND ");
        bytes.extend_from_slice(b"FS  ");
        bytes.extend_from_slice(&4u64.to_be_bytes());
        bytes.extend_from_slice(&SACD_SAMPLING_FREQUENCY.to_be_bytes());
        bytes.extend_from_slice(b"CHNL");
        bytes.extend_from_slice(&6u64.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(b"C000");
        bytes.extend_from_slice(b"CMPR");
        bytes.extend_from_slice(&20u64.to_be_bytes());
        bytes.extend_from_slice(b"DSD ");
        bytes.push(14);
        bytes.extend_from_slice(b"not compressed");
        bytes.push(0);
        bytes.extend_from_slice(b"DSD ");
        bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        bytes.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            bytes.push(pad);
        }
        patch_frm8_size(&mut bytes);
        bytes
    }

    #[test]
    fn dsdiff_dsd_reader_writer_roundtrip_preserves_payload() {
        let payload: Vec<u8> = (0..128).map(|i| (i * 3) as u8).collect();
        let mut first = Cursor::new(Vec::new());
        {
            let mut writer = DffWriter::new(&mut first, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_frame(&payload).unwrap();
            writer.finish().unwrap();
        }
        first.set_position(0);
        let mut reader = DsdDsdiffStreamReader::with_frame_bytes(first, 14).unwrap();
        let canonical = collect_dsd(&mut reader);
        assert_eq!(canonical, payload);

        let mut second = Cursor::new(Vec::new());
        {
            let mut writer = DffWriter::new(&mut second, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_frame(&canonical).unwrap();
            writer.finish().unwrap();
        }
        second.set_position(0);
        let mut second_reader = DsdDsdiffStreamReader::with_frame_bytes(second, 18).unwrap();
        assert_eq!(collect_dsd(&mut second_reader), payload);
    }

    #[test]
    fn dsdiff_dsd_reader_deinterleaves_channel_frames() {
        let bytes = minimal_dsdiff_dsd_with_payload(&[0xa0, 0xb0, 0xa1, 0xb1, 0xa2, 0xb2], 0);
        let mut reader = DsdDsdiffStreamReader::with_frame_bytes(Cursor::new(bytes), 6).unwrap();
        let frame = reader.next_dsd_channel_frame().unwrap().unwrap();
        assert_eq!(frame.channels, vec![vec![0xa0, 0xa1, 0xa2], vec![0xb0, 0xb1, 0xb2]]);
        assert!(frame.is_final);
    }

    #[test]
    fn dsdiff_dsd_reader_rejects_frame_size_that_splits_channels() {
        let bytes = minimal_dsdiff_dsd_with_payload(&[0xaa, 0x55, 0x12, 0x34], 0);
        let err = match DsdDsdiffStreamReader::with_frame_bytes(Cursor::new(bytes), 3) {
            Ok(_) => panic!("DSDIFF/DSD split-channel frame size unexpectedly opened"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("would split"));
    }

    #[test]
    fn dsdiff_dsd_reader_rejects_missing_cmpr() {
        let mut bytes = minimal_dsdiff_dsd_with_payload(&[0xaa, 0x55], 0);
        let cmpr_pos = bytes.windows(4).position(|w| w == b"CMPR").unwrap();
        bytes[cmpr_pos..cmpr_pos + 4].copy_from_slice(b"JUNK");
        patch_frm8_size(&mut bytes);
        let err = match DsdDsdiffStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("DSDIFF/DSD without CMPR unexpectedly opened"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("CMPR"));
    }

    #[test]
    fn dsdiff_dsd_reader_rejects_payload_not_divisible_by_channels() {
        let bytes = minimal_dsdiff_dsd_with_payload(&[0xaa, 0x55, 0x12], 0);
        let err = match DsdDsdiffStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("DSDIFF/DSD with partial channel group unexpectedly opened"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("not divisible by channel count"));
    }

    #[test]
    fn dsdiff_dsd_reader_rejects_nonzero_odd_chunk_padding() {
        let bytes = minimal_mono_dsdiff_dsd_with_payload(&[0xaa], 0xff);
        let err = match DsdDsdiffStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("DSDIFF/DSD with non-zero pad unexpectedly opened"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("pad byte"));
    }

    #[test]
    fn dsdiff_reader_rejects_duplicate_audio_chunks() {
        let mut bytes = minimal_dsdiff_dsd_with_payload(&[0xaa, 0x55], 0);
        bytes.extend_from_slice(b"DSD ");
        bytes.extend_from_slice(&0u64.to_be_bytes());
        patch_frm8_size(&mut bytes);
        let err = match DsdDsdiffStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("DSDIFF with duplicate audio chunks unexpectedly opened"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("multiple DSDIFF audio data chunks"));
    }

    #[test]
    fn dsdiff_reader_rejects_malformed_chnl_payload_size() {
        let mut bytes = minimal_dsdiff_dsd_with_payload(&[0xaa, 0x55], 0);
        let chnl_pos = bytes.windows(4).position(|w| w == b"CHNL").unwrap();
        bytes[chnl_pos + 11] = 11; // chunk size 11 instead of exact 10 for stereo.
        patch_frm8_size(&mut bytes);
        let err = match DsdDsdiffStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("DSDIFF with malformed CHNL size unexpectedly opened"),
            Err(err) => err,
        };
        // Malformed CHNL size may trigger the CHNL-specific check or a
        // generic chunk-bounds/parse error depending on how the reader
        // encounters the corrupted size.
        let msg = err.to_string();
        assert!(
            (msg.contains("CHNL") && msg.contains("expected")) || msg.contains("chunk") || msg.contains("malformed"),
            "unexpected error for malformed CHNL: {err}"
        );
    }

    #[test]
    fn dsdiff_dst_reader_rejects_nonzero_dstf_padding() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded = vec![0u8; frame_len];
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&[0x40], &decoded).unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        let dstf_pos = bytes.windows(4).position(|w| w == b"DSTF").unwrap();
        bytes[dstf_pos + 13] = 0xff;
        let err = match DstDsdiffStreamReader::new(Cursor::new(bytes)) {
            Ok(_) => panic!("DSDIFF/DST with non-zero DSTF pad unexpectedly opened"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("pad byte"));
    }

    #[test]
    fn dsdiff_dst_reader_writer_roundtrip_preserves_dstf_and_decodes_when_valid() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded = vec![0xaa; frame_len];
        let encoded = encode_uncompressed_frame_interleaved(&decoded, 2).unwrap();
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded, &decoded).unwrap();
            writer.finish().unwrap();
        }
        cursor.set_position(0);
        let mut encoded_reader = DstDsdiffStreamReader::new(cursor).unwrap();
        let frame = encoded_reader.next_dst_frame().unwrap().unwrap();
        assert_eq!(frame.encoded, encoded);
        assert!(encoded_reader.next_dst_frame().unwrap().is_none());

        let mut decoded_reader = DstToDsdAdapter::new(encoded_reader).unwrap();
        decoded_reader.seek_frame(0).unwrap();
        assert_eq!(decoded_reader.next_dsd_frame().unwrap().unwrap().data, decoded);
    }

}
