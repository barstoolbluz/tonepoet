// SPDX-License-Identifier: GPL-2.0-or-later
//! Asset-level model for SACD ISO tracks and DSF/DSDIFF files.
//!
//! The streaming/source layers answer "give me the next DSD or DST frame".
//! This module answers the higher-level question a planner, validator, TUI, or
//! metadata inspector usually needs first: "what audio asset is this, what audio
//! stream(s) does it expose, what metadata/provenance is known, and how should I
//! open the canonical source stream?"
//!
//! The model deliberately stays conservative:
//!
//! - one physical audio stream per DSF/DFF/DST file and per SACD track asset;
//! - raw metadata bytes are preserved where this crate can locate them safely;
//! - SACD textual metadata can be supplied by tonepoet's ScarletBook parser
//!   without making this crate depend on the TUI module;
//! - audio streaming still flows through [`crate::dsd_file::source::DsdSource`].

use crate::dsd_file::inspect::{
    DsdByteOrder, DsdCompression, DsdContainerDiagnostic, DsdContainerFormat, DsdContainerInfo,
};
use crate::frame::FrameFormat;
use crate::iso_reader::IsoReader;
use crate::dsd_file::source::{
    open_dsd_source, DsdFileSource, DsdSource, DsdSourceError, DsdSourceInfo, DsdSourceKind,
    IsoTrackRange, IsoTrackSource, IsoTrackSourceOptions,
};
use crate::dsd_file::reader::DsdReadError;
use std::fmt;
use std::io::{self, Read, Seek};

/// Stable asset family used by UI, planning, validation, and conversion code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdAssetKind {
    SacdIsoTrack,
    DsfFile,
    DsdiffDsdFile,
    DsdiffDstFile,
}

impl fmt::Display for DsdAssetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SacdIsoTrack => f.write_str("SACD ISO track"),
            Self::DsfFile => f.write_str("DSF file"),
            Self::DsdiffDsdFile => f.write_str("DSDIFF/DSD file"),
            Self::DsdiffDstFile => f.write_str("DSDIFF/DST file"),
        }
    }
}

/// One logical audio stream inside a DSD asset.
///
/// Current supported assets expose exactly one stream. The vector shape is
/// intentional: ScarletBook multi-area browsing, future multi-program file
/// containers, and planner UIs can grow without changing callers that already
/// iterate over streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdAudioStreamInfo {
    pub stream_index: u16,
    pub channel_count: u16,
    pub sample_rate: u32,
    pub compression: DsdCompression,
    pub byte_order: DsdByteOrder,
    pub sample_count_per_channel: Option<u64>,
    pub frame_count: Option<u64>,
    pub iso_range: Option<IsoTrackRange>,
    pub container_format: Option<DsdContainerFormat>,
    pub frame_format_hint: Option<FrameFormat>,
    pub channel_ids: Vec<[u8; 4]>,
}

impl DsdAudioStreamInfo {
    pub fn duration_seconds(&self) -> Option<f64> {
        let samples = self.sample_count_per_channel?;
        (self.sample_rate != 0).then_some(samples as f64 / self.sample_rate as f64)
    }

    pub fn is_dst_encoded(&self) -> bool {
        self.compression == DsdCompression::Dst
    }

    fn from_source_info(index: u16, info: &DsdSourceInfo) -> Self {
        let container = info.container.as_ref();
        Self {
            stream_index: index,
            channel_count: info.channel_count,
            sample_rate: info.sample_rate,
            compression: info.compression,
            byte_order: container.map(|c| c.byte_order).unwrap_or(DsdByteOrder::MsbFirst),
            sample_count_per_channel: info.sample_count_per_channel,
            frame_count: None,
            iso_range: info.iso_range,
            container_format: container.map(|c| c.format),
            frame_format_hint: None,
            channel_ids: container.map(|c| c.channel_ids.clone()).unwrap_or_default(),
        }
    }

