// SPDX-License-Identifier: GPL-2.0-or-later
//! Common internal source model for SACD ISO, DSF, DSDIFF/DSD, and
//! DSDIFF/DST input.
//!
//! The first reader passes introduced typed DSF/DSDIFF streaming readers. This
//! module is the next layer up: a single source abstraction that can yield
//! either canonical uncompressed DSD frames or encoded DST frames without making
//! the caller care whether the bytes came from ScarletBook ISO sectors, Sony
//! DSF, Philips DSDIFF/DSD, or Philips DSDIFF/DST.
//!
//! Design points:
//!
//! - The source layer is lossless by default. Existing DST payloads stay DST
//!   frames until a caller explicitly asks for decoded DSD.
//! - Uncompressed DSD is always canonical `sacd-rs` layout: channel-interleaved
//!   bytes, MSB-first bits.
//! - ISO sources preserve sector/timecode provenance; file sources preserve
//!   container offsets where the underlying reader exposes them.
//! - Decode adapters verify DSTC when present and fail closed on unsupported
//!   channel layouts.

use crate::dsd_file::inspect::{
    DsdByteOrder, DsdCompression, DsdContainerFormat, DsdContainerInfo,
};
use crate::dst::{decode_frame_with_rate_into, DstError, DstRate};
use crate::frame::{
    FrameError, FrameFormat, FrameReader, FrameReaderStats, FrameTimeFilter, Timecode,
};
use crate::iso_reader::IsoReader;
use crate::dsd_file::reader::{
    open_dsd_file, DsdFileReader, DsdFrame, DsdFrameReader, DsdFrameSeek, DsdReadError, DstCrcStatus, DstFrameReader,
};
use std::fmt;
use std::io::{self, Read, Seek};

/// The physical/logical source family behind a [`DsdSource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdSourceKind {
    SacdIsoTrack,
    Dsf,
    DsdiffDsd,
    DsdiffDst,
}

impl fmt::Display for DsdSourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SacdIsoTrack => f.write_str("SACD ISO track"),
            Self::Dsf => f.write_str("DSF"),
            Self::DsdiffDsd => f.write_str("DSDIFF/DSD"),
            Self::DsdiffDst => f.write_str("DSDIFF/DST"),
        }
    }
}

/// Stable source metadata used by extract/validate/convert paths.
#[derive(Debug, Clone)]
pub struct DsdSourceInfo {
    pub kind: DsdSourceKind,
    pub channel_count: u16,
    pub sample_rate: u32,
    /// Source compression as declared by the container or SACD area policy.
    pub compression: DsdCompression,
    /// Per-channel one-bit sample count when the source declares it or it can
    /// be derived exactly.
    pub sample_count_per_channel: Option<u64>,
    /// Original container inspection for DSF/DSDIFF input. ISO sources do not
    /// have a DSF/DSDIFF container, so this is `None`.
    pub container: Option<DsdContainerInfo>,
    /// ISO range provenance for SACD sources.
    pub iso_range: Option<IsoTrackRange>,
}

impl DsdSourceInfo {
    pub fn from_container(info: DsdContainerInfo) -> Self {
        let kind = match (info.format, info.compression) {
            (DsdContainerFormat::Dsf, _) => DsdSourceKind::Dsf,
            (DsdContainerFormat::Dsdiff, DsdCompression::Dsd) => DsdSourceKind::DsdiffDsd,
            (DsdContainerFormat::Dsdiff, DsdCompression::Dst) => DsdSourceKind::DsdiffDst,
            (DsdContainerFormat::Dsdiff, _) => DsdSourceKind::DsdiffDsd,
        };
        Self {
            kind,
            channel_count: info.channel_count,
            sample_rate: info.sample_rate,
            compression: info.compression,
            sample_count_per_channel: info.sample_count_per_channel,
            container: Some(info),
            iso_range: None,
        }
    }

