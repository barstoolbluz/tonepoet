//! Read-side DSF/DSDIFF container inspection.
//!
//! The extraction path writes DSF and DSDIFF, but a production-grade SACD/DSD
//! library also needs a cheap, side-effect-free way to recognize existing DSD
//! files and validate their structural headers before routing them through a
//! conversion pipeline. This module intentionally does **not** decode audio and
//! does not allocate data-chunk payloads; it parses only container metadata and
//! bounded chunk headers.
//!
//! Scope:
//! - Sony DSF (`DSD ` + `fmt ` + `data`) header inspection.
//! - Philips DSDIFF/DSD (`FRM8` form `DSD `, `PROP/SND`, `DSD ` data chunk)
//!   header inspection.
//! - Philips DSDIFF/DST (`FRM8` form `DSD `, `PROP/SND`, `DST ` chunk with
//!   optional `FRTE`) header inspection.
//! - Structured diagnostics for recoverable header anomalies.
//!
//! This is deliberately an inspector, not a permissive streaming reader. The
//! hot extraction path remains in `extract`, and full DSDIFF DST frame walking
//! belongs in a separate streaming API once CRC policy is exposed to callers.

use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};

const ID_DSF: [u8; 4] = *b"DSD ";
const ID_FMT: [u8; 4] = *b"fmt ";
const ID_DATA: [u8; 4] = *b"data";
const ID_FRM8: [u8; 4] = *b"FRM8";
const ID_FVER: [u8; 4] = *b"FVER";
const ID_PROP: [u8; 4] = *b"PROP";
const ID_SND: [u8; 4] = *b"SND ";
const ID_FS: [u8; 4] = *b"FS  ";
const ID_CHNL: [u8; 4] = *b"CHNL";
const ID_CMPR: [u8; 4] = *b"CMPR";
const ID_DST: [u8; 4] = *b"DST ";
const ID_FRTE: [u8; 4] = *b"FRTE";

const DSF_DSD_CHUNK_SIZE: u64 = 28;
const DSF_FMT_CHUNK_SIZE: u64 = 52;
const DSF_DATA_HEADER_SIZE: u64 = 12;
const DSDIFF_CHUNK_HEADER_SIZE: u64 = 12;
const DST_FRAME_RATE: u32 = 75;

/// Container family detected by [`inspect_dsd_container`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdContainerFormat {
    /// Sony DSF.
    Dsf,
    /// Philips DSDIFF/DSD or DSDIFF/DST.
    Dsdiff,
}

/// Audio coding carried by the container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdCompression {
    /// Uncompressed one-bit DSD.
    Dsd,
    /// DST-compressed DSD.
    Dst,
    /// Unrecognized DSDIFF `CMPR` or data chunk code.
    Unknown([u8; 4]),
}

impl DsdCompression {
    fn from_dsdiff_code(code: [u8; 4]) -> Self {
        match &code {
            b"DSD " => Self::Dsd,
            b"DST " => Self::Dst,
            _ => Self::Unknown(code),
        }
    }
}

impl fmt::Display for DsdCompression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dsd => f.write_str("DSD"),
            Self::Dst => f.write_str("DST"),
            Self::Unknown(code) => write!(f, "unknown({})", fourcc_lossy(*code)),
        }
    }
}

/// Byte-level bit order in the file payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdByteOrder {
    /// Sony DSF stores one-bit samples LSB-first within each byte.
    LsbFirst,
    /// SACD sectors and DSDIFF store one-bit samples MSB-first.
    MsbFirst,
}

/// Diagnostic severity for non-fatal structural findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdContainerDiagnosticSeverity {
    Warning,
    Error,
}

/// A structured container validation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdContainerDiagnostic {
    pub severity: DsdContainerDiagnosticSeverity,
    pub offset: u64,
    pub message: String,
}

impl DsdContainerDiagnostic {
    fn warning(offset: u64, message: impl Into<String>) -> Self {
        Self {
            severity: DsdContainerDiagnosticSeverity::Warning,
            offset,
            message: message.into(),
        }
    }

    fn error(offset: u64, message: impl Into<String>) -> Self {
        Self {
            severity: DsdContainerDiagnosticSeverity::Error,
            offset,
            message: message.into(),
        }
    }
}

impl fmt::Display for DsdContainerDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} at byte {}: {}", self.severity, self.offset, self.message)
    }
}

/// Parsed DSF/DSDIFF header state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdContainerInfo {
    pub format: DsdContainerFormat,
    pub compression: DsdCompression,
    pub byte_order: DsdByteOrder,
    pub channel_count: u16,
    pub sample_rate: u32,
    /// Per-channel one-bit sample count when declared or derivable. DSF stores
    /// this directly; DSDIFF/DST can derive it from `FRTE` frame count.
    pub sample_count_per_channel: Option<u64>,
    /// Absolute byte offset of the first audio data byte, or for DSDIFF/DST the
    /// first byte inside the top-level `DST ` chunk.
    pub data_offset: u64,
    /// Declared size of the data payload at `data_offset`, excluding the
    /// chunk-header bytes that led to it.
    pub data_size: u64,
    /// DSF ID3 metadata offset when present. DSDIFF footers are ordinary
    /// chunks, so this is `None` for DSDIFF.
    pub metadata_offset: Option<u64>,
    /// DSDIFF channel IDs from `CHNL`, preserved exactly. DSF has no channel
    /// ID strings, so this is empty for DSF.
    pub channel_ids: Vec<[u8; 4]>,
    /// DSF block size per channel when known. DSDIFF does not carry this value.
    pub dsf_block_size_per_channel: Option<u32>,
    /// Raw DSDIFF `CMPR` compression code when the container is DSDIFF. This is
    /// retained so strict readers can require `CMPR = "DSD "` for uncompressed
    /// DSDIFF rather than trusting only the audio chunk ID.
    pub dsdiff_cmpr_code: Option<[u8; 4]>,
    /// Declared DSDIFF `FRM8` payload size and computed end offset when known.
    /// These are recorded for asset inspection and strict reader diagnostics.
    pub dsdiff_frm8_size: Option<u64>,
    pub dsdiff_frm8_end: Option<u64>,
    pub diagnostics: Vec<DsdContainerDiagnostic>,
}

impl DsdContainerInfo {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == DsdContainerDiagnosticSeverity::Error)
    }

    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// Duration in seconds when both sample rate and sample count are known.
    pub fn duration_seconds(&self) -> Option<f64> {
        let samples = self.sample_count_per_channel?;
        (self.sample_rate != 0).then_some(samples as f64 / self.sample_rate as f64)
    }
}

/// Fatal container inspection errors. Non-fatal anomalies are retained in
/// [`DsdContainerInfo::diagnostics`].
#[derive(Debug)]
pub enum DsdContainerError {
    Io(io::Error),
    NotDsdContainer { magic: [u8; 4] },
    Malformed { offset: u64, reason: String },
}

impl fmt::Display for DsdContainerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error while inspecting DSD container: {}", e),
            Self::NotDsdContainer { magic } => {
                write!(f, "not a DSF/DSDIFF container: first four bytes are {}", fourcc_lossy(*magic))
            }
            Self::Malformed { offset, reason } => {
                write!(f, "malformed DSD container at byte {}: {}", offset, reason)
            }
        }
    }
}

impl std::error::Error for DsdContainerError {}

