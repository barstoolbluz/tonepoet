// SPDX-License-Identifier: GPL-2.0-or-later
//! Source-independent DSD stream operations.
//!
//! Phase 1 added typed DSF/DSDIFF readers. Phase 2 added decoded-DST
//! adaptation. This module is the third read-side pass: operations that should
//! not care whether their input was DSF, DSDIFF/DSD, or DSDIFF/DST.
//!
//! This is intentionally not the final common internal source model for SACD
//! ISO + files. It is the file-side validation/copy layer that makes that next
//! refactor smaller and less speculative.

use crate::dsd_file::inspect::{DsdCompression, DsdContainerDiagnostic, DsdContainerFormat, DsdContainerInfo};
use crate::dff_writer::DffWriter;
use crate::dsf_writer::DsfWriter;
use crate::dst::{DstDecoder, DstRate};
use crate::dsd_file::reader::{
    open_dsd_file, DsdDecodedFileReader, DsdFileReader, DsdFrameReader, DsdReadError,
    DstCrcStatus, DstFrameReader,
};
use std::fmt;
use std::io::{Read, Seek, Write};

/// How much audio work [`validate_dsd_stream`] should do after container open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdValidationMode {
    /// Parse and validate only the container header/index structures.
    ///
    /// For DSDIFF/DST this validates `FRTE`/`DSTF` structure, validates `DSTC`
    /// chunks when present, and validates `DSTI` when present. Encoded payload
    /// bytes are not read.
    HeaderOnly,
    /// Stream encoded frames for DSDIFF/DST and uncompressed frames for DSF or
    /// DSDIFF/DSD, but do not decode DST payloads.
    StreamPayloads,
    /// Fully validate decoded DSD. DSDIFF/DST frames are decoded with the
    /// in-tree DST decoder and checked against `DSTC` values when the file
    /// supplies them. Missing optional `DSTC` chunks are counted in the report.
    DecodeDst,
}

/// Validation options for source-independent DSF/DSDIFF file inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdValidationOptions {
    pub mode: DsdValidationMode,
    /// Optional frame limit for quick probes. When set, reaching the limit is
    /// recorded in the report and is not considered a validation failure.
    pub max_frames: Option<u64>,
}

impl Default for DsdValidationOptions {
    fn default() -> Self {
        Self {
            mode: DsdValidationMode::DecodeDst,
            max_frames: None,
        }
    }
}

impl DsdValidationOptions {
    pub fn header_only() -> Self {
        Self {
            mode: DsdValidationMode::HeaderOnly,
            max_frames: None,
        }
    }

    pub fn stream_payloads() -> Self {
        Self {
            mode: DsdValidationMode::StreamPayloads,
            max_frames: None,
        }
    }

    pub fn decode_dst() -> Self {
        Self::default()
    }

    pub fn with_max_frames(mut self, max_frames: u64) -> Self {
        self.max_frames = Some(max_frames);
        self
    }
}

/// High-level source kind after format autodetection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdSourceKind {
    Dsf,
    DsdiffDsd,
    DsdiffDst,
}

impl fmt::Display for DsdSourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dsf => f.write_str("DSF"),
            Self::DsdiffDsd => f.write_str("DSDIFF/DSD"),
            Self::DsdiffDst => f.write_str("DSDIFF/DST"),
        }
    }
}

/// Structured validation failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdValidationFailureKind {
    Open,
    Read,
    DstDecode,
    DstCrcMismatch,
    DstCrcMalformed,
    Unsupported,
}

/// One validation failure. Validation stops at the first fatal streaming error;
/// the partial counters in [`DsdValidationReport`] describe how far it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdValidationFailure {
    pub kind: DsdValidationFailureKind,
    pub frame_index: Option<u64>,
    pub offset: Option<u64>,
    pub message: String,
}

