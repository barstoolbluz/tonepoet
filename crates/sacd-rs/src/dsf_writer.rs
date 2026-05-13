//! Sony DSF (DSD Stream File) writer. Port of sacd-extract's
//! `libsacd/dsf.c`.
//!
//! ## DSF file layout
//!
//! ```text
//!  +----------------------------------------------------------+
//!  | DSD chunk (28 bytes):                                    |
//!  |   "DSD " | chunk_size=28 (u64 LE) | total_file_size (u64)|
//!  |   metadata_offset (u64, 0 = none, or offset to ID3v2 tag)|
//!  +----------------------------------------------------------+
//!  | fmt chunk (52 bytes):                                    |
//!  |   "fmt " | chunk_size=52 | version=1 | format_id=0 (DSD) |
//!  |   channel_type | channel_count | sample_frequency        |
//!  |   bits_per_sample=1 (LSB) | sample_count (u64, per chan, |
//!  |     in DSD samples = bits) | block_size=4096 | reserved=0|
//!  +----------------------------------------------------------+
//!  | data chunk:                                              |
//!  |   "data" | chunk_size (u64, includes the 12-byte header) |
//!  |   payload: blocks of 4096 bytes per channel, written     |
//!  |     channel-major within each block group:               |
//!  |       ch0_block0 (4096) | ch1_block0 (4096) | …          |
//!  |       ch0_block1 (4096) | ch1_block1 (4096) | …          |
//!  |     Bits within each byte are stored LSB-first.          |
//!  |     SACD ISO stores DSD MSB-first, so this writer        |
//!  |     bit-reverses every byte during write.                |
//!  +----------------------------------------------------------+
//!  | (optional ID3 footer)                                    |
//!  +----------------------------------------------------------+
//! ```
//!
//! Final block per channel is zero-padded to `BLOCK_SIZE_PER_CHANNEL`
//! when the input runs short.

use std::io::{self, Seek, SeekFrom, Write};

/// 4096 bytes per channel per block (Sony DSF spec).
pub const BLOCK_SIZE_PER_CHANNEL: usize = 4096;

/// DSD64 sample rate: 64 × 44.1 kHz.
pub const SACD_SAMPLING_FREQUENCY: u32 = 2_822_400;

/// LSB-first bit ordering within each byte (the "1" value for the
/// `bits_per_sample` field per the Sony spec).
const BITS_PER_SAMPLE_LSB: u32 = 1;

const DSD_CHUNK_SIZE: u64 = 28;
const FMT_CHUNK_SIZE: u64 = 52;
const DATA_CHUNK_HEADER_SIZE: u64 = 12;
const HEADER_TOTAL_SIZE: u64 = DSD_CHUNK_SIZE + FMT_CHUNK_SIZE + DATA_CHUNK_HEADER_SIZE;

const DSF_VERSION: u32 = 1;
const FORMAT_ID_DSD: u32 = 0;

/// Channel-type field values per Sony DSF spec. We map only the
/// configurations that actually appear on SACDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ChannelType {
    Mono = 1,
    Stereo = 2,
    /// L/R/C/Ls/Rs (5-channel surround, no LFE).
    Surround5 = 6,
    /// 5.1 (L/R/C/LFE/Ls/Rs).
    Surround51 = 7,
}

impl ChannelType {
    /// Pick the channel-type code for a given channel count. Mirrors
    /// the C reference's defaulting: anything we don't recognize
    /// falls through to Stereo.
    pub fn from_channel_count(n: u8) -> Self {
        match n {
            1 => Self::Mono,
            2 => Self::Stereo,
            5 => Self::Surround5,
            6 => Self::Surround51,
            _ => Self::Stereo,
        }
    }
}

/// Precomputed byte-wise bit-reverse table. Used to convert SACD-ISO
/// MSB-first DSD bytes into DSF's LSB-first storage. Verified
/// matching the table baked into sacd-extract's dsf.c.
const BIT_REVERSE: [u8; 256] = build_bit_reverse_table();

