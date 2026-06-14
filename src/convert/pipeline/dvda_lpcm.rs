#![forbid(unsafe_code)]

//! DVD-Audio LPCM unpacking.
//!
//! The MPEG-PS demuxer strips the DVD-Audio sub-header and hands this module the
//! per-packet LPCM payload plus the parsed PCM sub-header. The unpacking model
//! follows foo_input_dvda's `pcm_audio_stream_t`: each decode step reads one
//! group-2 raw block when its cadence says to do so, then one group-1 raw block,
//! and writes two output sample instants as group 1 followed by group 2.
//! Samples are written as signed little-endian 32-bit PCM. Source 16/20/24-bit
//! values are left-aligned in the 32-bit carrier, except for the group-2 20-bit
//! low-nibble placement which intentionally mirrors foo_input_dvda.

use std::fmt;
use std::io::{self, Write};

use super::dvda_channel_layout::{layout_for_assignment_code, DvdaChannelOrderPolicy};
use super::dvda_demux::DvdaPcmSubHeader;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct LpcmStreamExpectation {
    pub sample_rate: Option<u32>,
    pub channel_count: Option<u32>,
    pub bit_depth: Option<u32>,
    pub group1_sample_rate: Option<u32>,
    pub group2_sample_rate: Option<u32>,
    pub group1_bit_depth: Option<u32>,
    pub group2_bit_depth: Option<u32>,
    pub group1_channel_count: Option<u32>,
    pub group2_channel_count: Option<u32>,
    pub channel_assignment_code: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LpcmParams {
    pub sample_rate: u32,
    pub channel_count: u32,
    pub bit_depth: u32,
    pub block_size: usize,
    pub samples_per_block_per_channel: u32,
    pub group1_sample_rate: u32,
    pub group2_sample_rate: Option<u32>,
    pub group1_channels: u32,
    pub group2_channels: u32,
    pub channel_assignment_code: u8,
    pub group1_bits: u32,
    pub group2_bits: Option<u32>,
    raw_group1_size: usize,
    raw_group2_size: usize,
    raw_group2_factor: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct LpcmDecodeStats {
    pub packets: u64,
    pub payload_bytes: u64,
    pub bytes_decoded: u64,
    pub samples_per_channel: u64,
    pub first_header: Option<DvdaPcmSubHeader>,
    pub last_header: Option<DvdaPcmSubHeader>,
    pub format_change_count: u64,
    pub first_audio_frame_pointer: Option<u16>,
    pub group2_blocks_read: u64,
    pub group2_blocks_repeated: u64,
    pub channel_order_policy: DvdaChannelOrderPolicy,
}

#[derive(Debug)]
pub(super) enum LpcmDecodeError {
    MissingSampleRate,
    MissingChannelCount,
    MissingBitDepth,
    UnsupportedBitDepth(u32),
    UnsupportedChannelCount(u32),
    UnsupportedChannelAssignment(u8),
    UnsupportedGroupRateRatio { group1: u32, group2: u32 },
    UnsupportedGroupBitDepthCombination { group1: u32, group2: u32 },
    HeaderMismatch(String),
    TrailingPartialBlock { bytes: usize, next_block_size: usize },
    Write(io::Error),
}

impl fmt::Display for LpcmDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSampleRate => write!(f, "DVD-Audio LPCM sample rate is unknown"),
            Self::MissingChannelCount => write!(f, "DVD-Audio LPCM channel count is unknown"),
            Self::MissingBitDepth => write!(f, "DVD-Audio LPCM bit depth is unknown"),
            Self::UnsupportedBitDepth(bits) => write!(
                f,
                "DVD-Audio LPCM bit depth {bits} is unsupported; expected 16, 20, or 24"
            ),
            Self::UnsupportedChannelCount(channels) => write!(
                f,
                "DVD-Audio LPCM channel count {channels} is unsupported; expected 1 through 8"
            ),
            Self::UnsupportedChannelAssignment(code) => write!(
                f,
                "DVD-Audio LPCM channel-assignment code {code} is unsupported; expected 0 through 20"
            ),
            Self::UnsupportedGroupRateRatio { group1, group2 } => write!(
                f,
                "DVD-Audio LPCM group-2 sample rate {group2} Hz is not an integer divisor of group-1 sample rate {group1} Hz"
            ),
            Self::UnsupportedGroupBitDepthCombination { group1, group2 } => write!(
                f,
                "DVD-Audio LPCM group-2 bit depth {group2} exceeds group-1 bit depth {group1}; this is outside the foo_input_dvda reference model"
            ),
            Self::HeaderMismatch(msg) => write!(f, "DVD-Audio LPCM packet/header mismatch: {msg}"),
            Self::TrailingPartialBlock {
                bytes,
                next_block_size,
            } => write!(
                f,
                "DVD-Audio LPCM stream ended with {bytes} trailing bytes; next decode step needs {next_block_size} bytes"
            ),
            Self::Write(err) => write!(f, "failed to write decoded DVD-Audio LPCM samples: {err}"),
        }
    }
}