    pub fn sacd_iso_track(opts: &IsoTrackSourceOptions) -> Self {
        let compression = match opts.frame_format {
            Some(ff) if ff.is_dst_encoded() => DsdCompression::Dst,
            Some(_) => DsdCompression::Dsd,
            None => DsdCompression::Unknown(*b"MIXD"),
        };
        Self {
            kind: DsdSourceKind::SacdIsoTrack,
            channel_count: u16::from(opts.channel_count),
            sample_rate: opts.sample_rate,
            compression,
            sample_count_per_channel: None,
            container: None,
            iso_range: Some(IsoTrackRange {
                start_lsn: opts.start_lsn,
                end_lsn: opts.end_lsn,
            }),
        }
    }

    pub fn is_dst_source(&self) -> bool {
        self.compression == DsdCompression::Dst
    }
}

/// ISO sector range provenance for a SACD track extraction source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsoTrackRange {
    pub start_lsn: u64,
    pub end_lsn: u64,
}

/// Options for building an ISO-backed source reader.
#[derive(Debug, Clone)]
pub struct IsoTrackSourceOptions {
    pub start_lsn: u64,
    pub end_lsn: u64,
    pub channel_count: u8,
    pub sample_rate: u32,
    pub time_filter: Option<FrameTimeFilter>,
    pub frame_format: Option<FrameFormat>,
    pub strict_channel_count: bool,
    pub recover_sector_errors: bool,
}

impl IsoTrackSourceOptions {
    pub fn new(start_lsn: u64, end_lsn: u64, channel_count: u8, sample_rate: u32) -> Self {
        Self {
            start_lsn,
            end_lsn,
            channel_count,
            sample_rate,
            time_filter: None,
            frame_format: None,
            strict_channel_count: false,
            recover_sector_errors: false,
        }
    }

    pub fn with_time_filter(mut self, filter: FrameTimeFilter) -> Self {
        self.time_filter = Some(filter);
        self
    }

    pub fn with_frame_format(mut self, frame_format: FrameFormat) -> Self {
        self.frame_format = Some(frame_format);
        self
    }

    pub fn with_strict_channel_count(mut self, strict: bool) -> Self {
        self.strict_channel_count = strict;
        self
    }

    pub fn with_sector_recovery(mut self, recover: bool) -> Self {
        self.recover_sector_errors = recover;
        self
    }
}

/// Canonical uncompressed DSD frame yielded by the common source API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDsdFrame {
    pub frame_index: u64,
    pub data: Vec<u8>,
    pub channel_count: u16,
    pub sample_rate: u32,
    pub byte_order: DsdByteOrder,
    pub timecode: Option<Timecode>,
    pub is_final: bool,
}

/// Encoded DST frame yielded by the common source API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDstFrame {
    pub frame_index: u64,
    pub encoded: Vec<u8>,
    pub channel_count: u16,
    pub sample_rate: u32,
    pub timecode: Option<Timecode>,
    /// Optional raw DSDIFF `DSTC` checksum. ISO sectors do not carry this field;
    /// DSDIFF/DST file readers do.
    pub dstc: Option<u32>,
    /// Structured DSDIFF/DST CRC status. ISO sources report `NoCrc`; file
    /// sources report `NoCrc` or `PresentUnchecked` until decoded validation.
    pub crc_status: DstCrcStatus,
    /// Optional physical offset of the enclosing `DSTF` chunk header.
    pub chunk_offset: Option<u64>,
    pub is_final: bool,
}

