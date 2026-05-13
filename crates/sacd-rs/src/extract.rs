//! Track extraction orchestration: read DSD frames from an ISO sector
//! range, demux/passthrough to the selected output format, write a
//! valid DSF or DSDIFF file.
//!
//! ## Scope
//!
//! This module handles **uncompressed DSD only**. DST-encoded frames
//! cause `extract_track` to return `ExtractError::DstFrameUnsupported`.
//! DST decode lands in a later PR.
//!
//! ## Error semantics
//!
//! On any error mid-stream, the output writer is dropped without
//! calling `finish()`. The output file ends up with the placeholder
//! header (zero chunk sizes) plus the partial audio bytes from
//! sectors that succeeded before the error. The file is structurally
//! a valid DSF/DSDIFF but reports zero audio data in its header —
//! parsers will treat it as empty. **Callers should discard the
//! output file on any error.**
//!
//! ## Channel-count parameter
//!
//! `channel_count` must match the SACD area's actual channel layout.
//! Uncompressed DSD frames don't self-describe channel count
//! (`Frame.channel_count` is always 0 for uncompressed); the
//! orchestrator trusts the caller's parameter and never validates.

use crate::dff_writer::DffWriter;
use crate::dsf_writer::{DsfWriter, SACD_SAMPLING_FREQUENCY};
use crate::frame::{FrameError, FrameReader};
use crate::iso_reader::IsoReader;
use std::io::{self, Seek, Write};

/// Output container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Sony DSF (.dsf). Per-channel deinterleaved, LSB-first byte
    /// ordering, 4096-byte blocks per channel.
    Dsf,
    /// Philips DSDIFF (.dff). Clustered-frame passthrough,
    /// MSB-first byte ordering.
    Dff,
}

/// Errors from `extract_track`.
#[derive(Debug)]
pub enum ExtractError {
    /// Failure parsing the ISO frame stream.
    Frame(FrameError),
    /// Failure writing to the output sink.
    Io(io::Error),
    /// Encountered a DST-encoded frame. DST decode is not yet
    /// implemented; the caller should fall back to a different
    /// extraction path (e.g. the C sacd-extract binary) or convert
    /// the ISO to DST-decoded form externally.
    DstFrameUnsupported,
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Frame(e) => write!(f, "frame read error: {}", e),
            Self::Io(e) => write!(f, "output write error: {}", e),
            Self::DstFrameUnsupported => write!(f, "DST-encoded frames are not yet supported"),
        }
    }
}

impl std::error::Error for ExtractError {}

impl From<FrameError> for ExtractError {
    fn from(e: FrameError) -> Self {
        Self::Frame(e)
    }
}

impl From<io::Error> for ExtractError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Summary returned on successful extraction.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtractStats {
    /// Number of complete frames read from the ISO.
    pub frames_read: u64,
    /// Total audio bytes pushed to the writer (pre-pad).
    pub audio_bytes: u64,
}