const fn build_bit_reverse_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i: usize = 0;
    while i < 256 {
        let mut b = i as u8;
        let mut r: u8 = 0;
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

/// Streaming DSF writer. Accepts byte-interleaved DSD samples
/// (channel-cycling per byte, matching the layout of an uncompressed
/// SACD audio frame) and writes them out as a DSF file.
///
/// Header is written at construction time as a placeholder; the
/// final header (with sample_count + file size filled in) is written
/// in `finish()` after seeking back.
pub struct DsfWriter<W: Write + Seek> {
    writer: W,
    channel_count: u8,
    sample_rate: u32,
    channel_type: ChannelType,
    /// Per-channel running buffer, each up to BLOCK_SIZE_PER_CHANNEL.
    /// Bit-reversed bytes accumulate here; when all channels fill,
    /// they get flushed in channel-major order.
    channel_buffers: Vec<Vec<u8>>,
    /// Total audio data bytes written to the data chunk so far,
    /// including zero-padding emitted at finish().
    audio_data_size: u64,
    /// Total real (un-padded) bytes received via `write_interleaved`,
    /// summed across all channels. The fmt-chunk `sample_count`
    /// field is derived from this, **not** from `audio_data_size`,
    /// to match the C reference's `handle->sample_count /
    /// channel_count * 8` (where `handle->sample_count` only counts
    /// real bytes — partial-tail flush in `dsf_close` adds the real
    /// remainder, while `audio_data_size` gets the full padded
    /// block).
    real_bytes_total: u64,
}

impl<W: Write + Seek> DsfWriter<W> {
    /// Create a new writer and emit a placeholder header. The
    /// underlying writer's stream position must be at the file
    /// start; `new` seeks to position 0 explicitly.
    pub fn new(mut writer: W, channel_count: u8, sample_rate: u32) -> io::Result<Self> {
        if channel_count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "channel_count must be > 0",
            ));
        }
        let channel_type = ChannelType::from_channel_count(channel_count);
        // Reserve header space with zeros; finalized in finish().
        writer.seek(SeekFrom::Start(0))?;
        let zero_header = vec![0u8; HEADER_TOTAL_SIZE as usize];
        writer.write_all(&zero_header)?;
        Ok(Self {
            writer,
            channel_count,
            sample_rate,
            channel_type,
            channel_buffers: (0..channel_count)
                .map(|_| Vec::with_capacity(BLOCK_SIZE_PER_CHANNEL))
                .collect(),
            audio_data_size: 0,
            real_bytes_total: 0,
        })
    }

    /// Append byte-interleaved DSD samples. Input layout: bytes
    /// rotate through channels — `data[0]` is channel 0, `data[1]`
    /// is channel 1, ..., `data[N-1]` is channel N-1, `data[N]` is
    /// channel 0 again, etc. Each byte is bit-reversed (MSB→LSB) on
    /// the way in.
    pub fn write_interleaved(&mut self, data: &[u8]) -> io::Result<()> {
        let n = self.channel_count as usize;
        let mut idx = 0usize;
        while idx < data.len() {
            let ch = idx % n;
            self.channel_buffers[ch].push(BIT_REVERSE[data[idx] as usize]);
            self.real_bytes_total += 1;
            idx += 1;
            // After we put a byte into the LAST channel of a cycle,
            // check whether the buffers are full. They all fill at
            // the same rate so it's sufficient to peek at channel 0.
            if ch == n - 1 && self.channel_buffers[0].len() == BLOCK_SIZE_PER_CHANNEL {
                self.flush_block()?;
            }
        }
        Ok(())
    }

    /// Flush a full block from each channel's buffer to the writer
    /// in channel-major order. Assumes every channel's buffer holds
    /// exactly `BLOCK_SIZE_PER_CHANNEL` bytes.
    fn flush_block(&mut self) -> io::Result<()> {
        for buf in &mut self.channel_buffers {
            debug_assert_eq!(buf.len(), BLOCK_SIZE_PER_CHANNEL);
            self.writer.write_all(buf)?;
            buf.clear();
        }
        self.audio_data_size +=
            (BLOCK_SIZE_PER_CHANNEL as u64) * (self.channel_count as u64);
        Ok(())
    }

    /// Finalize the file: pad the last partial block per channel
    /// with zeros, flush it, then seek back and write the final
    /// header with the now-known sample_count and total_file_size.
    pub fn finish(mut self) -> io::Result<()> {
        // Pad any remaining bytes per channel.
        let partial = self.channel_buffers[0].len();
        if partial > 0 {
            for buf in &mut self.channel_buffers {
                buf.resize(BLOCK_SIZE_PER_CHANNEL, 0);
            }
            self.flush_block()?;
        }

        // sample_count is per-channel, in DSD samples (bits). The C
        // reference computes this from REAL bytes only (no zero
        // padding): `handle->sample_count / channel_count * 8`,
        // with integer truncation. Mirror that exactly so the fmt
        // chunk is byte-identical for the same input.
        let real_bytes_per_channel =
            self.real_bytes_total / (self.channel_count as u64);
        let sample_count_per_channel = real_bytes_per_channel * 8;
        let total_file_size = HEADER_TOTAL_SIZE + self.audio_data_size;

        self.writer.seek(SeekFrom::Start(0))?;
        let header = serialize_header(
            self.channel_type,
            self.channel_count,
            self.sample_rate,
            sample_count_per_channel,
            self.audio_data_size,
            total_file_size,
        );
        self.writer.write_all(&header)?;
        Ok(())
    }
}