impl std::error::Error for LpcmDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Write(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for LpcmDecodeError {
    fn from(err: io::Error) -> Self {
        Self::Write(err)
    }
}

pub(super) struct DvdAudioLpcmDecoder {
    expectation: LpcmStreamExpectation,
    params: Option<LpcmParams>,
    pending: Vec<u8>,
    stats: LpcmDecodeStats,
    channel_order_policy: DvdaChannelOrderPolicy,
    raw_group2_index: u32,
    last_group2_samples: Vec<i32>,
}

impl DvdAudioLpcmDecoder {
    pub(super) fn new(expectation: LpcmStreamExpectation) -> Self {
        Self {
            expectation,
            params: None,
            pending: Vec::new(),
            stats: LpcmDecodeStats::default(),
            channel_order_policy: DvdaChannelOrderPolicy::DEFAULT,
            raw_group2_index: 0,
            last_group2_samples: Vec::new(),
        }
    }

    pub(super) fn with_channel_order_policy(mut self, policy: DvdaChannelOrderPolicy) -> Self {
        self.channel_order_policy = policy;
        self.stats.channel_order_policy = policy;
        self
    }

    pub(super) fn decode_packet<W: Write>(
        &mut self,
        header: DvdaPcmSubHeader,
        payload: &[u8],
        out: &mut W,
    ) -> Result<(), LpcmDecodeError> {
        let params = self.resolve_params(header)?;
        self.validate_packet_header(header, params)?;

        if self.stats.first_header.is_none() {
            self.stats.first_header = Some(header);
            self.stats.first_audio_frame_pointer = Some(header.first_audio_frame);
        }
        if let Some(previous) = self.stats.last_header {
            if comparable_header(previous) != comparable_header(header) {
                self.stats.format_change_count = self.stats.format_change_count.saturating_add(1);
            }
        }
        self.stats.last_header = Some(header);
        self.stats.packets = self.stats.packets.saturating_add(1);
        self.stats.payload_bytes = self.stats.payload_bytes.saturating_add(payload.len() as u64);

        if !self.pending.is_empty() {
            self.pending.extend_from_slice(payload);
            let data = std::mem::take(&mut self.pending);
            self.decode_available(&data, params, out)?;
        } else {
            self.decode_available(payload, params, out)?;
        }

        Ok(())
    }

    pub(super) fn finish(self) -> Result<LpcmDecodeStats, LpcmDecodeError> {
        if !self.pending.is_empty() {
            let next_block_size = self
                .params
                .map(|params| params.next_raw_step_size(self.raw_group2_index))
                .unwrap_or(1);
            return Err(LpcmDecodeError::TrailingPartialBlock {
                bytes: self.pending.len(),
                next_block_size,
            });
        }
        Ok(self.stats)
    }

    pub(super) fn params(&self) -> Option<LpcmParams> {
        self.params
    }

    fn decode_available<W: Write>(
        &mut self,
        data: &[u8],
        params: LpcmParams,
        out: &mut W,
    ) -> Result<(), LpcmDecodeError> {
        let mut offset = 0usize;
        while offset < data.len() {
            let needed = params.next_raw_step_size(self.raw_group2_index);
            if data.len() - offset < needed {
                self.pending.extend_from_slice(&data[offset..]);
                break;
            }
            let step = &data[offset..offset + needed];
            self.decode_reference_step(step, params, out)?;
            offset += needed;
            self.stats.bytes_decoded = self.stats.bytes_decoded.saturating_add(needed as u64);
            self.stats.samples_per_channel = self.stats.samples_per_channel.saturating_add(2);
        }
        Ok(())
    }

    fn decode_reference_step<W: Write>(
        &mut self,
        step: &[u8],
        params: LpcmParams,
        out: &mut W,
    ) -> Result<(), LpcmDecodeError> {
        let mut cursor = 0usize;
        let group2 = if params.group2_channels == 0 {
            Vec::new()
        } else if self.raw_group2_index == 0 {
            let end = cursor + params.raw_group2_size;
            let samples = decode_group_samples(
                &step[cursor..end],
                params.group2_channels,
                params.group2_bits.unwrap_or(params.group1_bits),
                LpcmGroup::Group2,
            )?;
            cursor = end;
            self.last_group2_samples = samples.clone();
            self.stats.group2_blocks_read = self.stats.group2_blocks_read.saturating_add(1);
            samples
        } else {
            self.stats.group2_blocks_repeated = self.stats.group2_blocks_repeated.saturating_add(1);
            self.last_group2_samples.clone()
        };

        if params.group2_channels != 0 {
            self.raw_group2_index = self.raw_group2_index.saturating_add(1);
            if self.raw_group2_index == params.raw_group2_factor {
                self.raw_group2_index = 0;
            }
        }

        let group1_end = cursor + params.raw_group1_size;
        let group1 = decode_group_samples(
            &step[cursor..group1_end],
            params.group1_channels,
            params.group1_bits,
            LpcmGroup::Group1,
        )?;

        write_reference_interleave(&group1, &group2, params, self.channel_order_policy, out)?;
        Ok(())
    }

    fn resolve_params(&mut self, header: DvdaPcmSubHeader) -> Result<LpcmParams, LpcmDecodeError> {
        if let Some(params) = self.params {
            return Ok(params);
        }

        let layout = layout_for_assignment_code(header.channel_assignment)
            .ok_or(LpcmDecodeError::UnsupportedChannelAssignment(header.channel_assignment))?;
        if let Some(expected_assignment) = self.expectation.channel_assignment_code {
            let expected_layout = layout_for_assignment_code(expected_assignment)
                .ok_or(LpcmDecodeError::UnsupportedChannelAssignment(expected_assignment))?;
            if expected_layout.order_label() != layout.order_label() {
                return Err(LpcmDecodeError::HeaderMismatch(format!(
                    "IFO channel layout {} differs from LPCM packet layout {}",
                    expected_layout.group_label(),
                    layout.group_label()
                )));
            }
        }
        let group1_channels = choose_u32(
            self.expectation.group1_channel_count,
            Some(layout.group1_channel_count()),
            "IFO group 1 channel count",
            "LPCM packet channel-assignment group 1 channel count",
        )?
        .ok_or(LpcmDecodeError::MissingChannelCount)?;
        let group2_channels = choose_u32(
            self.expectation.group2_channel_count,
            Some(layout.group2_channel_count()),
            "IFO group 2 channel count",
            "LPCM packet channel-assignment group 2 channel count",
        )?
        .unwrap_or(0);
        let channel_count = group1_channels.saturating_add(group2_channels);
        if channel_count == 0 {
            return Err(LpcmDecodeError::MissingChannelCount);
        }
        if let Some(expected_channels) = self.expectation.channel_count {
            if expected_channels != channel_count {
                return Err(LpcmDecodeError::HeaderMismatch(format!(
                    "IFO channel count {expected_channels} differs from LPCM packet channel-assignment channel count {channel_count}"
                )));
            }
        }

        let group1_rate = choose_u32(
            self.expectation.group1_sample_rate.or(self.expectation.sample_rate),
            header.group1_sample_rate,
            "IFO group 1 sample rate",
            "LPCM packet group 1 sample rate",
        )?
        .or(header.group2_sample_rate)
        .ok_or(LpcmDecodeError::MissingSampleRate)?;
        let group2_rate = if group2_channels == 0 {
            None
        } else {
            Some(
                choose_u32(
                    self.expectation.group2_sample_rate,
                    header.group2_sample_rate,
                    "IFO group 2 sample rate",
                    "LPCM packet group 2 sample rate",
                )?
                .or(Some(group1_rate))
                .ok_or(LpcmDecodeError::MissingSampleRate)?,
            )
        };

        let group1_bits = choose_u32(
            self.expectation.group1_bit_depth.or(self.expectation.bit_depth),
            header.group1_bits,
            "IFO group 1 bit depth",
            "LPCM packet group 1 bit depth",
        )?
        .or(header.group2_bits)
        .ok_or(LpcmDecodeError::MissingBitDepth)?;
        let group2_bits = if group2_channels == 0 {
            None
        } else {
            Some(
                choose_u32(
                    self.expectation.group2_bit_depth,
                    header.group2_bits,
                    "IFO group 2 bit depth",
                    "LPCM packet group 2 bit depth",
                )?
                .or(Some(group1_bits))
                .ok_or(LpcmDecodeError::MissingBitDepth)?,
            )
        };
        if let Some(expected_bits) = self.expectation.bit_depth {
            let max_bits = group2_bits.unwrap_or(group1_bits).max(group1_bits);
            if expected_bits != max_bits && group2_bits.map_or(true, |bits| bits == group1_bits) {
                return Err(LpcmDecodeError::HeaderMismatch(format!(
                    "IFO bit depth {expected_bits} differs from LPCM packet bit depth {max_bits}"
                )));
            }
        }

        let params = LpcmParams::new(
            header.channel_assignment,
            group1_rate,
            group2_rate,
            group1_channels,
            group2_channels,
            group1_bits,
            group2_bits,
        )?;
        self.last_group2_samples = vec![0; (2 * group2_channels) as usize];
        self.params = Some(params);
        Ok(params)
    }

    fn validate_packet_header(
        &self,
        header: DvdaPcmSubHeader,
        params: LpcmParams,
    ) -> Result<(), LpcmDecodeError> {
        let layout = layout_for_assignment_code(header.channel_assignment)
            .ok_or(LpcmDecodeError::UnsupportedChannelAssignment(header.channel_assignment))?;
        let current = LpcmParams::new(
            header.channel_assignment,
            header.group1_sample_rate.unwrap_or(params.group1_sample_rate),
            if params.group2_channels == 0 {
                None
            } else {
                header.group2_sample_rate.or(params.group2_sample_rate)
            },
            layout.group1_channel_count(),
            layout.group2_channel_count(),
            header.group1_bits.unwrap_or(params.group1_bits),
            if params.group2_channels == 0 {
                None
            } else {
                header.group2_bits.or(params.group2_bits)
            },
        )?;
        if comparable_params(current) != comparable_params(params) {
            return Err(LpcmDecodeError::HeaderMismatch(format!(
                "LPCM packet format changed from {:?} to {:?}",
                comparable_params(params),
                comparable_params(current)
            )));
        }
        Ok(())
    }
}

impl LpcmParams {
    fn new(
        channel_assignment_code: u8,
        group1_sample_rate: u32,
        group2_sample_rate: Option<u32>,
        group1_channels: u32,
        group2_channels: u32,
        group1_bits: u32,
        group2_bits: Option<u32>,
    ) -> Result<Self, LpcmDecodeError> {
        let layout = layout_for_assignment_code(channel_assignment_code)
            .ok_or(LpcmDecodeError::UnsupportedChannelAssignment(channel_assignment_code))?;
        if layout.group1_channel_count() != group1_channels || layout.group2_channel_count() != group2_channels {
            return Err(LpcmDecodeError::HeaderMismatch(format!(
                "LPCM channel-assignment layout {} differs from resolved group counts {group1_channels}+{group2_channels}",
                layout.group_label()
            )));
        }
        let channel_count = group1_channels.saturating_add(group2_channels);
        if !(1..=8).contains(&channel_count) {
            return Err(LpcmDecodeError::UnsupportedChannelCount(channel_count));
        }
        validate_bits(group1_bits)?;
        if let Some(bits) = group2_bits {
            validate_bits(bits)?;
            if bits > group1_bits {
                return Err(LpcmDecodeError::UnsupportedGroupBitDepthCombination {
                    group1: group1_bits,
                    group2: bits,
                });
            }
        }
        let raw_group2_factor = if group2_channels == 0 {
            1
        } else {
            let group2_rate = group2_sample_rate.unwrap_or(group1_sample_rate);
            if group2_rate == 0 || group1_sample_rate < group2_rate || group1_sample_rate % group2_rate != 0 {
                return Err(LpcmDecodeError::UnsupportedGroupRateRatio {
                    group1: group1_sample_rate,
                    group2: group2_rate,
                });
            }
            group1_sample_rate / group2_rate
        };
        let raw_group1_size = raw_group_size(group1_channels, group1_bits)?;
        let raw_group2_size = if group2_channels == 0 {
            0
        } else {
            raw_group_size(group2_channels, group2_bits.unwrap_or(group1_bits))?
        };
        let bit_depth = group2_bits.unwrap_or(group1_bits).max(group1_bits);
        Ok(Self {
            sample_rate: group1_sample_rate,
            channel_count,
            bit_depth,
            block_size: raw_group1_size + raw_group2_size,
            samples_per_block_per_channel: 2,
            group1_sample_rate,
            group2_sample_rate,
            group1_channels,
            group2_channels,
            channel_assignment_code,
            group1_bits,
            group2_bits,
            raw_group1_size,
            raw_group2_size,
            raw_group2_factor,
        })
    }


    #[must_use]
    pub(super) fn source_channel_order_label(self) -> String {
        layout_for_assignment_code(self.channel_assignment_code)
            .map(|layout| layout.order_label())
            .unwrap_or_else(|| "unknown".to_string())
    }

    #[must_use]
    #[allow(dead_code)]
    pub(super) fn wave_channel_order_label(self) -> String {
        layout_for_assignment_code(self.channel_assignment_code)
            .map(|layout| layout.wave_order_label())
            .unwrap_or_else(|| "unknown".to_string())
    }

    #[must_use]
    pub(super) fn output_channel_order_label(self, policy: DvdaChannelOrderPolicy) -> String {
        layout_for_assignment_code(self.channel_assignment_code)
            .map(|layout| layout.output_order_label(policy))
            .unwrap_or_else(|| "unknown".to_string())
    }

    #[must_use]
    fn output_channel_indices(self, policy: DvdaChannelOrderPolicy) -> Vec<usize> {
        layout_for_assignment_code(self.channel_assignment_code)
            .map(|layout| layout.source_to_output_indices(policy))
            .unwrap_or_else(|| (0..self.channel_count as usize).collect())
    }
    fn next_raw_step_size(self, raw_group2_index: u32) -> usize {
        self.raw_group1_size
            + if self.group2_channels != 0 && raw_group2_index == 0 {
                self.raw_group2_size
            } else {
                0
            }
    }
}

fn comparable_header(header: DvdaPcmSubHeader) -> (u8, u8, u8, u8, u8) {
    (
        header.group1_bits_code,
        header.group2_bits_code,
        header.group1_sample_rate_code,
        header.group2_sample_rate_code,
        header.channel_assignment,
    )
}

fn comparable_params(params: LpcmParams) -> (u8, u32, Option<u32>, u32, u32, u32, Option<u32>) {
    (
        params.channel_assignment_code,
        params.group1_sample_rate,
        params.group2_sample_rate,
        params.group1_channels,
        params.group2_channels,
        params.group1_bits,
        params.group2_bits,
    )
}

fn choose_u32(
    expected: Option<u32>,
    packet: Option<u32>,
    expected_label: &str,
    packet_label: &str,
) -> Result<Option<u32>, LpcmDecodeError> {
    match (expected, packet) {
        (Some(lhs), Some(rhs)) if lhs != rhs => Err(LpcmDecodeError::HeaderMismatch(format!(
            "{expected_label} {lhs} differs from {packet_label} {rhs}"
        ))),
        (Some(value), _) | (_, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

fn validate_bits(bits: u32) -> Result<(), LpcmDecodeError> {
    match bits {
        16 | 20 | 24 => Ok(()),
        other => Err(LpcmDecodeError::UnsupportedBitDepth(other)),
    }
}

fn raw_group_size(channels: u32, bits: u32) -> Result<usize, LpcmDecodeError> {
    validate_bits(bits)?;
    Ok((channels as usize) * (bits as usize) / 4)
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LpcmGroup {
    Group1,
    Group2,
}

fn decode_group_samples(
    block: &[u8],
    channels: u32,
    bits: u32,
    group: LpcmGroup,
) -> Result<Vec<i32>, LpcmDecodeError> {
    let sample_count = (2 * channels) as usize;
    let mut samples = Vec::with_capacity(sample_count);
    match bits {
        16 => {
            for sample in block.chunks_exact(2).take(sample_count) {
                let signed = i16::from_be_bytes([sample[0], sample[1]]) as i32;
                samples.push(signed << 16);
            }
        }
        20 => {
            let packed_offset = 4 * channels as usize;
            for i in 0..sample_count {
                let high = [block[2 * i], block[2 * i + 1]];
                let packed = block[packed_offset + i / 2];
                let byte1 = match group {
                    LpcmGroup::Group1 if i % 2 == 0 => packed & 0xf0,
                    LpcmGroup::Group1 => packed << 4,
                    LpcmGroup::Group2 if i % 2 == 0 => packed >> 4,
                    LpcmGroup::Group2 => packed & 0x0f,
                };
                samples.push(i32::from_le_bytes([0, byte1, high[1], high[0]]));
            }
        }
        24 => {
            let packed_offset = 4 * channels as usize;
            for i in 0..sample_count {
                let high = [block[2 * i], block[2 * i + 1]];
                let low = block[packed_offset + i];
                samples.push(i32::from_le_bytes([0, low, high[1], high[0]]));
            }
        }
        other => return Err(LpcmDecodeError::UnsupportedBitDepth(other)),
    }
    Ok(samples)
}

fn write_reference_interleave<W: Write>(
    group1: &[i32],
    group2: &[i32],
    params: LpcmParams,
    policy: DvdaChannelOrderPolicy,
    out: &mut W,
) -> Result<(), LpcmDecodeError> {
    let g1 = params.group1_channels as usize;
    let g2 = params.group2_channels as usize;
    let reorder = params.output_channel_indices(policy);

    let mut first_frame = Vec::with_capacity((g1 + g2) as usize);
    first_frame.extend_from_slice(&group1[..g1]);
    first_frame.extend_from_slice(&group2[..g2]);
    write_ordered_frame(&first_frame, &reorder, out)?;

    let mut second_frame = Vec::with_capacity((g1 + g2) as usize);
    second_frame.extend_from_slice(&group1[g1..2 * g1]);
    second_frame.extend_from_slice(&group2[g2..2 * g2]);
    write_ordered_frame(&second_frame, &reorder, out)?;

    Ok(())
}

fn write_ordered_frame<W: Write>(
    source_order_frame: &[i32],
    source_to_output_indices: &[usize],
    out: &mut W,
) -> Result<(), LpcmDecodeError> {
    for &source_index in source_to_output_indices {
        let sample = source_order_frame.get(source_index).ok_or_else(|| {
            LpcmDecodeError::HeaderMismatch(format!(
                "LPCM channel reorder index {source_index} is outside decoded source frame with {} channels",
                source_order_frame.len()
            ))
        })?;
        out.write_all(&sample.to_le_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm_header(
        bits: u32,
        rate: u32,
        channel_assignment: u8,
        group2_bits: Option<u32>,
        group2_rate: Option<u32>,
    ) -> DvdaPcmSubHeader {
        DvdaPcmSubHeader {
            first_audio_frame: 0x1234,
            group1_bits_code: bits_code(bits),
            group2_bits_code: group2_bits.map(bits_code).unwrap_or(0),
            group1_sample_rate_code: rate_code(rate),
            group2_sample_rate_code: group2_rate.map(rate_code).unwrap_or(0),
            group1_bits: Some(bits),
            group2_bits,
            group1_sample_rate: Some(rate),
            group2_sample_rate: group2_rate,
            channel_assignment,
            cci: 0,
        }
    }

    fn bits_code(bits: u32) -> u8 {
        match bits {
            16 => 0,
            20 => 1,
            24 => 2,
            _ => 0xf,
        }
    }

    fn rate_code(rate: u32) -> u8 {
        match rate {
            48_000 => 0,
            96_000 => 1,
            192_000 => 2,
            44_100 => 8,
            88_200 => 9,
            176_400 => 10,
            _ => 0xf,
        }
    }

    fn s32le_words(bytes: &[u8]) -> Vec<i32> {
        bytes
            .chunks_exact(4)
            .map(|chunk| i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()
    }

    #[test]
    fn decodes_16_bit_stereo_in_foo_group_order() {
        let mut decoder = DvdAudioLpcmDecoder::new(LpcmStreamExpectation::default());
        let payload = [0x12, 0x34, 0xed, 0xcc, 0x56, 0x78, 0x80, 0x00];
        let mut out = Vec::new();

        decoder
            .decode_packet(pcm_header(16, 48_000, 1, None, None), &payload, &mut out)
            .expect("decode");
        let stats = decoder.finish().expect("finish");

        assert_eq!(stats.samples_per_channel, 2);
        assert_eq!(stats.first_audio_frame_pointer, Some(0x1234));
        assert_eq!(
            s32le_words(&out),
            vec![
                0x1234_0000_i32,
                (0xedcc_0000_u32 as i32),
                0x5678_0000_i32,
                (0x8000_0000_u32 as i32),
            ]
        );
    }

    #[test]
    fn decodes_24_bit_stereo_in_foo_group_order() {
        let mut decoder = DvdAudioLpcmDecoder::new(LpcmStreamExpectation::default());
        let payload = [
            0x00, 0x01, 0xff, 0xfe, 0x12, 0x34, 0x80, 0x00, 0x02, 0xfd, 0x56, 0x00,
        ];
        let mut out = Vec::new();

        decoder
            .decode_packet(pcm_header(24, 96_000, 1, None, None), &payload, &mut out)
            .expect("decode");
        let stats = decoder.finish().expect("finish");

        assert_eq!(stats.samples_per_channel, 2);
        assert_eq!(out.len(), 4 * 4);
        assert_eq!(&out[0..4], &0x0001_0200_i32.to_le_bytes());
        assert_eq!(&out[4..8], &(0xfffe_fd00_u32 as i32).to_le_bytes());
        assert_eq!(&out[8..12], &0x1234_5600_i32.to_le_bytes());
        assert_eq!(&out[12..16], &(0x8000_0000_u32 as i32).to_le_bytes());
    }

    #[test]
    fn decodes_group2_before_group1_and_repeats_lower_rate_group2() {
        let mut decoder = DvdAudioLpcmDecoder::new(LpcmStreamExpectation::default());
        let header = pcm_header(16, 96_000, 2, Some(16), Some(48_000));
        let payload = [
            0x11, 0x11, 0x22, 0x22, // group 2, one channel, two samples
            0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x04, 0x00, // group 1, two channels
            0x05, 0x00, 0x06, 0x00, 0x07, 0x00, 0x08, 0x00, // group 1 again; group 2 repeats
        ];
        let mut out = Vec::new();

        decoder.decode_packet(header, &payload, &mut out).expect("decode");
        let stats = decoder.finish().expect("finish");

        assert_eq!(stats.samples_per_channel, 4);
        assert_eq!(stats.group2_blocks_read, 1);
        assert_eq!(stats.group2_blocks_repeated, 1);
        assert_eq!(
            s32le_words(&out),
            vec![
                0x0100_0000,
                0x0200_0000,
                0x1111_0000,
                0x0300_0000,
                0x0400_0000,
                0x2222_0000,
                0x0500_0000,
                0x0600_0000,
                0x1111_0000,
                0x0700_0000,
                0x0800_0000,
                0x2222_0000,
            ]
        );
    }

    #[test]
    fn decodes_20_bit_group1_and_group2_nibbles_like_foo_input_dvda() {
        let group1 = decode_group_samples(
            &[0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x04, 0xab, 0xcd],
            1,
            20,
            LpcmGroup::Group1,
        )
        .expect("group1");
        let group2 = decode_group_samples(
            &[0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x04, 0xab, 0xcd],
            1,
            20,
            LpcmGroup::Group2,
        )
        .expect("group2");

        assert_eq!(group1[0].to_le_bytes(), [0x00, 0xa0, 0x01, 0x00]);
        assert_eq!(group1[1].to_le_bytes(), [0x00, 0xb0, 0x02, 0x00]);
        assert_eq!(group2[0].to_le_bytes(), [0x00, 0x0a, 0x01, 0x00]);
        assert_eq!(group2[1].to_le_bytes(), [0x00, 0x0b, 0x02, 0x00]);
    }

    #[test]
    fn wave_policy_reorders_lpcm_from_dvda_group_order() {
        let mut decoder = DvdAudioLpcmDecoder::new(LpcmStreamExpectation::default())
            .with_channel_order_policy(DvdaChannelOrderPolicy::WaveExtensible);
        let header = pcm_header(16, 48_000, 20, Some(16), Some(48_000));
        let payload = [
            0x03, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0a, 0x00, // group 2: C1,LFE1,C2,LFE2
            0x01, 0x00, 0x02, 0x00, 0x05, 0x00, 0x06, 0x00, // group 1 frame 1: L,R,Ls,Rs
            0x07, 0x00, 0x08, 0x00, 0x0b, 0x00, 0x0c, 0x00, // group 1 frame 2
        ];
        let mut out = Vec::new();

        decoder.decode_packet(header, &payload, &mut out).expect("decode");
        let stats = decoder.finish().expect("finish");

        assert_eq!(stats.channel_order_policy, DvdaChannelOrderPolicy::WaveExtensible);
        assert_eq!(
            s32le_words(&out),
            vec![
                0x0100_0000, 0x0200_0000, 0x0300_0000, 0x0400_0000, 0x0500_0000, 0x0600_0000,
                0x0700_0000, 0x0800_0000, 0x0900_0000, 0x0a00_0000, 0x0b00_0000, 0x0c00_0000,
            ]
        );
    }

    #[test]
    fn rejects_trailing_partial_reference_step() {
        let mut decoder = DvdAudioLpcmDecoder::new(LpcmStreamExpectation::default());

        decoder
            .decode_packet(pcm_header(16, 48_000, 1, None, None), &[0x01, 0x02], &mut Vec::new())
            .expect("packet can buffer partial data");
        let err = decoder.finish().expect_err("partial block rejected");
        assert!(matches!(err, LpcmDecodeError::TrailingPartialBlock { .. }));
    }

    #[test]
    fn rejects_non_integer_group_rate_ratio() {
        let mut decoder = DvdAudioLpcmDecoder::new(LpcmStreamExpectation {
            group1_sample_rate: Some(96_000),
            group2_sample_rate: Some(44_100),
            group1_channel_count: Some(2),
            group2_channel_count: Some(1),
            group1_bit_depth: Some(24),
            group2_bit_depth: Some(24),
            ..LpcmStreamExpectation::default()
        });
        let err = decoder
            .decode_packet(pcm_header(24, 96_000, 2, Some(24), Some(44_100)), &[0; 18], &mut Vec::new())
            .expect_err("unsupported ratio");
        assert!(matches!(err, LpcmDecodeError::UnsupportedGroupRateRatio { .. }));
    }

    #[test]
    fn rejects_group2_bit_depth_deeper_than_group1_reference_invalid() {
        let mut decoder = DvdAudioLpcmDecoder::new(LpcmStreamExpectation::default());
        let err = decoder
            .decode_packet(
                pcm_header(16, 48_000, 2, Some(20), Some(48_000)),
                &[0; 14],
                &mut Vec::new(),
            )
            .expect_err("group 2 cannot be deeper than group 1");
        assert!(matches!(
            err,
            LpcmDecodeError::UnsupportedGroupBitDepthCombination { group1: 16, group2: 20 }
        ));
    }

    #[test]
    fn lpcm_matches_foo_input_dvda_reference_vectors() {
        let Some(stdout) = run_foo_reference_generator() else {
            return;
        };

        let mut checked = 0usize;
        let mut assignment_seen = [false; 21];
        let mut group1_bits_seen = [false; 3];
        let mut group2_bits_seen = [false; 3];
        let mut ratio_seen = [false; 5];

        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields = parse_reference_vector_line(line);
            let code = parse_u8(&fields, "code");
            let g1_bits = parse_u32(&fields, "g1_bits");
            let g2_bits = parse_u32(&fields, "g2_bits");
            let g1_rate = parse_u32(&fields, "g1_rate");
            let g2_rate = parse_u32(&fields, "g2_rate");
            let ratio = parse_usize(&fields, "ratio");
            let payload = hex_to_bytes(required_field(&fields, "payload"));
            let expected_source = hex_to_bytes(required_field(&fields, "source_s32le"));
            let expected_wave = hex_to_bytes(required_field(&fields, "wave_s32le"));
            let layout = layout_for_assignment_code(code).expect("assignment covered by table");
            let header = pcm_header(
                g1_bits,
                g1_rate,
                code,
                if layout.group2_channel_count() == 0 { None } else { Some(g2_bits) },
                if g2_rate == 0 { None } else { Some(g2_rate) },
            );

            let source_output = decode_reference_vector(header, &payload, DvdaChannelOrderPolicy::PreserveDvdAudio);
            assert_eq!(
                source_output,
                expected_source,
                "source-order LPCM output differs from foo_input_dvda fixture for {line}"
            );

            let wave_output = decode_reference_vector(header, &payload, DvdaChannelOrderPolicy::WaveExtensible);
            assert_eq!(
                wave_output,
                expected_wave,
                "WAVEFORMATEXTENSIBLE-order LPCM output differs from foo_input_dvda fixture for {line}"
            );

            assignment_seen[code as usize] = true;
            mark_bits(&mut group1_bits_seen, g1_bits);
            if g2_bits != 0 {
                mark_bits(&mut group2_bits_seen, g2_bits);
            }
            if ratio < ratio_seen.len() {
                ratio_seen[ratio] = true;
            }
            checked += 1;
        }

        assert_eq!(checked, 348, "unexpected foo_input_dvda reference-vector count");
        assert!(assignment_seen.iter().all(|seen| *seen), "not every DVD-A LPCM assignment code was covered");
        assert!(group1_bits_seen.iter().all(|seen| *seen), "not every group-1 LPCM bit depth was covered");
        assert!(group2_bits_seen.iter().all(|seen| *seen), "not every group-2 LPCM bit depth was covered");
        assert!(ratio_seen[1] && ratio_seen[2] && ratio_seen[4], "not every supported group-rate ratio was covered");
    }

    fn decode_reference_vector(
        header: DvdaPcmSubHeader,
        payload: &[u8],
        policy: DvdaChannelOrderPolicy,
    ) -> Vec<u8> {
        let mut decoder = DvdAudioLpcmDecoder::new(LpcmStreamExpectation::default())
            .with_channel_order_policy(policy);
        let mut out = Vec::new();
        decoder.decode_packet(header, payload, &mut out).expect("decode reference vector");
        decoder.finish().expect("finish reference vector");
        out
    }

    fn run_foo_reference_generator() -> Option<String> {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let strict = std::env::var("TONEPOET_DVDA_STRICT_REFERENCE_TESTS")
            .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
            .unwrap_or(false);
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let source = manifest_dir.join("tests/fixtures/dvda_lpcm_foo_reference_vectors.cpp");
        if !source.exists() {
            if strict {
                panic!("foo_input_dvda LPCM reference-vector fixture is missing: {}", source.display());
            }
            eprintln!("skipping foo_input_dvda LPCM reference-vector comparison; fixture is missing: {}", source.display());
            return None;
        }

        let compiler = std::env::var("CXX").unwrap_or_else(|_| "c++".to_string());
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let out_dir = std::env::temp_dir().join(format!(
            "tonepoet-dvda-lpcm-reference-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&out_dir).expect("create reference-vector temp dir");
        let exe = out_dir.join(if cfg!(windows) {
            "dvda_lpcm_foo_reference_vectors.exe"
        } else {
            "dvda_lpcm_foo_reference_vectors"
        });

        let compile = Command::new(&compiler)
            .arg("-std=c++17")
            .arg("-O2")
            .arg(&source)
            .arg("-o")
            .arg(&exe)
            .output();
        let compile = match compile {
            Ok(output) => output,
            Err(err) => {
                if strict {
                    panic!("failed to run C++ compiler {compiler:?} for foo_input_dvda LPCM reference vectors: {err}");
                }
                eprintln!("skipping foo_input_dvda LPCM reference-vector comparison; failed to run C++ compiler {compiler:?}: {err}");
                return None;
            }
        };
        if !compile.status.success() {
            let stderr = String::from_utf8_lossy(&compile.stderr);
            if strict {
                panic!("failed to compile foo_input_dvda LPCM reference-vector fixture with {compiler:?}: {stderr}");
            }
            eprintln!("skipping foo_input_dvda LPCM reference-vector comparison; fixture did not compile with {compiler:?}: {stderr}");
            return None;
        }

        let run = Command::new(&exe).output().expect("run foo_input_dvda LPCM reference-vector fixture");
        if !run.status.success() {
            panic!(
                "foo_input_dvda LPCM reference-vector fixture failed: {}",
                String::from_utf8_lossy(&run.stderr)
            );
        }
        Some(String::from_utf8(run.stdout).expect("reference fixture stdout is utf-8"))
    }

    fn parse_reference_vector_line(line: &str) -> std::collections::BTreeMap<String, String> {
        line.split('\t')
            .map(|part| {
                let (key, value) = part.split_once('=').unwrap_or_else(|| {
                    panic!("malformed foo_input_dvda reference-vector field {part:?} in {line:?}")
                });
                (key.to_string(), value.to_string())
            })
            .collect()
    }

    fn required_field<'a>(fields: &'a std::collections::BTreeMap<String, String>, key: &str) -> &'a str {
        fields
            .get(key)
            .map(String::as_str)
            .unwrap_or_else(|| panic!("missing foo_input_dvda reference-vector field {key}"))
    }

    fn parse_u8(fields: &std::collections::BTreeMap<String, String>, key: &str) -> u8 {
        required_field(fields, key).parse().unwrap_or_else(|err| panic!("invalid {key}: {err}"))
    }

    fn parse_u32(fields: &std::collections::BTreeMap<String, String>, key: &str) -> u32 {
        required_field(fields, key).parse().unwrap_or_else(|err| panic!("invalid {key}: {err}"))
    }

    fn parse_usize(fields: &std::collections::BTreeMap<String, String>, key: &str) -> usize {
        required_field(fields, key).parse().unwrap_or_else(|err| panic!("invalid {key}: {err}"))
    }

    fn mark_bits(seen: &mut [bool; 3], bits: u32) {
        match bits {
            16 => seen[0] = true,
            20 => seen[1] = true,
            24 => seen[2] = true,
            other => panic!("unexpected LPCM bit depth in reference vector: {other}"),
        }
    }

    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        assert!(hex.len() % 2 == 0, "hex string length must be even");
        (0..hex.len())
            .step_by(2)
            .map(|offset| u8::from_str_radix(&hex[offset..offset + 2], 16).expect("hex byte"))
            .collect()
    }

}
