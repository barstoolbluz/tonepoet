//! BD-ROM LPCM payload handling for Blu-ray realization.
//!
//! This module owns the LPCM PES payload header validation, channel-order
//! decisions, big-endian to little-endian PCM conversion, bounded missing-PTS
//! pre-roll handling, and sample-frame chapter trimming. It deliberately does
//! not read TS packets or publish output files.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::disc::bluray_utils::{parse_bluray_lpcm_header, BlurayLpcmHeader};

use super::bluray_pts::{ceil_pts_to_samples, floor_pts_to_samples, samples_to_pts90, BlurayPtsMapper, PTS_CLOCK_HZ};
use super::bluray_ts_demux::{parse_lpcm_pes_packet, SelectedPesPacket};
use super::bluray_wav_validate::{expected_pcm_data_bytes, ExpectedAudio, LpcmWavFormat};
use super::errors::ConvertError;

const BLURAY_LPCM_PREROLL_MAX_PACKETS: usize = 8;
const BLURAY_LPCM_PREROLL_MAX_BYTES: usize = 4 * 1024 * 1024;

fn checked_sample_byte_offset(samples: u64, frame_bytes: usize) -> Result<usize, ConvertError> {
    samples
        .checked_mul(frame_bytes as u64)
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| {
            ConvertError::Realize(format!(
                "Blu-ray LPCM sample byte offset overflows: {samples} samples × {frame_bytes} bytes/frame"
            ))
        })
}

pub(crate) struct BlurayLpcmExtractionConfig {
    pub(crate) source: PathBuf,
    pub(crate) playlist_number: u32,
    pub(crate) chapter_number: u32,
    pub(crate) chapter_start_pts_90k: u64,
    pub(crate) chapter_end_pts_90k: Option<u64>,
    pub(crate) audio_pid: u16,
    pub(crate) audio_stream_index: u8,
    pub(crate) expected_sample_rate: Option<u32>,
    pub(crate) expected_bit_depth: Option<u32>,
    pub(crate) expected_channels: Option<u8>,
    pub(crate) expected_channel_layout: Option<String>,
    pub(crate) pts_mapper: BlurayPtsMapper,
}

pub(crate) struct BlurayLpcmExtractor {
    pub(crate) config: BlurayLpcmExtractionConfig,
    format: Option<LpcmWavFormat>,
    header: Option<BlurayLpcmHeader>,
    channel_map: Option<BlurayChannelMap>,
    pending_pcm: Vec<u8>,
    scratch: Vec<u8>,
    data_bytes_written: u64,
    accepted_pes_packets: u64,
    skipped_before_chapter: u64,
    saw_chapter_end: bool,
    preroll: PrerollBuffer,
    timestamp_anchor_title_pts_90k: Option<u64>,
    samples_since_timestamp_anchor: u64,
    last_selected_continuity_end: Option<u8>,
}

impl BlurayLpcmExtractor {
    pub(crate) fn new(config: BlurayLpcmExtractionConfig) -> Self {
        Self {
            config,
            format: None,
            header: None,
            channel_map: None,
            pending_pcm: Vec::new(),
            scratch: Vec::new(),
            data_bytes_written: 0,
            accepted_pes_packets: 0,
            skipped_before_chapter: 0,
            saw_chapter_end: false,
            preroll: PrerollBuffer::default_lpcm(),
            timestamp_anchor_title_pts_90k: None,
            samples_since_timestamp_anchor: 0,
            last_selected_continuity_end: None,
        }
    }

    pub(crate) fn peek_or_create_wav_format(&mut self, pes: &[u8]) -> Result<LpcmWavFormat, ConvertError> {
        if let Some(format) = self.format {
            return Ok(format);
        }
        let parsed = parse_lpcm_pes_packet(pes)?;
        let header = parse_lpcm_payload_header(parsed.payload, &self.config)?;
        self.prepare_format_from_header(header)
    }

