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

/// Time-based frame filter for excluding pre-gap and inter-track
/// pause frames. Matches sacd_extract's `frame_read_callback`
/// default behavior (`audio_frame_trimming = 1`): frames whose
/// absolute timecode is outside `[start_frame, start_frame +
/// duration_frames)` get silently dropped.
///
/// Source the values from SACDTRL2 — in tonepoet's parser, that's
/// `TrackEntry.start_time` and `TrackEntry.duration` (both as
/// `PlayTime`). Convert each to 75fps frame counts:
/// `m * 60 * 75 + s * 75 + f`. This formula is identical to
/// sacd_extract's `TIME_FRAMECOUNT` macro
/// (libsacd/scarletbook.h).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeFilter {
    /// Track start absolute timecode in 75fps frame units.
    pub start_frame: u32,
    /// Track duration in 75fps frame units.
    pub duration_frames: u32,
}

impl TimeFilter {
    /// Construct a filter from frame counts. Use the formula
    /// `m * 60 * 75 + s * 75 + f` to derive each value from a
    /// (minutes, seconds, frames) timecode triple.
    pub fn new(start_frame: u32, duration_frames: u32) -> Self {
        Self { start_frame, duration_frames }
    }

    /// True iff `tc ∈ [start_frame, start_frame + duration_frames)`,
    /// matching sacd_extract's keep-frame condition. Uses saturating
    /// arithmetic so adversarial inputs (e.g. `duration_frames =
    /// u32::MAX`) don't panic.
    pub fn includes(&self, tc: u32) -> bool {
        let end = self.start_frame.saturating_add(self.duration_frames);
        tc >= self.start_frame && tc < end
    }
}

/// Options bundle for [`extract_track`]. Forward-compatible with
/// future Series-3 parity items (ID3 mode, edit-master, concatenate,
/// etc.) — new knobs land here without changing the function
/// signature.
#[derive(Debug, Clone, Copy)]
pub struct ExtractOptions {
    pub start_lsn: u64,
    pub end_lsn: u64,
    pub channel_count: u8,
    pub format: OutputFormat,
    /// If set, frames whose timecode is outside the filter's range
    /// are silently dropped. Matches sacd_extract's default
    /// `audio_frame_trimming = 1` behavior. Use when passing the
    /// wider `area_toc.track_start..track_start_lsn[next]` LSN
    /// range; leave `None` when passing pre-trimmed SACDTRL1
    /// ranges (no frames will be out of timecode bounds anyway).
    pub time_filter: Option<TimeFilter>,
}

impl ExtractOptions {
    /// Construct options for the no-filter case (matches
    /// sacd_extract's `-b pauses` flag behavior).
    pub fn new(
        start_lsn: u64,
        end_lsn: u64,
        channel_count: u8,
        format: OutputFormat,
    ) -> Self {
        Self {
            start_lsn,
            end_lsn,
            channel_count,
            format,
            time_filter: None,
        }
    }

    /// Attach a time filter (sacd_extract's default behavior).
    pub fn with_time_filter(mut self, filter: TimeFilter) -> Self {
        self.time_filter = Some(filter);
        self
    }
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
    /// Number of frames **written to output**. Frames dropped by
    /// the time filter are not counted, matching sacd_extract's
    /// `count_frames` semantics (incremented only inside the
    /// keep-range branch of `frame_read_callback`).
    pub frames_read: u64,
    /// Total audio bytes pushed to the writer (pre-pad,
    /// post-filter).
    pub audio_bytes: u64,
}