/// Build the 92-byte header (DSD + fmt + data-header chunks). Pure
/// function for testability.
pub fn serialize_header(
    channel_type: ChannelType,
    channel_count: u8,
    sample_rate: u32,
    sample_count_per_channel: u64,
    audio_data_size: u64,
    total_file_size: u64,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(HEADER_TOTAL_SIZE as usize);

    // DSD chunk
    buf.extend_from_slice(b"DSD ");
    buf.extend_from_slice(&DSD_CHUNK_SIZE.to_le_bytes());
    buf.extend_from_slice(&total_file_size.to_le_bytes());
    let metadata_offset: u64 = 0; // no ID3 tag — wired in a later PR
    buf.extend_from_slice(&metadata_offset.to_le_bytes());

    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&FMT_CHUNK_SIZE.to_le_bytes());
    buf.extend_from_slice(&DSF_VERSION.to_le_bytes());
    buf.extend_from_slice(&FORMAT_ID_DSD.to_le_bytes());
    buf.extend_from_slice(&(channel_type as u32).to_le_bytes());
    buf.extend_from_slice(&(channel_count as u32).to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&BITS_PER_SAMPLE_LSB.to_le_bytes());
    buf.extend_from_slice(&sample_count_per_channel.to_le_bytes());
    buf.extend_from_slice(&(BLOCK_SIZE_PER_CHANNEL as u32).to_le_bytes());
    let reserved: u32 = 0;
    buf.extend_from_slice(&reserved.to_le_bytes());

    // data chunk header
    buf.extend_from_slice(b"data");
    let data_chunk_size = DATA_CHUNK_HEADER_SIZE + audio_data_size;
    buf.extend_from_slice(&data_chunk_size.to_le_bytes());

    debug_assert_eq!(buf.len(), HEADER_TOTAL_SIZE as usize);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn bit_reverse_table_known_values() {
        assert_eq!(BIT_REVERSE[0x00], 0x00);
        assert_eq!(BIT_REVERSE[0xFF], 0xFF);
        assert_eq!(BIT_REVERSE[0x01], 0x80);
        assert_eq!(BIT_REVERSE[0x80], 0x01);
        assert_eq!(BIT_REVERSE[0xAA], 0x55);
        assert_eq!(BIT_REVERSE[0x55], 0xAA);
        // Spot-check matches the C reference's hand-computed table.
        assert_eq!(BIT_REVERSE[0x12], 0x48);
        assert_eq!(BIT_REVERSE[0x9C], 0x39);
    }

    #[test]
    fn channel_type_mapping_matches_spec() {
        assert_eq!(ChannelType::from_channel_count(1), ChannelType::Mono);
        assert_eq!(ChannelType::from_channel_count(2), ChannelType::Stereo);
        assert_eq!(ChannelType::from_channel_count(5), ChannelType::Surround5);
        assert_eq!(ChannelType::from_channel_count(6), ChannelType::Surround51);
        // Unrecognized falls through to Stereo (matches C reference).
        assert_eq!(ChannelType::from_channel_count(3), ChannelType::Stereo);
        assert_eq!(ChannelType::from_channel_count(7), ChannelType::Stereo);
    }

    #[test]
    fn header_byte_layout_for_stereo_dsd64() {
        let header = serialize_header(
            ChannelType::Stereo,
            2,
            SACD_SAMPLING_FREQUENCY,
            // 1 second of DSD64 stereo: 2_822_400 samples per channel.
            2_822_400,
            // bytes: 2_822_400 / 8 * 2 channels = 705_600.
            705_600,
            HEADER_TOTAL_SIZE + 705_600,
        );
        assert_eq!(header.len(), 92);

        // DSD chunk magic
        assert_eq!(&header[0..4], b"DSD ");
        // DSD chunk size = 28
        assert_eq!(u64::from_le_bytes(header[4..12].try_into().unwrap()), 28);
        // total file size = header + audio
        assert_eq!(
            u64::from_le_bytes(header[12..20].try_into().unwrap()),
            92 + 705_600,
        );
        // metadata offset = 0
        assert_eq!(u64::from_le_bytes(header[20..28].try_into().unwrap()), 0);

        // fmt chunk magic
        assert_eq!(&header[28..32], b"fmt ");
        // fmt chunk size = 52
        assert_eq!(u64::from_le_bytes(header[32..40].try_into().unwrap()), 52);
        // version = 1
        assert_eq!(u32::from_le_bytes(header[40..44].try_into().unwrap()), 1);
        // format_id = 0 (DSD)
        assert_eq!(u32::from_le_bytes(header[44..48].try_into().unwrap()), 0);
        // channel_type = 2 (Stereo)
        assert_eq!(u32::from_le_bytes(header[48..52].try_into().unwrap()), 2);
        // channel_count = 2
        assert_eq!(u32::from_le_bytes(header[52..56].try_into().unwrap()), 2);
        // sample_frequency = 2_822_400 (DSD64)
        assert_eq!(
            u32::from_le_bytes(header[56..60].try_into().unwrap()),
            2_822_400,
        );
        // bits_per_sample = 1 (LSB)
        assert_eq!(u32::from_le_bytes(header[60..64].try_into().unwrap()), 1);
        // sample_count = 2_822_400 per channel
        assert_eq!(
            u64::from_le_bytes(header[64..72].try_into().unwrap()),
            2_822_400,
        );
        // block_size_per_channel = 4096
        assert_eq!(
            u32::from_le_bytes(header[72..76].try_into().unwrap()),
            4096,
        );
        // reserved = 0
        assert_eq!(u32::from_le_bytes(header[76..80].try_into().unwrap()), 0);

        // data chunk magic
        assert_eq!(&header[80..84], b"data");
        // data chunk size = 12 + audio
        assert_eq!(
            u64::from_le_bytes(header[84..92].try_into().unwrap()),
            12 + 705_600,
        );
    }

    /// Build interleaved test data for `channels` channels, `per_ch`
    /// bytes per channel. Channel `c`'s byte at index `i` is
    /// `(c * 100 + i) & 0xFF` so demux mistakes are detectable.
    fn synth_interleaved(channels: usize, per_ch: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(channels * per_ch);
        for i in 0..per_ch {
            for c in 0..channels {
                v.push(((c * 100 + i) & 0xFF) as u8);
            }
        }
        v
    }

    /// Write `payload` through a fresh DsfWriter and return the
    /// finalized file bytes.
    fn run_write_and_capture(channel_count: u8, payload: &[u8]) -> Vec<u8> {
        let buf: Vec<u8> = Vec::new();
        let mut cursor = Cursor::new(buf);
        {
            let mut w = DsfWriter::new(&mut cursor, channel_count, SACD_SAMPLING_FREQUENCY)
                .unwrap();
            w.write_interleaved(payload).unwrap();
            w.finish().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn stereo_single_block_roundtrips_bytes_and_layout() {
        let payload = synth_interleaved(2, BLOCK_SIZE_PER_CHANNEL);
        let out = run_write_and_capture(2, &payload);
        // Expected size: 92 header + 2 channels × 4096 bytes = 8284.
        assert_eq!(out.len(), 92 + 2 * 4096);
        // Header sample_count should equal 4096 bytes × 8 = 32768.
        assert_eq!(
            u64::from_le_bytes(out[64..72].try_into().unwrap()),
            32_768,
        );
        // Channel 0's block is bytes 92..92+4096; verify first byte
        // is bit-reverse of synth payload's first byte (c=0, i=0 → 0).
        assert_eq!(out[92], BIT_REVERSE[0]);
        // Channel 1's block starts at 92 + 4096; first byte is
        // bit-reverse of c=1, i=0 → 100.
        assert_eq!(out[92 + 4096], BIT_REVERSE[100]);
        // Spot check a later byte in channel 0 (i=50): payload byte
        // was (0*100 + 50) = 50, so ch0_block[50] == BIT_REVERSE[50].
        assert_eq!(out[92 + 50], BIT_REVERSE[50]);
    }

    #[test]
    fn stereo_partial_block_pads_with_zeros() {
        // 2 channels × 100 bytes = 200 bytes interleaved. The
        // remaining (4096 - 100) bytes per channel should be zeros.
        let payload = synth_interleaved(2, 100);
        let out = run_write_and_capture(2, &payload);
        assert_eq!(out.len(), 92 + 2 * 4096);
        // sample_count must reflect REAL bytes per channel, not the
        // padded block size — matching the C reference, which only
        // bumps `handle->sample_count` by the real partial length in
        // dsf_close. 100 real bytes × 8 bits = 800 samples/channel.
        assert_eq!(
            u64::from_le_bytes(out[64..72].try_into().unwrap()),
            800,
        );
        // ch0 byte 99 was the last real byte: bit-reverse of 99.
        assert_eq!(out[92 + 99], BIT_REVERSE[99]);
        // ch0 byte 100 and onward should be zero.
        assert_eq!(out[92 + 100], 0);
        assert_eq!(out[92 + 4095], 0);
        // ch1's block starts at 92 + 4096; first byte = bit-reverse(100).
        assert_eq!(out[92 + 4096], BIT_REVERSE[100]);
        // ch1 last real byte was at i=99 → payload byte 199 → BIT_REVERSE[199].
        assert_eq!(out[92 + 4096 + 99], BIT_REVERSE[199]);
        // ch1 padded zone is zeros.
        assert_eq!(out[92 + 4096 + 100], 0);
    }

    #[test]
    fn empty_write_produces_header_only_file() {
        let out = run_write_and_capture(2, &[]);
        assert_eq!(out.len(), 92);
        // sample_count = 0.
        assert_eq!(u64::from_le_bytes(out[64..72].try_into().unwrap()), 0);
        // total_file_size = 92 (header only).
        assert_eq!(u64::from_le_bytes(out[12..20].try_into().unwrap()), 92);
        // data chunk size = 12 (header only, no payload).
        assert_eq!(u64::from_le_bytes(out[84..92].try_into().unwrap()), 12);
    }

    #[test]
    fn sample_count_excludes_padding_with_full_plus_partial_block() {
        // One full block per channel (4096 real bytes/ch) plus a
        // 100-byte partial. C reference: sample_count_per_channel =
        // (4096 + 100) * 8 = 33_568. audio_data_size = 2 full blocks
        // worth = 2 channels × 2 × 4096 = 16384 (padded).
        let payload = synth_interleaved(2, BLOCK_SIZE_PER_CHANNEL + 100);
        let out = run_write_and_capture(2, &payload);
        assert_eq!(out.len(), 92 + 2 * 2 * 4096);
        assert_eq!(
            u64::from_le_bytes(out[64..72].try_into().unwrap()),
            (4096 + 100) * 8,
        );
        // total_file_size = 92 + padded audio.
        assert_eq!(
            u64::from_le_bytes(out[12..20].try_into().unwrap()),
            92 + 2 * 2 * 4096,
        );
    }

    #[test]
    fn six_channel_block_layout() {
        // 6 channels × 4096 bytes per channel = 24576 interleaved.
        let payload = synth_interleaved(6, BLOCK_SIZE_PER_CHANNEL);
        let out = run_write_and_capture(6, &payload);
        assert_eq!(out.len(), 92 + 6 * 4096);
        // channel_type = 7 (5.1) for channel_count=6.
        assert_eq!(u32::from_le_bytes(out[48..52].try_into().unwrap()), 7);
        // channel_count = 6.
        assert_eq!(u32::from_le_bytes(out[52..56].try_into().unwrap()), 6);
        // Each channel's block should be at offset 92 + ch * 4096.
        for ch in 0..6 {
            let block_start = 92 + ch * 4096;
            // First byte of channel ch should be bit-reverse(ch * 100).
            let expected = BIT_REVERSE[((ch * 100) & 0xFF) as usize];
            assert_eq!(
                out[block_start], expected,
                "ch{} first byte mismatch", ch,
            );
        }
    }
}