    pub(crate) fn process_pes_packet(
        &mut self,
        pes: SelectedPesPacket,
        output: &mut File,
    ) -> Result<PesProcessingOutcome, ConvertError> {
        let raw_pts_90k = parse_lpcm_pes_packet(&pes.payload)?.pts_90k;
        let Some(raw_pts_90k) = raw_pts_90k else {
            return self.process_missing_pts_pes(pes, output);
        };
        let parsed = parse_lpcm_pes_packet(&pes.payload)?;

        let Some(title_pts_90k) = self
            .config
            .pts_mapper
            .map_pes_pts_to_title_pts(raw_pts_90k)
        else {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray LPCM PES PTS {} in playlist {:05} PID 0x{:04x} could not be mapped to the title timeline for chapter {}",
                raw_pts_90k,
                self.config.playlist_number,
                self.config.audio_pid,
                self.config.chapter_number
            )));
        };

        if !self.preroll.is_empty() {
            if let Some(end) = self.config.chapter_end_pts_90k {
                if title_pts_90k >= end {
                    self.preroll.clear();
                    self.saw_chapter_end = true;
                    self.mark_packet_consumed(&pes);
                    return Ok(PesProcessingOutcome::Stop);
                }
            }

            if title_pts_90k < self.config.chapter_start_pts_90k {
                // Buffered PES precede this timestamped PES, so a timestamp
                // before the chapter proves the whole pre-roll buffer is before
                // the chapter as well.
                self.preroll.clear();
            } else {
                if !self.preroll.has_clean_continuity_to(&pes) {
                    return Err(ConvertError::TrackValidation(format!(
                        "Blu-ray LPCM pre-roll before playlist {:05} chapter {} cannot be attached to first in-range PTS because selected PID continuity counters have a gap",
                        self.config.playlist_number,
                        self.config.chapter_number
                    )));
                }
                if self.flush_preroll_before_title_pts(title_pts_90k, output)?
                    == PesProcessingOutcome::Stop
                {
                    self.mark_packet_consumed(&pes);
                    return Ok(PesProcessingOutcome::Stop);
                }
            }
        }

        let (outcome, pes_samples) = self.process_lpcm_payload_at_title_pts(
            parsed.payload,
            title_pts_90k,
            output,
        )?;
        self.record_timestamped_pes(title_pts_90k, pes_samples);
        self.mark_packet_consumed(&pes);
        Ok(outcome)
    }

    fn process_missing_pts_pes(
        &mut self,
        pes: SelectedPesPacket,
        output: &mut File,
    ) -> Result<PesProcessingOutcome, ConvertError> {
        if self.timestamp_anchor_title_pts_90k.is_none() {
            self.preroll.push(PendingPes::from_selected(pes), self.config.playlist_number, self.config.audio_pid)?;
            return Ok(PesProcessingOutcome::Continue);
        }

        if !self.continuity_is_clean_after_last(pes.continuity_start) {
            return Err(ConvertError::TrackValidation(format!(
                "selected Blu-ray LPCM PES is missing PTS after timestamp lock following a TS continuity gap on PID 0x{:04x} in playlist {:05}",
                self.config.audio_pid,
                self.config.playlist_number
            )));
        }

        let parsed = parse_lpcm_pes_packet(&pes.payload)?;
        let payload = parsed.payload;
        let pes_samples = self.exact_lpcm_sample_count(payload)?;
        let title_pts_90k = self.derived_next_title_pts()?.ok_or_else(|| {
            ConvertError::TrackValidation(format!(
                "selected Blu-ray LPCM PES is missing PTS after timestamp lock, but no exact LPCM sample timeline is available for playlist {:05} PID 0x{:04x}",
                self.config.playlist_number,
                self.config.audio_pid
            ))
        })?;

        let (outcome, processed_samples) = self.process_lpcm_payload_at_title_pts(
            payload,
            title_pts_90k,
            output,
        )?;
        debug_assert_eq!(processed_samples, pes_samples);
        self.record_derived_pes(pes_samples)?;
        self.mark_packet_consumed(&pes);
        Ok(outcome)
    }

    fn flush_preroll_before_title_pts(
        &mut self,
        following_title_pts_90k: u64,
        output: &mut File,
    ) -> Result<PesProcessingOutcome, ConvertError> {
        if self.preroll.is_empty() {
            return Ok(PesProcessingOutcome::Continue);
        }

        let pending = self.preroll.take_all();
        let mut total_preroll_duration_90k = 0u64;
        let mut pending_with_samples = Vec::with_capacity(pending.len());
        for pending_pes in pending {
            let parsed = parse_lpcm_pes_packet(&pending_pes.payload)?;
            if parsed.pts_90k.is_some() {
                return Err(ConvertError::Realize(
                    "internal Blu-ray pre-roll buffer contained a timestamped PES".to_string(),
                ));
            }
            let samples = self.exact_lpcm_sample_count(parsed.payload)?;
            let format = self.format.ok_or_else(|| {
                ConvertError::Realize("Blu-ray LPCM format missing after pre-roll sample analysis".to_string())
            })?;
            total_preroll_duration_90k = total_preroll_duration_90k
                .checked_add(samples_to_pts90(samples, format.sample_rate)?)
                .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM pre-roll duration overflow".to_string()))?;
            pending_with_samples.push((pending_pes, samples));
        }

        let mut title_pts_90k = following_title_pts_90k.saturating_sub(total_preroll_duration_90k);
        for (pending_pes, samples) in pending_with_samples {
            let parsed = parse_lpcm_pes_packet(&pending_pes.payload)?;
            let (outcome, processed_samples) = self.process_lpcm_payload_at_title_pts(
                parsed.payload,
                title_pts_90k,
                output,
            )?;
            debug_assert_eq!(processed_samples, samples);
            self.last_selected_continuity_end = Some(pending_pes.continuity_end);
            if outcome == PesProcessingOutcome::Stop {
                return Ok(PesProcessingOutcome::Stop);
            }
            let format = self.format.ok_or_else(|| {
                ConvertError::Realize("Blu-ray LPCM format missing after pre-roll flush".to_string())
            })?;
            title_pts_90k = title_pts_90k
                .checked_add(samples_to_pts90(samples, format.sample_rate)?)
                .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM pre-roll PTS overflow".to_string()))?;
        }

        Ok(PesProcessingOutcome::Continue)
    }

    fn prepare_format_from_header(
        &mut self,
        header: BlurayLpcmHeader,
    ) -> Result<LpcmWavFormat, ConvertError> {
        if let Some(prior) = self.header {
            if prior != header {
                return Err(ConvertError::TrackValidation(format!(
                    "Blu-ray LPCM header changed within playlist {:05} PID 0x{:04x}: first {:?}, later {:?}",
                    self.config.playlist_number, self.config.audio_pid, prior, header
                )));
            }
            return self.format.ok_or_else(|| {
                ConvertError::Realize("Blu-ray LPCM header was set without a WAV format".to_string())
            });
        }

        validate_lpcm_header_against_expectations(&self.config, header)?;
        let channel_map = BlurayChannelMap::from_lpcm_header(header)?;
        let format = LpcmWavFormat::new(
            header.sample_rate,
            header.channels,
            header.container_bits,
            header.valid_bits,
            channel_map.wav_channel_mask,
        )?;
        self.header = Some(header);
        self.channel_map = Some(channel_map);
        self.format = Some(format);
        Ok(format)
    }

    fn process_lpcm_payload_at_title_pts(
        &mut self,
        pes_payload: &[u8],
        title_pts_90k: u64,
        output: &mut File,
    ) -> Result<(PesProcessingOutcome, u64), ConvertError> {
        let header = parse_lpcm_payload_header(pes_payload, &self.config)?;
        let format = self.prepare_format_from_header(header)?;
        let pcm_payload = &pes_payload[4..];
        let timing = self.lpcm_payload_timing(pcm_payload, format)?;
        let pes_samples = timing.sample_frames;

        if pes_samples == 0 {
            return Ok((PesProcessingOutcome::Continue, 0));
        }

        let chapter_start = self.config.chapter_start_pts_90k;
        let chapter_end = self.config.chapter_end_pts_90k;
        if let Some(end) = chapter_end {
            if title_pts_90k >= end {
                self.saw_chapter_end = true;
                return Ok((PesProcessingOutcome::Stop, pes_samples));
            }
        }

        let skip_samples = if title_pts_90k < chapter_start {
            ceil_pts_to_samples(chapter_start - title_pts_90k, format.sample_rate)?
        } else {
            0
        };

        let keep_until_samples = if let Some(end) = chapter_end {
            floor_pts_to_samples(end.saturating_sub(title_pts_90k), format.sample_rate)?
        } else {
            pes_samples
        };

        let skip_samples = skip_samples.min(pes_samples);
        let keep_until_samples = keep_until_samples.min(pes_samples);
        let mut outcome = PesProcessingOutcome::Continue;
        if chapter_end.is_some() && keep_until_samples < pes_samples {
            self.saw_chapter_end = true;
            outcome = PesProcessingOutcome::Stop;
        }

        if skip_samples >= keep_until_samples {
            if title_pts_90k < chapter_start {
                self.skipped_before_chapter = self.skipped_before_chapter.saturating_add(1);
            }
            return Ok((outcome, pes_samples));
        }

        let start_byte = checked_sample_byte_offset(skip_samples, timing.input_frame_bytes)?;
        let end_byte = checked_sample_byte_offset(keep_until_samples, timing.input_frame_bytes)?;
        let written = self.write_pcm_payload(output, &pcm_payload[start_byte..end_byte], format)?;
        self.data_bytes_written = self
            .data_bytes_written
            .checked_add(written)
            .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM WAV size overflow".to_string()))?;
        self.accepted_pes_packets = self.accepted_pes_packets.saturating_add(1);
        Ok((outcome, pes_samples))
    }

    fn exact_lpcm_sample_count(&mut self, pes_payload: &[u8]) -> Result<u64, ConvertError> {
        let header = parse_lpcm_payload_header(pes_payload, &self.config)?;
        let format = self.prepare_format_from_header(header)?;
        let pcm_payload = &pes_payload[4..];
        Ok(self.lpcm_payload_timing(pcm_payload, format)?.sample_frames)
    }

    fn lpcm_payload_timing(
        &self,
        pcm_payload: &[u8],
        format: LpcmWavFormat,
    ) -> Result<LpcmPayloadTiming, ConvertError> {
        let header = self.header.ok_or_else(|| {
            ConvertError::Realize("Blu-ray LPCM payload arrived before header initialization".to_string())
        })?;
        let bytes_per_sample = usize::from(format.bytes_per_sample());
        let input_frame_bytes = usize::from(header.coded_channels())
            .checked_mul(bytes_per_sample)
            .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM frame size overflow".to_string()))?;
        if input_frame_bytes == 0 {
            return Err(ConvertError::Realize(
                "Blu-ray LPCM frame size is zero".to_string(),
            ));
        }
        if pcm_payload.len() % input_frame_bytes != 0 {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray LPCM PES payload length {} for playlist {:05} PID 0x{:04x} is not an exact multiple of the LPCM audio-frame size {}",
                pcm_payload.len(),
                self.config.playlist_number,
                self.config.audio_pid,
                input_frame_bytes
            )));
        }
        let sample_frames = u64::try_from(pcm_payload.len() / input_frame_bytes).map_err(|_| {
            ConvertError::Realize("Blu-ray LPCM sample-frame count overflow".to_string())
        })?;
        Ok(LpcmPayloadTiming {
            input_frame_bytes,
            sample_frames,
        })
    }

    fn write_pcm_payload(
        &mut self,
        output: &mut File,
        payload: &[u8],
        format: LpcmWavFormat,
    ) -> Result<u64, ConvertError> {
        if payload.is_empty() {
            return Ok(0);
        }
        let header = self.header.ok_or_else(|| {
            ConvertError::Realize("Blu-ray LPCM payload arrived before header initialization".to_string())
        })?;
        let channel_map = self.channel_map.as_ref().ok_or_else(|| {
            ConvertError::Realize("Blu-ray LPCM channel map was not initialized".to_string())
        })?;
        let bytes_per_sample = usize::from(format.bytes_per_sample());
        let coded_channels = usize::from(header.coded_channels());
        let frame_bytes = coded_channels.checked_mul(bytes_per_sample).ok_or_else(|| {
            ConvertError::Realize("Blu-ray LPCM frame size overflow".to_string())
        })?;
        if frame_bytes == 0 {
            return Err(ConvertError::Realize(
                "Blu-ray LPCM frame size is zero".to_string(),
            ));
        }
        if payload.len() % frame_bytes != 0 {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray LPCM write payload length {} is not aligned to frame size {}",
                payload.len(), frame_bytes
            )));
        }

        self.scratch.clear();
        let output_frame_bytes = usize::from(header.channels)
            .checked_mul(bytes_per_sample)
            .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM output frame size overflow".to_string()))?;
        self.scratch.reserve(payload.len() / frame_bytes * output_frame_bytes);

        for frame in payload.chunks_exact(frame_bytes) {
            for &input_channel in &channel_map.wav_to_bluray_channel {
                let sample_start = usize::from(input_channel)
                    .checked_mul(bytes_per_sample)
                    .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM sample offset overflow".to_string()))?;
                let sample = &frame[sample_start..sample_start + bytes_per_sample];
                match bytes_per_sample {
                    2 => self.scratch.extend_from_slice(&[sample[1], sample[0]]),
                    3 => self.scratch.extend_from_slice(&[sample[2], sample[1], sample[0]]),
                    _ => {
                        return Err(ConvertError::TrackValidation(format!(
                            "unsupported Blu-ray LPCM container sample size {bytes_per_sample} byte(s)"
                        )))
                    }
                }
            }
        }

        output.write_all(&self.scratch).map_err(|err| {
            ConvertError::Realize(format!(
                "failed to write Blu-ray LPCM samples for playlist {:05} PID 0x{:04x}: {err}",
                self.config.playlist_number, self.config.audio_pid
            ))
        })?;
        u64::try_from(self.scratch.len()).map_err(|_| {
            ConvertError::Realize("Blu-ray LPCM written byte count overflow".to_string())
        })
    }

    fn derived_next_title_pts(&self) -> Result<Option<u64>, ConvertError> {
        let Some(anchor) = self.timestamp_anchor_title_pts_90k else {
            return Ok(None);
        };
        let Some(format) = self.format else {
            return Ok(None);
        };
        anchor
            .checked_add(samples_to_pts90(
                self.samples_since_timestamp_anchor,
                format.sample_rate,
            )?)
            .map(Some)
            .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM derived PTS overflow".to_string()))
    }

    fn record_timestamped_pes(&mut self, title_pts_90k: u64, pes_samples: u64) {
        self.timestamp_anchor_title_pts_90k = Some(title_pts_90k);
        self.samples_since_timestamp_anchor = pes_samples;
    }

    fn record_derived_pes(&mut self, pes_samples: u64) -> Result<(), ConvertError> {
        self.samples_since_timestamp_anchor = self
            .samples_since_timestamp_anchor
            .checked_add(pes_samples)
            .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM derived sample count overflow".to_string()))?;
        Ok(())
    }

    fn continuity_is_clean_after_last(&self, continuity_start: u8) -> bool {
        self.last_selected_continuity_end
            .map_or(true, |last| continuity_start == ((last + 1) & 0x0f))
    }

    fn mark_packet_consumed(&mut self, pes: &SelectedPesPacket) {
        self.last_selected_continuity_end = Some(pes.continuity_end);
    }


    pub(crate) fn pending_pcm_len(&self) -> usize {
        self.pending_pcm.len()
    }

    pub(crate) fn data_bytes_written(&self) -> u64 {
        self.data_bytes_written
    }

    pub(crate) fn format(&self) -> Option<LpcmWavFormat> {
        self.format
    }

    pub(crate) fn expected_audio(&self) -> Result<ExpectedAudio, ConvertError> {
        let format = self.format.ok_or_else(|| {
            ConvertError::Realize("Blu-ray LPCM WAV format was not initialized".to_string())
        })?;
        Ok(ExpectedAudio {
            sample_rate: format.sample_rate,
            channels: u8::try_from(format.channels).map_err(|_| {
                ConvertError::Realize("Blu-ray LPCM WAV channel count exceeds u8".to_string())
            })?,
            container_bits: format.container_bits_per_sample,
            valid_bits: format.valid_bits_per_sample,
            channel_mask: format.channel_mask,
            chapter_duration_pts_90k: self
                .config
                .chapter_end_pts_90k
                .and_then(|end| end.checked_sub(self.config.chapter_start_pts_90k)),
        })
    }

    pub(crate) fn validate_reasonable_output_size(&self) -> Result<(), ConvertError> {
        let Some(format) = self.format else {
            return Ok(());
        };
        let Some(end_pts) = self.config.chapter_end_pts_90k else {
            return Ok(());
        };
        if end_pts <= self.config.chapter_start_pts_90k {
            return Ok(());
        }
        let duration_pts = end_pts - self.config.chapter_start_pts_90k;
        if duration_pts < PTS_CLOCK_HZ {
            return Ok(());
        }
        let expected = expected_pcm_data_bytes(duration_pts, format)?;
        if expected == 0 {
            return Ok(());
        }
        let one_second = u64::from(format.byte_rate()?);
        let max_reasonable = expected
            .checked_mul(2)
            .and_then(|value| value.checked_add(one_second))
            .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM reasonableness bound overflow".to_string()))?;
        if self.data_bytes_written > max_reasonable {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray LPCM playlist {:05} chapter {} output is much larger than expected from chapter PTS: wrote {} byte(s), expected about {} byte(s)",
                self.config.playlist_number,
                self.config.chapter_number,
                self.data_bytes_written,
                expected
            )));
        }

        let min_reasonable = expected / 200;
        if expected > one_second && self.data_bytes_written < min_reasonable {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray LPCM playlist {:05} chapter {} output is much smaller than expected from chapter PTS: wrote {} byte(s), expected about {} byte(s)",
                self.config.playlist_number,
                self.config.chapter_number,
                self.data_bytes_written,
                expected
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PesProcessingOutcome {
    Continue,
    Stop,
}

fn parse_lpcm_payload_header(
    payload: &[u8],
    config: &BlurayLpcmExtractionConfig,
) -> Result<BlurayLpcmHeader, ConvertError> {
    if payload.len() < 4 {
        return Err(ConvertError::TrackValidation(format!(
            "Blu-ray LPCM PES payload for playlist {:05} PID 0x{:04x} is shorter than the four-byte LPCM header",
            config.playlist_number, config.audio_pid
        )));
    }
    parse_bluray_lpcm_header([payload[0], payload[1], payload[2], payload[3]]).map_err(|err| {
        ConvertError::TrackValidation(format!(
            "Blu-ray LPCM PES header parse failed for playlist {:05} PID 0x{:04x}: {err}",
            config.playlist_number, config.audio_pid
        ))
    })
}

#[derive(Debug, Clone)]
struct PendingPes {
    payload: Vec<u8>,
    continuity_start: u8,
    continuity_end: u8,
    byte_len: usize,
}

impl PendingPes {
    fn from_selected(pes: SelectedPesPacket) -> Self {
        let byte_len = pes.payload.len();
        Self {
            payload: pes.payload,
            continuity_start: pes.continuity_start,
            continuity_end: pes.continuity_end,
            byte_len,
        }
    }
}

#[derive(Debug, Clone)]
struct PrerollBuffer {
    pes: VecDeque<PendingPes>,
    max_packets: usize,
    max_bytes: usize,
    byte_len: usize,
}

impl PrerollBuffer {
    pub(crate) fn new(max_packets: usize, max_bytes: usize) -> Self {
        Self {
            pes: VecDeque::new(),
            max_packets,
            max_bytes,
            byte_len: 0,
        }
    }

    fn default_lpcm() -> Self {
        Self::new(BLURAY_LPCM_PREROLL_MAX_PACKETS, BLURAY_LPCM_PREROLL_MAX_BYTES)
    }

    fn is_empty(&self) -> bool {
        self.pes.is_empty()
    }

    fn push(
        &mut self,
        pes: PendingPes,
        playlist_number: u32,
        audio_pid: u16,
    ) -> Result<(), ConvertError> {
        let new_packet_count = self.pes.len().checked_add(1).ok_or_else(|| {
            ConvertError::Realize("Blu-ray LPCM pre-roll packet count overflow".to_string())
        })?;
        let new_byte_len = self.byte_len.checked_add(pes.byte_len).ok_or_else(|| {
            ConvertError::Realize("Blu-ray LPCM pre-roll byte count overflow".to_string())
        })?;
        if new_packet_count > self.max_packets || new_byte_len > self.max_bytes {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray LPCM pre-roll buffer exceeded bounded limit for playlist {:05} PID 0x{:04x}: {} PES packet(s), {} byte(s); refusing unbounded missing-PTS buffering",
                playlist_number,
                audio_pid,
                new_packet_count,
                new_byte_len
            )));
        }
        self.byte_len = new_byte_len;
        self.pes.push_back(pes);
        Ok(())
    }

    fn clear(&mut self) {
        self.pes.clear();
        self.byte_len = 0;
    }

    fn take_all(&mut self) -> VecDeque<PendingPes> {
        self.byte_len = 0;
        std::mem::take(&mut self.pes)
    }

    fn has_clean_continuity_to(&self, following: &SelectedPesPacket) -> bool {
        let mut previous_end = None;
        for pending in &self.pes {
            if let Some(end) = previous_end {
                if pending.continuity_start != ((end + 1) & 0x0f) {
                    return false;
                }
            }
            previous_end = Some(pending.continuity_end);
        }
        previous_end.map_or(true, |end| following.continuity_start == ((end + 1) & 0x0f))
    }
}