impl SourceDstFrame {
    /// Decode to canonical DSD and validate the DSDIFF `DSTC` when present.
    pub fn decode_checked(&self) -> Result<SourceDsdFrame, DsdSourceError> {
        let channels = u8::try_from(self.channel_count).map_err(|_| {
            DsdSourceError::Unsupported {
                reason: format!("DST channel count {} exceeds decoder interface", self.channel_count),
            }
        })?;
        let rate = DstRate::from_sample_rate(self.sample_rate).map_err(DsdSourceError::Dst)?;
        let expected_len = rate
            .frame_bytes_per_channel()
            .and_then(|bytes| {
                bytes.checked_mul(usize::from(channels)).ok_or(
                    DstError::ArithmeticDecodeFailure("decoded DST frame byte count overflow"),
                )
            })
            .map_err(DsdSourceError::Dst)?;
        let mut decoded = vec![0u8; expected_len];
        let decoded_len = decode_frame_with_rate_into(
            &self.encoded,
            channels,
            rate,
            &mut decoded,
        )
        .map_err(DsdSourceError::Dst)?;
        if decoded_len != expected_len {
            return Err(DsdSourceError::Malformed {
                reason: format!(
                    "decoded DST frame {} has {} byte(s), expected {}",
                    self.frame_index,
                    decoded_len,
                    expected_len
                ),
            });
        }
        let crc_status = DstCrcStatus::verify(self.dstc, &decoded);
        if matches!(&crc_status, DstCrcStatus::PresentFailed { .. } | DstCrcStatus::Malformed { .. }) {
            return Err(DsdSourceError::Malformed {
                reason: format!("DSTC validation failed for frame {}: {}", self.frame_index, crc_status),
            });
        }
        Ok(SourceDsdFrame {
            frame_index: self.frame_index,
            data: decoded,
            channel_count: self.channel_count,
            sample_rate: self.sample_rate,
            byte_order: DsdByteOrder::MsbFirst,
            timecode: self.timecode,
            is_final: self.is_final,
        })
    }
}

/// Lossless source frame: either canonical DSD or encoded DST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DsdSourceFrame {
    Dsd(SourceDsdFrame),
    Dst(SourceDstFrame),
}

impl DsdSourceFrame {
    pub fn frame_index(&self) -> u64 {
        match self {
            Self::Dsd(f) => f.frame_index,
            Self::Dst(f) => f.frame_index,
        }
    }

    pub fn is_dst(&self) -> bool {
        matches!(self, Self::Dst(_))
    }

    pub fn into_decoded_dsd(self) -> Result<SourceDsdFrame, DsdSourceError> {
        match self {
            Self::Dsd(frame) => Ok(frame),
            Self::Dst(frame) => frame.decode_checked(),
        }
    }
}

/// Common lossless source reader. Callers that want to preserve source DST
/// payloads use this trait directly; callers that need DSD use
/// [`DecodedDsdSourceReader`].
pub trait DsdSource {
    fn source_info(&self) -> &DsdSourceInfo;
    fn next_source_frame(&mut self) -> Result<Option<DsdSourceFrame>, DsdSourceError>;
}

/// Optional frame-index seeking for the common source model.
pub trait DsdSourceSeek {
    fn source_frame_count(&self) -> Option<u64>;
    fn current_source_frame_index(&self) -> u64;
    fn seek_source_frame(&mut self, frame_index: u64) -> Result<(), DsdSourceError>;
}

/// Decoded view over any [`DsdSource`].
pub trait DecodedDsdSource {
    fn source_info(&self) -> &DsdSourceInfo;
    fn next_decoded_dsd_frame(&mut self) -> Result<Option<SourceDsdFrame>, DsdSourceError>;
}

/// Source-layer errors. These wrap the lower-level ISO frame reader,
/// DSF/DSDIFF stream readers, and DST decoder without erasing failure class.
#[derive(Debug)]
pub enum DsdSourceError {
    Io(io::Error),
    Frame(FrameError),
    Read(DsdReadError),
    Dst(DstError),
    Unsupported { reason: String },
    Malformed { reason: String },
}

impl fmt::Display for DsdSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error while reading DSD source: {}", e),
            Self::Frame(e) => write!(f, "SACD ISO frame-source error: {}", e),
            Self::Read(e) => write!(f, "DSD file-source error: {}", e),
            Self::Dst(e) => write!(f, "DST decode error in source adapter: {}", e),
            Self::Unsupported { reason } => write!(f, "unsupported DSD source: {}", reason),
            Self::Malformed { reason } => write!(f, "malformed DSD source: {}", reason),
        }
    }
}

