// SPDX-License-Identifier: GPL-2.0-or-later
// Pure-Rust DST decoder. The implementation keeps tonepoet's existing
// allocation-returning `decode_frame()` API, but the parser below now covers
// the legal 1..=6 channel range, DSD64/DSD128/DSD256 frame geometry, and the
// general DST segment/mapping syntax used by MPEG-4 DST. It is informed by the
// public reference-decoder model and by the `bleggett/dst-decoder` reference
// oracle, but it is maintained as first-party GPL-2.0-or-later code and does
// not vendor or depend on that crate.

use super::bitreader::BitReader;
use super::tables::{
    log2_floor_u32, prob_dst_x_bit, FilterTable, Table, FSETS_CODE_PRED_COEFF,
    MAX_CHANNELS, MAX_ELEMENTS, MAX_TABLE_LEN, PROBS_CODE_PRED_COEFF,
};
use super::DstError;

const DSD64_SAMPLE_RATE: u32 = 2_822_400;
const DSD128_SAMPLE_RATE: u32 = 5_644_800;
const DSD256_SAMPLE_RATE: u32 = 11_289_600;

const MAX_FILTER_SEGMENTS: usize = 4;
const MAX_PROBABILITY_SEGMENTS: usize = 8;
const MAX_SEGMENTS: usize = MAX_PROBABILITY_SEGMENTS;
const MIN_FILTER_SEGMENT_BITS: usize = 1024;
const MIN_PROBABILITY_SEGMENT_BITS: usize = 32;
const ARITHMETIC_ONE: u32 = 4096;
const ARITHMETIC_HALF: u32 = 2048;

/// MPEG-4 DST frame-rate geometry.
///
/// SACD ISO extraction remains DSD64-specific, but ordinary DSDIFF/DST files
/// may carry higher-rate DST frames. A DST frame contains
/// `588 * Fs44 / 8` DSD bytes per channel, where `Fs44` is 64, 128, or 256.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DstRate {
    Dsd64,
    Dsd128,
    Dsd256,
}

impl DstRate {
    pub fn from_sample_rate(sample_rate: u32) -> Result<Self, DstError> {
        match sample_rate {
            DSD64_SAMPLE_RATE => Ok(Self::Dsd64),
            DSD128_SAMPLE_RATE => Ok(Self::Dsd128),
            DSD256_SAMPLE_RATE => Ok(Self::Dsd256),
            _ => Err(DstError::UnsupportedRate { sample_rate }),
        }
    }

    pub fn sample_rate(self) -> u32 {
        match self {
            Self::Dsd64 => DSD64_SAMPLE_RATE,
            Self::Dsd128 => DSD128_SAMPLE_RATE,
            Self::Dsd256 => DSD256_SAMPLE_RATE,
        }
    }

    pub fn fs44(self) -> usize {
        match self {
            Self::Dsd64 => 64,
            Self::Dsd128 => 128,
            Self::Dsd256 => 256,
        }
    }

    pub fn frame_bytes_per_channel(self) -> Result<usize, DstError> {
        588usize
            .checked_mul(self.fs44())
            .and_then(|n| n.checked_div(8))
            .ok_or(DstError::ArithmeticDecodeFailure(
                "DST frame byte geometry overflow",
            ))
    }

    pub fn frame_bits_per_channel(self) -> Result<usize, DstError> {
        self.frame_bytes_per_channel()?
            .checked_mul(8)
            .ok_or(DstError::ArithmeticDecodeFailure(
                "DST frame bit geometry overflow",
            ))
    }
}

/// Decode one DSD64 DST frame to canonical interleaved MSB-first DSD bytes.
///
/// This preserves the historical tonepoet API used by SACD extraction.
pub fn decode_frame(input: &[u8], channel_count: u8) -> Result<Vec<u8>, DstError> {
    decode_frame_with_rate(input, channel_count, DstRate::Dsd64)
}

/// Decode one DSD64 DST frame into caller-owned decoded-output storage.
///
/// On success, returns the exact number of decoded bytes written into `output`.
/// Bytes after the returned length are left untouched. This API covers the DST
/// decoder's output storage; higher-level adapters that return owned frame
/// structs may still allocate to satisfy their ownership contract.
pub fn decode_frame_into(
    input: &[u8],
    channel_count: u8,
    output: &mut [u8],
) -> Result<usize, DstError> {
    decode_frame_with_rate_into(input, channel_count, DstRate::Dsd64, output)
}

/// Decode one DST frame at an explicit DSD rate.
pub fn decode_frame_with_rate(
    input: &[u8],
    channel_count: u8,
    rate: DstRate,
) -> Result<Vec<u8>, DstError> {
    let mut decoder = DstDecoder::new(channel_count, rate)?;
    decoder.decode_frame(input)
}

/// Decode one DST frame at an explicit DSD rate into caller-owned decoded-output storage.
///
/// On success, returns the exact number of decoded bytes written into `output`.
/// Bytes after the returned length are left untouched. This API covers the DST
/// decoder's output storage; higher-level adapters that return owned frame
/// structs may still allocate to satisfy their ownership contract.
pub fn decode_frame_with_rate_into(
    input: &[u8],
    channel_count: u8,
    rate: DstRate,
    output: &mut [u8],
) -> Result<usize, DstError> {
    let mut decoder = DstDecoder::new(channel_count, rate)?;
    decoder.decode_frame_into(input, output)
}