impl From<io::Error> for DsdContainerError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Inspect either DSF or DSDIFF by signature. The reader is left positioned at
/// the beginning of the audio payload on success.
pub fn inspect_dsd_container<R: Read + Seek>(reader: &mut R) -> Result<DsdContainerInfo, DsdContainerError> {
    reader.seek(SeekFrom::Start(0))?;
    let magic = read_fourcc(reader)?;
    reader.seek(SeekFrom::Start(0))?;
    match magic {
        ID_DSF => inspect_dsf(reader),
        ID_FRM8 => inspect_dsdiff(reader),
        other => Err(DsdContainerError::NotDsdContainer { magic: other }),
    }
}

/// Inspect a Sony DSF stream. The reader is left positioned at the first audio
/// payload byte on success.
pub fn inspect_dsf<R: Read + Seek>(reader: &mut R) -> Result<DsdContainerInfo, DsdContainerError> {
    let file_len = stream_len(reader)?;
    reader.seek(SeekFrom::Start(0))?;
    let mut diagnostics = Vec::new();

    let dsd_id = read_fourcc(reader)?;
    if dsd_id != ID_DSF {
        return Err(malformed(0, format!("expected DSF magic 'DSD ', got {}", fourcc_lossy(dsd_id))));
    }
    let dsd_chunk_size = read_u64_le(reader)?;
    if dsd_chunk_size < DSF_DSD_CHUNK_SIZE {
        return Err(malformed(4, format!("DSD chunk too small: {} bytes", dsd_chunk_size)));
    }
    let total_file_size = read_u64_le(reader)?;
    let metadata_offset_raw = read_u64_le(reader)?;
    if dsd_chunk_size != DSF_DSD_CHUNK_SIZE {
        return Err(malformed(
            4,
            format!("DSF DSD chunk size is {}; expected exactly {}", dsd_chunk_size, DSF_DSD_CHUNK_SIZE),
        ));
    }
    if total_file_size != 0 && total_file_size != file_len {
        diagnostics.push(DsdContainerDiagnostic::error(
            12,
            format!("DSF total_file_size declares {}, actual file length is {}", total_file_size, file_len),
        ));
    }
    let metadata_offset = if metadata_offset_raw == 0 {
        None
    } else {
        if metadata_offset_raw >= file_len {
            diagnostics.push(DsdContainerDiagnostic::error(
                20,
                format!("metadata_offset {} is beyond file length {}", metadata_offset_raw, file_len),
            ));
        }
        Some(metadata_offset_raw)
    };

    reader.seek(SeekFrom::Start(dsd_chunk_size))?;
    let fmt_offset = reader.stream_position()?;
    let fmt_id = read_fourcc(reader)?;
    if fmt_id != ID_FMT {
        return Err(malformed(fmt_offset, format!("expected DSF fmt chunk, got {}", fourcc_lossy(fmt_id))));
    }
    let fmt_chunk_size = read_u64_le(reader)?;
    if fmt_chunk_size != DSF_FMT_CHUNK_SIZE {
        return Err(malformed(
            fmt_offset + 4,
            format!("DSF fmt chunk size is {}; expected exactly {}", fmt_chunk_size, DSF_FMT_CHUNK_SIZE),
        ));
    }
    let fmt_payload_len = fmt_chunk_size
        .checked_sub(12)
        .ok_or_else(|| malformed(fmt_offset + 4, "fmt chunk size underflow"))?;
    let mut fmt_payload = vec![0u8; fmt_payload_len as usize];
    reader.read_exact(&mut fmt_payload)?;
    if fmt_payload.len() < 40 {
        return Err(malformed(fmt_offset + 12, "fmt payload shorter than mandatory 40 bytes"));
    }

    let version = le_u32_at(&fmt_payload, 0);
    let format_id = le_u32_at(&fmt_payload, 4);
    let channel_type = le_u32_at(&fmt_payload, 8);
    let channel_count = le_u32_at(&fmt_payload, 12);
    let sample_rate = le_u32_at(&fmt_payload, 16);
    let bits_per_sample = le_u32_at(&fmt_payload, 20);
    let sample_count_per_channel = le_u64_at(&fmt_payload, 24);
    let block_size = le_u32_at(&fmt_payload, 32);
    let reserved = le_u32_at(&fmt_payload, 36);

    if version != 1 {
        diagnostics.push(DsdContainerDiagnostic::warning(fmt_offset + 12, format!("DSF version is {}, expected 1", version)));
    }
    if format_id != 0 {
        diagnostics.push(DsdContainerDiagnostic::error(fmt_offset + 16, format!("DSF format_id is {}, expected 0 for DSD", format_id)));
    }
    if channel_count == 0 || channel_count > u16::MAX as u32 {
        return Err(malformed(fmt_offset + 24, format!("invalid channel_count {}", channel_count)));
    }
    if !dsf_channel_type_matches(channel_type, channel_count as u16) {
        diagnostics.push(DsdContainerDiagnostic::error(
            fmt_offset + 20,
            format!("channel_type {} is inconsistent with {} channels", channel_type, channel_count),
        ));
    }
    if sample_rate == 0 {
        diagnostics.push(DsdContainerDiagnostic::error(fmt_offset + 28, "sample_frequency is zero"));
    } else if !is_supported_dsf_sample_rate(sample_rate) {
        diagnostics.push(DsdContainerDiagnostic::error(
            fmt_offset + 28,
            format!(
                "unsupported DSF sample_frequency {}; expected a 44.1 kHz-family DSD rate from DSD64 through DSD1024",
                sample_rate
            ),
        ));
    }
    if bits_per_sample != 1 {
        diagnostics.push(DsdContainerDiagnostic::error(
            fmt_offset + 32,
            format!("bits_per_sample is {}, expected 1 for DSF LSB-first DSD", bits_per_sample),
        ));
    }
    if block_size == 0 {
        diagnostics.push(DsdContainerDiagnostic::error(fmt_offset + 44, "block_size_per_channel is zero"));
    } else if block_size != 4096 {
        diagnostics.push(DsdContainerDiagnostic::error(
            fmt_offset + 44,
            format!("unsupported block_size_per_channel {}; expected exactly 4096", block_size),
        ));
    }
    if reserved != 0 {
        diagnostics.push(DsdContainerDiagnostic::warning(fmt_offset + 48, format!("reserved fmt field is {}", reserved)));
    }
    if sample_count_per_channel % 8 != 0 {
        diagnostics.push(DsdContainerDiagnostic::error(
            fmt_offset + 36,
            format!(
                "sample_count_per_channel {} is not byte-aligned; this reader exposes byte-granular DSD frames",
                sample_count_per_channel
            ),
        ));
    }

    let data_offset = fmt_offset
        .checked_add(fmt_chunk_size)
        .ok_or_else(|| malformed(fmt_offset + 4, "fmt chunk end offset overflow"))?;
    reader.seek(SeekFrom::Start(data_offset))?;
    let data_id = read_fourcc(reader)?;
    if data_id != ID_DATA {
        return Err(malformed(data_offset, format!("expected DSF data chunk, got {}", fourcc_lossy(data_id))));
    }
    let data_chunk_size = read_u64_le(reader)?;
    if data_chunk_size < DSF_DATA_HEADER_SIZE {
        return Err(malformed(data_offset + 4, format!("data chunk too small: {}", data_chunk_size)));
    }
    let audio_offset = data_offset
        .checked_add(DSF_DATA_HEADER_SIZE)
        .ok_or_else(|| malformed(data_offset, "DSF data payload offset overflow"))?;
    let audio_size = data_chunk_size - DSF_DATA_HEADER_SIZE;
    let audio_end = match audio_offset.checked_add(audio_size) {
        Some(end) => {
            if end > file_len {
                diagnostics.push(DsdContainerDiagnostic::error(
                    data_offset + 4,
                    format!("data chunk ends at {}, beyond file length {}", end, file_len),
                ));
            }
            end
        }
        None => {
            diagnostics.push(DsdContainerDiagnostic::error(data_offset + 4, "data chunk end offset overflow"));
            0
        }
    };

    if let Some(meta) = metadata_offset {
        if audio_end != 0 {
            if meta < audio_end {
                diagnostics.push(DsdContainerDiagnostic::error(
                    20,
                    format!("metadata_offset {} points inside the DSF audio data chunk ending at {}", meta, audio_end),
                ));
            } else if meta > audio_end {
                diagnostics.push(DsdContainerDiagnostic::error(
                    20,
                    format!("metadata_offset {} leaves {} byte(s) between audio payload end and metadata", meta, meta - audio_end),
                ));
            }
        }
        if meta < file_len {
            match validate_id3v2_tag_at(reader, meta, file_len) {
                Ok(_) => {}
                Err(err) => diagnostics.push(DsdContainerDiagnostic::error(20, err.to_string())),
            }
        }
    }

    match expected_dsf_audio_payload_size(sample_count_per_channel, block_size, channel_count) {
        Some(expected) if audio_size != expected => diagnostics.push(DsdContainerDiagnostic::error(
            data_offset + 4,
            format!(
                "data payload {} bytes does not match expected padded DSF payload {} bytes for sample_count {}, block_size {}, and {} channels",
                audio_size, expected, sample_count_per_channel, block_size, channel_count
            ),
        )),
        None => diagnostics.push(DsdContainerDiagnostic::error(
            fmt_offset + 36,
            "sample_count × block_size × channel_count overflow while validating DSF data size",
        )),
        _ => {}
    }

    reader.seek(SeekFrom::Start(audio_offset))?;
    Ok(DsdContainerInfo {
        format: DsdContainerFormat::Dsf,
        compression: DsdCompression::Dsd,
        byte_order: DsdByteOrder::LsbFirst,
        channel_count: channel_count as u16,
        sample_rate,
        sample_count_per_channel: Some(sample_count_per_channel),
        data_offset: audio_offset,
        data_size: audio_size,
        metadata_offset,
        channel_ids: Vec::new(),
        dsf_block_size_per_channel: Some(block_size),
        dsdiff_cmpr_code: None,
        dsdiff_frm8_size: None,
        dsdiff_frm8_end: None,
        diagnostics,
    })
}