impl std::error::Error for DsdSourceError {}

impl From<io::Error> for DsdSourceError {
    fn from(e: io::Error) -> Self { Self::Io(e) }
}
impl From<FrameError> for DsdSourceError {
    fn from(e: FrameError) -> Self { Self::Frame(e) }
}
impl From<DsdReadError> for DsdSourceError {
    fn from(e: DsdReadError) -> Self { Self::Read(e) }
}
impl From<DstError> for DsdSourceError {
    fn from(e: DstError) -> Self { Self::Dst(e) }
}

/// SACD ISO track-range implementation of [`DsdSource`].
pub struct IsoTrackSource<'a> {
    reader: FrameReader<'a>,
    info: DsdSourceInfo,
    frame_index: u64,
}

impl<'a> IsoTrackSource<'a> {
    pub fn new(iso: &'a mut IsoReader, opts: IsoTrackSourceOptions) -> Self {
        let mut reader = FrameReader::new(iso, opts.start_lsn, opts.end_lsn);
        reader.set_expected_channel_count(opts.channel_count);
        if let Some(frame_format) = opts.frame_format {
            reader.set_expected_frame_format(frame_format);
        }
        reader.set_strict_channel_count(opts.strict_channel_count);
        reader.set_recover_sector_errors(opts.recover_sector_errors);
        if let Some(filter) = opts.time_filter {
            reader.set_timecode_filter(filter.start_frame, filter.end_frame.saturating_sub(filter.start_frame));
        }
        let info = DsdSourceInfo::sacd_iso_track(&opts);
        Self { reader, info, frame_index: 0 }
    }

    pub fn frame_reader_stats(&self) -> FrameReaderStats {
        self.reader.stats()
    }
}

impl DsdSource for IsoTrackSource<'_> {
    fn source_info(&self) -> &DsdSourceInfo {
        &self.info
    }

    fn next_source_frame(&mut self) -> Result<Option<DsdSourceFrame>, DsdSourceError> {
        let Some(frame) = self.reader.next_frame()? else {
            return Ok(None);
        };
        let frame_index = self.frame_index;
        self.frame_index = self.frame_index.checked_add(1).ok_or_else(|| {
            DsdSourceError::Malformed { reason: "ISO source frame index overflow".to_string() }
        })?;
        if frame.dst_encoded {
            Ok(Some(DsdSourceFrame::Dst(SourceDstFrame {
                frame_index,
                encoded: frame.data,
                channel_count: self.info.channel_count,
                sample_rate: self.info.sample_rate,
                timecode: Some(frame.timecode),
                dstc: None,
                crc_status: DstCrcStatus::NoCrc,
                chunk_offset: None,
                is_final: false,
            })))
        } else {
            Ok(Some(DsdSourceFrame::Dsd(SourceDsdFrame {
                frame_index,
                data: frame.data,
                channel_count: self.info.channel_count,
                sample_rate: self.info.sample_rate,
                byte_order: DsdByteOrder::MsbFirst,
                timecode: Some(frame.timecode),
                is_final: false,
            })))
        }
    }
}

/// File-backed common source over DSF, DSDIFF/DSD, or DSDIFF/DST.
pub struct DsdFileSource<R: Read + Seek> {
    reader: DsdFileReader<R>,
    info: DsdSourceInfo,
}

impl<R: Read + Seek> DsdFileSource<R> {
    pub fn new(reader: R) -> Result<Self, DsdSourceError> {
        let reader = open_dsd_file(reader)?;
        let info = DsdSourceInfo::from_container(reader.info().clone());
        Ok(Self { reader, info })
    }

    /// Build a common source from an already-open typed file reader. This is
    /// used by the asset layer after it has performed metadata inspection.
    pub fn from_open_reader(reader: DsdFileReader<R>) -> Self {
        let info = DsdSourceInfo::from_container(reader.info().clone());
        Self { reader, info }
    }