    fn from_iso_options(index: u16, opts: &IsoTrackSourceOptions) -> Self {
        let compression = match opts.frame_format {
            Some(ff) if ff.is_dst_encoded() => DsdCompression::Dst,
            Some(_) => DsdCompression::Dsd,
            None => DsdCompression::Unknown(*b"MIXD"),
        };
        Self {
            stream_index: index,
            channel_count: u16::from(opts.channel_count),
            sample_rate: opts.sample_rate,
            compression,
            byte_order: DsdByteOrder::MsbFirst,
            sample_count_per_channel: None,
            frame_count: None,
            iso_range: Some(IsoTrackRange { start_lsn: opts.start_lsn, end_lsn: opts.end_lsn }),
            container_format: None,
            frame_format_hint: opts.frame_format,
            channel_ids: Vec::new(),
        }
    }
}

/// Metadata fields common enough to use in planning/UI without binding this
/// crate to tonepoet's ScarletBook parser or to a full ID3 implementation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DsdAssetMetadata {
    pub album_title: Option<String>,
    pub track_title: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub composer: Option<String>,
    pub genre: Option<String>,
    pub date: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub isrc: Option<String>,
    pub catalog_number: Option<String>,
    /// Raw DSF ID3v2 tag bytes when safely locatable. This module intentionally
    /// preserves bytes instead of claiming a complete ID3 parser.
    pub raw_id3v2: Option<Vec<u8>>,
    /// Raw DSDIFF footer/metadata chunks when a caller supplies them. The
    /// current DSDIFF stream reader validates footer placement but does not yet
    /// parse all footer chunk semantics.
    pub raw_dsdiff_footer: Option<Vec<u8>>,
    pub notes: Vec<String>,
}

impl DsdAssetMetadata {
    pub fn has_user_metadata(&self) -> bool {
        self.album_title.is_some()
            || self.track_title.is_some()
            || self.artist.is_some()
            || self.album_artist.is_some()
            || self.composer.is_some()
            || self.genre.is_some()
            || self.date.is_some()
            || self.track_number.is_some()
            || self.disc_number.is_some()
            || self.isrc.is_some()
            || self.catalog_number.is_some()
            || self.raw_id3v2.is_some()
            || self.raw_dsdiff_footer.is_some()
    }

    pub fn with_track_title(mut self, title: impl Into<String>) -> Self {
        self.track_title = Some(title.into());
        self
    }

    pub fn with_album_title(mut self, title: impl Into<String>) -> Self {
        self.album_title = Some(title.into());
        self
    }

    pub fn with_artist(mut self, artist: impl Into<String>) -> Self {
        self.artist = Some(artist.into());
        self
    }

    pub fn with_isrc(mut self, isrc: impl Into<String>) -> Self {
        self.isrc = Some(isrc.into());
        self
    }
}

/// Provenance retained for auditability and conversion planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdAssetProvenance {
    pub kind: DsdAssetKind,
    pub iso_range: Option<IsoTrackRange>,
    pub container: Option<DsdContainerInfo>,
    pub diagnostics: Vec<DsdContainerDiagnostic>,
}

impl DsdAssetProvenance {
    fn from_source_info(kind: DsdAssetKind, info: &DsdSourceInfo) -> Self {
        Self {
            kind,
            iso_range: info.iso_range,
            diagnostics: info
                .container
                .as_ref()
                .map(|c| c.diagnostics.clone())
                .unwrap_or_default(),
            container: info.container.clone(),
        }
    }
}

/// Asset-level description. This is the object metadata inspectors and planners
/// should use before they select a source/sink pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdAssetInfo {
    pub kind: DsdAssetKind,
    pub streams: Vec<DsdAudioStreamInfo>,
    pub metadata: DsdAssetMetadata,
    pub provenance: DsdAssetProvenance,
}

impl DsdAssetInfo {
    pub fn primary_stream(&self) -> Option<&DsdAudioStreamInfo> {
        self.streams.first()
    }

    pub fn duration_seconds(&self) -> Option<f64> {
        self.primary_stream()?.duration_seconds()
    }

    pub fn channel_count(&self) -> Option<u16> {
        Some(self.primary_stream()?.channel_count)
    }