/// Stateful DST decoder for a fixed channel count and DSD rate.
///
/// The state object mostly carries validated geometry and reusable scratch
/// arrays. It is intentionally safe Rust only; callers that need parallelism
/// should create one decoder per worker.
pub struct DstDecoder {
    channel_count: usize,
    rate: DstRate,
    frame_bytes_per_channel: usize,
    frame_bits_per_channel: usize,
}

impl DstDecoder {
    pub fn new(channel_count: u8, rate: DstRate) -> Result<Self, DstError> {
        let channels = validate_channel_count(channel_count)?;
        let frame_bytes_per_channel = rate.frame_bytes_per_channel()?;
        let frame_bits_per_channel = rate.frame_bits_per_channel()?;
        Ok(Self {
            channel_count: channels,
            rate,
            frame_bytes_per_channel,
            frame_bits_per_channel,
        })
    }

    pub fn from_sample_rate(channel_count: u8, sample_rate: u32) -> Result<Self, DstError> {
        Self::new(channel_count, DstRate::from_sample_rate(sample_rate)?)
    }

    pub fn channel_count(&self) -> u8 {
        self.channel_count as u8
    }

    pub fn rate(&self) -> DstRate {
        self.rate
    }

    pub fn dsd_frame_bytes(&self) -> Result<usize, DstError> {
        self.frame_bytes_per_channel
            .checked_mul(self.channel_count)
            .ok_or(DstError::ArithmeticDecodeFailure(
                "DST decoded frame byte count overflow",
            ))
    }

    pub fn dsd_frame_bits_per_channel(&self) -> usize {
        self.frame_bits_per_channel
    }

    pub fn decode_frame(&mut self, input: &[u8]) -> Result<Vec<u8>, DstError> {
        let expected = self.dsd_frame_bytes()?;
        let mut out = vec![0u8; expected];
        let written = self.decode_frame_into(input, &mut out)?;
        if written != expected {
            return Err(DstError::OutputSizeMismatch {
                expected,
                actual: written,
            });
        }
        Ok(out)
    }