/// Inspect a Philips DSDIFF stream. The reader is left positioned at the first
/// audio payload byte for DSD, or the first byte inside the `DST ` chunk for
/// DSDIFF/DST.
pub fn inspect_dsdiff<R: Read + Seek>(reader: &mut R) -> Result<DsdContainerInfo, DsdContainerError> {
    let file_len = stream_len(reader)?;
    reader.seek(SeekFrom::Start(0))?;
    let mut diagnostics = Vec::new();

    let frm8_id = read_fourcc(reader)?;
    if frm8_id != ID_FRM8 {
        return Err(malformed(0, format!("expected DSDIFF FRM8 chunk, got {}", fourcc_lossy(frm8_id))));
    }
    let frm8_size = read_u64_be(reader)?;
    if frm8_size < 4 {
        return Err(malformed(4, format!("FRM8 chunk size is {}, but must include the 4-byte form type", frm8_size)));
    }
    let form_type = read_fourcc(reader)?;
    if form_type != ID_DSF {
        return Err(malformed(12, format!("expected DSDIFF form type 'DSD ', got {}", fourcc_lossy(form_type))));
    }
    let frm8_end = 12u64
        .checked_add(frm8_size)
        .ok_or_else(|| malformed(4, "FRM8 end offset overflow"))?;
    if frm8_end > file_len {
        diagnostics.push(DsdContainerDiagnostic::error(
            4,
            format!("FRM8 declares end {}, beyond file length {}", frm8_end, file_len),
        ));
    } else if frm8_end < file_len {
        diagnostics.push(DsdContainerDiagnostic::error(
            4,
            format!("{} byte(s) trail after declared FRM8 end", file_len - frm8_end),
        ));
    }

    let mut sample_rate = None;
    let mut channel_count = None;
    let mut channel_ids = Vec::new();
    let mut cmpr = None;
    let mut data_chunk = None;
    let mut sample_count_per_channel = None;
    let mut fver_count = 0u32;
    let mut prop_count = 0u32;
    let mut audio_chunk_count = 0u32;
    let scan_end = frm8_end.min(file_len);

    let mut pos = 16u64;
    while has_complete_chunk_header(pos, scan_end) {
        reader.seek(SeekFrom::Start(pos))?;
        let chunk_id = read_fourcc(reader)?;
        let chunk_size = read_u64_be(reader)?;
        let data_offset = pos
            .checked_add(DSDIFF_CHUNK_HEADER_SIZE)
            .ok_or_else(|| malformed(pos, "DSDIFF chunk data offset overflow"))?;
        let next = padded_chunk_end(data_offset, chunk_size).ok_or_else(|| {
            malformed(pos + 4, format!("{} chunk end offset overflow", fourcc_lossy(chunk_id)))
        })?;
        let payload_end = data_offset.checked_add(chunk_size).ok_or_else(|| {
            malformed(pos + 4, format!("{} chunk payload end overflow", fourcc_lossy(chunk_id)))
        })?;
        if payload_end > scan_end {
            diagnostics.push(DsdContainerDiagnostic::error(
                pos + 4,
                format!("{} chunk payload ends at {}, beyond FRM8/file end {}", fourcc_lossy(chunk_id), payload_end, scan_end),
            ));
            break;
        }
        validate_zero_pad_byte(reader, payload_end, chunk_size, scan_end, pos, &mut diagnostics)?;

        match chunk_id {
            ID_FVER => {
                fver_count += 1;
                if fver_count > 1 {
                    diagnostics.push(DsdContainerDiagnostic::error(pos, "duplicate DSDIFF FVER chunk"));
                }
                if chunk_size >= 4 {
                    let version = read_u32_be(reader)?;
                    if version != 0x0105_0000 {
                        diagnostics.push(DsdContainerDiagnostic::warning(
                            data_offset,
                            format!("DSDIFF FVER is 0x{:08x}, expected 0x01050000", version),
                        ));
                    }
                } else {
                    diagnostics.push(DsdContainerDiagnostic::warning(data_offset, "FVER chunk shorter than 4 bytes"));
                }
            }
            ID_PROP => {
                prop_count += 1;
                if prop_count > 1 {
                    diagnostics.push(DsdContainerDiagnostic::error(pos, "duplicate DSDIFF PROP chunk"));
                }
                parse_prop_chunk(
                    reader,
                    data_offset,
                    chunk_size,
                    &mut sample_rate,
                    &mut channel_count,
                    &mut channel_ids,
                    &mut cmpr,
                    &mut diagnostics,
                )?;
            }
            ID_DSF | ID_DST => {
                audio_chunk_count += 1;
                if audio_chunk_count > 1 {
                    diagnostics.push(DsdContainerDiagnostic::error(pos, "multiple DSDIFF audio data chunks"));
                }
                data_chunk = Some((chunk_id, data_offset, chunk_size));
                if chunk_id == ID_DST {
                    sample_count_per_channel = parse_dst_frame_count(reader, data_offset, chunk_size, sample_rate, &mut diagnostics)?;
                }
                // Continue scanning: some files append DIIN/COMT/ID3 chunks after
                // audio and callers benefit from trailing-size diagnostics.
            }
            _ => {}
        }
        pos = next;
    }
    if pos != scan_end {
        diagnostics.push(DsdContainerDiagnostic::error(
            pos,
            format!("FRM8 has {} trailing byte(s) after complete top-level chunks", scan_end.checked_sub(pos).unwrap_or(0)),
        ));
    }

    let channel_count = channel_count.ok_or_else(|| malformed(0, "DSDIFF PROP/CHNL channel count missing"))?;
    let sample_rate = sample_rate.ok_or_else(|| malformed(0, "DSDIFF PROP/FS sample rate missing"))?;
    let (data_id, data_offset, data_size) = data_chunk.ok_or_else(|| malformed(0, "DSDIFF DSD/DST data chunk missing"))?;
    let data_compression = DsdCompression::from_dsdiff_code(data_id);
    if let Some(cmpr_code) = cmpr {
        let cmpr_compression = DsdCompression::from_dsdiff_code(cmpr_code);
        if cmpr_compression != data_compression {
            diagnostics.push(DsdContainerDiagnostic::error(
                0,
                format!("CMPR declares {}, but data chunk is {}", cmpr_compression, data_compression),
            ));
        }
        if matches!(cmpr_compression, DsdCompression::Unknown(_)) {
            diagnostics.push(DsdContainerDiagnostic::error(
                0,
                format!("unrecognized DSDIFF compression code {}", fourcc_lossy(cmpr_code)),
            ));
        }
    } else {
        diagnostics.push(DsdContainerDiagnostic::error(0, "DSDIFF PROP/CMPR compression code missing"));
    }

    if data_compression == DsdCompression::Dsd {
        if channel_count == 0 {
            diagnostics.push(DsdContainerDiagnostic::error(data_offset, "DSDIFF/DSD channel count is zero"));
        } else if data_size % u64::from(channel_count) != 0 {
            diagnostics.push(DsdContainerDiagnostic::error(
                data_offset,
                format!(
                    "DSDIFF/DSD payload size {} is not divisible by channel count {}",
                    data_size, channel_count
                ),
            ));
        } else {
            let bytes_per_channel = data_size / u64::from(channel_count);
            match bytes_per_channel.checked_mul(8) {
                Some(samples) => sample_count_per_channel = Some(samples),
                None => diagnostics.push(DsdContainerDiagnostic::error(
                    data_offset,
                    format!("DSDIFF/DSD sample-count derivation overflow: {} bytes/channel × 8", bytes_per_channel),
                )),
            }
        }
    }

    validate_dsdiff_channel_ids(channel_count, &channel_ids, &mut diagnostics);
    if sample_rate == 0 {
        diagnostics.push(DsdContainerDiagnostic::error(0, "DSDIFF sample rate is zero"));
    }

    reader.seek(SeekFrom::Start(data_offset))?;
    Ok(DsdContainerInfo {
        format: DsdContainerFormat::Dsdiff,
        compression: data_compression,
        byte_order: DsdByteOrder::MsbFirst,
        channel_count,
        sample_rate,
        sample_count_per_channel,
        data_offset,
        data_size,
        metadata_offset: None,
        channel_ids,
        dsf_block_size_per_channel: None,
        dsdiff_cmpr_code: cmpr,
        dsdiff_frm8_size: Some(frm8_size),
        dsdiff_frm8_end: Some(frm8_end),
        diagnostics,
    })
}