#[derive(Debug, Clone, Copy)]
struct LpcmPayloadTiming {
    input_frame_bytes: usize,
    sample_frames: u64,
}


struct BlurayChannelMap {
    wav_channel_mask: Option<u32>,
    wav_to_bluray_channel: Vec<u8>,
}

impl BlurayChannelMap {
    fn from_lpcm_header(header: BlurayLpcmHeader) -> Result<Self, ConvertError> {
        if header.bd_channel_order.len() != usize::from(header.channels) {
            return Err(ConvertError::Realize(format!(
                "Blu-ray LPCM parser channel assignment {} has {} BD channels, but reported {} channel(s)",
                header.channel_assignment,
                header.bd_channel_order.len(),
                header.channels
            )));
        }
        if header.wav_channel_order.len() != usize::from(header.channels) {
            return Err(ConvertError::Realize(format!(
                "Blu-ray LPCM parser channel assignment {} has {} WAV channels, but reported {} channel(s)",
                header.channel_assignment,
                header.wav_channel_order.len(),
                header.channels
            )));
        }

        let mut wav_to_bluray_channel = Vec::with_capacity(header.wav_channel_order.len());
        for wav_channel in header.wav_channel_order {
            let Some(input_index) = header
                .bd_channel_order
                .iter()
                .position(|bd_channel| bd_channel == wav_channel)
            else {
                return Err(ConvertError::Realize(format!(
                    "Blu-ray LPCM parser channel assignment {} maps WAV channel {:?}, but it is absent from the BD channel order {:?}",
                    header.channel_assignment,
                    wav_channel,
                    header.bd_channel_order
                )));
            };
            let input_index = u8::try_from(input_index).map_err(|_| {
                ConvertError::Realize("Blu-ray LPCM channel index exceeds u8".to_string())
            })?;
            if input_index >= header.coded_channels() {
                return Err(ConvertError::Realize(format!(
                    "Blu-ray LPCM channel map for assignment {} references channel {} outside coded count {}",
                    header.channel_assignment,
                    input_index,
                    header.coded_channels()
                )));
            }
            wav_to_bluray_channel.push(input_index);
        }

        Ok(Self {
            wav_channel_mask: header.wav_channel_mask,
            wav_to_bluray_channel,
        })
    }
}