    pub fn sample_rate(&self) -> Option<u32> {
        Some(self.primary_stream()?.sample_rate)
    }

    pub fn compression(&self) -> Option<DsdCompression> {
        Some(self.primary_stream()?.compression)
    }

    pub fn is_dst_encoded(&self) -> bool {
        self.primary_stream().map(|s| s.is_dst_encoded()).unwrap_or(false)
    }

    pub fn has_container_errors(&self) -> bool {
        self.provenance
            .container
            .as_ref()
            .map(|c| c.has_errors())
            .unwrap_or(false)
    }

    pub fn summary_line(&self) -> String {
        let stream = match self.primary_stream() {
            Some(s) => s,
            None => return format!("{}: no audio streams", self.kind),
        };
        let duration = stream
            .duration_seconds()
            .map(|d| format!(", {:.3}s", d))
            .unwrap_or_default();
        format!(
            "{}: {} ch, {} Hz, {}{}",
            self.kind, stream.channel_count, stream.sample_rate, stream.compression, duration
        )
    }

    fn from_source_info(kind: DsdAssetKind, info: &DsdSourceInfo, metadata: DsdAssetMetadata) -> Self {
        Self {
            kind,
            streams: vec![DsdAudioStreamInfo::from_source_info(0, info)],
            metadata,
            provenance: DsdAssetProvenance::from_source_info(kind, info),
        }
    }

    fn from_iso_options(opts: &IsoTrackSourceOptions, metadata: DsdAssetMetadata) -> Self {
        let source_info = DsdSourceInfo::sacd_iso_track(opts);
        Self {
            kind: DsdAssetKind::SacdIsoTrack,
            streams: vec![DsdAudioStreamInfo::from_iso_options(0, opts)],
            metadata,
            provenance: DsdAssetProvenance::from_source_info(DsdAssetKind::SacdIsoTrack, &source_info),
        }
    }
}

/// Asset-model errors. The streaming errors are left intact so callers can
/// still distinguish malformed containers from unsupported layouts.
#[derive(Debug)]
pub enum DsdAssetError {
    Io(io::Error),
    Read(DsdReadError),
    Source(DsdSourceError),
    Unsupported { reason: String },
}

impl fmt::Display for DsdAssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error while opening DSD asset: {}", e),
            Self::Read(e) => write!(f, "DSD asset reader error: {}", e),
            Self::Source(e) => write!(f, "DSD asset source error: {}", e),
            Self::Unsupported { reason } => write!(f, "unsupported DSD asset: {}", reason),
        }
    }
}

impl std::error::Error for DsdAssetError {}

impl From<io::Error> for DsdAssetError {
    fn from(e: io::Error) -> Self { Self::Io(e) }
}
impl From<DsdReadError> for DsdAssetError {
    fn from(e: DsdReadError) -> Self { Self::Read(e) }
}
impl From<DsdSourceError> for DsdAssetError {
    fn from(e: DsdSourceError) -> Self { Self::Source(e) }
}

/// Common asset trait for code that only needs stable description before it
/// decides which concrete source pipeline to open.
pub trait DsdAsset {
    fn asset_info(&self) -> &DsdAssetInfo;
}

/// File-backed DSF/DSDIFF asset. The reader is already opened and validated;
/// call [`Self::into_source`] when ready to stream frames.
pub struct DsdFileAsset<R: Read + Seek> {
    source: DsdFileSource<R>,
    info: DsdAssetInfo,
}

impl<R: Read + Seek> DsdFileAsset<R> {
    pub fn open(reader: R) -> Result<Self, DsdAssetError> {
        let mut source = open_dsd_source(reader)?;
        let kind = asset_kind_for_source(source.source_info().kind);
        let mut metadata = DsdAssetMetadata::default();
        if let Some(id3) = source.reader_mut().read_dsf_id3_footer()? {
            metadata.raw_id3v2 = Some(id3);
        }
        let info = DsdAssetInfo::from_source_info(kind, source.source_info(), metadata);
        Ok(Self { source, info })
    }