fn parse_prop_chunk<R: Read + Seek>(
    reader: &mut R,
    prop_offset: u64,
    prop_size: u64,
    sample_rate: &mut Option<u32>,
    channel_count: &mut Option<u16>,
    channel_ids: &mut Vec<[u8; 4]>,
    cmpr: &mut Option<[u8; 4]>,
    diagnostics: &mut Vec<DsdContainerDiagnostic>,
) -> Result<(), DsdContainerError> {
    if prop_size < 4 {
        return Err(malformed(prop_offset, "PROP chunk shorter than property type"));
    }
    reader.seek(SeekFrom::Start(prop_offset))?;
    let prop_type = read_fourcc(reader)?;
    if prop_type != ID_SND {
        diagnostics.push(DsdContainerDiagnostic::error(
            prop_offset,
            format!("PROP property type is {}, expected SND", fourcc_lossy(prop_type)),
        ));
    }
    let prop_end = prop_offset
        .checked_add(prop_size)
        .ok_or_else(|| malformed(prop_offset, "PROP end offset overflow"))?;
    let mut pos = prop_offset
        .checked_add(4)
        .ok_or_else(|| malformed(prop_offset, "PROP first subchunk offset overflow"))?;
    let mut fs_count = 0u32;
    let mut chnl_count = 0u32;
    let mut cmpr_count = 0u32;
    while has_complete_chunk_header(pos, prop_end) {
        reader.seek(SeekFrom::Start(pos))?;
        let id = read_fourcc(reader)?;
        let size = read_u64_be(reader)?;
        let data_offset = pos
            .checked_add(DSDIFF_CHUNK_HEADER_SIZE)
            .ok_or_else(|| malformed(pos, "DSDIFF property data offset overflow"))?;
        let next = padded_chunk_end(data_offset, size).ok_or_else(|| {
            malformed(pos + 4, format!("{} property chunk end offset overflow", fourcc_lossy(id)))
        })?;
        let payload_end = data_offset.checked_add(size).ok_or_else(|| {
            malformed(pos + 4, format!("{} property payload end overflow", fourcc_lossy(id)))
        })?;
        if payload_end > prop_end {
            diagnostics.push(DsdContainerDiagnostic::error(
                pos + 4,
                format!("{} property payload exceeds PROP end", fourcc_lossy(id)),
            ));
            break;
        }
        validate_zero_pad_byte(reader, payload_end, size, prop_end, pos, diagnostics)?;
        match id {
            ID_FS => {
                fs_count += 1;
                if fs_count > 1 {
                    diagnostics.push(DsdContainerDiagnostic::error(pos, "duplicate PROP/FS chunk"));
                }
                if size != 4 {
                    diagnostics.push(DsdContainerDiagnostic::error(
                        data_offset,
                        format!("FS chunk size is {}, expected exactly 4", size),
                    ));
                } else {
                    reader.seek(SeekFrom::Start(data_offset))?;
                    *sample_rate = Some(read_u32_be(reader)?);
                }
            }
            ID_CHNL => {
                chnl_count += 1;
                if chnl_count > 1 {
                    diagnostics.push(DsdContainerDiagnostic::error(pos, "duplicate PROP/CHNL chunk"));
                }
                if size < 2 {
                    diagnostics.push(DsdContainerDiagnostic::error(data_offset, "CHNL chunk shorter than channel-count field"));
                } else {
                    reader.seek(SeekFrom::Start(data_offset))?;
                    let n = read_u16_be(reader)?;
                    *channel_count = Some(n);
                    channel_ids.clear();
                    let available_id_bytes = match size.checked_sub(2) {
                        Some(n) => n,
                        None => {
                            diagnostics.push(DsdContainerDiagnostic::error(
                                data_offset,
                                "CHNL chunk size underflow while counting channel IDs",
                            ));
                            0
                        }
                    };
                    let available_ids = available_id_bytes / 4;
                    for _ in 0..available_ids.min(u16::MAX as u64) {
                        channel_ids.push(read_fourcc(reader)?);
                    }
                    if available_ids != u64::from(n) {
                        diagnostics.push(DsdContainerDiagnostic::error(
                            data_offset,
                            format!("CHNL count is {}, but payload contains {} channel IDs", n, available_ids),
                        ));
                    }
                    let expected_size = 2u64
                        .checked_add(4u64.checked_mul(u64::from(n)).unwrap_or(u64::MAX))
                        .unwrap_or(u64::MAX);
                    if size != expected_size {
                        diagnostics.push(DsdContainerDiagnostic::error(
                            data_offset,
                            format!("CHNL chunk size is {}, expected exactly {} for {} channel IDs", size, expected_size, n),
                        ));
                    }
                }
            }
            ID_CMPR => {
                cmpr_count += 1;
                if cmpr_count > 1 {
                    diagnostics.push(DsdContainerDiagnostic::error(pos, "duplicate PROP/CMPR chunk"));
                }
                if size < 4 {
                    diagnostics.push(DsdContainerDiagnostic::error(data_offset, "CMPR chunk shorter than compression code"));
                } else {
                    reader.seek(SeekFrom::Start(data_offset))?;
                    *cmpr = Some(read_fourcc(reader)?);
                }
            }
            _ => {}
        }
        pos = next;
    }
    if pos != prop_end {
        diagnostics.push(DsdContainerDiagnostic::error(
            pos,
            format!("PROP chunk has {} trailing byte(s) after complete property chunks", prop_end.checked_sub(pos).unwrap_or(0)),
        ));
    }
    Ok(())
}