/// Validation report for a DSF, DSDIFF/DSD, or DSDIFF/DST file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdValidationReport {
    pub info: Option<DsdContainerInfo>,
    pub source_kind: Option<DsdSourceKind>,
    pub requested_mode: DsdValidationMode,
    pub container_diagnostics: Vec<DsdContainerDiagnostic>,
    pub dsd_frames_seen: u64,
    pub dst_frames_seen: u64,
    pub decoded_dsd_bytes: u64,
    pub encoded_dst_bytes: u64,
    /// Backward-compatible alias for `dstc_passed_frames`: frames decoded and
    /// successfully checked against a present `DSTC`.
    pub dstc_checked_frames: u64,
    /// Backward-compatible alias for `dstc_no_crc_frames`.
    pub dstc_missing_frames: u64,
    /// Frames with no accompanying `DSTC` chunk.
    pub dstc_no_crc_frames: u64,
    /// Frames with a structurally valid `DSTC` that was streamed but not decoded.
    pub dstc_present_unchecked_frames: u64,
    /// Frames with present `DSTC` whose decoded DSD CRC passed.
    pub dstc_passed_frames: u64,
    /// Frames with present `DSTC` whose decoded DSD CRC failed.
    pub dstc_failed_frames: u64,
    /// Malformed `DSTC` chunks, for example wrong chunk size.
    pub dstc_malformed_frames: u64,
    pub reached_eof: bool,
    pub stopped_at_frame_limit: bool,
    pub failures: Vec<DsdValidationFailure>,
}

impl DsdValidationReport {
    fn new(mode: DsdValidationMode) -> Self {
        Self {
            info: None,
            source_kind: None,
            requested_mode: mode,
            container_diagnostics: Vec::new(),
            dsd_frames_seen: 0,
            dst_frames_seen: 0,
            decoded_dsd_bytes: 0,
            encoded_dst_bytes: 0,
            dstc_checked_frames: 0,
            dstc_missing_frames: 0,
            dstc_no_crc_frames: 0,
            dstc_present_unchecked_frames: 0,
            dstc_passed_frames: 0,
            dstc_failed_frames: 0,
            dstc_malformed_frames: 0,
            reached_eof: false,
            stopped_at_frame_limit: false,
            failures: Vec::new(),
        }
    }

    pub fn is_success(&self) -> bool {
        self.failures.is_empty() && (self.reached_eof || self.stopped_at_frame_limit)
    }

    pub fn frames_seen(&self) -> u64 {
        self.dsd_frames_seen + self.dst_frames_seen
    }

    pub fn duration_seconds(&self) -> Option<f64> {
        self.info.as_ref()?.duration_seconds()
    }
}

/// Copy/transcode statistics for decoded DSD file operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DsdStreamCopyStats {
    pub frames_read: u64,
    pub bytes_written: u64,
}

/// Validate a DSF, DSDIFF/DSD, or DSDIFF/DST stream without committing to an
/// output container.
///
/// The function is report-oriented rather than exception-oriented: container
/// open failures and streaming failures are represented inside the returned
/// report so callers can show partial progress and exact failure class.
pub fn validate_dsd_stream<R: Read + Seek>(reader: R, options: DsdValidationOptions) -> DsdValidationReport {
    let mut report = DsdValidationReport::new(options.mode);
    let file = match open_dsd_file(reader) {
        Ok(file) => file,
        Err(err) => {
            let (frame_index, offset) = match &err {
                DsdReadError::DstCrc { frame_index, offset, .. } => (*frame_index, Some(*offset)),
                DsdReadError::Malformed { offset, .. } => (None, Some(*offset)),
                _ => (None, None),
            };
            if matches!(&err, DsdReadError::DstCrc { status: DstCrcStatus::Malformed { .. }, .. }) {
                report.dstc_malformed_frames = 1;
            }
            report.failures.push(DsdValidationFailure {
                kind: failure_kind_for_error(&err),
                frame_index,
                offset,
                message: err.to_string(),
            });
            return report;
        }
    };

    report.info = Some(file.info().clone());
    report.container_diagnostics = file.info().diagnostics.clone();
    report.source_kind = Some(source_kind(&file));

    match options.mode {
        DsdValidationMode::HeaderOnly => {
            report.reached_eof = true;
        }
        DsdValidationMode::StreamPayloads => validate_payloads(file, options.max_frames, false, &mut report),
        DsdValidationMode::DecodeDst => validate_payloads(file, options.max_frames, true, &mut report),
    }
    report
}