    pub fn decode_frame_into(
        &mut self,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<usize, DstError> {
        let expected = self.dsd_frame_bytes()?;
        if output.len() < expected {
            return Err(DstError::OutputBufferTooSmall {
                required: expected,
                actual: output.len(),
            });
        }
        if input.is_empty() {
            return Err(DstError::UnexpectedEof { consumed: 0 });
        }

        let out = &mut output[..expected];
        out.fill(0);
        let mut reader = BitReader::new(input);

        match reader.read_bit()? {
            0 => decode_uncompressed_dst_payload(&mut reader, out)?,
            1 => self.decode_compressed_dst_payload(&mut reader, out)?,
            _ => unreachable!(),
        }

        Ok(expected)
    }

    fn decode_compressed_dst_payload(
        &self,
        reader: &mut BitReader<'_>,
        out: &mut [u8],
    ) -> Result<(), DstError> {
        let syntax = CompressedSyntax::read(reader, self.channel_count, self.frame_bytes_per_channel)?;

        if reader.read_bit()? != 0 {
            return Err(DstError::InvalidArithmeticCode);
        }

        let mut arithmetic = ArithmeticCoder::new(reader)?;
        let filters = build_filters(&syntax.filter_tables)?;
        let mut filter_cursor = SegmentCursorSet::new(
            &syntax.filter_segments,
            self.channel_count,
            self.frame_bits_per_channel,
            syntax.filter_tables.elements,
            SegmentKind::Filter,
        )?;
        let mut probability_cursor = SegmentCursorSet::new(
            &syntax.probability_segments,
            self.channel_count,
            self.frame_bits_per_channel,
            syntax.probability_tables.elements,
            SegmentKind::Probability,
        )?;

        let mut status = [[0xAAu8; 16]; MAX_CHANNELS];
        let first_probability = prob_dst_x_bit(syntax.filter_tables.coeff[0][0]);
        let _ = arithmetic.get(reader, first_probability)?;

        for bit_index in 0..self.frame_bits_per_channel {
            let bit_in_byte = 7 - (bit_index & 7);
            let byte_base = (bit_index >> 3)
                .checked_mul(self.channel_count)
                .ok_or(DstError::ArithmeticDecodeFailure(
                    "DST output byte index overflow",
                ))?;

            for ch in 0..self.channel_count {
                let filter_element = filter_cursor.table_for_bit(
                    &syntax.filter_segments,
                    ch,
                    bit_index,
                    self.frame_bits_per_channel,
                    syntax.filter_tables.elements,
                    SegmentKind::Filter,
                )?;

                let mut predict = 0i32;
                for tap in 0..16 {
                    predict += i32::from(filters[filter_element][tap][usize::from(status[ch][tap])]);
                }

                let probability = if syntax.half_probability[ch]
                    && bit_index < syntax.half_probability_bits[ch]
                {
                    128
                } else {
                    let probability_element = probability_cursor.table_for_bit(
                        &syntax.probability_segments,
                        ch,
                        bit_index,
                        self.frame_bits_per_channel,
                        syntax.probability_tables.elements,
                        SegmentKind::Probability,
                    )?;
                    let length = syntax.probability_tables.length[probability_element];
                    if length == 0 {
                        return Err(DstError::InvalidProbabilityTable(
                            "empty probability table",
                        ));
                    }
                    let abs_predict = predict.unsigned_abs() as usize;
                    let idx = (abs_predict >> 3).min(length - 1);
                    let value = syntax.probability_tables.coeff[probability_element][idx];
                    if !(1..=128).contains(&value) {
                        return Err(DstError::InvalidProbabilityTable(
                            "probability coefficient out of range",
                        ));
                    }
                    value
                };

                let residual = i32::from(arithmetic.get(reader, probability)?);
                let dsd_bit = ((predict >> 15) ^ residual) & 1;
                let output_index = checked_decoded_output_index(byte_base, ch, out.len())?;
                if dsd_bit != 0 {
                    out[output_index] |= 1u8 << bit_in_byte;
                }

                push_status_bit(&mut status[ch], dsd_bit as u8);
            }
        }

        Ok(())
    }
}

fn validate_channel_count(channel_count: u8) -> Result<usize, DstError> {
    match channel_count {
        1..=6 => Ok(usize::from(channel_count)),
        _ => Err(DstError::InvalidChannelCount { channel_count }),
    }
}

fn checked_decoded_output_index(
    byte_base: usize,
    channel: usize,
    output_len: usize,
) -> Result<usize, DstError> {
    let index = byte_base
        .checked_add(channel)
        .ok_or(DstError::ArithmeticDecodeFailure(
            "DST output byte index overflow",
        ))?;
    if index >= output_len {
        return Err(DstError::OutputOverflow { limit: output_len });
    }
    Ok(index)
}

fn decode_uncompressed_dst_payload(
    reader: &mut BitReader<'_>,
    out: &mut [u8],
) -> Result<(), DstError> {
    let _marker = reader.read_bit()?;
    // 6 reserved bits — the spec says these should be zero, but some
    // early Japanese SACDs set non-zero values. The reference C decoder
    // (libdstdec) ignores them, so we do the same.
    let _reserved = reader.read_bits(6)?;

    for byte in out.iter_mut() {
        *byte = reader.read_bits(8)? as u8;
    }

    ensure_remaining_bits_are_zero(reader)?;
    Ok(())
}

fn ensure_remaining_bits_are_zero(reader: &mut BitReader<'_>) -> Result<(), DstError> {
    loop {
        match reader.read_bit() {
            Ok(0) => {}
            Ok(_) => return Err(DstError::MalformedFrame("non-zero DST stuffing bits")),
            Err(DstError::UnexpectedEof { .. }) => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

#[derive(Clone, Debug)]
struct SegmentData {
    resolution: usize,
    segment_count: [usize; MAX_CHANNELS],
    segment_len: [[usize; MAX_SEGMENTS]; MAX_CHANNELS],
    table_for_segment: [[usize; MAX_SEGMENTS]; MAX_CHANNELS],
}

impl Default for SegmentData {
    fn default() -> Self {
        Self {
            resolution: 1,
            segment_count: [1; MAX_CHANNELS],
            segment_len: [[0; MAX_SEGMENTS]; MAX_CHANNELS],
            table_for_segment: [[0; MAX_SEGMENTS]; MAX_CHANNELS],
        }
    }
}

impl SegmentData {
    fn read(
        reader: &mut BitReader<'_>,
        channels: usize,
        frame_bytes_per_channel: usize,
        max_segments: usize,
        min_segment_bits: usize,
    ) -> Result<(Self, bool), DstError> {
        let mut data = Self::default();
        let same_segment_all_channels = reader.read_bit()? != 0;
        if same_segment_all_channels {
            data.read_one_channel_segments(
                reader,
                0,
                frame_bytes_per_channel,
                max_segments,
                min_segment_bits,
            )?;
            for ch in 1..channels {
                data.segment_count[ch] = data.segment_count[0];
                for seg in 0..data.segment_count[0] {
                    data.segment_len[ch][seg] = data.segment_len[0][seg];
                }
            }
        } else {
            for ch in 0..channels {
                data.read_one_channel_segments(
                    reader,
                    ch,
                    frame_bytes_per_channel,
                    max_segments,
                    min_segment_bits,
                )?;
            }
        }
        Ok((data, same_segment_all_channels))
    }

    fn read_one_channel_segments(
        &mut self,
        reader: &mut BitReader<'_>,
        ch: usize,
        frame_bytes_per_channel: usize,
        max_segments: usize,
        min_segment_bits: usize,
    ) -> Result<(), DstError> {
        let mut defined_bits = 0usize;
        let mut max_segment_bytes = frame_bytes_per_channel
            .checked_sub(min_segment_bits / 8)
            .ok_or(DstError::InvalidSegment("frame is shorter than minimum segment"))?;
        let mut segment_index = 0usize;
        let mut resolution_read = false;

        loop {
            let end_of_channel = reader.read_bit()? != 0;
            if end_of_channel {
                self.segment_count[ch] = segment_index
                    .checked_add(1)
                    .ok_or(DstError::InvalidSegment("segment count overflow"))?;
                self.segment_len[ch][segment_index] = 0;
                if !resolution_read {
                    self.resolution = 1;
                }
                return Ok(());
            }

            // Reserve one slot for the required final segment that fills the
            // rest of the frame. `max_segments` is the total table capacity,
            // not the number of non-final boundary records.
            if segment_index + 1 >= max_segments {
                return Err(DstError::InvalidSegment("too many DST segments"));
            }

            if !resolution_read {
                let max_resolution = frame_bytes_per_channel
                    .checked_sub(min_segment_bits / 8)
                    .ok_or(DstError::InvalidSegment("invalid segment resolution bound"))?;
                let bits = log2_round_up(max_resolution as u32);
                let resolution = reader.read_bits(bits)? as usize;
                if resolution == 0 || resolution > max_resolution {
                    return Err(DstError::InvalidSegment("invalid segment resolution"));
                }
                self.resolution = resolution;
                resolution_read = true;
            }

            let max_units = max_segment_bytes
                .checked_div(self.resolution)
                .ok_or(DstError::InvalidSegment("invalid segment unit divisor"))?;
            let bits = log2_round_up(max_units as u32);
            let len_units = reader.read_bits(bits)? as usize;
            let len_bits = self
                .resolution
                .checked_mul(8)
                .and_then(|n| n.checked_mul(len_units))
                .ok_or(DstError::InvalidSegment("segment length overflow"))?;
            let remaining_after = frame_bytes_per_channel
                .checked_mul(8)
                .and_then(|n| n.checked_sub(defined_bits))
                .and_then(|n| n.checked_sub(min_segment_bits))
                .ok_or(DstError::InvalidSegment("segment exceeds frame length"))?;
            if len_bits < min_segment_bits || len_bits > remaining_after {
                return Err(DstError::InvalidSegment("invalid segment length"));
            }

            self.segment_len[ch][segment_index] = len_units;
            defined_bits = defined_bits
                .checked_add(len_bits)
                .ok_or(DstError::InvalidSegment("defined segment bits overflow"))?;
            max_segment_bytes = max_segment_bytes
                .checked_sub(
                    self.resolution
                        .checked_mul(len_units)
                        .ok_or(DstError::InvalidSegment("segment byte length overflow"))?,
                )
                .ok_or(DstError::InvalidSegment("segment byte budget underflow"))?;
            segment_index += 1;
        }
    }

    fn copy_from_filter_for_probability(filter: &SegmentData, channels: usize) -> Result<Self, DstError> {
        let probability = filter.clone();
        for ch in 0..channels {
            for seg in 0..probability.segment_count[ch] {
                let len_units = probability.segment_len[ch][seg];
                if len_units != 0 {
                    let len_bits = probability
                        .resolution
                        .checked_mul(8)
                        .and_then(|n| n.checked_mul(len_units))
                        .ok_or(DstError::InvalidSegment(
                            "copied probability segment length overflow",
                        ))?;
                    if len_bits < MIN_PROBABILITY_SEGMENT_BITS {
                        return Err(DstError::InvalidSegment(
                            "copied filter segment is too short for probability map",
                        ));
                    }
                }
            }
        }
        Ok(probability)
    }

    fn copy_mapping_from_filter(&mut self, filter: &SegmentData, channels: usize) -> Result<(), DstError> {
        for ch in 0..channels {
            if self.segment_count[ch] != filter.segment_count[ch] {
                return Err(DstError::InvalidMapping(
                    "probability and filter segment counts differ",
                ));
            }
            for seg in 0..self.segment_count[ch] {
                self.table_for_segment[ch][seg] = filter.table_for_segment[ch][seg];
            }
        }
        Ok(())
    }

    fn validate_decode_mapping(
        &self,
        channels: usize,
        bits_per_channel: usize,
        table_count: usize,
        kind: SegmentKind,
    ) -> Result<(), DstError> {
        for ch in 0..channels {
            let mut start = 0usize;
            let count = self.segment_count[ch];
            if count == 0 || count > MAX_SEGMENTS {
                return Err(DstError::InvalidSegment("invalid segment count"));
            }
            for seg in 0..count {
                let table = self.table_for_segment[ch][seg];
                if table >= table_count {
                    return Err(DstError::InvalidMapping(kind.undefined_table_message()));
                }
                let end = self.segment_end_from_start(ch, seg, count, start, bits_per_channel)?;
                if end > bits_per_channel || end < start {
                    return Err(DstError::InvalidSegment("segment boundary exceeds frame"));
                }
                start = end;
            }
            if start != bits_per_channel {
                return Err(DstError::InvalidSegment("segments do not cover frame"));
            }
        }
        Ok(())
    }

    fn segment_end_from_start(
        &self,
        ch: usize,
        seg: usize,
        segment_count: usize,
        start: usize,
        bits_per_channel: usize,
    ) -> Result<usize, DstError> {
        if seg + 1 == segment_count {
            return Ok(bits_per_channel);
        }
        start
            .checked_add(self.segment_len_bits(ch, seg)?)
            .ok_or(DstError::InvalidSegment("expanded segment boundary overflow"))
    }

    fn segment_len_bits(&self, ch: usize, seg: usize) -> Result<usize, DstError> {
        self.resolution
            .checked_mul(8)
            .and_then(|n| n.checked_mul(self.segment_len[ch][seg]))
            .ok_or(DstError::InvalidSegment("expanded segment length overflow"))
    }
}

#[derive(Clone, Copy, Debug)]
enum SegmentKind {
    Filter,
    Probability,
}

impl SegmentKind {
    fn undefined_table_message(self) -> &'static str {
        match self {
            Self::Filter => "filter segment references undefined table",
            Self::Probability => "probability segment references undefined table",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct SegmentCursor {
    segment_index: usize,
    segment_start_bit: usize,
    segment_end_bit: usize,
}

#[derive(Debug)]
struct SegmentCursorSet {
    cursors: [SegmentCursor; MAX_CHANNELS],
}

impl SegmentCursorSet {
    fn new(
        segments: &SegmentData,
        channels: usize,
        bits_per_channel: usize,
        table_count: usize,
        kind: SegmentKind,
    ) -> Result<Self, DstError> {
        segments.validate_decode_mapping(channels, bits_per_channel, table_count, kind)?;

        let mut cursors = [SegmentCursor::default(); MAX_CHANNELS];
        for ch in 0..channels {
            let count = segments.segment_count[ch];
            cursors[ch].segment_end_bit = segments.segment_end_from_start(
                ch,
                0,
                count,
                0,
                bits_per_channel,
            )?;
        }
        Ok(Self { cursors })
    }

    fn table_for_bit(
        &mut self,
        segments: &SegmentData,
        ch: usize,
        bit_index: usize,
        bits_per_channel: usize,
        table_count: usize,
        kind: SegmentKind,
    ) -> Result<usize, DstError> {
        let count = segments.segment_count[ch];
        let cursor = &mut self.cursors[ch];
        while bit_index >= cursor.segment_end_bit && cursor.segment_index + 1 < count {
            cursor.segment_index += 1;
            cursor.segment_start_bit = cursor.segment_end_bit;
            cursor.segment_end_bit = segments.segment_end_from_start(
                ch,
                cursor.segment_index,
                count,
                cursor.segment_start_bit,
                bits_per_channel,
            )?;
        }

        if bit_index < cursor.segment_start_bit || bit_index >= cursor.segment_end_bit {
            return Err(DstError::InvalidSegment("segments do not cover frame"));
        }

        let table = segments.table_for_segment[ch][cursor.segment_index];
        if table >= table_count {
            return Err(DstError::InvalidMapping(kind.undefined_table_message()));
        }
        Ok(table)
    }
}

#[derive(Debug)]
struct CompressedSyntax {
    filter_segments: SegmentData,
    probability_segments: SegmentData,
    filter_tables: Table,
    probability_tables: Table,
    half_probability: [bool; MAX_CHANNELS],
    half_probability_bits: [usize; MAX_CHANNELS],
}

impl CompressedSyntax {
    fn read(
        reader: &mut BitReader<'_>,
        channels: usize,
        frame_bytes_per_channel: usize,
    ) -> Result<Self, DstError> {
        let probability_segments_same_as_filter = reader.read_bit()? != 0;
        let (mut filter_segments, _filter_same_segments) = SegmentData::read(
            reader,
            channels,
            frame_bytes_per_channel,
            MAX_FILTER_SEGMENTS,
            MIN_FILTER_SEGMENT_BITS,
        )?;
        let mut probability_segments = if probability_segments_same_as_filter {
            SegmentData::copy_from_filter_for_probability(&filter_segments, channels)?
        } else {
            SegmentData::read(
                reader,
                channels,
                frame_bytes_per_channel,
                MAX_PROBABILITY_SEGMENTS,
                MIN_PROBABILITY_SEGMENT_BITS,
            )?
            .0
        };

        let probability_map_same_as_filter = reader.read_bit()? != 0;
        let filter_table_count = read_table_mapping_data(
            reader,
            channels,
            2 * channels,
            &mut filter_segments,
        )?;
        let probability_table_count = if probability_map_same_as_filter {
            probability_segments.copy_mapping_from_filter(&filter_segments, channels)?;
            filter_table_count
        } else {
            read_table_mapping_data(
                reader,
                channels,
                2 * channels,
                &mut probability_segments,
            )?
        };

        let mut half_probability = [false; MAX_CHANNELS];
        for slot in half_probability.iter_mut().take(channels) {
            *slot = reader.read_bit()? != 0;
        }

        let mut filter_tables = Table::default();
        filter_tables.elements = filter_table_count;
        read_table(reader, &mut filter_tables, &FSETS_CODE_PRED_COEFF, 7, 9, true, 0)?;

        let mut probability_tables = Table::default();
        probability_tables.elements = probability_table_count;
        read_table(
            reader,
            &mut probability_tables,
            &PROBS_CODE_PRED_COEFF,
            6,
            7,
            false,
            1,
        )?;

        let mut half_probability_bits = [0usize; MAX_CHANNELS];
        for ch in 0..channels {
            let table = filter_segments.table_for_segment[ch][0];
            if table >= filter_tables.elements {
                return Err(DstError::InvalidMapping(
                    "half-probability filter table reference is invalid",
                ));
            }
            half_probability_bits[ch] = filter_tables.length[table];
        }

        Ok(Self {
            filter_segments,
            probability_segments,
            filter_tables,
            probability_tables,
            half_probability,
            half_probability_bits,
        })
    }
}

fn read_table_mapping_data(
    reader: &mut BitReader<'_>,
    channels: usize,
    max_table_count: usize,
    segments: &mut SegmentData,
) -> Result<usize, DstError> {
    let mut count_tables = 1usize;
    segments.table_for_segment[0][0] = 0;
    let same_map_all_channels = reader.read_bit()? != 0;

    if same_map_all_channels {
        for seg in 1..segments.segment_count[0] {
            let value = read_table_number(reader, count_tables)?;
            segments.table_for_segment[0][seg] = value;
            if value == count_tables {
                count_tables = count_tables
                    .checked_add(1)
                    .ok_or(DstError::InvalidMapping("table count overflow"))?;
            } else if value > count_tables {
                return Err(DstError::InvalidMapping("invalid table number"));
            }
        }
        for ch in 1..channels {
            if segments.segment_count[ch] != segments.segment_count[0] {
                return Err(DstError::InvalidMapping(
                    "shared mapping with different per-channel segment counts",
                ));
            }
            for seg in 0..segments.segment_count[0] {
                segments.table_for_segment[ch][seg] = segments.table_for_segment[0][seg];
            }
        }
    } else {
        for ch in 0..channels {
            for seg in 0..segments.segment_count[ch] {
                if ch == 0 && seg == 0 {
                    continue;
                }
                let value = read_table_number(reader, count_tables)?;
                segments.table_for_segment[ch][seg] = value;
                if value == count_tables {
                    count_tables = count_tables
                        .checked_add(1)
                        .ok_or(DstError::InvalidMapping("table count overflow"))?;
                } else if value > count_tables {
                    return Err(DstError::InvalidMapping("invalid table number"));
                }
            }
        }
    }

    if count_tables == 0 || count_tables > max_table_count || count_tables > MAX_ELEMENTS {
        return Err(DstError::InvalidMapping("too many DST tables"));
    }
    Ok(count_tables)
}

fn read_table_number(reader: &mut BitReader<'_>, count_tables: usize) -> Result<usize, DstError> {
    let bits = log2_round_up(count_tables as u32);
    Ok(reader.read_bits(bits)? as usize)
}

fn read_table(
    reader: &mut BitReader<'_>,
    table: &mut Table,
    code_pred_coeff: &[[i32; 3]; 3],
    length_bits: usize,
    coeff_bits: usize,
    signed: bool,
    offset: i32,
) -> Result<(), DstError> {
    if table.elements == 0 || table.elements > MAX_ELEMENTS {
        return Err(DstError::InvalidMapping("invalid table element count"));
    }
    for element in 0..table.elements {
        let length = reader.read_bits(length_bits)? as usize + 1;
        if length > MAX_TABLE_LEN {
            return Err(if signed {
                DstError::MalformedFrame("filter table length too large")
            } else {
                DstError::InvalidProbabilityTable("probability table length too large")
            });
        }
        table.length[element] = length;

        if reader.read_bit()? == 0 {
            for idx in 0..length {
                table.coeff[element][idx] = read_uncoded_coeff(reader, coeff_bits, signed, offset)?;
            }
        } else {
            let method = reader.read_bits(2)? as usize;
            if method >= code_pred_coeff.len() {
                return Err(DstError::MalformedFrame("invalid coefficient predictor"));
            }

            let warmup = method + 1;
            if warmup >= length {
                return Err(DstError::MalformedFrame(
                    "coefficient predictor exceeds table length",
                ));
            }

            for idx in 0..warmup {
                table.coeff[element][idx] = read_uncoded_coeff(reader, coeff_bits, signed, offset)?;
            }

            let lsb_size = reader.read_bits(3)? as usize;
            for idx in warmup..length {
                let mut predicted = 0i32;
                for tap in 0..warmup {
                    predicted += code_pred_coeff[method][tap] * table.coeff[element][idx - tap - 1];
                }

                let mut coeff = get_sr_golomb_dst(reader, lsb_size)?;
                if predicted >= 0 {
                    coeff -= (predicted + 4) / 8;
                } else {
                    coeff += (-predicted + 3) / 8;
                }

                validate_coeff(coeff, coeff_bits, signed, offset)?;
                table.coeff[element][idx] = coeff;
            }
        }

        if !signed {
            for idx in 0..length {
                if !(1..=128).contains(&table.coeff[element][idx]) {
                    return Err(DstError::InvalidProbabilityTable(
                        "probability coefficient out of range",
                    ));
                }
            }
        }

        for idx in length..MAX_TABLE_LEN {
            table.coeff[element][idx] = 0;
        }
    }

    Ok(())
}

fn read_uncoded_coeff(
    reader: &mut BitReader<'_>,
    coeff_bits: usize,
    signed: bool,
    offset: i32,
) -> Result<i32, DstError> {
    if signed {
        reader.read_signed(coeff_bits)
    } else {
        Ok(reader.read_bits(coeff_bits)? as i32 + offset)
    }
}

fn validate_coeff(
    coeff: i32,
    coeff_bits: usize,
    signed: bool,
    offset: i32,
) -> Result<(), DstError> {
    if signed {
        let min = -(1i32 << (coeff_bits - 1));
        let max = (1i32 << (coeff_bits - 1)) - 1;
        if coeff < min || coeff > max {
            return Err(DstError::MalformedFrame("signed coefficient out of range"));
        }
    } else {
        let min = offset;
        let max = offset + (1i32 << coeff_bits) - 1;
        if coeff < min || coeff > max {
            return Err(DstError::InvalidProbabilityTable(
                "probability coefficient out of range",
            ));
        }
    }

    Ok(())
}

fn get_sr_golomb_dst(reader: &mut BitReader<'_>, k: usize) -> Result<i32, DstError> {
    if k > 30 {
        return Err(DstError::MalformedFrame("Rice code width too large"));
    }

    let mut prefix = 0i32;
    while reader.read_bit()? == 0 {
        prefix += 1;
        if prefix > 1_000_000 {
            return Err(DstError::MalformedFrame("runaway Rice code"));
        }
    }

    let mut value = (prefix << k) + reader.read_bits(k)? as i32;
    if value != 0 && reader.read_bit()? != 0 {
        value = -value;
    }

    Ok(value)
}

fn build_filters(fsets: &Table) -> Result<FilterTable, DstError> {
    let mut filters = [[[0i16; 256]; 16]; MAX_ELEMENTS];

    for element in 0..fsets.elements {
        for byte_tap in 0..16 {
            let base = byte_tap * 8;
            let available = fsets.length[element].saturating_sub(base).min(8);

            for history in 0..256usize {
                let mut total = 0i32;
                for bit in 0..available {
                    let history_bit = if ((history >> bit) & 1) != 0 { 1 } else { -1 };
                    total += history_bit * fsets.coeff[element][base + bit];
                }
                filters[element][byte_tap][history] = total as i16;
            }
        }
    }

    Ok(filters)
}

fn push_status_bit(status: &mut [u8; 16], bit: u8) {
    let mut carry = bit & 1;
    for byte in status.iter_mut() {
        let next_carry = (*byte >> 7) & 1;
        *byte = (*byte << 1) | carry;
        carry = next_carry;
    }
}

struct ArithmeticCoder {
    a: u32,
    c: u32,
}

impl ArithmeticCoder {
    fn new(reader: &mut BitReader<'_>) -> Result<Self, DstError> {
        let c = reader.read_bits(12)?;
        reader.set_zero_pad_after_eof(true);
        Ok(Self { a: ARITHMETIC_ONE - 1, c })
    }

    fn get(&mut self, reader: &mut BitReader<'_>, probability: i32) -> Result<u8, DstError> {
        if !(1..=128).contains(&probability) {
            return Err(DstError::InvalidProbabilityTable(
                "invalid arithmetic probability",
            ));
        }

        let p = probability as u32;
        let k = (self.a >> 8) | ((self.a >> 7) & 1);
        let q = k
            .checked_mul(p)
            .ok_or(DstError::ArithmeticDecodeFailure(
                "arithmetic probability product overflow",
            ))?;
        let a_minus_q = self
            .a
            .checked_sub(q)
            .ok_or(DstError::ArithmeticDecodeFailure(
                "arithmetic coder interval underflow",
            ))?;

        let bit = if self.c < a_minus_q {
            self.a = a_minus_q;
            1u8
        } else {
            self.a = q;
            self.c -= a_minus_q;
            0u8
        };

        if self.a == 0 {
            return Err(DstError::ArithmeticDecodeFailure(
                "arithmetic coder interval collapsed",
            ));
        }

        if self.a < ARITHMETIC_HALF {
            let n = 11 - log2_floor_u32(self.a);
            self.a = self
                .a
                .checked_shl(n)
                .ok_or(DstError::ArithmeticDecodeFailure(
                    "arithmetic interval shift overflow",
                ))?;
            self.c = self
                .c
                .checked_shl(n)
                .ok_or(DstError::ArithmeticDecodeFailure(
                    "arithmetic code shift overflow",
                ))?
                | reader.read_bits(n as usize)?;
        }

        Ok(bit)
    }
}

fn log2_round_up(x: u32) -> usize {
    let mut y = 0usize;
    while x >= (1u32 << y) {
        y += 1;
    }
    y
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_dst_frame(channel_count: u8, rate: DstRate, fill: u8) -> Vec<u8> {
        let bytes = rate.frame_bytes_per_channel().unwrap() * usize::from(channel_count);
        let mut frame = Vec::with_capacity(bytes + 1);
        frame.push(0); // DSTCoded=0, dummy=0, six zero stuffing bits.
        frame.extend(std::iter::repeat(fill).take(bytes));
        frame
    }

    #[test]
    fn uncompressed_decode_supports_legal_channels_and_rates() {
        for channels in 1..=6u8 {
            for rate in [DstRate::Dsd64, DstRate::Dsd128, DstRate::Dsd256] {
                let input = raw_dst_frame(channels, rate, channels);
                let decoded = decode_frame_with_rate(&input, channels, rate).unwrap();
                assert_eq!(
                    decoded.len(),
                    rate.frame_bytes_per_channel().unwrap() * usize::from(channels)
                );
                assert!(decoded.iter().all(|&b| b == channels));
            }
        }
    }

    #[test]
    fn decode_frame_into_writes_exact_frame_and_preserves_tail() {
        let channels = 2;
        let rate = DstRate::Dsd64;
        let expected = rate.frame_bytes_per_channel().unwrap() * usize::from(channels);
        let input = raw_dst_frame(channels, rate, 0x3c);
        let mut output = vec![0xa5; expected + 16];

        let written = decode_frame_with_rate_into(&input, channels, rate, &mut output).unwrap();

        assert_eq!(written, expected);
        assert!(output[..expected].iter().all(|&b| b == 0x3c));
        assert!(output[expected..].iter().all(|&b| b == 0xa5));
    }

    #[test]
    fn decode_frame_into_rejects_undersized_output_before_reading() {
        let channels = 2;
        let rate = DstRate::Dsd64;
        let expected = rate.frame_bytes_per_channel().unwrap() * usize::from(channels);
        let mut output = vec![0xa5; expected - 1];

        let err = decode_frame_with_rate_into(&[], channels, rate, &mut output).unwrap_err();

        assert!(matches!(
            err,
            DstError::OutputBufferTooSmall {
                required,
                actual
            } if required == expected && actual == expected - 1
        ));
        assert!(output.iter().all(|&b| b == 0xa5));
    }

    #[test]
    fn compressed_output_index_guard_reports_output_overflow() {
        // Public decode APIs pass a slice sized to exact frame geometry, making
        // this branch unreachable through a well-formed public call. Exercise
        // the private compressed-path guard directly so future sink changes keep
        // oversized decoded output structured as OutputOverflow.
        let err = checked_decoded_output_index(9, 1, 10).unwrap_err();

        assert!(matches!(err, DstError::OutputOverflow { limit: 10 }));
    }

    #[test]
    fn stateful_decoder_into_clears_reused_buffer() {
        let channels = 1;
        let rate = DstRate::Dsd64;
        let expected = rate.frame_bytes_per_channel().unwrap();
        let mut output = vec![0xff; expected];
        let mut decoder = DstDecoder::new(channels, rate).unwrap();

        decoder
            .decode_frame_into(&raw_dst_frame(channels, rate, 0x00), &mut output)
            .unwrap();

        assert!(output.iter().all(|&b| b == 0x00));
    }

    #[test]
    fn legacy_decode_frame_is_dsd64() {
        let input = raw_dst_frame(3, DstRate::Dsd64, 0x5a);
        let decoded = decode_frame(&input, 3).unwrap();
        assert_eq!(decoded.len(), 4704 * 3);
        assert!(decoded.iter().all(|&b| b == 0x5a));
    }

    #[test]
    fn rejects_invalid_channel_count() {
        let err = decode_frame(&[0], 0).unwrap_err();
        assert!(matches!(err, DstError::InvalidChannelCount { channel_count: 0 }));
        let err = decode_frame(&[0], 7).unwrap_err();
        assert!(matches!(err, DstError::InvalidChannelCount { channel_count: 7 }));
    }

    #[test]
    fn rejects_unsupported_rate() {
        let err = DstRate::from_sample_rate(48_000).unwrap_err();
        assert!(matches!(err, DstError::UnsupportedRate { sample_rate: 48_000 }));
    }

    #[test]
    fn rejects_truncated_raw_frame() {
        let input = vec![0u8; 128];
        let err = decode_frame(&input, 2).unwrap_err();
        assert!(matches!(err, DstError::UnexpectedEof { .. }));
    }

    #[test]
    fn rejects_nonzero_raw_stuffing_bits() {
        let mut input = raw_dst_frame(1, DstRate::Dsd64, 0x11);
        input.push(0x80);
        let err = decode_frame(&input, 1).unwrap_err();
        assert!(matches!(err, DstError::MalformedFrame(_)));
    }

    #[test]
    fn rejects_invalid_segment_resolution() {
        // DSTCoded=1, PSameSegAsF=1, filter SameSegAllCh=1, not end-of-channel,
        // then an all-zero resolution field. This exercises the full segment
        // parser instead of the old simple-segmentation rejection path.
        let input = [0b1110_0000u8, 0, 0, 0];
        let err = decode_frame(&input, 2).unwrap_err();
        assert!(matches!(err, DstError::InvalidSegment(_)) | matches!(err, DstError::UnexpectedEof { .. }));
    }

    #[test]
    fn decoder_geometry_matches_spec_formula() {
        let cases = [
            (DstRate::Dsd64, 4704usize),
            (DstRate::Dsd128, 9408usize),
            (DstRate::Dsd256, 18816usize),
        ];
        for (rate, bytes_per_channel) in cases {
            assert_eq!(rate.frame_bytes_per_channel().unwrap(), bytes_per_channel);
            assert_eq!(rate.frame_bits_per_channel().unwrap(), bytes_per_channel * 8);
        }
    }

    #[test]
    fn pinned_stereo_dst_fixtures_decode_byte_exact() {
        use sha2::{Digest as _, Sha256};

        const CASES: [(&[u8], &[u8], &str, &str); 3] = [
            (
                include_bytes!("fixtures/frame_001.dst.bin"),
                include_bytes!("fixtures/frame_001.dsd.bin"),
                "a788eb38dd9cf5bf5313ed521dabca62107332e2ffa02bc0943384fe5b1e87e4",
                "4ba636974ba4217e348137a0ff9dda2df3f5ec1d80df03ad71c369a7a4f45ef7",
            ),
            (
                include_bytes!("fixtures/frame_002.dst.bin"),
                include_bytes!("fixtures/frame_002.dsd.bin"),
                "fd77fa6f66e793eb309963fcda75fecbd5927dd816538d158d3440359c40efc9",
                "d138a1d886e52c6fcd741d3eaf7ffe482b1bcb460e7a10062fa78bdf6e48d913",
            ),
            (
                include_bytes!("fixtures/frame_003.dst.bin"),
                include_bytes!("fixtures/frame_003.dsd.bin"),
                "9a788271fa0893b190ea180d47bf14019612b6f97c9680f71e80df41982ee921",
                "506f08c2eb6c82cd5ead58328f6b9a77c2676a391c49b90782ac3cda3fa4ff21",
            ),
        ];

        for (encoded, expected, encoded_sha256, decoded_sha256) in CASES {
            assert_eq!(format!("{:x}", Sha256::digest(encoded)), encoded_sha256);
            assert_eq!(format!("{:x}", Sha256::digest(expected)), decoded_sha256);
            let actual = decode_frame(encoded, 2).expect("pinned DST fixture must decode");
            assert_eq!(actual.as_slice(), expected);
        }
    }
}