/// Extract a single track's DSD audio from `iso` into `output`,
/// per `opts`.
///
/// `opts.channel_count` must match the SACD area's channel layout
/// (2, 5, or 6 for real SACDs). Uncompressed DSD frames don't
/// self-describe channel count; the orchestrator trusts the caller.
///
/// ## LSN range + time filter
///
/// Two valid call patterns produce identical output for real SACDs:
///
/// 1. **Pre-trimmed LSN range, no filter** — pass tonepoet's
///    `TrackEntry.start_lsn` + `length_lsn` from SACDTRL1, with
///    `opts.time_filter = None`. The SACDTRL1 range already
///    excludes pre-gaps + inter-track pauses, so no frame filter
///    is needed.
///
/// 2. **Wide LSN range + time filter** — pass the
///    `area_toc.track_start..track_start_lsn[next_track]` range
///    plus `opts.time_filter = Some(TimeFilter { ... })` built from
///    SACDTRL2's per-track start time + duration. This matches
///    sacd_extract's default behavior (`audio_frame_trimming = 1`).
///
/// Both patterns produce sacd_extract-default-equivalent audio
/// output. Pattern 1 is more efficient (fewer sectors read);
/// pattern 2 is sacd_extract-faithful when reproducing legacy
/// behavior matters.
///
/// On error, the output writer is dropped without `finish()`;
/// the file ends up with a placeholder header (zero chunk sizes)
/// plus partial audio bytes. **Discard the output on any error.**
pub fn extract_track<W: Write + Seek>(
    iso: &mut IsoReader,
    output: &mut W,
    opts: ExtractOptions,
) -> Result<ExtractStats, ExtractError> {
    let mut reader = FrameReader::new(iso, opts.start_lsn, opts.end_lsn);

    match opts.format {
        OutputFormat::Dsf => {
            let mut writer =
                DsfWriter::new(output, opts.channel_count, SACD_SAMPLING_FREQUENCY)?;
            let stats = drain_frames(&mut reader, opts.time_filter, |data| {
                writer.write_interleaved(data).map_err(ExtractError::Io)
            })?;
            writer.finish()?;
            Ok(stats)
        }
        OutputFormat::Dff => {
            let mut writer =
                DffWriter::new(output, opts.channel_count, SACD_SAMPLING_FREQUENCY)?;
            let stats = drain_frames(&mut reader, opts.time_filter, |data| {
                writer.write_frame(data).map_err(ExtractError::Io)
            })?;
            writer.finish()?;
            Ok(stats)
        }
    }
}