fn validate_zero_pad_byte<R: Read + Seek>(
    reader: &mut R,
    payload_end: u64,
    size: u64,
    enclosing_end: u64,
    chunk_offset: u64,
    diagnostics: &mut Vec<DsdContainerDiagnostic>,
) -> Result<(), DsdContainerError> {
    if size % 2 == 0 {
        return Ok(());
    }
    let pad_pos = payload_end;
    if pad_pos >= enclosing_end {
        diagnostics.push(DsdContainerDiagnostic::error(
            chunk_offset,
            "odd-sized DSDIFF chunk has no room for required pad byte",
        ));
        return Ok(());
    }
    let saved = reader.stream_position()?;
    reader.seek(SeekFrom::Start(pad_pos))?;
    let mut b = [0u8; 1];
    reader.read_exact(&mut b)?;
    reader.seek(SeekFrom::Start(saved))?;
    if b[0] != 0 {
        diagnostics.push(DsdContainerDiagnostic::error(
            pad_pos,
            format!("DSDIFF odd-chunk pad byte is 0x{:02x}, expected 0x00", b[0]),
        ));
    }
    Ok(())
}

fn validate_dsdiff_channel_ids(
    channel_count: u16,
    channel_ids: &[[u8; 4]],
    diagnostics: &mut Vec<DsdContainerDiagnostic>,
) {
    if channel_count == 0 {
        diagnostics.push(DsdContainerDiagnostic::error(0, "CHNL channel count is zero"));
        return;
    }
    if channel_ids.len() != channel_count as usize {
        diagnostics.push(DsdContainerDiagnostic::error(
            0,
            format!("CHNL advertised {} channels but {} channel IDs were parsed", channel_count, channel_ids.len()),
        ));
        return;
    }
    for (idx, id) in channel_ids.iter().enumerate() {
        if id.iter().all(|b| *b == 0) {
            diagnostics.push(DsdContainerDiagnostic::error(
                0,
                format!("CHNL channel ID {} is all NUL bytes", idx),
            ));
        }
        if !id.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
            diagnostics.push(DsdContainerDiagnostic::error(
                0,
                format!("CHNL channel ID {} contains non-printable bytes: {:?}", idx, id),
            ));
        }
    }
    for i in 0..channel_ids.len() {
        for j in (i + 1)..channel_ids.len() {
            if channel_ids[i] == channel_ids[j] {
                diagnostics.push(DsdContainerDiagnostic::error(
                    0,
                    format!("CHNL channel IDs {} and {} are both {}", i, j, fourcc_lossy(channel_ids[i])),
                ));
            }
        }
    }
    const STEREO: [[u8; 4]; 2] = [*b"SLFT", *b"SRGT"];
    const FIVE: [[u8; 4]; 5] = [*b"MLFT", *b"MRGT", *b"C   ", *b"LS  ", *b"RS  " ];
    const SIX: [[u8; 4]; 6] = [*b"MLFT", *b"MRGT", *b"C   ", *b"LFE ", *b"LS  ", *b"RS  " ];
    let expected: Option<&[[u8; 4]]> = match channel_count {
        2 => Some(&STEREO),
        5 => Some(&FIVE),
        6 => Some(&SIX),
        _ => None,
    };
    if let Some(expected) = expected {
        if channel_ids != expected {
            diagnostics.push(DsdContainerDiagnostic::error(
                0,
                format!("CHNL IDs for {} channels are {:?}, expected {:?}", channel_count, channel_ids, expected),
            ));
        }
        return;
    }

    // For channel counts not assigned a standard DSDIFF loudspeaker map by this
    // crate, accept only the deterministic generic C000, C001, ... sequence
    // emitted by our DFF writer. This prevents arbitrary, misspelled, or
    // duplicated speaker labels from entering the common source model while
    // retaining round-trip support for non-standard channel counts.
    if channel_count > 1000 {
        diagnostics.push(DsdContainerDiagnostic::error(
            0,
            format!("generic CHNL validation supports at most 1000 channels, got {}", channel_count),
        ));
        return;
    }
    for (idx, id) in channel_ids.iter().enumerate() {
        let expected = generic_channel_id(idx as u16);
        if *id != expected {
            diagnostics.push(DsdContainerDiagnostic::error(
                0,
                format!(
                    "generic CHNL ID {} is {}, expected {}",
                    idx,
                    fourcc_lossy(*id),
                    fourcc_lossy(expected)
                ),
            ));
        }
    }
}

fn generic_channel_id(index: u16) -> [u8; 4] {
    [
        b'C',
        b'0' + ((index / 100) % 10) as u8,
        b'0' + ((index / 10) % 10) as u8,
        b'0' + (index % 10) as u8,
    ]
}