/// Write decoded DSD frames from any supported decoded reader to DSDIFF/DSD.
///
/// This is deliberately a small library helper, not the final tonepoet planner
/// integration. It gives tests and callers one source-independent operation to
/// prove that DSF, DSDIFF/DSD, and DSDIFF/DST can all feed the same sink.
pub fn write_decoded_dsd_to_dff<R, W>(
    reader: &mut DsdDecodedFileReader<R>,
    writer: W,
) -> Result<DsdStreamCopyStats, DsdReadError>
where
    R: Read + Seek,
    W: Write + Seek,
{
    let info = reader.info().clone();
    let channel_count = u8_channel_count(&info)?;
    let mut out = DffWriter::new(writer, channel_count, info.sample_rate)?;
    let mut stats = DsdStreamCopyStats {
        frames_read: 0,
        bytes_written: 0,
    };
    while let Some(frame) = reader.next_dsd_frame()? {
        out.write_frame(&frame.data)?;
        stats.frames_read = stats.frames_read.checked_add(1).ok_or_else(|| {
            DsdReadError::Malformed { offset: 0, reason: "DFF copy frame counter overflow".to_string() }
        })?;
        stats.bytes_written = stats.bytes_written.checked_add(frame.data.len() as u64).ok_or_else(|| {
            DsdReadError::Malformed { offset: 0, reason: "DFF copy byte counter overflow".to_string() }
        })?;
    }
    out.finish()?;
    Ok(stats)
}

/// Write decoded DSD frames from any supported decoded reader to DSF.
pub fn write_decoded_dsd_to_dsf<R, W>(
    reader: &mut DsdDecodedFileReader<R>,
    writer: W,
) -> Result<DsdStreamCopyStats, DsdReadError>
where
    R: Read + Seek,
    W: Write + Seek,
{
    let info = reader.info().clone();
    let channel_count = u8_channel_count(&info)?;
    let mut out = DsfWriter::new(writer, channel_count, info.sample_rate)?;
    let mut stats = DsdStreamCopyStats {
        frames_read: 0,
        bytes_written: 0,
    };
    while let Some(frame) = reader.next_dsd_frame()? {
        out.write_interleaved(&frame.data)?;
        stats.frames_read = stats.frames_read.checked_add(1).ok_or_else(|| {
            DsdReadError::Malformed { offset: 0, reason: "DSF copy frame counter overflow".to_string() }
        })?;
        stats.bytes_written = stats.bytes_written.checked_add(frame.data.len() as u64).ok_or_else(|| {
            DsdReadError::Malformed { offset: 0, reason: "DSF copy byte counter overflow".to_string() }
        })?;
    }
    out.finish()?;
    Ok(stats)
}

fn validate_payloads<R: Read + Seek>(
    file: DsdFileReader<R>,
    max_frames: Option<u64>,
    decode_dst: bool,
    report: &mut DsdValidationReport,
) {
    match file {
        DsdFileReader::Dsf(mut r) => validate_dsd_reader(&mut r, max_frames, report),
        DsdFileReader::DsdiffDsd(mut r) => validate_dsd_reader(&mut r, max_frames, report),
        DsdFileReader::DsdiffDst(mut r) => validate_dst_reader(&mut r, max_frames, decode_dst, report),
    }
}