/// Drain frames from `reader`, applying `time_filter` (if any),
/// erroring on DST frames in the keep-range, and forwarding each
/// kept frame's data to `write_data`.
///
/// Filter-then-DST order mirrors sacd_extract's `frame_read_callback`
/// nesting: out-of-range DST frames are silently dropped without
/// triggering an unsupported-format error.
fn drain_frames<F>(
    reader: &mut FrameReader<'_>,
    time_filter: Option<TimeFilter>,
    mut write_data: F,
) -> Result<ExtractStats, ExtractError>
where
    F: FnMut(&[u8]) -> Result<(), ExtractError>,
{
    let mut stats = ExtractStats::default();
    while let Some(frame) = reader.next_frame()? {
        // (1) Time filter: matches sacd_extract's outer-guard
        // ordering. Out-of-range frames drop silently regardless
        // of compression.
        if let Some(filter) = time_filter {
            if !filter.includes(frame.timecode.as_frame_count()) {
                continue;
            }
        }
        // (2) DST check: only applies to in-range frames.
        if frame.dst_encoded {
            return Err(ExtractError::DstFrameUnsupported);
        }
        // (3) Write to format-specific sink.
        write_data(&frame.data)?;
        stats.frames_read += 1;
        stats.audio_bytes += frame.data.len() as u64;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsf_writer::BLOCK_SIZE_PER_CHANNEL;
    use crate::frame::{Timecode, FRAME_SIZE_UNCOMPRESSED};
    use crate::test_util::{
        sha256_hex, synth_audio_sector, synth_continuation_sector, synth_dst_sector,
        tc_at, write_iso,
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
        run_extract_with(sectors, channel_count, format, None)
    }

    /// Same as `run_extract` but with an optional time filter. End
    /// LSN is set to the sector count.
    fn run_extract_with(
        sectors: Vec<Vec<u8>>,
        channel_count: u8,
        format: OutputFormat,
        time_filter: Option<TimeFilter>,
    ) -> (Vec<u8>, ExtractStats) {
        let end_lsn = sectors.len() as u64;
        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let mut opts = ExtractOptions::new(0, end_lsn, channel_count, format);
        if let Some(tf) = time_filter {
            opts = opts.with_time_filter(tf);
        }
        let stats = extract_track(&mut iso, &mut output, opts)
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
        // Hash-pinned canonical output. If this fails after an
        // intentional output-format change (PR 3c DFF footers,
        // etc.), copy the actual hash from the failure message.
        assert_eq!(
            sha256_hex(&out),
            "10c9f7c4adb39d98bc7b6056a79afdcf34df23ed9d85e6e6a108201d37e91961",
        );
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
        // Hash-pinned canonical output. Re-derive on writer changes
        // (PR 3b DSF ID3 footer, etc.).
        assert_eq!(
            sha256_hex(&out),
            "f19d02521726829bf74bf410dfaac73e13a46e6783e1a428b41b7ff1c52c089c",
        );
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
        // Hash-pinned canonical output for 6-channel DFF.
        assert_eq!(
            sha256_hex(&out),
            "5c113971a54c52abba78c07fd2ff1a765e0b36630e7e05680a2710a79343c4d1",
        );
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
        let err = extract_track(
            &mut iso,
            &mut output,
            ExtractOptions::new(0, 1, 2, OutputFormat::Dff),
        )
        .expect_err("DST frame must error");
        assert!(matches!(err, ExtractError::DstFrameUnsupported), "got {:?}", err);
    }

    #[test]
    fn extract_zero_frames_produces_header_only_dff() {
        // Empty range — nothing read, header-only output.
        let td = write_iso(&[]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let stats = extract_track(
            &mut iso,
            &mut output,
            ExtractOptions::new(0, 0, 2, OutputFormat::Dff),
        )
        .unwrap();
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
        let stats = extract_track(
            &mut iso,
            &mut output,
            ExtractOptions::new(0, 0, 2, OutputFormat::Dsf),
        )
        .unwrap();
        let out = output.into_inner();
        assert_eq!(out.len(), 92);
        assert_eq!(read_u64_le(&out, 64), 0); // sample_count
        assert_eq!(stats.frames_read, 0);
        assert_eq!(stats.audio_bytes, 0);
    }

    // ============================================================
    //  PR 3a — TimeFilter tests
    // ============================================================

    #[test]
    fn time_filter_includes_in_range_frame() {
        // Range [150, 11281) — Solo Monk track 1 from PR 1e
        // validation.
        let tf = TimeFilter::new(150, 11131);
        assert!(!tf.includes(0), "tc 0 should be out (pre-gap)");
        assert!(!tf.includes(149), "tc 149 (one before start) should be out");
        assert!(tf.includes(150), "tc 150 (start, inclusive) should be in");
        assert!(tf.includes(5000), "tc 5000 (mid-track) should be in");
        assert!(tf.includes(11280), "tc 11280 (end-1, inclusive) should be in");
        assert!(!tf.includes(11281), "tc 11281 (end, exclusive) should be out");
        assert!(!tf.includes(50000), "tc 50000 (post-track) should be out");
    }

    #[test]
    fn time_filter_with_zero_duration_rejects_everything() {
        let tf = TimeFilter::new(100, 0);
        for tc in [0, 99, 100, 101, 1000, u32::MAX] {
            assert!(!tf.includes(tc), "tc {} should be rejected (duration=0)", tc);
        }
    }

    #[test]
    fn time_filter_overflow_saturates() {
        // start=u32::MAX - 50, duration=100 would mathematically end
        // at u32::MAX + 50; saturating arithmetic clamps end to
        // u32::MAX. The half-open interval [MAX-50, MAX) thus
        // includes MAX-50..MAX-1 inclusive but excludes MAX itself.
        // No panic, deterministic behavior on adversarial inputs.
        let tf = TimeFilter::new(u32::MAX - 50, 100);
        assert!(tf.includes(u32::MAX - 50), "start (inclusive) included");
        assert!(tf.includes(u32::MAX - 1), "in-range value included");
        assert!(!tf.includes(u32::MAX), "MAX is the exclusive saturated end");
        assert!(!tf.includes(u32::MAX - 51), "before start excluded");
    }

    #[test]
    fn extract_with_filter_drops_pre_gap_frames() {
        // Three frames at tc=100, tc=200, tc=300. Filter [150, 250)
        // keeps only the tc=200 frame.
        let frame_pre = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let frame_mid: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 17) & 0xFF) as u8)
            .collect();
        let frame_post: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 31) & 0xFF) as u8)
            .collect();
        let mut sectors = synth_uncompressed_frame_sectors(&frame_pre, tc_at(100));
        sectors.extend(synth_uncompressed_frame_sectors(&frame_mid, tc_at(200)));
        sectors.extend(synth_uncompressed_frame_sectors(&frame_post, tc_at(300)));

        let (out, stats) = run_extract_with(
            sectors,
            2,
            OutputFormat::Dff,
            Some(TimeFilter::new(150, 100)), // range [150, 250)
        );

        assert_eq!(stats.frames_read, 1, "only frame_mid kept");
        assert_eq!(stats.audio_bytes, 9408);
        // DFF header (144) + 9408 audio bytes (just frame_mid).
        assert_eq!(out.len(), 144 + 9408);
        assert_eq!(&out[144..144 + 9408], &frame_mid[..]);
        // Hash-pinned: pins the filter execution path output.
        assert_eq!(
            sha256_hex(&out),
            "785b247e0cb9a3b0a124d312f9024d89893d04fa961781faa31f129a05a4b97c",
        );
    }

    #[test]
    fn extract_with_filter_drops_post_track_frames() {
        // Three frames at tc=100, tc=200, tc=300. Filter [50, 200)
        // keeps tc=100 only (tc=200 is at the exclusive end).
        let frame_a = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let frame_b: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 17) & 0xFF) as u8)
            .collect();
        let frame_c: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 31) & 0xFF) as u8)
            .collect();
        let mut sectors = synth_uncompressed_frame_sectors(&frame_a, tc_at(100));
        sectors.extend(synth_uncompressed_frame_sectors(&frame_b, tc_at(200)));
        sectors.extend(synth_uncompressed_frame_sectors(&frame_c, tc_at(300)));

        let (out, stats) = run_extract_with(
            sectors,
            2,
            OutputFormat::Dff,
            Some(TimeFilter::new(50, 150)), // range [50, 200)
        );

        assert_eq!(stats.frames_read, 1, "only frame_a kept (tc=100)");
        assert_eq!(&out[144..144 + 9408], &frame_a[..]);
    }

    #[test]
    fn extract_with_filter_boundary_frames() {
        // tc=150 (= start, INCLUDED), tc=11280 (= end-1, INCLUDED),
        // tc=11281 (= end, EXCLUDED). Filter {start:150, dur:11131}.
        let frame_at_start = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let frame_at_end_minus_one: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 7) & 0xFF) as u8)
            .collect();
        let frame_at_end: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 41) & 0xFF) as u8)
            .collect();
        let mut sectors = synth_uncompressed_frame_sectors(&frame_at_start, tc_at(150));
        sectors.extend(synth_uncompressed_frame_sectors(
            &frame_at_end_minus_one, tc_at(11280),
        ));
        sectors.extend(synth_uncompressed_frame_sectors(&frame_at_end, tc_at(11281)));

        let (out, stats) = run_extract_with(
            sectors,
            2,
            OutputFormat::Dff,
            Some(TimeFilter::new(150, 11131)), // range [150, 11281)
        );

        assert_eq!(stats.frames_read, 2, "start and end-1 kept; end excluded");
        assert_eq!(stats.audio_bytes, 9408 * 2);
        assert_eq!(&out[144..144 + 9408], &frame_at_start[..]);
        assert_eq!(&out[144 + 9408..144 + 9408 * 2], &frame_at_end_minus_one[..]);
    }

    #[test]
    fn extract_with_filter_on_dsf_drops_out_of_range_frames() {
        // Two frames; only tc=200 in range. DSF demuxes + bit-reverses
        // just that single frame's bytes.
        let frame_pre = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let frame_mid: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 17) & 0xFF) as u8)
            .collect();
        let mut sectors = synth_uncompressed_frame_sectors(&frame_pre, tc_at(100));
        sectors.extend(synth_uncompressed_frame_sectors(&frame_mid, tc_at(200)));

        let (out, stats) = run_extract_with(
            sectors,
            2,
            OutputFormat::Dsf,
            Some(TimeFilter::new(150, 100)), // range [150, 250) — drops pre
        );

        assert_eq!(stats.frames_read, 1);
        // Per-channel real bytes = 4704 = 1 full block + 608 partial.
        // File = 92 header + 2 * 2 * 4096 = 16476 bytes.
        assert_eq!(out.len(), 92 + 2 * 2 * BLOCK_SIZE_PER_CHANNEL);
        // ch0 block 0 byte 0 = bit_reverse of frame_mid[0] (not
        // frame_pre[0]).
        assert_eq!(out[92], bit_reverse(frame_mid[0]));
        assert_eq!(out[92 + 4096], bit_reverse(frame_mid[1]));
        // sample_count = 4704 * 8 (real bytes/channel × 8 bits).
        assert_eq!(read_u64_le(&out, 64), (FRAME_SIZE_UNCOMPRESSED as u64) * 8);
        // Hash-pinned: pins DSF + filter interaction.
        assert_eq!(
            sha256_hex(&out),
            "fe112487cab4fb38be81212595f29038fd1eaaaaccd7a487bebb50c9ad71f0b9",
        );
    }

    #[test]
    fn filter_drops_out_of_range_dst_frame_silently() {
        // Critical ordering check: filter MUST run before the DST
        // check. Out-of-range DST frames should drop silently (no
        // DstFrameUnsupported error) — matching sacd_extract's
        // frame_read_callback nesting where the timecode filter is
        // the outer guard.
        //
        // If someone refactors to DST-then-filter, the in-range
        // case still errors but THIS case starts erroring too,
        // diverging from sacd_extract behavior. This test pins the
        // semantic contract.
        let payload = vec![0xDEu8; 100];
        let sectors = vec![synth_dst_sector(
            &payload,
            2,
            1,
            tc_at(50), // tc=50, outside filter [150, 250)
        )];
        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, 1, 2, OutputFormat::Dff)
            .with_time_filter(TimeFilter::new(150, 100));
        let stats = extract_track(&mut iso, &mut output, opts)
            .expect("out-of-range DST must drop silently, not error");
        assert_eq!(stats.frames_read, 0);
        assert_eq!(stats.audio_bytes, 0);
        // Output is a valid header-only DFF (filter dropped everything).
        let out = output.into_inner();
        assert_eq!(out.len(), 144);
    }

    #[test]
    fn filter_keeps_in_range_dst_frame_then_errors() {
        // Complement to the silent-drop test: when filter includes
        // a DST frame, the orchestrator errors (because we don't
        // decode DST yet). This pins the second half of the
        // filter-then-DST nesting.
        let payload = vec![0xDEu8; 100];
        let sectors = vec![synth_dst_sector(
            &payload,
            2,
            1,
            tc_at(200), // tc=200, inside filter [150, 250)
        )];
        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, 1, 2, OutputFormat::Dff)
            .with_time_filter(TimeFilter::new(150, 100));
        let err = extract_track(&mut iso, &mut output, opts)
            .expect_err("in-range DST must error");
        assert!(matches!(err, ExtractError::DstFrameUnsupported), "got {:?}", err);
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