fn derive_dst_sample_count_from_frte(
    frames: u64,
    sample_rate: u32,
    frame_rate: u32,
    frte_payload_offset: u64,
    diagnostics: &mut Vec<DsdContainerDiagnostic>,
) -> Option<u64> {
    if frame_rate == 0 {
        diagnostics.push(DsdContainerDiagnostic::error(
            frte_payload_offset + 4,
            "DST FRTE frame rate is zero",
        ));
        return None;
    }
    if sample_rate % frame_rate != 0 {
        diagnostics.push(DsdContainerDiagnostic::warning(
            frte_payload_offset + 4,
            format!(
                "sample rate {} is not an integer multiple of DST frame rate {}",
                sample_rate, frame_rate
            ),
        ));
    }
    let samples_per_frame = u64::from(sample_rate / frame_rate);
    match frames.checked_mul(samples_per_frame) {
        Some(samples) => Some(samples),
        None => {
            diagnostics.push(DsdContainerDiagnostic::error(
                frte_payload_offset,
                format!(
                    "DST FRTE sample count overflow: {} frame(s) × {} samples/frame exceeds u64",
                    frames, samples_per_frame
                ),
            ));
            None
        }
    }
}

fn parse_dst_frame_count<R: Read + Seek>(
    reader: &mut R,
    dst_offset: u64,
    dst_size: u64,
    sample_rate: Option<u32>,
    diagnostics: &mut Vec<DsdContainerDiagnostic>,
) -> Result<Option<u64>, DsdContainerError> {
    let dst_end = dst_offset
        .checked_add(dst_size)
        .ok_or_else(|| malformed(dst_offset, "DST chunk end offset overflow"))?;
    let mut pos = dst_offset;
    while has_complete_chunk_header(pos, dst_end) {
        reader.seek(SeekFrom::Start(pos))?;
        let id = read_fourcc(reader)?;
        let size = read_u64_be(reader)?;
        let data_offset = pos
            .checked_add(DSDIFF_CHUNK_HEADER_SIZE)
            .ok_or_else(|| malformed(pos, "DST subchunk data offset overflow"))?;
        let next = padded_chunk_end(data_offset, size).ok_or_else(|| malformed(pos + 4, "DST subchunk end offset overflow"))?;
        let payload_end = data_offset
            .checked_add(size)
            .ok_or_else(|| malformed(pos + 4, "DST subchunk payload end offset overflow"))?;
        if payload_end > dst_end {
            diagnostics.push(DsdContainerDiagnostic::error(
                pos + 4,
                format!("DST subchunk {} exceeds enclosing DST chunk", fourcc_lossy(id)),
            ));
            return Ok(None);
        }
        if id == ID_FRTE {
            if size < 6 {
                diagnostics.push(DsdContainerDiagnostic::error(data_offset, "FRTE chunk shorter than frame-count/rate fields"));
                return Ok(None);
            }
            reader.seek(SeekFrom::Start(data_offset))?;
            let frames = read_u32_be(reader)? as u64;
            let frame_rate = read_u16_be(reader)? as u32;
            if frame_rate != DST_FRAME_RATE {
                diagnostics.push(DsdContainerDiagnostic::warning(
                    data_offset + 4,
                    format!("DST FRTE frame rate is {}, expected {}", frame_rate, DST_FRAME_RATE),
                ));
            }
            return Ok(sample_rate.and_then(|rate| {
                derive_dst_sample_count_from_frte(frames, rate, frame_rate, data_offset, diagnostics)
            }));
        }
        pos = next;
    }
    diagnostics.push(DsdContainerDiagnostic::warning(dst_offset, "DST chunk has no FRTE frame-count header"));
    Ok(None)
}

fn read_fourcc<R: Read>(reader: &mut R) -> Result<[u8; 4], DsdContainerError> {
    let mut b = [0u8; 4];
    reader.read_exact(&mut b)?;
    Ok(b)
}

fn read_u16_be<R: Read>(reader: &mut R) -> Result<u16, DsdContainerError> {
    let mut b = [0u8; 2];
    reader.read_exact(&mut b)?;
    Ok(u16::from_be_bytes(b))
}

fn read_u32_be<R: Read>(reader: &mut R) -> Result<u32, DsdContainerError> {
    let mut b = [0u8; 4];
    reader.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

fn read_u64_be<R: Read>(reader: &mut R) -> Result<u64, DsdContainerError> {
    let mut b = [0u8; 8];
    reader.read_exact(&mut b)?;
    Ok(u64::from_be_bytes(b))
}

fn read_u64_le<R: Read>(reader: &mut R) -> Result<u64, DsdContainerError> {
    let mut b = [0u8; 8];
    reader.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn stream_len<R: Seek>(reader: &mut R) -> Result<u64, DsdContainerError> {
    let pos = reader.stream_position()?;
    let len = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(pos))?;
    Ok(len)
}

fn has_complete_chunk_header(pos: u64, end: u64) -> bool {
    match pos.checked_add(DSDIFF_CHUNK_HEADER_SIZE) {
        Some(header_end) => header_end <= end,
        None => false,
    }
}

fn padded_chunk_end(data_offset: u64, data_size: u64) -> Option<u64> {
    let pad = data_size & 1;
    data_offset.checked_add(data_size)?.checked_add(pad)
}

fn le_u32_at(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn le_u64_at(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
        buf[off + 4],
        buf[off + 5],
        buf[off + 6],
        buf[off + 7],
    ])
}

fn is_supported_dsf_sample_rate(rate: u32) -> bool {
    let mut r = 2_822_400u32;
    let mut i = 0;
    while i <= 4 {
        if rate == r {
            return true;
        }
        match r.checked_mul(2) {
            Some(next) => r = next,
            None => break,
        }
        i += 1;
    }
    false
}

fn expected_dsf_audio_payload_size(
    sample_count_per_channel: u64,
    block_size_per_channel: u32,
    channel_count: u32,
) -> Option<u64> {
    if block_size_per_channel == 0 || channel_count == 0 {
        return None;
    }
    let bytes_per_channel = sample_count_per_channel.checked_add(7)? / 8;
    let block = u64::from(block_size_per_channel);
    let padded_per_channel = if bytes_per_channel == 0 {
        0
    } else {
        bytes_per_channel
            .checked_add(block - 1)?
            .checked_div(block)?
            .checked_mul(block)?
    };
    padded_per_channel.checked_mul(u64::from(channel_count))
}

fn dsf_channel_type_matches(channel_type: u32, channel_count: u16) -> bool {
    dsf_channel_layout_name(channel_type, channel_count).is_some()
}

fn dsf_channel_layout_name(channel_type: u32, channel_count: u16) -> Option<&'static str> {
    match (channel_type, channel_count) {
        (1, 1) => Some("mono"),
        (2, 2) => Some("stereo"),
        (3, 3) => Some("3-channel L/R/C"),
        (4, 4) => Some("quad L/R/BL/BR"),
        (5, 4) => Some("4-channel L/R/C/LFE"),
        (6, 5) => Some("5-channel L/R/C/BL/BR"),
        (7, 6) => Some("5.1-channel L/R/C/LFE/BL/BR"),
        _ => None,
    }
}