pub(crate) fn validate_bluray_lpcm_materialization_facts(
    source: &Path,
    playlist_number: u32,
    chapter_number: u32,
    audio_pid: u16,
    sample_rate: Option<u32>,
    bit_depth: Option<u32>,
    channels: Option<u8>,
) -> Result<(), ConvertError> {
    let sample_rate = sample_rate.ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "Blu-ray LPCM playlist {playlist_number:05} chapter {chapter_number} PID 0x{audio_pid:04x} in '{}' is missing a materialized sample-rate assertion",
            source.display()
        ))
    })?;
    let bit_depth = bit_depth.ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "Blu-ray LPCM playlist {playlist_number:05} chapter {chapter_number} PID 0x{audio_pid:04x} in '{}' is missing a probed bit-depth assertion",
            source.display()
        ))
    })?;
    let channels = channels.ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "Blu-ray LPCM playlist {playlist_number:05} chapter {chapter_number} PID 0x{audio_pid:04x} in '{}' is missing a channel-count assertion",
            source.display()
        ))
    })?;

    if !matches!(sample_rate, 48_000 | 96_000 | 192_000) {
        return Err(ConvertError::TrackValidation(format!(
            "unsupported Blu-ray LPCM sample rate {sample_rate}; expected 48000, 96000, or 192000 Hz"
        )));
    }
    if !matches!(bit_depth, 16 | 20 | 24) {
        return Err(ConvertError::TrackValidation(format!(
            "unsupported Blu-ray LPCM bit depth {bit_depth}; expected 16, 20, or 24"
        )));
    }
    if channels == 0 || channels > 8 {
        return Err(ConvertError::TrackValidation(format!(
            "unsupported Blu-ray LPCM channel count {channels}; expected 1..=8"
        )));
    }
    Ok(())
}