    pub fn reader_mut(&mut self) -> &mut DsdFileReader<R> {
        &mut self.reader
    }

    pub fn into_inner(self) -> DsdFileReader<R> {
        self.reader
    }
}

/// Detect DSF, DSDIFF/DSD, or DSDIFF/DST and expose it as a common source.
pub fn open_dsd_source<R: Read + Seek>(reader: R) -> Result<DsdFileSource<R>, DsdSourceError> {
    DsdFileSource::new(reader)
}

impl<R: Read + Seek> DsdSource for DsdFileSource<R> {
    fn source_info(&self) -> &DsdSourceInfo {
        &self.info
    }

    fn next_source_frame(&mut self) -> Result<Option<DsdSourceFrame>, DsdSourceError> {
        match &mut self.reader {
            DsdFileReader::Dsf(r) => dsd_stream_frame_to_source(r.next_dsd_frame()?),
            DsdFileReader::DsdiffDsd(r) => dsd_stream_frame_to_source(r.next_dsd_frame()?),
            DsdFileReader::DsdiffDst(r) => {
                let Some(frame) = r.next_dst_frame()? else { return Ok(None); };
                Ok(Some(DsdSourceFrame::Dst(SourceDstFrame {
                    frame_index: frame.frame_index,
                    encoded: frame.encoded,
                    channel_count: frame.channel_count,
                    sample_rate: frame.sample_rate,
                    timecode: None,
                    dstc: frame.dstc,
                    crc_status: frame.crc_status,
                    chunk_offset: Some(frame.chunk_offset),
                    is_final: frame.is_final,
                })))
            }
        }
    }
}

impl<R: Read + Seek> DsdSourceSeek for DsdFileSource<R> {
    fn source_frame_count(&self) -> Option<u64> {
        match &self.reader {
            DsdFileReader::Dsf(r) => r.frame_count(),
            DsdFileReader::DsdiffDsd(r) => r.frame_count(),
            DsdFileReader::DsdiffDst(r) => r.frame_count(),
        }
    }

    fn current_source_frame_index(&self) -> u64 {
        match &self.reader {
            DsdFileReader::Dsf(r) => r.current_frame_index(),
            DsdFileReader::DsdiffDsd(r) => r.current_frame_index(),
            DsdFileReader::DsdiffDst(r) => r.current_frame_index(),
        }
    }

    fn seek_source_frame(&mut self, frame_index: u64) -> Result<(), DsdSourceError> {
        match &mut self.reader {
            DsdFileReader::Dsf(r) => r.seek_frame(frame_index)?,
            DsdFileReader::DsdiffDsd(r) => r.seek_frame(frame_index)?,
            DsdFileReader::DsdiffDst(r) => r.seek_frame(frame_index)?,
        }
        Ok(())
    }
}

/// Decode-on-read adapter for any common source.
pub struct SourceToDsdAdapter<S: DsdSource> {
    inner: S,
    decoded_frame_index: u64,
}

impl<S: DsdSource> SourceToDsdAdapter<S> {
    pub fn new(inner: S) -> Self {
        Self { inner, decoded_frame_index: 0 }
    }

    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: DsdSource> DecodedDsdSource for SourceToDsdAdapter<S> {
    fn source_info(&self) -> &DsdSourceInfo {
        self.inner.source_info()
    }

    fn next_decoded_dsd_frame(&mut self) -> Result<Option<SourceDsdFrame>, DsdSourceError> {
        let Some(frame) = self.inner.next_source_frame()? else { return Ok(None); };
        let mut decoded = frame.into_decoded_dsd()?;
        decoded.frame_index = self.decoded_frame_index;
        self.decoded_frame_index = self.decoded_frame_index.checked_add(1).ok_or_else(|| {
            DsdSourceError::Malformed { reason: "decoded source frame index overflow".to_string() }
        })?;
        Ok(Some(decoded))
    }
}

impl<S: DsdSource + DsdSourceSeek> DsdSourceSeek for SourceToDsdAdapter<S> {
    fn source_frame_count(&self) -> Option<u64> {
        self.inner.source_frame_count()
    }