    pub fn into_source(self) -> DsdFileSource<R> {
        self.source
    }

    pub fn source_info(&self) -> &DsdSourceInfo {
        self.source.source_info()
    }
}

impl<R: Read + Seek> DsdAsset for DsdFileAsset<R> {
    fn asset_info(&self) -> &DsdAssetInfo {
        &self.info
    }
}

/// Detect DSF, DSDIFF/DSD, or DSDIFF/DST and expose asset-level metadata plus
/// the common source stream.
pub fn open_dsd_asset<R: Read + Seek>(reader: R) -> Result<DsdFileAsset<R>, DsdAssetError> {
    DsdFileAsset::open(reader)
}

/// SACD ISO track asset descriptor. The ISO reader itself is supplied only when
/// the caller actually opens the source, which lets a TUI/planner build many
/// track assets from parsed TOC state without holding many mutable ISO borrows.
#[derive(Debug, Clone)]
pub struct SacdIsoTrackAsset {
    options: IsoTrackSourceOptions,
    info: DsdAssetInfo,
}

impl SacdIsoTrackAsset {
    pub fn new(options: IsoTrackSourceOptions) -> Self {
        Self::with_metadata(options, DsdAssetMetadata::default())
    }

    pub fn with_metadata(options: IsoTrackSourceOptions, metadata: DsdAssetMetadata) -> Self {
        let info = DsdAssetInfo::from_iso_options(&options, metadata);
        Self { options, info }
    }

    pub fn source_options(&self) -> &IsoTrackSourceOptions {
        &self.options
    }

    pub fn open_source<'a>(&self, iso: &'a mut IsoReader) -> IsoTrackSource<'a> {
        IsoTrackSource::new(iso, self.options.clone())
    }
}

impl DsdAsset for SacdIsoTrackAsset {
    fn asset_info(&self) -> &DsdAssetInfo {
        &self.info
    }
}