fn validate_id3v2_tag_at<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    file_len: u64,
) -> Result<u64, DsdContainerError> {
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
    let size = parse_id3v2_synchsafe_size(&header[6..10])
        .ok_or_else(|| malformed(offset + 6, "ID3v2 tag size is not synchsafe"))?;
    let total = 10u64
        .checked_add(size)
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

fn malformed(offset: u64, reason: impl Into<String>) -> DsdContainerError {
    DsdContainerError::Malformed {
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
    use crate::dff_writer::DffWriter;
    use crate::dsf_writer::{ChannelType, DsfWriter, SACD_SAMPLING_FREQUENCY};
    use std::io::Cursor;

    #[test]
    fn inspect_dsf_written_by_crate() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DsfWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved(&[0xaa, 0x55, 0x12, 0x34]).unwrap();
            writer.finish().unwrap();
        }
        cursor.set_position(0);
        let info = inspect_dsd_container(&mut cursor).unwrap();
        assert_eq!(info.format, DsdContainerFormat::Dsf);
        assert_eq!(info.compression, DsdCompression::Dsd);
        assert_eq!(info.byte_order, DsdByteOrder::LsbFirst);
        assert_eq!(info.channel_count, 2);
        assert_eq!(info.sample_rate, SACD_SAMPLING_FREQUENCY);
        assert_eq!(info.sample_count_per_channel, Some(16));
        assert_eq!(info.data_offset, 92);
        assert_eq!(info.data_size, 8192);
        assert!(info.diagnostics.is_empty(), "{:?}", info.diagnostics);
        assert_eq!(cursor.position(), info.data_offset);
    }

    #[test]
    fn inspect_dsf_errors_on_channel_type_mismatch() {
        let bytes = crate::dsf_writer::serialize_header(
            ChannelType::Surround51,
            2,
            SACD_SAMPLING_FREQUENCY,
            0,
            0,
            92,
        );
        let mut cursor = Cursor::new(bytes);
        let info = inspect_dsf(&mut cursor).unwrap();
        assert!(info.diagnostics.iter().any(|d| {
            d.severity == DsdContainerDiagnosticSeverity::Error
                && d.message.contains("channel_type")
        }));
    }

    #[test]
    fn inspect_dsf_rejects_non_exact_fmt_size() {
        let mut bytes = crate::dsf_writer::serialize_header(
            ChannelType::Stereo,
            2,
            SACD_SAMPLING_FREQUENCY,
            0,
            0,
            92,
        );
        bytes[32..40].copy_from_slice(&60u64.to_le_bytes());
        let mut cursor = Cursor::new(bytes);
        let err = inspect_dsf(&mut cursor).unwrap_err();
        assert!(err.to_string().contains("fmt chunk size"));
    }

    #[test]
    fn inspect_dsf_flags_non_byte_aligned_sample_count() {
        let bytes = crate::dsf_writer::serialize_header(
            ChannelType::Stereo,
            2,
            SACD_SAMPLING_FREQUENCY,
            1,
            8192,
            92 + 8192,
        );
        let mut cursor = Cursor::new(bytes);
        let info = inspect_dsf(&mut cursor).unwrap();
        assert!(info.diagnostics.iter().any(|d| {
            d.severity == DsdContainerDiagnosticSeverity::Error
                && d.message.contains("not byte-aligned")
        }));
    }

    #[test]
    fn inspect_dsf_rejects_unpadded_data_size() {
        let mut bytes = crate::dsf_writer::serialize_header(
            ChannelType::Stereo,
            2,
            SACD_SAMPLING_FREQUENCY,
            8,
            2,
            94,
        );
        bytes.extend_from_slice(&[0, 0]);
        let mut cursor = Cursor::new(bytes);
        let info = inspect_dsf(&mut cursor).unwrap();
        assert!(info.diagnostics.iter().any(|d| {
            d.severity == DsdContainerDiagnosticSeverity::Error
                && d.message.contains("expected padded DSF payload")
        }));
    }

    #[test]
    fn inspect_dsf_rejects_unsupported_sample_rate() {
        let bytes = crate::dsf_writer::serialize_header(
            ChannelType::Stereo,
            2,
            123_456,
            0,
            0,
            92,
        );
        let mut cursor = Cursor::new(bytes);
        let info = inspect_dsf(&mut cursor).unwrap();
        assert!(info.diagnostics.iter().any(|d| {
            d.severity == DsdContainerDiagnosticSeverity::Error
                && d.message.contains("unsupported DSF sample_frequency")
        }));
    }

    #[test]
    fn inspect_dsf_rejects_metadata_offset_inside_audio() {
        let audio_size = 8192u64;
        let mut bytes = crate::dsf_writer::serialize_header_with_metadata(
            ChannelType::Stereo,
            2,
            SACD_SAMPLING_FREQUENCY,
            8,
            audio_size,
            92 + audio_size,
            100,
        );
        bytes.resize((92 + audio_size) as usize, 0);
        let mut cursor = Cursor::new(bytes);
        let info = inspect_dsf(&mut cursor).unwrap();
        assert!(info.diagnostics.iter().any(|d| {
            d.severity == DsdContainerDiagnosticSeverity::Error
                && d.message.contains("inside the DSF audio data chunk")
        }));
    }

    #[test]
    fn inspect_dsf_rejects_metadata_offset_without_id3() {
        let audio_size = 8192u64;
        let metadata_offset = 92 + audio_size;
        let mut bytes = crate::dsf_writer::serialize_header_with_metadata(
            ChannelType::Stereo,
            2,
            SACD_SAMPLING_FREQUENCY,
            8,
            audio_size,
            metadata_offset + 10,
            metadata_offset,
        );
        bytes.resize(metadata_offset as usize, 0);
        bytes.extend_from_slice(b"NOTANID3!!");
        let mut cursor = Cursor::new(bytes);
        let info = inspect_dsf(&mut cursor).unwrap();
        assert!(info.diagnostics.iter().any(|d| {
            d.severity == DsdContainerDiagnosticSeverity::Error
                && d.message.contains("does not point to an ID3v2 tag")
        }));
    }

    #[test]
    fn inspect_dsdiff_written_by_crate() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_frame(&[0xaa, 0x55, 0x12, 0x34]).unwrap();
            writer.finish().unwrap();
        }
        cursor.set_position(0);
        let info = inspect_dsd_container(&mut cursor).unwrap();
        assert_eq!(info.format, DsdContainerFormat::Dsdiff);
        assert_eq!(info.compression, DsdCompression::Dsd);
        assert_eq!(info.byte_order, DsdByteOrder::MsbFirst);
        assert_eq!(info.channel_count, 2);
        assert_eq!(info.sample_rate, SACD_SAMPLING_FREQUENCY);
        assert_eq!(info.data_size, 4);
        assert_eq!(info.channel_ids, vec![*b"SLFT", *b"SRGT"]);
        assert!(info.diagnostics.is_empty(), "{:?}", info.diagnostics);
        assert_eq!(cursor.position(), info.data_offset);
    }


    #[test]
    fn dst_frte_sample_count_overflow_is_structured_diagnostic() {
        let mut diagnostics = Vec::new();
        let samples = derive_dst_sample_count_from_frte(
            u64::MAX,
            u32::MAX,
            1,
            1234,
            &mut diagnostics,
        );

        assert_eq!(samples, None);
        assert!(diagnostics.iter().any(|d| {
            d.severity == DsdContainerDiagnosticSeverity::Error
                && d.offset == 1234
                && d.message.contains("sample count overflow")
        }), "missing overflow diagnostic: {:?}", diagnostics);
    }

    #[test]
    fn inspect_dsdiff_dst_frte_derives_sample_count() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FRM8");
        bytes.extend_from_slice(&0u64.to_be_bytes());
        bytes.extend_from_slice(b"DSD ");
        bytes.extend_from_slice(b"PROP");
        bytes.extend_from_slice(&88u64.to_be_bytes());
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
        bytes.extend_from_slice(b"DST ");
        bytes.push(14);
        bytes.extend_from_slice(b"DST compressed");
        bytes.push(0);
        bytes.extend_from_slice(b"LSCO");
        bytes.extend_from_slice(&2u64.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(b"DST ");
        bytes.extend_from_slice(&18u64.to_be_bytes());
        bytes.extend_from_slice(b"FRTE");
        bytes.extend_from_slice(&6u64.to_be_bytes());
        bytes.extend_from_slice(&3u32.to_be_bytes());
        bytes.extend_from_slice(&75u16.to_be_bytes());
        let frm8_size = (bytes.len() - 12) as u64;
        bytes[4..12].copy_from_slice(&frm8_size.to_be_bytes());

        let mut cursor = Cursor::new(bytes);
        let info = inspect_dsdiff(&mut cursor).unwrap();
        assert_eq!(info.compression, DsdCompression::Dst);
        assert_eq!(info.sample_count_per_channel, Some(3 * 37_632));
        assert!(info.diagnostics.is_empty(), "{:?}", info.diagnostics);
    }

    #[test]
    fn inspect_dsf_accepts_all_spec_channel_type_pairs() {
        let cases = [
            (1u32, 1u8),
            (2, 2),
            (3, 3),
            (4, 4),
            (5, 4),
            (6, 5),
            (7, 6),
        ];
        for (channel_type, channel_count) in cases {
            assert!(
                dsf_channel_type_matches(channel_type, channel_count as u16),
                "channel_type {} / channel_count {} should be accepted",
                channel_type,
                channel_count
            );
        }
        assert!(!dsf_channel_type_matches(5, 5));
        assert!(!dsf_channel_type_matches(7, 2));
        assert!(!dsf_channel_type_matches(8, 8));
    }

    #[test]
    fn inspect_dsf_rejects_metadata_gap_before_id3() {
        let audio_size = 8192u64;
        let metadata_offset = 92 + audio_size + 4;
        let mut bytes = crate::dsf_writer::serialize_header_with_metadata(
            ChannelType::Stereo,
            2,
            SACD_SAMPLING_FREQUENCY,
            8,
            audio_size,
            metadata_offset + 10,
            metadata_offset,
        );
        bytes.resize(metadata_offset as usize, 0);
        bytes.extend_from_slice(b"ID3\x04\x00\x00\x00\x00\x00\x00");
        let mut cursor = Cursor::new(bytes);
        let info = inspect_dsf(&mut cursor).unwrap();
        assert!(info.diagnostics.iter().any(|d| {
            d.severity == DsdContainerDiagnosticSeverity::Error
                && d.message.contains("leaves 4 byte")
        }));
    }

    #[test]
    fn inspect_dsf_rejects_metadata_id3_size_past_eof() {
        let audio_size = 8192u64;
        let metadata_offset = 92 + audio_size;
        let mut bytes = crate::dsf_writer::serialize_header_with_metadata(
            ChannelType::Stereo,
            2,
            SACD_SAMPLING_FREQUENCY,
            8,
            audio_size,
            metadata_offset + 10,
            metadata_offset,
        );
        bytes.resize(metadata_offset as usize, 0);
        // ID3v2.4 header declaring 127 payload bytes but only carrying none.
        bytes.extend_from_slice(b"ID3\x04\x00\x00\x00\x00\x00\x7f");
        let mut cursor = Cursor::new(bytes);
        let info = inspect_dsf(&mut cursor).unwrap();
        assert!(info.diagnostics.iter().any(|d| {
            d.severity == DsdContainerDiagnosticSeverity::Error
                && d.message.contains("declares end")
        }));
    }

    #[test]
    fn inspect_dsf_rejects_trailing_bytes_after_id3() {
        let audio_size = 8192u64;
        let metadata_offset = 92 + audio_size;
        let mut bytes = crate::dsf_writer::serialize_header_with_metadata(
            ChannelType::Stereo,
            2,
            SACD_SAMPLING_FREQUENCY,
            8,
            audio_size,
            metadata_offset + 11,
            metadata_offset,
        );
        bytes.resize(metadata_offset as usize, 0);
        bytes.extend_from_slice(b"ID3\x04\x00\x00\x00\x00\x00\x00X");
        let mut cursor = Cursor::new(bytes);
        let info = inspect_dsf(&mut cursor).unwrap();
        assert!(info.diagnostics.iter().any(|d| {
            d.severity == DsdContainerDiagnosticSeverity::Error
                && d.message.contains("trailing metadata bytes")
        }));
    }

    #[test]
    fn inspect_dsf_rejects_non_synchsafe_id3_size() {
        let audio_size = 8192u64;
        let metadata_offset = 92 + audio_size;
        let mut bytes = crate::dsf_writer::serialize_header_with_metadata(
            ChannelType::Stereo,
            2,
            SACD_SAMPLING_FREQUENCY,
            8,
            audio_size,
            metadata_offset + 10,
            metadata_offset,
        );
        bytes.resize(metadata_offset as usize, 0);
        bytes.extend_from_slice(b"ID3\x04\x00\x00\x80\x00\x00\x00");
        let mut cursor = Cursor::new(bytes);
        let info = inspect_dsf(&mut cursor).unwrap();
        assert!(info.diagnostics.iter().any(|d| {
            d.severity == DsdContainerDiagnosticSeverity::Error
                && d.message.contains("not synchsafe")
        }));
    }

    #[test]
    fn inspect_dsf_payload_size_overflow_is_diagnostic() {
        let mut bytes = crate::dsf_writer::serialize_header(
            ChannelType::Stereo,
            2,
            SACD_SAMPLING_FREQUENCY,
            u64::MAX,
            0,
            92,
        );
        let mut cursor = Cursor::new(bytes.split_off(0));
        let info = inspect_dsf(&mut cursor).unwrap();
        assert!(info.diagnostics.iter().any(|d| {
            d.severity == DsdContainerDiagnosticSeverity::Error
                && d.message.contains("overflow")
        }));
    }


    #[test]
    fn inspect_dsdiff_frm8_end_overflow_is_malformed() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FRM8");
        bytes.extend_from_slice(&u64::MAX.to_be_bytes());
        bytes.extend_from_slice(b"DSD ");
        let err = inspect_dsdiff(&mut Cursor::new(bytes)).unwrap_err();
        assert!(err.to_string().contains("FRM8 end offset overflow"));
    }

    #[test]
    fn inspect_dsdiff_rejects_nonzero_odd_top_level_padding() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_frame(&[0xaa, 0x55]).unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        let mut odd_chunk = Vec::new();
        odd_chunk.extend_from_slice(b"MARK");
        odd_chunk.extend_from_slice(&1u64.to_be_bytes());
        odd_chunk.push(0x12);
        odd_chunk.push(0xff);
        bytes.splice(32..32, odd_chunk);
        let frm8_size = (bytes.len() as u64).checked_sub(12).unwrap();
        bytes[4..12].copy_from_slice(&frm8_size.to_be_bytes());
        let info = inspect_dsdiff(&mut Cursor::new(bytes)).unwrap();
        assert!(info.diagnostics.iter().any(|d| {
            d.severity == DsdContainerDiagnosticSeverity::Error && d.message.contains("pad byte")
        }));
    }

}