fn validate_lpcm_header_against_expectations(
    config: &BlurayLpcmExtractionConfig,
    header: BlurayLpcmHeader,
) -> Result<(), ConvertError> {
    if let Some(expected) = config.expected_sample_rate {
        if header.sample_rate != expected {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray LPCM sample-rate mismatch for playlist {:05} PID 0x{:04x}: materializer expected {}, PES header says {}",
                config.playlist_number,
                config.audio_pid,
                expected,
                header.sample_rate
            )));
        }
    }
    if let Some(expected) = config.expected_bit_depth {
        if u32::from(header.valid_bits) != expected {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray LPCM bit-depth mismatch for playlist {:05} PID 0x{:04x}: materializer expected {}, PES header says {}",
                config.playlist_number,
                config.audio_pid,
                expected,
                u32::from(header.valid_bits)
            )));
        }
    }
    if let Some(expected) = config.expected_channels {
        if header.channels != expected {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray LPCM channel-count mismatch for playlist {:05} PID 0x{:04x}: materializer expected {}, PES header says {}",
                config.playlist_number,
                config.audio_pid,
                expected,
                header.channels
            )));
        }
    }
    if let Some(expected) = config.expected_channel_layout.as_deref() {
        if !expected.eq_ignore_ascii_case(header.channel_layout_label()) {
            log::warn!(
                "Blu-ray LPCM layout label mismatch after discovery: materializer says '{}', PES header says '{}'; using PES channel assignment code {}",
                expected,
                header.channel_layout_label(),
                header.channel_assignment
            );
        }
    }
    Ok(())
}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::bluray_backend::BlurayPtsContinuitySegment;
    use super::super::bluray_pts::PTS_CLOCK_HZ;
    use super::super::bluray_wav_validate::{read_wav_info, validate_bluray_lpcm_wav, write_wav_header};

    fn encode_pts(pts: u64) -> [u8; 5] {
        let pts = pts & ((1u64 << 33) - 1);
        [
            0x20 | (((pts >> 30) as u8 & 0x07) << 1) | 1,
            (pts >> 22) as u8,
            (((pts >> 15) as u8 & 0x7f) << 1) | 1,
            (pts >> 7) as u8,
            ((pts as u8 & 0x7f) << 1) | 1,
        ]
    }

    fn pes_packet(pts: u64, lpcm_header: [u8; 4], pcm_payload: &[u8]) -> Vec<u8> {
        let mut pes = vec![0x00, 0x00, 0x01, 0xBD];
        let packet_len = 3 + 5 + 4 + pcm_payload.len();
        pes.extend_from_slice(&(packet_len as u16).to_be_bytes());
        pes.extend_from_slice(&[0x80, 0x80, 0x05]);
        pes.extend_from_slice(&encode_pts(pts));
        pes.extend_from_slice(&lpcm_header);
        pes.extend_from_slice(pcm_payload);
        pes
    }

    fn pes_packet_without_pts(lpcm_header: [u8; 4], pcm_payload: &[u8]) -> Vec<u8> {
        let mut pes = vec![0x00, 0x00, 0x01, 0xBD];
        let packet_len = 3 + 4 + pcm_payload.len();
        pes.extend_from_slice(&(packet_len as u16).to_be_bytes());
        pes.extend_from_slice(&[0x80, 0x00, 0x00]);
        pes.extend_from_slice(&lpcm_header);
        pes.extend_from_slice(pcm_payload);
        pes
    }

    fn selected_pes(payload: Vec<u8>) -> SelectedPesPacket {
        SelectedPesPacket {
            payload,
            continuity_start: 0,
            continuity_end: 0,
        }
    }

    fn selected_pes_with_cc(
        payload: Vec<u8>,
        continuity_start: u8,
        continuity_end: u8,
    ) -> SelectedPesPacket {
        SelectedPesPacket {
            payload,
            continuity_start,
            continuity_end,
        }
    }

    fn stereo_extractor(
        chapter_start_pts_90k: u64,
        chapter_end_pts_90k: Option<u64>,
        pts_mapper: BlurayPtsMapper,
    ) -> BlurayLpcmExtractor {
        BlurayLpcmExtractor::new(BlurayLpcmExtractionConfig {
            source: PathBuf::from("disc"),
            playlist_number: 800,
            chapter_number: 1,
            chapter_start_pts_90k,
            chapter_end_pts_90k,
            audio_pid: 0x1100,
            audio_stream_index: 0,
            expected_sample_rate: Some(48_000),
            expected_bit_depth: Some(16),
            expected_channels: Some(2),
            expected_channel_layout: Some("stereo".to_string()),
            pts_mapper,
        })
    }

    fn two_clip_restart_mapper(anchor_title_pts_90k: u64) -> BlurayPtsMapper {
        let mapper = BlurayPtsMapper::segmented(vec![
            BlurayPtsContinuitySegment {
                title_start_pts_90k: 0,
                title_end_pts_90k: 100,
                clip_ref: 0,
                clip_start_pts_90k: 0,
                clip_end_pts_90k: 100,
            },
            BlurayPtsContinuitySegment {
                title_start_pts_90k: 100,
                title_end_pts_90k: 200,
                clip_ref: 1,
                clip_start_pts_90k: 0,
                clip_end_pts_90k: 100,
            },
        ])
        .unwrap();
        mapper.prime_for_title_pts(anchor_title_pts_90k);
        mapper
    }

    fn temp_output_file(test_name: &str) -> (PathBuf, File) {
        let dir = std::env::temp_dir().join(format!(
            "bluray-lpcm-test-{test_name}-{}",
            std::process::id(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.wav");
        let file = File::create(&path).unwrap();
        (path, file)
    }

    #[test]
    fn lpcm_payload_header_uses_project_bluray_parser_layout() {
        let config = BlurayLpcmExtractionConfig {
            source: PathBuf::from("disc"),
            playlist_number: 800,
            chapter_number: 1,
            chapter_start_pts_90k: 0,
            chapter_end_pts_90k: None,
            audio_pid: 0x1100,
            audio_stream_index: 0,
            expected_sample_rate: Some(96_000),
            expected_bit_depth: Some(24),
            expected_channels: Some(6),
            expected_channel_layout: Some("5.1".to_string()),
            pts_mapper: BlurayPtsMapper::identity_continuous(),
        };
        let parsed = parse_lpcm_payload_header(&[0, 0, (9 << 4) | 4, 3 << 6], &config).unwrap();
        assert_eq!(parsed.channels, 6);
        assert_eq!(parsed.sample_rate, 96_000);
        assert_eq!(parsed.valid_bits, 24);
    }

    #[test]
    fn channel_map_reorders_bluray_7_1_to_wave_mask_order() {
        let header = parse_bluray_lpcm_header([0, 0, (11 << 4) | 1, 3 << 6]).unwrap();
        let map = BlurayChannelMap::from_lpcm_header(header).unwrap();
        assert_eq!(map.wav_channel_mask, Some(0x63f));
        assert_eq!(map.wav_to_bluray_channel, vec![0, 1, 2, 3, 6, 7, 4, 5]);
    }

    #[test]
    fn extractor_swaps_big_endian_16_bit_stereo_samples() {
        let mut extractor = stereo_extractor(0, Some(90_000), BlurayPtsMapper::identity_continuous());
        let (path, mut file) = temp_output_file("swap16");
        let pes = pes_packet(
            0,
            [0, 0, (3 << 4) | 1, 1 << 6],
            &[0x12, 0x34, 0x56, 0x78],
        );

        let format = extractor.peek_or_create_wav_format(&pes).unwrap();
        write_wav_header(&mut file, format, 0).unwrap();
        extractor.process_pes_packet(selected_pes(pes), &mut file).unwrap();
        file.flush().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let data_offset = bytes.windows(4).position(|window| window == b"data").unwrap() + 8;
        assert_eq!(&bytes[data_offset..data_offset + 4], &[0x34, 0x12, 0x78, 0x56]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn missing_initial_pts_attaches_to_first_in_range_timestamp() {
        let mut extractor = stereo_extractor(0, Some(PTS_CLOCK_HZ), BlurayPtsMapper::identity_continuous());
        let (path, mut file) = temp_output_file("preroll-attach");
        let lpcm = [0, 0, (3 << 4) | 1, 1 << 6];
        let missing = selected_pes_with_cc(pes_packet_without_pts(lpcm, &[0x12, 0x34, 0x56, 0x78]), 0, 0);
        let timed = selected_pes_with_cc(pes_packet(1, lpcm, &[0x9a, 0xbc, 0xde, 0xf0]), 1, 1);

        assert_eq!(extractor.process_pes_packet(missing, &mut file).unwrap(), PesProcessingOutcome::Continue);
        assert_eq!(extractor.process_pes_packet(timed, &mut file).unwrap(), PesProcessingOutcome::Continue);
        file.flush().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 8);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn missing_pts_after_continuity_gap_errors() {
        let mut extractor = stereo_extractor(0, None, BlurayPtsMapper::identity_continuous());
        let (_path, mut file) = temp_output_file("missing-pts-gap");
        let lpcm = [0, 0, (3 << 4) | 1, 1 << 6];
        extractor
            .process_pes_packet(selected_pes_with_cc(pes_packet(0, lpcm, &[0; 4]), 0, 0), &mut file)
            .unwrap();
        let err = extractor
            .process_pes_packet(selected_pes_with_cc(pes_packet_without_pts(lpcm, &[0; 4]), 5, 5), &mut file)
            .unwrap_err();
        assert!(err.to_string().contains("missing PTS after timestamp lock"));
        assert!(err.to_string().contains("continuity gap"));
    }

    #[test]
    fn sample_accurate_start_trim_uses_ceil() {
        let mut extractor = stereo_extractor(1, None, BlurayPtsMapper::identity_continuous());
        let (path, mut file) = temp_output_file("trim-start");
        let lpcm = [0, 0, (3 << 4) | 1, 1 << 6];
        let pes = selected_pes(pes_packet(0, lpcm, &[
            0x01, 0x02, 0x03, 0x04,
            0x05, 0x06, 0x07, 0x08,
        ]));

        extractor.process_pes_packet(pes, &mut file).unwrap();
        file.flush().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(extractor.data_bytes_written(), 4);
        assert_eq!(bytes, vec![0x06, 0x05, 0x08, 0x07]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn sample_accurate_end_trim_uses_floor() {
        let mut extractor = stereo_extractor(0, Some(2), BlurayPtsMapper::identity_continuous());
        let (path, mut file) = temp_output_file("trim-end");
        let lpcm = [0, 0, (3 << 4) | 1, 1 << 6];
        let pes = selected_pes(pes_packet(0, lpcm, &[
            0x01, 0x02, 0x03, 0x04,
            0x05, 0x06, 0x07, 0x08,
            0x09, 0x0a, 0x0b, 0x0c,
        ]));

        extractor.process_pes_packet(pes, &mut file).unwrap();
        file.flush().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(extractor.data_bytes_written(), 4);
        assert_eq!(bytes, vec![0x02, 0x01, 0x04, 0x03]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn segmented_mapper_extracts_chapter_entirely_inside_second_clip() {
        let mapper = two_clip_restart_mapper(120);
        let mut extractor = stereo_extractor(120, Some(123), mapper);
        let (path, mut file) = temp_output_file("multiclip-clip2-only");
        let lpcm = [0, 0, (3 << 4) | 1, 1 << 6];

        extractor
            .process_pes_packet(selected_pes(pes_packet(20, lpcm, &[
                0x01, 0x02, 0x03, 0x04,
                0x05, 0x06, 0x07, 0x08,
                0x09, 0x0a, 0x0b, 0x0c,
            ])), &mut file)
            .unwrap();
        file.flush().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(extractor.data_bytes_written(), bytes.len() as u64);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn segmented_mapper_trims_chapter_crossing_clip_boundary() {
        let mapper = two_clip_restart_mapper(98);
        let mut extractor = stereo_extractor(98, Some(103), mapper);
        let (path, mut file) = temp_output_file("multiclip-cross-boundary");
        let lpcm = [0, 0, (3 << 4) | 1, 1 << 6];

        extractor
            .process_pes_packet(selected_pes(pes_packet(97, lpcm, &[
                0x11, 0x12, 0x13, 0x14,
                0x15, 0x16, 0x17, 0x18,
                0x19, 0x1a, 0x1b, 0x1c,
            ])), &mut file)
            .unwrap();
        extractor
            .process_pes_packet(selected_pes(pes_packet(0, lpcm, &[
                0x21, 0x22, 0x23, 0x24,
                0x25, 0x26, 0x27, 0x28,
                0x29, 0x2a, 0x2b, 0x2c,
            ])), &mut file)
            .unwrap();
        file.flush().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(extractor.data_bytes_written() > 0);
        assert_eq!(extractor.data_bytes_written() % 4, 0);
        assert_eq!(extractor.data_bytes_written(), bytes.len() as u64);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn adjacent_chapters_around_clip_boundary_do_not_share_samples() {
        let lpcm = [0, 0, (3 << 4) | 1, 1 << 6];

        let mut before = stereo_extractor(98, Some(100), two_clip_restart_mapper(98));
        let (before_path, mut before_file) = temp_output_file("boundary-before");
        before
            .process_pes_packet(selected_pes(pes_packet(98, lpcm, &[
                0x31, 0x32, 0x33, 0x34,
                0x35, 0x36, 0x37, 0x38,
                0x39, 0x3a, 0x3b, 0x3c,
            ])), &mut before_file)
            .unwrap();
        before
            .process_pes_packet(selected_pes(pes_packet(0, lpcm, &[
                0x41, 0x42, 0x43, 0x44,
                0x45, 0x46, 0x47, 0x48,
            ])), &mut before_file)
            .unwrap();
        before_file.flush().unwrap();

        let mut after = stereo_extractor(100, Some(102), two_clip_restart_mapper(100));
        let (after_path, mut after_file) = temp_output_file("boundary-after");
        after
            .process_pes_packet(selected_pes(pes_packet(0, lpcm, &[
                0x41, 0x42, 0x43, 0x44,
                0x45, 0x46, 0x47, 0x48,
            ])), &mut after_file)
            .unwrap();
        after_file.flush().unwrap();

        assert!(before.data_bytes_written() > 0);
        assert!(after.data_bytes_written() > 0);
        assert_ne!(std::fs::read(&before_path).unwrap(), std::fs::read(&after_path).unwrap());
        let _ = std::fs::remove_dir_all(before_path.parent().unwrap());
        let _ = std::fs::remove_dir_all(after_path.parent().unwrap());
    }

    #[test]
    fn missing_pts_preroll_across_clip_boundary_is_trimmed_safely() {
        let mapper = two_clip_restart_mapper(100);
        let mut extractor = stereo_extractor(100, Some(103), mapper);
        let (path, mut file) = temp_output_file("boundary-preroll");
        let lpcm = [0, 0, (3 << 4) | 1, 1 << 6];
        let missing = selected_pes_with_cc(
            pes_packet_without_pts(lpcm, &[
                0x51, 0x52, 0x53, 0x54,
                0x55, 0x56, 0x57, 0x58,
            ]),
            0,
            0,
        );
        let timed = selected_pes_with_cc(
            pes_packet(0, lpcm, &[
                0x61, 0x62, 0x63, 0x64,
                0x65, 0x66, 0x67, 0x68,
            ]),
            1,
            1,
        );

        extractor.process_pes_packet(missing, &mut file).unwrap();
        extractor.process_pes_packet(timed, &mut file).unwrap();
        file.flush().unwrap();

        assert!(extractor.data_bytes_written() > 0);
        assert_eq!(extractor.data_bytes_written() % 4, 0);
        assert_eq!(extractor.data_bytes_written(), std::fs::read(&path).unwrap().len() as u64);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn realizer_rejects_lpcm_header_mismatches_from_materializer_facts() {
        let lpcm_stereo_96 = [0, 0, (3 << 4) | 4, 1 << 6];
        let lpcm_stereo_24 = [0, 0, (3 << 4) | 1, 3 << 6];
        let lpcm_5_1_48 = [0, 0, (9 << 4) | 1, 1 << 6];

        let (_path, mut file) = temp_output_file("lpcm-mismatch-rate");
        let mut extractor = stereo_extractor(0, None, BlurayPtsMapper::identity_continuous());
        let err = extractor
            .process_pes_packet(selected_pes(pes_packet(0, lpcm_stereo_96, &[0; 4])), &mut file)
            .unwrap_err();
        assert!(err.to_string().contains("sample-rate mismatch"));

        let (_path, mut file) = temp_output_file("lpcm-mismatch-depth");
        let mut extractor = stereo_extractor(0, None, BlurayPtsMapper::identity_continuous());
        let err = extractor
            .process_pes_packet(selected_pes(pes_packet(0, lpcm_stereo_24, &[0; 6])), &mut file)
            .unwrap_err();
        assert!(err.to_string().contains("bit-depth mismatch"));

        let (_path, mut file) = temp_output_file("lpcm-mismatch-channels");
        let mut extractor = stereo_extractor(0, None, BlurayPtsMapper::identity_continuous());
        let err = extractor
            .process_pes_packet(selected_pes(pes_packet(0, lpcm_5_1_48, &[0; 12])), &mut file)
            .unwrap_err();
        assert!(err.to_string().contains("channel-count mismatch"));
    }
}