fn validate_dsd_reader<R: DsdFrameReader>(reader: &mut R, max_frames: Option<u64>, report: &mut DsdValidationReport) {
    loop {
        if frame_limit_reached(report.frames_seen(), max_frames) {
            report.stopped_at_frame_limit = true;
            return;
        }
        match reader.next_dsd_frame() {
            Ok(Some(frame)) => {
                report.dsd_frames_seen = match report.dsd_frames_seen.checked_add(1) {
                    Some(n) => n,
                    None => {
                        push_failure(report, DsdValidationFailureKind::Read, Some(frame.frame_index), None, "DSD frame counter overflow");
                        return;
                    }
                };
                report.decoded_dsd_bytes = match report.decoded_dsd_bytes.checked_add(frame.data.len() as u64) {
                    Some(n) => n,
                    None => {
                        push_failure(report, DsdValidationFailureKind::Read, Some(frame.frame_index), None, "decoded DSD byte counter overflow");
                        return;
                    }
                };
            }
            Ok(None) => {
                report.reached_eof = true;
                return;
            }
            Err(err) => {
                push_failure(report, failure_kind_for_error(&err), None, None, err.to_string());
                return;
            }
        }
    }
}

fn validate_dst_reader<R: DstFrameReader>(
    reader: &mut R,
    max_frames: Option<u64>,
    decode_dst: bool,
    report: &mut DsdValidationReport,
) {
    let channels = match u8_channel_count(reader.info()) {
        Ok(n) => n,
        Err(err) => {
            push_failure(report, DsdValidationFailureKind::Unsupported, None, None, err.to_string());
            return;
        }
    };
    let rate = if decode_dst {
        match DstRate::from_sample_rate(reader.info().sample_rate) {
            Ok(rate) => Some(rate),
            Err(err) => {
                push_failure(report, DsdValidationFailureKind::Unsupported, None, None, err.to_string());
                return;
            }
        }
    } else {
        None
    };
    let expected_decoded_len = if let Some(rate) = rate {
        match rate.frame_bytes_per_channel().and_then(|bytes| {
            bytes.checked_mul(usize::from(channels)).ok_or(
                crate::dst::DstError::ArithmeticDecodeFailure("decoded DST frame byte count overflow"),
            )
        }) {
            Ok(n) => Some(n),
            Err(err) => {
                push_failure(report, DsdValidationFailureKind::Unsupported, None, None, err.to_string());
                return;
            }
        }
    } else {
        None
    };
    let mut dst_decoder = if let Some(rate) = rate {
        match DstDecoder::new(channels, rate) {
            Ok(decoder) => Some(decoder),
            Err(err) => {
                push_failure(report, DsdValidationFailureKind::Unsupported, None, None, err.to_string());
                return;
            }
        }
    } else {
        None
    };
    let mut decoded_buffer = expected_decoded_len.map(|len| vec![0u8; len]);

    loop {
        if frame_limit_reached(report.frames_seen(), max_frames) {
            report.stopped_at_frame_limit = true;
            return;
        }
        let frame = match reader.next_dst_frame() {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                report.reached_eof = true;
                return;
            }
            Err(err) => {
                push_failure(report, failure_kind_for_error(&err), None, None, err.to_string());
                return;
            }
        };
        report.dst_frames_seen = match report.dst_frames_seen.checked_add(1) {
            Some(n) => n,
            None => {
                push_failure(report, DsdValidationFailureKind::Read, Some(frame.frame_index), Some(frame.payload_offset), "DST frame counter overflow");
                return;
            }
        };
        report.encoded_dst_bytes = match report.encoded_dst_bytes.checked_add(frame.encoded.len() as u64) {
            Some(n) => n,
            None => {
                push_failure(report, DsdValidationFailureKind::Read, Some(frame.frame_index), Some(frame.payload_offset), "encoded DST byte counter overflow");
                return;
            }
        };
        if !decode_dst {
            if !record_dst_crc_status(report, Some(frame.frame_index), Some(frame.payload_offset), &frame.crc_status) {
                return;
            }
        }
        if decode_dst {
            let Some(decoded_buffer) = decoded_buffer.as_mut() else {
                push_failure(report, DsdValidationFailureKind::DstDecode, Some(frame.frame_index), Some(frame.payload_offset), "decoded DST validation buffer is not initialized");
                return;
            };
            let Some(decoder) = dst_decoder.as_mut() else {
                push_failure(report, DsdValidationFailureKind::DstDecode, Some(frame.frame_index), Some(frame.payload_offset), "DST decoder is not initialized");
                return;
            };
            let decoded_len = match decoder.decode_frame_into(&frame.encoded, decoded_buffer) {
                Ok(decoded_len) => decoded_len,
                Err(err) => {
                    push_failure(report, DsdValidationFailureKind::DstDecode, Some(frame.frame_index), Some(frame.payload_offset), err.to_string());
                    return;
                }
            };
            if Some(decoded_len) != expected_decoded_len {
                push_failure(
                    report,
                    DsdValidationFailureKind::DstDecode,
                    Some(frame.frame_index),
                    Some(frame.payload_offset),
                    format!("DST decoder returned {} bytes, expected {}", decoded_len, expected_decoded_len.unwrap_or(0)),
                );
                return;
            }
            let decoded = &decoded_buffer[..decoded_len];
            let crc_status = DstCrcStatus::verify(frame.dstc, decoded);
            if !record_dst_crc_status(report, Some(frame.frame_index), Some(frame.payload_offset), &crc_status) {
                return;
            }
            if matches!(&crc_status, DstCrcStatus::PresentFailed { .. } | DstCrcStatus::Malformed { .. }) {
                return;
            }
            report.decoded_dsd_bytes = match report.decoded_dsd_bytes.checked_add(decoded_len as u64) {
                Some(n) => n,
                None => {
                    push_failure(report, DsdValidationFailureKind::Read, Some(frame.frame_index), Some(frame.payload_offset), "decoded DSD byte counter overflow");
                    return;
                }
            };
        }
    }
}