    fn current_source_frame_index(&self) -> u64 {
        self.decoded_frame_index
    }

    fn seek_source_frame(&mut self, frame_index: u64) -> Result<(), DsdSourceError> {
        self.inner.seek_source_frame(frame_index)?;
        self.decoded_frame_index = frame_index;
        Ok(())
    }
}

impl<T: DecodedDsdSource + ?Sized> DecodedDsdSource for Box<T> {
    fn source_info(&self) -> &DsdSourceInfo {
        (**self).source_info()
    }

    fn next_decoded_dsd_frame(&mut self) -> Result<Option<SourceDsdFrame>, DsdSourceError> {
        (**self).next_decoded_dsd_frame()
    }
}

fn dsd_stream_frame_to_source(frame: Option<DsdFrame>) -> Result<Option<DsdSourceFrame>, DsdSourceError> {
    let Some(frame) = frame else { return Ok(None); };
    Ok(Some(DsdSourceFrame::Dsd(SourceDsdFrame {
        frame_index: frame.frame_index,
        data: frame.data,
        channel_count: frame.channel_count,
        sample_rate: frame.sample_rate,
        byte_order: frame.byte_order,
        timecode: None,
        is_final: frame.is_final,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::DSD64_SAMPLE_RATE;
    use crate::dff_dst_writer::DffDstWriter;
    use crate::dff_writer::DffWriter;
    use crate::frame::{FrameFormat, FRAME_SIZE_UNCOMPRESSED, Timecode};
    use crate::test_util::{synth_audio_sector, synth_continuation_sector, write_iso};
    use std::io::Cursor;

    const PART_SIZE: usize = 2000;

    fn synth_uncompressed_frame_sectors(frame_bytes: &[u8], tc: Timecode) -> Vec<Vec<u8>> {
        let mut sectors = Vec::new();
        let first = frame_bytes.len().min(PART_SIZE);
        sectors.push(synth_audio_sector(true, &frame_bytes[..first], tc));
        let mut off = first;
        while off < frame_bytes.len() {
            let chunk = (frame_bytes.len() - off).min(PART_SIZE);
            sectors.push(synth_continuation_sector(&frame_bytes[off..off + chunk]));
            off += chunk;
        }
        sectors
    }

    #[test]
    fn iso_track_source_yields_common_dsd_frame() {
        let frame = vec![0xa5; FRAME_SIZE_UNCOMPRESSED * 2];
        let sectors = synth_uncompressed_frame_sectors(&frame, Timecode { minutes: 0, seconds: 0, frames: 1 });
        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let opts = IsoTrackSourceOptions::new(0, sectors.len() as u64, 2, DSD64_SAMPLE_RATE)
            .with_frame_format(FrameFormat::Dsd3In14);
        let mut source = IsoTrackSource::new(&mut iso, opts);
        assert_eq!(source.source_info().kind, DsdSourceKind::SacdIsoTrack);
        let got = source.next_source_frame().unwrap().unwrap();
        match got {
            DsdSourceFrame::Dsd(dsd) => {
                assert_eq!(dsd.data, frame);
                assert_eq!(dsd.channel_count, 2);
                assert_eq!(dsd.timecode.unwrap().frames, 1);
            }
            DsdSourceFrame::Dst(_) => panic!("expected plain DSD"),
        }
        assert!(source.next_source_frame().unwrap().is_none());
        assert_eq!(source.frame_reader_stats().frames_emitted, 1);
    }

    #[test]
    fn dsdiff_dsd_file_source_yields_common_dsd_frame() {
        let frame = vec![0x3c; FRAME_SIZE_UNCOMPRESSED * 2];
        let mut out = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffWriter::new(&mut out, 2, DSD64_SAMPLE_RATE).unwrap();
            writer.write_frame(&frame).unwrap();
            writer.finish().unwrap();
        }
        out.set_position(0);
        let mut source = open_dsd_source(out).unwrap();
        assert_eq!(source.source_info().kind, DsdSourceKind::DsdiffDsd);
        let got = source.next_source_frame().unwrap().unwrap();
        match got {
            DsdSourceFrame::Dsd(dsd) => assert_eq!(dsd.data, frame),
            DsdSourceFrame::Dst(_) => panic!("expected DSDIFF/DSD source frame"),
        }
    }

    #[test]
    fn dsdiff_dst_file_source_preserves_dst_until_adapter_decodes() {
        let frame = vec![0; FRAME_SIZE_UNCOMPRESSED * 2];
        let mut out = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffDstWriter::new(&mut out, 2, DSD64_SAMPLE_RATE).unwrap();
            writer.write_interleaved_frame_allowing_raw_fallback(&frame).unwrap();
            writer.finish().unwrap();
        }
        let bytes = out.into_inner();
        let mut source = open_dsd_source(Cursor::new(bytes.clone())).unwrap();
        assert_eq!(source.source_info().kind, DsdSourceKind::DsdiffDst);
        let got = source.next_source_frame().unwrap().unwrap();
        assert!(got.is_dst());

        let source = open_dsd_source(Cursor::new(bytes)).unwrap();
        let mut decoded = SourceToDsdAdapter::new(source);
        let got = decoded.next_decoded_dsd_frame().unwrap().unwrap();
        assert_eq!(got.data, frame);
        assert!(decoded.next_decoded_dsd_frame().unwrap().is_none());
    }

    #[test]
    fn file_source_seek_uses_common_trait() {
        let frame_a = vec![0x11; FRAME_SIZE_UNCOMPRESSED * 2];
        let frame_b = vec![0x22; FRAME_SIZE_UNCOMPRESSED * 2];
        let mut out = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffWriter::new(&mut out, 2, DSD64_SAMPLE_RATE).unwrap();
            writer.write_frame(&frame_a).unwrap();
            writer.write_frame(&frame_b).unwrap();
            writer.finish().unwrap();
        }
        out.set_position(0);
        let mut source = open_dsd_source(out).unwrap();
        assert_eq!(source.source_frame_count(), Some(2));
        source.seek_source_frame(1).unwrap();
        let got = source.next_source_frame().unwrap().unwrap();
        match got {
            DsdSourceFrame::Dsd(dsd) => assert_eq!(dsd.data, frame_b),
            DsdSourceFrame::Dst(_) => panic!("expected DSD"),
        }
    }
}

/// Aggregate counters for decoded-source drains and copy/conversion paths.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DsdSourceDrainStats {
    pub frames_read: u64,
    pub bytes_read: u64,
}

/// Drain any decoded DSD source into a caller-supplied sink.
///
/// This is the common inner loop for future DSF/DFF/file/ISO conversion paths:
/// source selection and DST decoding live outside the sink, while the sink only
/// sees canonical DSD bytes.
pub fn drain_decoded_dsd_source<S, F>(
    source: &mut S,
    mut write_frame: F,
) -> Result<DsdSourceDrainStats, DsdSourceError>
where
    S: DecodedDsdSource + ?Sized,
    F: FnMut(&SourceDsdFrame) -> Result<(), DsdSourceError>,
{
    let mut stats = DsdSourceDrainStats::default();
    while let Some(frame) = source.next_decoded_dsd_frame()? {
        write_frame(&frame)?;
        stats.frames_read = stats.frames_read.checked_add(1).ok_or_else(|| {
            DsdSourceError::Malformed { reason: "decoded source frame counter overflow".to_string() }
        })?;
        stats.bytes_read = stats.bytes_read.checked_add(frame.data.len() as u64).ok_or_else(|| {
            DsdSourceError::Malformed { reason: "decoded source byte counter overflow".to_string() }
        })?;
    }
    Ok(stats)
}