/// Extract a single track's DSD audio from `iso` (sector range
/// `[start_lsn, end_lsn)`) into `output`, formatted per `format`.
///
/// `channel_count` is the area's channel layout (2, 5, or 6 for real
/// SACDs). It populates the output container's metadata; uncompressed
/// DSD frames don't carry channel count themselves, so the value
/// must match the area_toc.
///
/// On error, the output is left in an inconsistent state (header
/// shows zero audio but file contains partial bytes). Caller should
/// delete the output.
pub fn extract_track<W: Write + Seek>(
    iso: &mut IsoReader,
    output: &mut W,
    start_lsn: u64,
    end_lsn: u64,
    channel_count: u8,
    format: OutputFormat,
) -> Result<ExtractStats, ExtractError> {
    let mut reader = FrameReader::new(iso, start_lsn, end_lsn);

    match format {
        OutputFormat::Dsf => {
            let mut writer = DsfWriter::new(output, channel_count, SACD_SAMPLING_FREQUENCY)?;
            let mut stats = ExtractStats::default();
            while let Some(frame) = reader.next_frame()? {
                if frame.dst_encoded {
                    return Err(ExtractError::DstFrameUnsupported);
                }
                writer.write_interleaved(&frame.data)?;
                stats.frames_read += 1;
                stats.audio_bytes += frame.data.len() as u64;
            }
            writer.finish()?;
            Ok(stats)
        }
        OutputFormat::Dff => {
            let mut writer = DffWriter::new(output, channel_count, SACD_SAMPLING_FREQUENCY)?;
            let mut stats = ExtractStats::default();
            while let Some(frame) = reader.next_frame()? {
                if frame.dst_encoded {
                    return Err(ExtractError::DstFrameUnsupported);
                }
                writer.write_frame(&frame.data)?;
                stats.frames_read += 1;
                stats.audio_bytes += frame.data.len() as u64;
            }
            writer.finish()?;
            Ok(stats)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsf_writer::BLOCK_SIZE_PER_CHANNEL;
    use crate::frame::{Timecode, FRAME_SIZE_UNCOMPRESSED};
    use crate::test_util::{
        synth_audio_sector, synth_continuation_sector, synth_dst_sector, write_iso,
    };

    const PART_SIZE: usize = 2000;

    /// Build sectors that encode `frame_bytes` as a single uncompressed
    /// frame starting with frame_start in the first sector.
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

    /// Test pattern: byte i = (i & 0xFF). Easy to spot demux/bit-flip bugs.
    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i & 0xFF) as u8).collect()
    }

    fn read_u16_be(b: &[u8], off: usize) -> u16 {
        u16::from_be_bytes(b[off..off + 2].try_into().unwrap())
    }
    fn read_u64_be(b: &[u8], off: usize) -> u64 {
        u64::from_be_bytes(b[off..off + 8].try_into().unwrap())
    }
    fn read_u64_le(b: &[u8], off: usize) -> u64 {
        u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
    }

    /// DSF bit-reverse table (LSB-first storage). Computed inline so
    /// the test doesn't depend on dsf_writer's private const.
    fn bit_reverse(b: u8) -> u8 {
        let mut r = 0u8;
        let mut v = b;
        for _ in 0..8 {
            r = (r << 1) | (v & 1);
            v >>= 1;
        }
        r
    }

    fn run_extract(sectors: Vec<Vec<u8>>, channel_count: u8, format: OutputFormat)
        -> (Vec<u8>, ExtractStats)
    {
        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let stats = extract_track(
            &mut iso,
            &mut output,
            0,
            sectors.len() as u64,
            channel_count,
            format,
        )
        .expect("extract should succeed");
        (output.into_inner(), stats)
    }

    #[test]
    fn extract_uncompressed_stereo_to_dff_preserves_bytes() {
        // One stereo uncompressed frame = 2 * 4704 = 9408 bytes.
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode { minutes: 0, seconds: 0, frames: 1 },
        );
        let (out, stats) = run_extract(sectors, 2, OutputFormat::Dff);

        // DFF stereo header = 144 bytes. Audio payload starts at 144,
        // length = 9408 (even — no pad byte).
        assert_eq!(out.len(), 144 + 9408);
        assert_eq!(&out[144..144 + 9408], &frame[..]);
        // DSD-data.chunk_data_size (BE u64 at offset 136) = 9408.
        assert_eq!(read_u64_be(&out, 136), 9408);
        assert_eq!(stats.frames_read, 1);
        assert_eq!(stats.audio_bytes, 9408);
    }

    #[test]
    fn extract_uncompressed_stereo_to_dsf_demuxes_correctly() {
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode { minutes: 0, seconds: 0, frames: 1 },
        );
        let (out, stats) = run_extract(sectors, 2, OutputFormat::Dsf);

        // DSF header = 92 bytes. Per-channel data = 4704 bytes,
        // emitted as one full 4096-byte block + 608 real + 3488 zero
        // bytes in a second block. So per channel: 2 * 4096 = 8192.
        // Total file: 92 + 2 channels * 8192 = 92 + 16384 = 16476.
        assert_eq!(out.len(), 92 + 2 * 2 * BLOCK_SIZE_PER_CHANNEL);

        // ch0 block 0 at offset 92..(92+4096). Each byte i is the
        // bit-reverse of frame[i * 2] (channel 0 = even-indexed
        // bytes of the interleaved input).
        for i in 0..BLOCK_SIZE_PER_CHANNEL {
            assert_eq!(out[92 + i], bit_reverse(frame[i * 2]),
                "ch0 block0 byte {} mismatch", i);
        }
        // ch1 block 0 at offset (92+4096)..(92+2*4096). Channel 1 =
        // odd-indexed bytes.
        let ch1_b0 = 92 + BLOCK_SIZE_PER_CHANNEL;
        for i in 0..BLOCK_SIZE_PER_CHANNEL {
            assert_eq!(out[ch1_b0 + i], bit_reverse(frame[i * 2 + 1]),
                "ch1 block0 byte {} mismatch", i);
        }
        // ch0 block 1: first 608 bytes are real (continuing the
        // even-indexed stream), rest are zero pad.
        let ch0_b1 = 92 + 2 * BLOCK_SIZE_PER_CHANNEL;
        for i in 0..608 {
            assert_eq!(out[ch0_b1 + i], bit_reverse(frame[(BLOCK_SIZE_PER_CHANNEL + i) * 2]),
                "ch0 block1 real byte {} mismatch", i);
        }
        for i in 608..BLOCK_SIZE_PER_CHANNEL {
            assert_eq!(out[ch0_b1 + i], 0, "ch0 block1 pad byte {} not zero", i);
        }
        // sample_count (fmt chunk, offset 64, LE u64) = real bits per
        // channel = 4704 * 8 = 37_632. No padding contribution.
        assert_eq!(read_u64_le(&out, 64), (FRAME_SIZE_UNCOMPRESSED as u64) * 8);
        assert_eq!(stats.frames_read, 1);
        assert_eq!(stats.audio_bytes, 9408);
    }

    #[test]
    fn extract_six_channel_to_dff_passes_clustered_bytes_through() {
        // One 6-channel uncompressed frame = 6 * 4704 = 28_224 bytes.
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 6);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode { minutes: 0, seconds: 0, frames: 1 },
        );
        let (out, _stats) = run_extract(sectors, 6, OutputFormat::Dff);

        // 6-channel DFF header = 160 bytes. Audio payload follows.
        assert_eq!(out.len(), 160 + 28_224);
        assert_eq!(&out[160..160 + 28_224], &frame[..]);
        // CHNL chunk_count = 6 (BE u16 at offset 76).
        assert_eq!(read_u16_be(&out, 76), 6);
    }

    #[test]
    fn extract_partial_block_in_dsf_pads_with_zeros() {
        // Same input as the demux test, but assert ONLY the padding
        // contract: per-channel real bytes = 4704, which is 4096 +
        // 608. The 608 real bytes start each second block; the
        // remaining 3488 must be zero. Independent of the
        // bit-reverse correctness (covered by the demux test).
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode { minutes: 0, seconds: 0, frames: 1 },
        );
        let (out, _) = run_extract(sectors, 2, OutputFormat::Dsf);

        let ch0_b1 = 92 + 2 * BLOCK_SIZE_PER_CHANNEL;
        let ch1_b1 = 92 + 3 * BLOCK_SIZE_PER_CHANNEL;
        // Zero-pad zones in both block 1's.
        assert!(out[ch0_b1 + 608..ch0_b1 + BLOCK_SIZE_PER_CHANNEL]
            .iter().all(|&b| b == 0));
        assert!(out[ch1_b1 + 608..ch1_b1 + BLOCK_SIZE_PER_CHANNEL]
            .iter().all(|&b| b == 0));
    }

    #[test]
    fn extract_dst_frame_returns_dst_unsupported() {
        let payload = vec![0xDEu8; 100];
        let sectors = vec![synth_dst_sector(
            &payload,
            2,
            1, // sector_count: decrements to 0 after the audio packet → complete
            Timecode { minutes: 0, seconds: 0, frames: 1 },
        )];
        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let err = extract_track(&mut iso, &mut output, 0, 1, 2, OutputFormat::Dff)
            .expect_err("DST frame must error");
        assert!(matches!(err, ExtractError::DstFrameUnsupported), "got {:?}", err);
    }

    #[test]
    fn extract_zero_frames_produces_header_only_dff() {
        // Empty range — nothing read, header-only output.
        let td = write_iso(&[]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let stats = extract_track(&mut iso, &mut output, 0, 0, 2, OutputFormat::Dff).unwrap();
        let out = output.into_inner();
        assert_eq!(out.len(), 144);
        assert_eq!(read_u64_be(&out, 136), 0); // DSD-data.chunk_data_size
        assert_eq!(read_u64_be(&out, 4), 132); // FRM8.chunk_data_size
        assert_eq!(stats.frames_read, 0);
        assert_eq!(stats.audio_bytes, 0);
    }

    #[test]
    fn extract_zero_frames_produces_header_only_dsf() {
        let td = write_iso(&[]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let stats = extract_track(&mut iso, &mut output, 0, 0, 2, OutputFormat::Dsf).unwrap();
        let out = output.into_inner();
        assert_eq!(out.len(), 92);
        assert_eq!(read_u64_le(&out, 64), 0); // sample_count
        assert_eq!(stats.frames_read, 0);
        assert_eq!(stats.audio_bytes, 0);
    }

    #[test]
    fn extract_stats_reports_correct_counts_for_two_frames() {
        // Two complete stereo frames, back-to-back. First frame_start
        // sector for frame A, continuation sectors, then a fresh
        // frame_start sector for frame B which finalizes A, then more
        // continuation, then EOR flushes B.
        let frame_a = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let frame_b: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 13) & 0xFF) as u8)
            .collect();
        let mut sectors = synth_uncompressed_frame_sectors(
            &frame_a,
            Timecode { minutes: 0, seconds: 0, frames: 1 },
        );
        sectors.extend(synth_uncompressed_frame_sectors(
            &frame_b,
            Timecode { minutes: 0, seconds: 0, frames: 2 },
        ));
        let (out, stats) = run_extract(sectors, 2, OutputFormat::Dff);

        assert_eq!(stats.frames_read, 2);
        assert_eq!(stats.audio_bytes, 9408 * 2);
        // Concatenated audio = frame_a then frame_b.
        assert_eq!(&out[144..144 + 9408], &frame_a[..]);
        assert_eq!(&out[144 + 9408..144 + 9408 * 2], &frame_b[..]);
    }
}