fn record_dst_crc_status(
    report: &mut DsdValidationReport,
    frame_index: Option<u64>,
    offset: Option<u64>,
    status: &DstCrcStatus,
) -> bool {
    match status {
        DstCrcStatus::NoCrc => {
            if !increment_counter(&mut report.dstc_no_crc_frames) {
                push_failure(report, DsdValidationFailureKind::Read, frame_index, offset, "DSTC no-CRC counter overflow");
                return false;
            }
            if !increment_counter(&mut report.dstc_missing_frames) {
                push_failure(report, DsdValidationFailureKind::Read, frame_index, offset, "DSTC missing-frame counter overflow");
                return false;
            }
        }
        DstCrcStatus::PresentUnchecked { .. } => {
            if !increment_counter(&mut report.dstc_present_unchecked_frames) {
                push_failure(report, DsdValidationFailureKind::Read, frame_index, offset, "DSTC unchecked-frame counter overflow");
                return false;
            }
        }
        DstCrcStatus::PresentPassed { .. } => {
            if !increment_counter(&mut report.dstc_passed_frames) {
                push_failure(report, DsdValidationFailureKind::Read, frame_index, offset, "DSTC passed-frame counter overflow");
                return false;
            }
            if !increment_counter(&mut report.dstc_checked_frames) {
                push_failure(report, DsdValidationFailureKind::Read, frame_index, offset, "DSTC checked-frame counter overflow");
                return false;
            }
        }
        DstCrcStatus::PresentFailed { .. } => {
            if !increment_counter(&mut report.dstc_failed_frames) {
                push_failure(report, DsdValidationFailureKind::Read, frame_index, offset, "DSTC failed-frame counter overflow");
                return false;
            }
            push_failure(report, DsdValidationFailureKind::DstCrcMismatch, frame_index, offset, status.to_string());
            return false;
        }
        DstCrcStatus::Malformed { .. } => {
            if !increment_counter(&mut report.dstc_malformed_frames) {
                push_failure(report, DsdValidationFailureKind::Read, frame_index, offset, "DSTC malformed-frame counter overflow");
                return false;
            }
            push_failure(report, DsdValidationFailureKind::DstCrcMalformed, frame_index, offset, status.to_string());
            return false;
        }
    }
    true
}