fn asset_kind_for_source(kind: DsdSourceKind) -> DsdAssetKind {
    match kind {
        DsdSourceKind::SacdIsoTrack => DsdAssetKind::SacdIsoTrack,
        DsdSourceKind::Dsf => DsdAssetKind::DsfFile,
        DsdSourceKind::DsdiffDsd => DsdAssetKind::DsdiffDsdFile,
        DsdSourceKind::DsdiffDst => DsdAssetKind::DsdiffDstFile,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::DSD64_SAMPLE_RATE;
    use crate::dff_dst_writer::DffDstWriter;
    use crate::dff_writer::DffWriter;
    use crate::dsf_writer::{DsfWriter, SACD_SAMPLING_FREQUENCY};
    use crate::frame::{FrameFormat, FRAME_SIZE_UNCOMPRESSED, Timecode};
    use crate::dsd_file::source::DsdSourceFrame;
    use crate::test_util::{synth_audio_sector, synth_continuation_sector, write_iso};
    use std::io::Cursor;

    fn synth_uncompressed_frame_sectors(frame_bytes: &[u8], tc: Timecode) -> Vec<Vec<u8>> {
        const PART_SIZE: usize = 2000;
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
    fn dsf_asset_exposes_metadata_and_canonical_stream() {
        let frame = vec![0xa5; FRAME_SIZE_UNCOMPRESSED * 2];
        let id3 = b"ID3\x04\x00\x00\x00\x00\x00\x04TEST".to_vec();
        let mut out = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DsfWriter::new(&mut out, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.set_id3_footer(id3.clone());
            writer.write_interleaved(&frame).unwrap();
            writer.finish().unwrap();
        }
        out.set_position(0);
        let asset = open_dsd_asset(out).unwrap();
        assert_eq!(asset.asset_info().kind, DsdAssetKind::DsfFile);
        assert_eq!(asset.asset_info().channel_count(), Some(2));
        assert_eq!(asset.asset_info().sample_rate(), Some(SACD_SAMPLING_FREQUENCY));
        assert_eq!(asset.asset_info().metadata.raw_id3v2, Some(id3));
        assert!(asset.asset_info().summary_line().contains("DSF file"));

        let mut source = asset.into_source();
        let mut decoded = Vec::new();
        while let Some(got) = source.next_source_frame().unwrap() {
            match got {
                DsdSourceFrame::Dsd(dsd) => decoded.extend_from_slice(&dsd.data),
                DsdSourceFrame::Dst(_) => panic!("expected DSF to expose DSD source frames"),
            }
        }
        assert_eq!(decoded, frame);
    }

    #[test]
    fn dsdiff_dsd_asset_maps_to_single_audio_stream() {
        let frame = vec![0x3c; FRAME_SIZE_UNCOMPRESSED * 2];
        let mut out = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffWriter::new(&mut out, 2, DSD64_SAMPLE_RATE).unwrap();
            writer.write_frame(&frame).unwrap();
            writer.finish().unwrap();
        }
        out.set_position(0);
        let asset = open_dsd_asset(out).unwrap();
        let stream = asset.asset_info().primary_stream().unwrap();
        assert_eq!(asset.asset_info().kind, DsdAssetKind::DsdiffDsdFile);
        assert_eq!(stream.compression, DsdCompression::Dsd);
        assert_eq!(stream.container_format, Some(DsdContainerFormat::Dsdiff));
        assert_eq!(stream.byte_order, DsdByteOrder::MsbFirst);
    }

    #[test]
    fn dsdiff_dst_asset_preserves_dst_source_until_streamed() {
        let frame = vec![0; FRAME_SIZE_UNCOMPRESSED * 2];
        let mut out = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffDstWriter::new(&mut out, 2, DSD64_SAMPLE_RATE).unwrap();
            writer.write_interleaved_frame_allowing_raw_fallback(&frame).unwrap();
            writer.finish().unwrap();
        }
        out.set_position(0);
        let asset = open_dsd_asset(out).unwrap();
        assert_eq!(asset.asset_info().kind, DsdAssetKind::DsdiffDstFile);
        assert_eq!(asset.asset_info().compression(), Some(DsdCompression::Dst));
        assert!(asset.asset_info().is_dst_encoded());

        let mut source = asset.into_source();
        let got = source.next_source_frame().unwrap().unwrap();
        assert!(matches!(got, DsdSourceFrame::Dst(_)));
    }

    #[test]
    fn sacd_iso_asset_uses_supplied_metadata_without_opening_audio() {
        let opts = IsoTrackSourceOptions::new(0, 1, 2, DSD64_SAMPLE_RATE)
            .with_frame_format(FrameFormat::Dsd3In14);
        let meta = DsdAssetMetadata::default()
            .with_album_title("Album")
            .with_track_title("Track")
            .with_artist("Artist")
            .with_isrc("USXXX2600001");
        let asset = SacdIsoTrackAsset::with_metadata(opts, meta.clone());
        assert_eq!(asset.asset_info().kind, DsdAssetKind::SacdIsoTrack);
        assert_eq!(asset.asset_info().metadata, meta);
        assert_eq!(asset.asset_info().primary_stream().unwrap().iso_range.unwrap().end_lsn, 1);
    }

    #[test]
    fn sacd_iso_asset_opens_common_source_when_requested() {
        let frame = vec![0x55; FRAME_SIZE_UNCOMPRESSED * 2];
        let sectors = synth_uncompressed_frame_sectors(&frame, Timecode { minutes: 0, seconds: 0, frames: 0 });
        let td = write_iso(&sectors);
        let opts = IsoTrackSourceOptions::new(0, sectors.len() as u64, 2, DSD64_SAMPLE_RATE)
            .with_frame_format(FrameFormat::Dsd3In14);
        let asset = SacdIsoTrackAsset::new(opts);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut source = asset.open_source(&mut iso);
        match source.next_source_frame().unwrap().unwrap() {
            DsdSourceFrame::Dsd(dsd) => assert_eq!(dsd.data, frame),
            DsdSourceFrame::Dst(_) => panic!("expected DSD"),
        }
    }
}