fn increment_counter(counter: &mut u64) -> bool {
    match counter.checked_add(1) {
        Some(n) => {
            *counter = n;
            true
        }
        None => false,
    }
}

fn source_kind<R: Read + Seek>(file: &DsdFileReader<R>) -> DsdSourceKind {
    match file {
        DsdFileReader::Dsf(_) => DsdSourceKind::Dsf,
        DsdFileReader::DsdiffDsd(_) => DsdSourceKind::DsdiffDsd,
        DsdFileReader::DsdiffDst(_) => DsdSourceKind::DsdiffDst,
    }
}

fn frame_limit_reached(frames_seen: u64, max_frames: Option<u64>) -> bool {
    max_frames.map(|limit| frames_seen >= limit).unwrap_or(false)
}

fn push_failure(
    report: &mut DsdValidationReport,
    kind: DsdValidationFailureKind,
    frame_index: Option<u64>,
    offset: Option<u64>,
    message: impl Into<String>,
) {
    report.failures.push(DsdValidationFailure {
        kind,
        frame_index,
        offset,
        message: message.into(),
    });
}

fn failure_kind_for_error(err: &DsdReadError) -> DsdValidationFailureKind {
    match err {
        DsdReadError::Container(_) => DsdValidationFailureKind::Open,
        DsdReadError::UnsupportedFormat { .. } => DsdValidationFailureKind::Unsupported,
        DsdReadError::Dst(_) => DsdValidationFailureKind::DstDecode,
        DsdReadError::DstCrc { status: DstCrcStatus::Malformed { .. }, .. } => DsdValidationFailureKind::DstCrcMalformed,
        DsdReadError::DstCrc { .. } => DsdValidationFailureKind::DstCrcMismatch,
        DsdReadError::Io(_) | DsdReadError::Malformed { .. } => DsdValidationFailureKind::Read,
    }
}

fn u8_channel_count(info: &DsdContainerInfo) -> Result<u8, DsdReadError> {
    if info.channel_count == 0 {
        return Err(DsdReadError::UnsupportedFormat {
            reason: "channel count is zero".to_string(),
        });
    }
    u8::try_from(info.channel_count).map_err(|_| DsdReadError::UnsupportedFormat {
        reason: format!("channel count {} exceeds u8", info.channel_count),
    })
}

/// Format/compression pair for report display without exposing enum nesting to
/// UI code.
pub fn describe_container(info: &DsdContainerInfo) -> String {
    let format = match info.format {
        DsdContainerFormat::Dsf => "DSF",
        DsdContainerFormat::Dsdiff => "DSDIFF",
    };
    let compression = match info.compression {
        DsdCompression::Dsd => "DSD",
        DsdCompression::Dst => "DST",
        DsdCompression::Unknown(_) => "unknown",
    };
    format!(
        "{}/{}: {} channel(s), {} Hz, {} byte(s) of payload",
        format, compression, info.channel_count, info.sample_rate, info.data_size
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dff_dst_writer::{DffDstWriter, SACD_SAMPLING_FREQUENCY};
    use crate::dff_writer::DffWriter;
    use crate::dsf_writer::DsfWriter;
    use crate::dst::{dst_interleaved_frame_len, encode_uncompressed_frame_interleaved};
    use crate::dsd_file::reader::open_dsd_as_decoded_reader;
    use std::io::Cursor;

    #[test]
    fn validation_reports_dsf_success() {
        let payload = vec![0x12, 0x34, 0xab, 0xcd];
        let mut file = Cursor::new(Vec::new());
        {
            let mut writer = DsfWriter::new(&mut file, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved(&payload).unwrap();
            writer.finish().unwrap();
        }
        file.set_position(0);
        let report = validate_dsd_stream(file, DsdValidationOptions::decode_dst());
        assert!(report.is_success(), "{:?}", report.failures);
        assert_eq!(report.source_kind, Some(DsdSourceKind::Dsf));
        assert_eq!(report.dsd_frames_seen, 1);
        assert_eq!(report.decoded_dsd_bytes, payload.len() as u64);
    }

    #[test]
    fn validation_header_only_does_not_read_payloads() {
        let payload = vec![0xaa, 0x55, 0xaa, 0x55];
        let mut file = Cursor::new(Vec::new());
        {
            let mut writer = DffWriter::new(&mut file, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_frame(&payload).unwrap();
            writer.finish().unwrap();
        }
        file.set_position(0);
        let report = validate_dsd_stream(file, DsdValidationOptions::header_only());
        assert!(report.is_success(), "{:?}", report.failures);
        assert_eq!(report.source_kind, Some(DsdSourceKind::DsdiffDsd));
        assert_eq!(report.frames_seen(), 0);
        assert_eq!(report.decoded_dsd_bytes, 0);
    }

    #[test]
    fn validation_decodes_dst_and_checks_crc() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded = vec![0xaa; frame_len];
        let encoded = encode_uncompressed_frame_interleaved(&decoded, 2).unwrap();
        let mut file = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut file, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded, &decoded).unwrap();
            writer.finish().unwrap();
        }
        file.set_position(0);
        let report = validate_dsd_stream(file, DsdValidationOptions::decode_dst());
        assert!(report.is_success(), "{:?}", report.failures);
        assert_eq!(report.source_kind, Some(DsdSourceKind::DsdiffDst));
        assert_eq!(report.dst_frames_seen, 1);
        assert_eq!(report.dstc_checked_frames, 1);
        assert_eq!(report.dstc_passed_frames, 1);
        assert_eq!(report.dstc_failed_frames, 0);
        assert_eq!(report.dstc_no_crc_frames, 0);
        assert_eq!(report.decoded_dsd_bytes, frame_len as u64);
        assert!(report.encoded_dst_bytes > 0);
    }

    #[test]
    fn validation_reports_dstc_mismatch_without_panicking() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded = vec![0xaa; frame_len];
        let encoded = encode_uncompressed_frame_interleaved(&decoded, 2).unwrap();
        let mut file = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut file, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded, &decoded).unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = file.into_inner();
        let dstc_pos = bytes.windows(4).position(|w| w == b"DSTC").unwrap();
        bytes[dstc_pos + 15] ^= 0x01;
        let report = validate_dsd_stream(Cursor::new(bytes), DsdValidationOptions::decode_dst());
        assert!(!report.is_success());
        assert_eq!(report.failures[0].kind, DsdValidationFailureKind::DstCrcMismatch);
        assert_eq!(report.dst_frames_seen, 1);
        assert_eq!(report.dstc_failed_frames, 1);
        assert_eq!(report.dstc_passed_frames, 0);
    }



    #[test]
    fn validation_reports_no_crc_frames() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded = vec![0xaa; frame_len];
        let encoded = encode_uncompressed_frame_interleaved(&decoded, 2).unwrap();
        let mut file = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut file, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded, &decoded).unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = file.into_inner();
        let dstc_pos = bytes.windows(4).position(|w| w == b"DSTC").unwrap();
        bytes.truncate(dstc_pos);
        let frte_pos = bytes.windows(4).position(|w| w == b"FRTE").unwrap();
        let top_level_dst_pos = frte_pos - 12;
        let dst_payload_start = top_level_dst_pos + 12;
        let dst_size = (bytes.len() - dst_payload_start) as u64;
        bytes[top_level_dst_pos + 4..top_level_dst_pos + 12].copy_from_slice(&dst_size.to_be_bytes());
        let frm8_size = (bytes.len() - 12) as u64;
        bytes[4..12].copy_from_slice(&frm8_size.to_be_bytes());

        let report = validate_dsd_stream(Cursor::new(bytes), DsdValidationOptions::decode_dst());
        assert!(report.is_success(), "{:?}", report.failures);
        assert_eq!(report.dstc_no_crc_frames, 1);
        assert_eq!(report.dstc_missing_frames, 1);
        assert_eq!(report.dstc_passed_frames, 0);
    }

    #[test]
    fn validation_reports_present_unchecked_crc_in_payload_mode() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded = vec![0xaa; frame_len];
        let encoded = encode_uncompressed_frame_interleaved(&decoded, 2).unwrap();
        let mut file = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut file, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded, &decoded).unwrap();
            writer.finish().unwrap();
        }
        file.set_position(0);
        let report = validate_dsd_stream(file, DsdValidationOptions::stream_payloads());
        assert!(report.is_success(), "{:?}", report.failures);
        assert_eq!(report.dst_frames_seen, 1);
        assert_eq!(report.dstc_present_unchecked_frames, 1);
        assert_eq!(report.dstc_passed_frames, 0);
        assert_eq!(report.dstc_no_crc_frames, 0);
    }

    #[test]
    fn validation_reports_malformed_dstc() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded = vec![0xaa; frame_len];
        let encoded = encode_uncompressed_frame_interleaved(&decoded, 2).unwrap();
        let mut file = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut file, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded, &decoded).unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = file.into_inner();
        let dstc_pos = bytes.windows(4).position(|w| w == b"DSTC").unwrap();
        bytes[dstc_pos + 4..dstc_pos + 12].copy_from_slice(&3u64.to_be_bytes());
        let report = validate_dsd_stream(Cursor::new(bytes), DsdValidationOptions::decode_dst());
        assert!(!report.is_success());
        assert_eq!(report.failures[0].kind, DsdValidationFailureKind::DstCrcMalformed);
        assert_eq!(report.dstc_malformed_frames, 1);
    }

    #[test]
    fn validation_respects_max_frame_limit() {
        let mut file = Cursor::new(Vec::new());
        {
            let mut writer = DffWriter::new(&mut file, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_frame(&[0, 1, 2, 3]).unwrap();
            writer.write_frame(&[4, 5, 6, 7]).unwrap();
            writer.finish().unwrap();
        }
        file.set_position(0);
        let report = validate_dsd_stream(file, DsdValidationOptions::stream_payloads().with_max_frames(1));
        assert!(report.is_success(), "{:?}", report.failures);
        assert!(report.stopped_at_frame_limit);
        assert_eq!(report.frames_seen(), 1);
    }

    #[test]
    fn decoded_reader_can_copy_dsf_to_dff() {
        let payload = vec![0x80, 0x01, 0xaa, 0x55, 0xf0, 0x0f];
        let mut source = Cursor::new(Vec::new());
        {
            let mut writer = DsfWriter::new(&mut source, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved(&payload).unwrap();
            writer.finish().unwrap();
        }
        source.set_position(0);
        let mut reader = open_dsd_as_decoded_reader(source).unwrap();
        let mut out = Cursor::new(Vec::new());
        let stats = write_decoded_dsd_to_dff(&mut reader, &mut out).unwrap();
        assert_eq!(stats.bytes_written, payload.len() as u64);
        out.set_position(0);
        let report = validate_dsd_stream(out, DsdValidationOptions::stream_payloads());
        assert!(report.is_success(), "{:?}", report.failures);
        assert_eq!(report.source_kind, Some(DsdSourceKind::DsdiffDsd));
    }

    #[test]
    fn decoded_reader_can_copy_dff_dst_to_dsf() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded = vec![0x55; frame_len];
        let encoded = encode_uncompressed_frame_interleaved(&decoded, 2).unwrap();
        let mut source = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut source, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded, &decoded).unwrap();
            writer.finish().unwrap();
        }
        source.set_position(0);
        let mut reader = open_dsd_as_decoded_reader(source).unwrap();
        let mut out = Cursor::new(Vec::new());
        let stats = write_decoded_dsd_to_dsf(&mut reader, &mut out).unwrap();
        assert_eq!(stats.frames_read, 1);
        out.set_position(0);
        let report = validate_dsd_stream(out, DsdValidationOptions::stream_payloads());
        assert!(report.is_success(), "{:?}", report.failures);
        assert_eq!(report.source_kind, Some(DsdSourceKind::Dsf));
    }
}
