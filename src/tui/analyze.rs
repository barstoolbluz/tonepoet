//! Audio analysis: DR meter, peak, RMS, clipping, DC bias, bit depth.
//!
//! Single-pass PCM decode via ffmpeg-next computes DR-family metrics.
//! Production LUFS and ReplayGain reporting peak use the same native
//! NativeEbu2023 observation path as conversion and metadata writing.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoudnessUnavailableReason {
    TooShort { frames: u64, required_frames: u64 },
    BelowAbsoluteGate,
    BelowRelativeGate,
    NoEligibleBlocks,
}

impl From<crate::convert::replaygain::MathematicalUnavailability>
    for LoudnessUnavailableReason
{
    fn from(reason: crate::convert::replaygain::MathematicalUnavailability) -> Self {
        match reason {
            crate::convert::replaygain::MathematicalUnavailability::TooShort {
                frames,
                required_frames,
            } => Self::TooShort {
                frames,
                required_frames,
            },
            crate::convert::replaygain::MathematicalUnavailability::BelowAbsoluteGate => {
                Self::BelowAbsoluteGate
            }
            crate::convert::replaygain::MathematicalUnavailability::BelowRelativeGate => {
                Self::BelowRelativeGate
            }
            crate::convert::replaygain::MathematicalUnavailability::NoEligibleBlocks => {
                Self::NoEligibleBlocks
            }
        }
    }
}

impl std::fmt::Display for LoudnessUnavailableReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort {
                frames,
                required_frames,
            } => write!(f, "too short ({frames} frames; {required_frames} required)"),
            Self::BelowAbsoluteGate => f.write_str("below absolute loudness gate"),
            Self::BelowRelativeGate => f.write_str("below relative loudness gate"),
            Self::NoEligibleBlocks => f.write_str("no eligible loudness blocks"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoudnessAnalysisStatus {
    NotScanned,
    Available,
    Unavailable(LoudnessUnavailableReason),
    Failed(String),
}

/// Results of a single-file audio analysis.
#[derive(Debug, Clone)]
pub struct AnalysisResult {
    /// Physical source file that produced this analysis row. For ordinary
    /// files this is identical to `path`; single-image CUE rows keep the
    /// carrier here while `path` is replaced with a synthetic display name.
    pub source_path: PathBuf,
    pub path: PathBuf,
    /// Legacy ID3 wrappers surrounding a native FLAC stream. `None` means the
    /// source is not routed as native FLAC; `Some(default())` is a clean FLAC.
    pub(crate) flac_wrappers: Option<crate::flac_envelope::LegacyFlacWrappers>,
    /// Physical native-FLAC carrier to repair when `flac_wrappers` reports a
    /// legacy wrapper. This remains the source path even when `path` is later
    /// replaced with a synthetic CUE-track display name.
    pub(crate) flac_wrapper_repair_path: Option<PathBuf>,
    /// TT Dynamic Range value (integer, 1-20+). Higher = more dynamic.
    pub dr_value: i32,
    /// Sample peak in dBFS.
    pub peak_db: f64,
    /// Overall RMS level in dBFS.
    pub rms_db: f64,
    /// Number of samples at digital ceiling (potential clipping).
    pub clipping_count: u64,
    /// Mean sample value (0.0 = centered, nonzero = DC offset).
    pub dc_bias: f64,
    /// Actual bit depth used (may be less than declared).
    pub actual_bit_depth: u32,
    /// Declared bit depth from stream parameters.
    pub declared_bit_depth: Option<u32>,
    pub sample_rate: u32,
    pub channels: u32,
    pub duration_secs: f64,
    /// NativeEbu2023 integrated loudness in LUFS (None when mathematically unavailable).
    pub lufs: Option<f64>,
    /// ReplayGain reporting peak in dBTP (None for digital silence).
    pub true_peak_dbtp: Option<f64>,
    /// Distinguishes a finite native loudness result, typed mathematical
    /// unavailability, and operational scan failure.
    pub loudness_status: LoudnessAnalysisStatus,
    /// CD pre-emphasis detection result from fast checks (metadata + catalog).
    pub preemphasis: Option<super::preemphasis::PreemphasisConfidence>,
    /// Pre-emphasis diagnostic correlation value.
    pub preemphasis_corr: Option<f64>,
    /// Human-readable pre-emphasis detail string (e.g., "CUE file confirmed",
    /// "catalog 35DP-25 matches known PE pressing: Asia - Asia").
    pub preemphasis_detail: Option<String>,
    /// HDCD detection result. None = not scanned (e.g., not 16-bit).
    pub hdcd_detected: Option<bool>,
    /// Human-readable HDCD detail (active/passive, peak extend, packets).
    pub hdcd_detail: Option<String>,
}

/// Analyze an audio file: decode to PCM and compute all metrics in one pass.
///
/// Optional `start_sample` seeks to that position before decoding.
/// Optional `max_samples` stops decoding after that many per-channel samples.
/// Both default to `None` for whole-file analysis.
pub fn analyze_file(
    path: &Path,
    start_sample: Option<u64>,
    max_samples: Option<u64>,
) -> Result<AnalysisResult, String> {
    use ffmpeg_next as ffmpeg;
    use ffmpeg_next::media::Type;
    use ffmpeg_next::util::format::sample::{Sample, Type as SampleType};

    crate::tui::probe::ensure_ffmpeg_init_pub();

    let mut ictx = ffmpeg::format::input(&path).map_err(|e| format!("open failed: {}", e))?;

    let audio_stream = ictx.streams().best(Type::Audio).ok_or("no audio stream")?;
    let stream_idx = audio_stream.index();
    let time_base = audio_stream.time_base();

    let codec_params = audio_stream.parameters();
    let codec_ctx = ffmpeg::codec::context::Context::from_parameters(codec_params)
        .map_err(|e| format!("codec params: {}", e))?;
    let mut decoder = codec_ctx
        .decoder()
        .audio()
        .map_err(|e| format!("decoder: {}", e))?;

    let sample_rate = decoder.rate();
    let channels = decoder.channels() as u32;
    let sample_fmt = decoder.format();

    // Declared bit depth from stream parameters.
    let declared_bit_depth = unsafe {
        let params = audio_stream.parameters().as_ptr();
        let raw = (*params).bits_per_raw_sample;
        if raw > 0 {
            Some(raw as u32)
        } else {
            None
        }
    };

    // Duration: use max_samples if provided, otherwise stream metadata.
    let duration_secs = if let Some(max) = max_samples {
        max as f64 / sample_rate as f64
    } else {
        audio_stream.duration() as f64 * f64::from(time_base)
    };

    // ── Accumulator state ────────────────────────────────────────

    // 3-second blocks for DR. At 44.1 kHz the original TT meter uses
    // 3 × 44160 = 132480 instead of 132300 (compatibility quirk present
    // in dr14_t.meter and foobar2000's foo_dr_meter).
    let block_size = if sample_rate == 44100 {
        3 * 44160 // 132480
    } else {
        (sample_rate * 3) as usize
    };
    let mut peak_abs: f64 = 0.0;
    let mut rms_sum: f64 = 0.0;
    let mut sample_count: u64 = 0;
    let mut clipping_count: u64 = 0;
    let mut dc_sum: f64 = 0.0;
    let mut bit_or_mask: u32 = 0;
    let is_integer_fmt = matches!(sample_fmt, Sample::I16(_) | Sample::I32(_));

    // Per-block accumulators for DR (per-channel, then combined).
    // Block arrays store LINEAR values (not dB) for correct aggregation.
    let mut block_rms_sums: Vec<f64> = vec![0.0; channels as usize];
    let mut block_peaks: Vec<f64> = vec![0.0; channels as usize];
    let mut block_sample_count: usize = 0;
    let mut dr_block_rms: Vec<Vec<f64>> = vec![Vec::new(); channels as usize];
    let mut dr_block_peak: Vec<Vec<f64>> = vec![Vec::new(); channels as usize];

    // ── Process a batch of samples (one channel) ─────────────────

    // Global-stats accumulator: peak, RMS, DC, clipping, bit depth.
    // Does NOT touch per-block DR accumulators — those are handled
    // by the split-accumulate loop below.
    macro_rules! accumulate_global {
        ($samples:expr, $raw_i32:expr) => {{
            let raw_opt: Option<&[i32]> = $raw_i32.map(|v| v as &[i32]);
            for (i, &s) in $samples.iter().enumerate() {
                let abs_val = s.abs();
                if abs_val > peak_abs {
                    peak_abs = abs_val;
                }
                rms_sum += s * s;
                dc_sum += s;
                sample_count += 1;
                if abs_val >= 0.9999695 {
                    clipping_count += 1;
                }

                if is_integer_fmt {
                    if let Some(raw) = raw_opt {
                        bit_or_mask |= (raw[i] as u32) | (raw[i].wrapping_neg() as u32);
                    }
                }
            }
        }};
    }

    // Temporary per-channel float buffers, reused each frame.
    let mut ch_floats: Vec<Vec<f64>> = vec![Vec::new(); channels as usize];

    // ── Decode loop ──────────────────────────────────────────────

    let mut decoded = ffmpeg::util::frame::Audio::empty();

    macro_rules! process_frame {
        ($frame_offset:expr, $frame_count:expr) => {
            let frame_offset: usize = $frame_offset;
            let n: usize = $frame_count;
            let frame_total = decoded.samples();
            if n == 0 {
                return Err("internal analysis error: empty selected frame".to_string());
            }

            // ── Extract per-channel samples + accumulate global stats ──
            for ch in 0..channels as usize {
                match sample_fmt {
                    Sample::I16(SampleType::Planar) => {
                        let plane = &decoded.plane::<i16>(ch)[frame_offset..frame_offset + n];
                        let floats: Vec<f64> = plane.iter().map(|&s| s as f64 / 32768.0).collect();
                        let i32s: Vec<i32> = plane.iter().map(|&s| (s as i32) << 16).collect();
                        accumulate_global!(floats, Some(&i32s));
                        ch_floats[ch] = floats;
                    }
                    Sample::I32(SampleType::Planar) => {
                        let plane = &decoded.plane::<i32>(ch)[frame_offset..frame_offset + n];
                        let floats: Vec<f64> =
                            plane.iter().map(|&s| s as f64 / 2147483648.0).collect();
                        accumulate_global!(floats, Some(plane));
                        ch_floats[ch] = floats;
                    }
                    Sample::F32(SampleType::Planar) => {
                        let plane = &decoded.plane::<f32>(ch)[frame_offset..frame_offset + n];
                        let floats: Vec<f64> = plane.iter().map(|&s| s as f64).collect();
                        accumulate_global!(floats, None::<&[i32]>);
                        ch_floats[ch] = floats;
                    }
                    Sample::F64(SampleType::Planar) => {
                        let plane = &decoded.plane::<f64>(ch)[frame_offset..frame_offset + n];
                        let floats: Vec<f64> = plane.to_vec();
                        accumulate_global!(floats, None::<&[i32]>);
                        ch_floats[ch] = floats;
                    }
                    // Packed formats: decoded.plane() only returns nb_samples
                    // elements, but the interleaved buffer has nb_samples ×
                    // channels values. Use data(0) for the full buffer.
                    Sample::I16(SampleType::Packed) => {
                        let raw = decoded.data(0);
                        let full: &[i16] = unsafe {
                            std::slice::from_raw_parts(
                                raw.as_ptr() as *const i16,
                                frame_total * channels as usize,
                            )
                        };
                        let floats: Vec<f64> = full
                            .iter()
                            .skip(frame_offset * channels as usize + ch)
                            .step_by(channels as usize)
                            .take(n)
                            .map(|&s| s as f64 / 32768.0)
                            .collect();
                        let i32s: Vec<i32> = full
                            .iter()
                            .skip(frame_offset * channels as usize + ch)
                            .step_by(channels as usize)
                            .take(n)
                            .map(|&s| (s as i32) << 16)
                            .collect();
                        accumulate_global!(floats, Some(&i32s));
                        ch_floats[ch] = floats;
                    }
                    Sample::I32(SampleType::Packed) => {
                        let raw = decoded.data(0);
                        let full: &[i32] = unsafe {
                            std::slice::from_raw_parts(
                                raw.as_ptr() as *const i32,
                                frame_total * channels as usize,
                            )
                        };
                        let floats: Vec<f64> = full
                            .iter()
                            .skip(frame_offset * channels as usize + ch)
                            .step_by(channels as usize)
                            .take(n)
                            .map(|&s| s as f64 / 2147483648.0)
                            .collect();
                        let i32_ch: Vec<i32> = full
                            .iter()
                            .skip(frame_offset * channels as usize + ch)
                            .step_by(channels as usize)
                            .take(n)
                            .copied()
                            .collect();
                        accumulate_global!(floats, Some(&i32_ch));
                        ch_floats[ch] = floats;
                    }
                    Sample::F32(SampleType::Packed) => {
                        let raw = decoded.data(0);
                        let full: &[f32] = unsafe {
                            std::slice::from_raw_parts(
                                raw.as_ptr() as *const f32,
                                frame_total * channels as usize,
                            )
                        };
                        let floats: Vec<f64> = full
                            .iter()
                            .skip(frame_offset * channels as usize + ch)
                            .step_by(channels as usize)
                            .take(n)
                            .map(|&s| s as f64)
                            .collect();
                        accumulate_global!(floats, None::<&[i32]>);
                        ch_floats[ch] = floats;
                    }
                    Sample::F64(SampleType::Packed) => {
                        let raw = decoded.data(0);
                        let full: &[f64] = unsafe {
                            std::slice::from_raw_parts(
                                raw.as_ptr() as *const f64,
                                frame_total * channels as usize,
                            )
                        };
                        let floats: Vec<f64> = full
                            .iter()
                            .skip(frame_offset * channels as usize + ch)
                            .step_by(channels as usize)
                            .take(n)
                            .copied()
                            .collect();
                        accumulate_global!(floats, None::<&[i32]>);
                        ch_floats[ch] = floats;
                    }
                    _ => {
                        return Err(format!("unsupported sample format: {:?}", sample_fmt));
                    }
                }
            }

            // ── Split-accumulate: flush DR blocks at exact boundaries ──
            // Each frame's per-channel samples are split into sub-slices
            // at the block boundary so no energy leaks across blocks.
            let mut offset = 0usize;
            let mut remaining = n;
            while remaining > 0 {
                let space = block_size - block_sample_count;
                let chunk = remaining.min(space);
                for c in 0..channels as usize {
                    for &s in &ch_floats[c][offset..offset + chunk] {
                        let abs_val = s.abs();
                        if abs_val > block_peaks[c] {
                            block_peaks[c] = abs_val;
                        }
                        block_rms_sums[c] += s * s;
                    }
                }
                block_sample_count += chunk;
                offset += chunk;
                remaining -= chunk;
                if block_sample_count >= block_size {
                    for c in 0..channels as usize {
                        // AES-17 ×2 factor: matches foobar2000's foo_dr_meter.
                        let rms_linear = (2.0 * block_rms_sums[c] / block_size as f64).sqrt();
                        dr_block_rms[c].push(rms_linear);
                        dr_block_peak[c].push(block_peaks[c]);
                        block_rms_sums[c] = 0.0;
                        block_peaks[c] = 0.0;
                    }
                    block_sample_count = 0;
                }
            }
        };
    }

    // Seek to start position if specified. `Input::seek` uses FFmpeg's base
    // timebase when no stream index is supplied, not the audio stream's
    // sample/timebase units.
    if let Some(start) = start_sample {
        use ffmpeg::{rescale, Rescale};
        let ts = (start as i64).rescale((1, sample_rate as i32), rescale::TIME_BASE);
        // Seek to the nearest keyframe at or before the target.
        ictx.seek(ts, ..ts)
            .map_err(|e| format!("seek failed: {}", e))?;
    }

    let sample_limit = max_samples.unwrap_or(u64::MAX);
    let requested_start = start_sample.unwrap_or(0);
    let requested_end = max_samples.and_then(|count| requested_start.checked_add(count));
    let mut total_decoded: u64 = 0;
    let mut selected_samples: u64 = 0;
    // Whole-file decoding can safely fall back to a zero-based cursor if a
    // decoder omits timestamps. A seeked analysis cannot: the first decoded
    // frame may begin before the requested point, so exact CUE boundaries
    // require its best-effort timestamp.
    let mut decoded_cursor = if start_sample.is_none() { Some(0u64) } else { None };
    // The legacy wrapper exception is intentionally whole-file only. A seeked
    // or bounded analysis does not establish that the decoder reached the
    // STREAMINFO terminal extent and therefore retains ordinary error handling.
    let wrapped_flac = crate::flac_envelope::WrappedFlacDecodeGuard::for_path(
        path,
        start_sample.is_none() && max_samples.is_none(),
    )
    .map_err(|error| format!("inspect FLAC wrapper: {error}"))?;
    let mut accepted_wrapped_eof = false;
    let mut reached_sample_limit = false;

    macro_rules! process_decoded_frame {
        () => {{
            use ffmpeg::Rescale;

            let frame_samples = decoded.samples() as u64;
            let next_decoded = wrapped_flac
                .checked_advance(total_decoded, frame_samples)
                .map_err(|error| error.to_string())?;

            let frame_start = if let Some(timestamp) = decoded.timestamp() {
                let sample = timestamp.rescale(time_base, (1, sample_rate as i32));
                if sample < 0 { 0 } else { sample as u64 }
            } else if let Some(cursor) = decoded_cursor {
                cursor
            } else {
                return Err(
                    "seeked analysis frame has no timestamp; exact sample boundary unavailable"
                        .to_string(),
                );
            };
            let frame_end = frame_start.saturating_add(frame_samples);
            decoded_cursor = Some(frame_end);
            total_decoded = next_decoded;

            if frame_end <= requested_start {
                false
            } else if requested_end.is_some_and(|end| frame_start >= end) {
                true
            } else {
                let selected_start = requested_start.max(frame_start);
                let selected_end = requested_end
                    .map(|end| end.min(frame_end))
                    .unwrap_or(frame_end);
                if selected_end > selected_start {
                    let frame_offset = (selected_start - frame_start) as usize;
                    let frame_count = (selected_end - selected_start) as usize;
                    process_frame!(frame_offset, frame_count);
                    selected_samples = selected_samples.saturating_add(frame_count as u64);
                }
                selected_samples >= sample_limit
                    || requested_end.is_some_and(|end| frame_end >= end)
            }
        }};
    }

    'decode: loop {
        let mut packet = ffmpeg::Packet::empty();
        match packet.read(&mut ictx) {
            Ok(()) => {}
            Err(ffmpeg::Error::Eof) => break,
            Err(error) if wrapped_flac.accepts_post_extent_error(total_decoded) => {
                accepted_wrapped_eof = true;
                log::debug!(
                    "accepted legacy ID3-wrapped FLAC demux EOF after declared extent: path={}, frames={}, error={error}",
                    path.display(),
                    total_decoded,
                );
                break;
            }
            Err(error) => return Err(format!("demux failed: {error}")),
        }
        if packet.stream() != stream_idx {
            continue;
        }
        if let Err(error) = decoder.send_packet(&packet) {
            if wrapped_flac.accepts_post_extent_error(total_decoded) {
                accepted_wrapped_eof = true;
                log::debug!(
                    "accepted legacy ID3-wrapped FLAC packet EOF after declared extent: path={}, frames={}, error={error}",
                    path.display(),
                    total_decoded,
                );
                break;
            }
            return Err(format!("send_packet: {error}"));
        }

        loop {
            match decoder.receive_frame(&mut decoded) {
                Ok(()) => {
                    if process_decoded_frame!() {
                        reached_sample_limit = true;
                        break 'decode;
                    }
                }
                Err(ffmpeg::Error::Eof) => break,
                Err(ffmpeg::Error::Other { errno })
                    if errno == ffmpeg::util::error::EAGAIN => break,
                Err(error) if wrapped_flac.accepts_post_extent_error(total_decoded) => {
                    accepted_wrapped_eof = true;
                    log::debug!(
                        "accepted legacy ID3-wrapped FLAC decoder EOF after declared extent: path={}, frames={}, error={error}",
                        path.display(),
                        total_decoded,
                    );
                    break 'decode;
                }
                Err(error) => return Err(format!("audio decoder failed: {error}")),
            }
        }
    }

    // Flush decoder — process remaining buffered frames.
    if !reached_sample_limit && !accepted_wrapped_eof {
        if let Err(error) = decoder.send_eof() {
            if wrapped_flac.accepts_post_extent_error(total_decoded) {
                accepted_wrapped_eof = true;
                log::debug!(
                    "accepted legacy ID3-wrapped FLAC flush EOF after declared extent: path={}, frames={}, error={error}",
                    path.display(),
                    total_decoded,
                );
            } else {
                return Err(format!("send_eof: {error}"));
            }
        }
    }
    if !reached_sample_limit && !accepted_wrapped_eof {
        loop {
            match decoder.receive_frame(&mut decoded) {
                Ok(()) => {
                    if process_decoded_frame!() {
                        break;
                    }
                }
                Err(ffmpeg::Error::Eof) => break,
                Err(ffmpeg::Error::Other { errno })
                    if errno == ffmpeg::util::error::EAGAIN => {
                        return Err("decoder flush ended without clean EOF".to_string())
                    }
                Err(error) if wrapped_flac.accepts_post_extent_error(total_decoded) => {
                    log::debug!(
                        "accepted legacy ID3-wrapped FLAC decoder EOF after declared extent: path={}, frames={}, error={error}",
                        path.display(),
                        total_decoded,
                    );
                    break;
                }
                Err(error) => return Err(format!("audio decoder failed: {error}")),
            }
        }
    }

    wrapped_flac
        .validate_complete(total_decoded)
        .map_err(|error| error.to_string())?;

    // Flush last partial block (reference: dr_rms divides by actual count).
    if block_sample_count > 0 {
        for c in 0..channels as usize {
            let rms_linear = (2.0 * block_rms_sums[c] / block_sample_count as f64).sqrt();
            dr_block_rms[c].push(rms_linear);
            dr_block_peak[c].push(block_peaks[c]);
        }
    }

    // ── Compute final metrics ────────────────────────────────────

    if sample_count == 0 {
        return Err("no audio samples decoded".into());
    }

    let peak_db = if peak_abs > 0.0 {
        20.0 * peak_abs.log10()
    } else {
        -120.0
    };
    let rms_db = if sample_count > 0 {
        // AES-17 ×2 factor, matching foobar2000's foo_dr_meter display.
        let rms = (2.0 * rms_sum / sample_count as f64).sqrt();
        if rms > 0.0 {
            20.0 * rms.log10()
        } else {
            -120.0
        }
    } else {
        -120.0
    };
    let dc_bias = dc_sum / sample_count as f64;

    // Bit depth from OR mask (integer formats only).
    let actual_bit_depth = if is_integer_fmt && bit_or_mask != 0 {
        32 - bit_or_mask.trailing_zeros()
    } else {
        declared_bit_depth.unwrap_or(0)
    };

    // ── DR calculation (TT algorithm) ────────────────────────────

    let dr_value = compute_dr(&dr_block_rms, &dr_block_peak, channels as usize);

    let flac_wrappers = crate::flac_envelope::inspect_legacy_flac_wrappers(path)?;
    let flac_wrapper_repair_path = flac_wrappers
        .filter(|wrappers| wrappers.any())
        .map(|_| path.to_path_buf());

    Ok(AnalysisResult {
        source_path: path.to_path_buf(),
        path: path.to_path_buf(),
        flac_wrappers,
        flac_wrapper_repair_path,
        dr_value,
        peak_db,
        rms_db,
        clipping_count,
        dc_bias,
        actual_bit_depth,
        declared_bit_depth,
        sample_rate,
        channels,
        duration_secs: duration_secs.abs(),
        lufs: None,
        true_peak_dbtp: None,
        loudness_status: LoudnessAnalysisStatus::NotScanned,
        preemphasis: None,
        preemphasis_corr: None,
        preemphasis_detail: None,
        hdcd_detected: None,
        hdcd_detail: None,
    })
}

/// Compute the TT Dynamic Range value from per-channel block data.
///
/// Both `block_rms` and `block_peak` contain LINEAR amplitude values
/// (not dB). Block RMS includes the AES-17 ×2 factor (matching
/// foobar2000's foo_dr_meter and dr14_t.meter).
fn compute_dr(
    block_rms: &[Vec<f64>],  // per-channel Vec of linear RMS values
    block_peak: &[Vec<f64>], // per-channel Vec of linear peak values
    channels: usize,
) -> i32 {
    if channels == 0 || block_rms[0].is_empty() {
        return 0;
    }

    let mut channel_drs: Vec<f64> = Vec::new();

    for ch in 0..channels {
        let rms = &block_rms[ch];
        let peaks = &block_peak[ch];
        if rms.is_empty() {
            continue;
        }

        // Sort RMS descending, take top 20% (floor, at least 1).
        let mut sorted_rms: Vec<f64> = rms.clone();
        sorted_rms.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let top_count = ((sorted_rms.len() as f64 * 0.2).floor() as usize).max(1);

        // Quadratic mean (RMS-of-RMS) of the top 20% in linear domain.
        let rms_sq_sum: f64 = sorted_rms[..top_count].iter().map(|r| r * r).sum();
        let rms_score = (rms_sq_sum / top_count as f64).sqrt();

        // Second-highest per-channel block peak (robustness against spikes).
        let mut sorted_peaks: Vec<f64> = peaks.clone();
        sorted_peaks.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let peak_score = if sorted_peaks.len() >= 2 {
            sorted_peaks[1]
        } else {
            sorted_peaks[0]
        };

        // Per-channel DR in dB.
        if rms_score > 0.0 && peak_score > 0.0 {
            let dr = 20.0 * (peak_score / rms_score).log10();
            channel_drs.push(dr);
        }
    }

    if channel_drs.is_empty() {
        return 0;
    }

    // Arithmetic mean of per-channel DRs (matching foobar2000's
    // foo_dr_meter and MacinMeter). For mono, uses the only value.
    let dr: f64 = channel_drs.iter().sum::<f64>() / channel_drs.len() as f64;

    dr.round() as i32
}

/// Measure production loudness and ReplayGain reporting peak without writing tags.
///
/// This is deliberately the same raw observation path used by conversion and
/// metadata writing. ReplayGain-only scan demand is integrated-only: LRA state
/// is not constructed merely for this UI analysis.
#[derive(Debug, Clone)]
pub(crate) struct NativeLoudnessScan {
    pub integrated_lufs: Option<f64>,
    pub reporting_peak_dbtp: Option<f64>,
    pub unavailable: Option<crate::convert::replaygain::MathematicalUnavailability>,
}

pub(crate) async fn measure_loudness(path: &Path) -> Result<NativeLoudnessScan, String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let member = format!("scan-only:{}", path.display());
        let scan = crate::convert::replaygain::measure_paths(
            std::slice::from_ref(&path),
            std::slice::from_ref(&member),
            tonepoet_pipeline::ReplayGainMode::Track,
            crate::convert::replaygain::MetricDemand::IntegratedOnly,
        )
        .map_err(|error| error.to_string())?;
        let summary = scan
            .tracks
            .first()
            .ok_or_else(|| "native loudness scan returned no track observation".to_string())?;
        let unavailable = crate::convert::replaygain::integrated_unavailability(summary)
            .map_err(|error| error.to_string())?;
        Ok(NativeLoudnessScan {
            integrated_lufs: crate::convert::replaygain::finite_integrated_lufs(summary),
            reporting_peak_dbtp: crate::convert::replaygain::reporting_peak_dbtp(summary),
            unavailable,
        })
    })
    .await
    .map_err(|error| format!("native loudness scan task failed: {error}"))?
}

// ── HDCD detection ──────────────────────────────────────────────────

/// HDCD detection result from ffmpeg's af_hdcd filter.
pub struct HdcdResult {
    pub detected: bool,
    pub peak_extend: bool,
    pub total_packets: u64,
    pub max_gain: f64,
    pub packet_type: String,
    pub detail: String,
}

pub(crate) fn known_bit_depth(result: &AnalysisResult) -> Option<u32> {
    known_bit_depth_values(result.actual_bit_depth, result.declared_bit_depth)
}

fn known_bit_depth_values(actual_bit_depth: u32, declared_bit_depth: Option<u32>) -> Option<u32> {
    if actual_bit_depth > 0 {
        Some(actual_bit_depth)
    } else {
        declared_bit_depth.filter(|depth| *depth > 0)
    }
}

pub(crate) fn bit_depth_display(result: &AnalysisResult) -> String {
    bit_depth_display_values(result.actual_bit_depth, result.declared_bit_depth)
}

fn bit_depth_display_values(actual_bit_depth: u32, declared_bit_depth: Option<u32>) -> String {
    let Some(depth) = known_bit_depth_values(actual_bit_depth, declared_bit_depth) else {
        return "unknown / not applicable".to_string();
    };
    match declared_bit_depth {
        Some(declared) if declared != depth => format!("{depth}-bit ({declared} declared)"),
        _ => format!("{depth}-bit"),
    }
}

pub(crate) fn hdcd_eligible(result: &AnalysisResult) -> bool {
    hdcd_eligible_depth(result.declared_bit_depth, result.actual_bit_depth)
}

fn hdcd_eligible_depth(declared_bit_depth: Option<u32>, actual_bit_depth: u32) -> bool {
    declared_bit_depth == Some(16)
        || (declared_bit_depth.is_none() && actual_bit_depth > 0 && actual_bit_depth <= 16)
}

fn hdcd_ffmpeg_args(seek_secs: Option<f64>, duration_secs: Option<f64>) -> Vec<String> {
    let mut args = vec![
        "-hide_banner".to_string(),
        "-nostats".to_string(),
        "-y".to_string(),
        "-v".to_string(),
        "info".to_string(),
    ];
    if let Some(ss) = seek_secs {
        args.push("-ss".to_string());
        args.push(format!("{ss:.6}"));
    }
    if let Some(duration) = duration_secs {
        args.push("-t".to_string());
        args.push(format!("{duration:.6}"));
    }
    args
}

/// Detect HDCD encoding by running ffmpeg's af_hdcd filter and parsing
/// the info-level stderr output.
///
/// Optional `seek_secs` and `duration_secs` allow scanning a specific
/// segment of a single-image file (per-track HDCD detection).
pub async fn detect_hdcd(
    path: &Path,
    seek_secs: Option<f64>,
    duration_secs: Option<f64>,
) -> Option<HdcdResult> {
    use tokio::process::Command;

    let mut cmd = Command::new("ffmpeg");
    cmd.args(hdcd_ffmpeg_args(seek_secs, duration_secs));
    cmd.arg("-i").arg(path);
    cmd.args(["-af", "hdcd", "-f", "s24le", "/dev/null"]);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::piped());

    let output = cmd.output().await.ok()?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    parse_hdcd_output(&stderr)
}

/// Parse ffmpeg's HDCD filter info-level output into a structured result.
///
/// The info-level output produces a single summary line per channel, e.g.:
/// `[Parsed_hdcd_0 @ ...] HDCD detected: yes, peak_extend: enabled permanently, max_gain_adj: 0.0 dB, transient_filter: detected, detectable errors: 0`
/// or:
/// `[Parsed_hdcd_0 @ ...] HDCD detected: no`
fn parse_hdcd_output(stderr: &str) -> Option<HdcdResult> {
    // Find the HDCD detection summary line(s). There may be multiple
    // (one per output context). Any "yes" means HDCD is present.
    let mut detected = false;
    let mut summary_line = String::new();
    for line in stderr.lines() {
        if line.contains("HDCD detected:") {
            if line.contains("HDCD detected: yes") {
                detected = true;
                summary_line = line.to_string();
            } else if summary_line.is_empty() {
                summary_line = line.to_string();
            }
        }
    }

    if summary_line.is_empty() {
        return None;
    }

    // Parse details from the summary line.
    let peak_extend = summary_line.contains("peak_extend: enabled");
    let transient_filter = summary_line.contains("transient_filter: detected");

    let max_gain = summary_line
        .split("max_gain_adj:")
        .nth(1)
        .and_then(|s| s.split("dB").next())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(0.0);

    let errors = summary_line
        .split("detectable errors:")
        .nth(1)
        .and_then(|s| s.trim().split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    // Build human-readable detail.
    let detail = if detected {
        let mut parts = Vec::new();
        if peak_extend {
            parts.push("peak extend".to_string());
        }
        if max_gain.abs() > 0.0 {
            parts.push(format!("gain adj {:.1} dB", max_gain));
        }
        if transient_filter {
            parts.push("transient filter".to_string());
        }
        if !peak_extend && max_gain.abs() == 0.0 && !transient_filter {
            parts.push("passive (no features used)".to_string());
        }
        if errors > 0 {
            parts.push(format!("{} errors", errors));
        }
        format!("HDCD ({})", parts.join(", "))
    } else {
        "not detected".to_string()
    };

    Some(HdcdResult {
        detected,
        peak_extend,
        total_packets: 0, // not reported at info level
        max_gain,
        packet_type: String::new(), // not reported at info level
        detail,
    })
}

/// Collect the distinct physical carriers represented by wrapped Analyze rows.
/// Single-image CUE tracks intentionally share one carrier even though each row
/// has its own synthetic display path.
pub(crate) fn flac_wrapper_repair_targets(results: &[AnalysisResult]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    results
        .iter()
        .filter(|result| {
            result
                .flac_wrappers
                .is_some_and(|wrappers| wrappers.any())
        })
        .filter_map(|result| result.flac_wrapper_repair_path.as_ref())
        .filter(|path| seen.insert((*path).clone()))
        .cloned()
        .collect()
}

pub(crate) fn has_synthetic_source_rows(results: &[AnalysisResult]) -> bool {
    results.iter().any(|result| result.path != result.source_path)
}

pub(crate) fn display_name(result: &AnalysisResult) -> String {
    if result.path != result.source_path {
        if let Some(parent) = result.source_path.parent() {
            if let Ok(relative) = result.path.strip_prefix(parent) {
                return relative.to_string_lossy().to_string();
            }
        }
    }
    result
        .path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| result.path.display().to_string())
}

/// Order Analysis rows without ever handing synthetic CUE display paths to
/// filesystem-aware sort helpers. Ordinary rows retain the existing tag-aware
/// physical-file ordering; a single-image CUE batch is ordered by its generated
/// display label, whose leading two-digit track number preserves CUE order.
pub(crate) fn sort_results_for_display(results: &mut Vec<AnalysisResult>) {
    if has_synthetic_source_rows(results) {
        results.sort_by_cached_key(|result| display_name(result).to_ascii_lowercase());
        return;
    }

    let mut paths = results
        .iter()
        .map(|result| result.source_path.clone())
        .collect::<Vec<_>>();
    super::probe::sort_paths_by_track(&mut paths);
    results.sort_by(|a, b| {
        let ai = paths
            .iter()
            .position(|path| *path == a.source_path)
            .unwrap_or(usize::MAX);
        let bi = paths
            .iter()
            .position(|path| *path == b.source_path)
            .unwrap_or(usize::MAX);
        ai.cmp(&bi)
    });
}

/// Refresh wrapper state after an Analyze repair without losing the physical
/// carrier provenance carried separately from the row's display path.
pub(crate) fn refresh_flac_wrapper_state(result: &mut AnalysisResult) -> Result<(), String> {
    let Some(repair_path) = result.flac_wrapper_repair_path.clone() else {
        return Ok(());
    };
    let wrappers = crate::flac_envelope::inspect_legacy_flac_wrappers(&repair_path)?;
    result.flac_wrappers = wrappers;
    result.flac_wrapper_repair_path = wrappers
        .filter(|wrappers| wrappers.any())
        .map(|_| repair_path);
    Ok(())
}

/// DR value quality label.
pub fn dr_label(dr: i32) -> &'static str {
    match dr {
        0..=3 => "crushed",
        4..=7 => "compressed",
        8..=13 => "good",
        14..=20 => "excellent",
        _ => "exceptional",
    }
}

#[cfg(test)]
mod loudness_status_tests {
    use super::*;

    fn write_pcm16_mono_wav(path: &Path, sample_rate: u32, samples: &[i16]) {
        use std::io::Write;

        let data_len = (samples.len() * std::mem::size_of::<i16>()) as u32;
        let mut file = std::fs::File::create(path).expect("create wav");
        file.write_all(b"RIFF").expect("riff");
        file.write_all(&(36u32 + data_len).to_le_bytes())
            .expect("riff size");
        file.write_all(b"WAVEfmt ").expect("wave fmt");
        file.write_all(&16u32.to_le_bytes()).expect("fmt size");
        file.write_all(&1u16.to_le_bytes()).expect("pcm format");
        file.write_all(&1u16.to_le_bytes()).expect("channels");
        file.write_all(&sample_rate.to_le_bytes()).expect("sample rate");
        file.write_all(&(sample_rate * 2).to_le_bytes())
            .expect("byte rate");
        file.write_all(&2u16.to_le_bytes()).expect("block align");
        file.write_all(&16u16.to_le_bytes()).expect("bits");
        file.write_all(b"data").expect("data");
        file.write_all(&data_len.to_le_bytes()).expect("data size");
        for sample in samples {
            file.write_all(&sample.to_le_bytes()).expect("sample");
        }
    }

    #[test]
    fn seeked_analysis_matches_exact_isolated_region() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sample_rate = 44_100u32;
        let region_samples = sample_rate as usize * 2;
        let mut album = vec![1_000i16; region_samples];
        album.extend(std::iter::repeat(-12_000i16).take(region_samples));
        let isolated = vec![-12_000i16; region_samples];
        let album_path = temp.path().join("album.wav");
        let isolated_path = temp.path().join("track2.wav");
        write_pcm16_mono_wav(&album_path, sample_rate, &album);
        write_pcm16_mono_wav(&isolated_path, sample_rate, &isolated);

        let seeked = analyze_file(
            &album_path,
            Some(region_samples as u64),
            Some(region_samples as u64),
        )
        .expect("seeked track analysis");
        let direct = analyze_file(&isolated_path, None, None).expect("isolated track analysis");

        assert_eq!(seeked.dr_value, direct.dr_value);
        assert!((seeked.peak_db - direct.peak_db).abs() < 1e-9);
        assert!((seeked.rms_db - direct.rms_db).abs() < 1e-9);
        assert!((seeked.dc_bias - direct.dc_bias).abs() < 1e-12);
        assert_eq!(seeked.clipping_count, direct.clipping_count);
        assert_eq!(seeked.actual_bit_depth, direct.actual_bit_depth);
        assert!((seeked.duration_secs - 2.0).abs() < 1e-9);
    }

    #[test]
    fn whole_file_hdcd_args_do_not_impose_one_second_limit() {
        let whole = hdcd_ffmpeg_args(None, None);
        assert!(!whole.iter().any(|arg| arg == "-t"));

        let segment = hdcd_ffmpeg_args(Some(180.0), Some(42.5));
        assert!(segment
            .windows(2)
            .any(|pair| pair[0] == "-ss" && pair[1] == "180.000000"));
        assert!(segment
            .windows(2)
            .any(|pair| pair[0] == "-t" && pair[1] == "42.500000"));
    }

    fn write_late_hdcd_packet_wav(path: &Path) {
        use std::io::Write;

        const SAMPLE_RATE: u32 = 44_100;
        const CHANNELS: u16 = 2;
        const SECONDS: usize = 2;
        const PREFIX_WINDOW: u32 = 0x7de0_a5d0;
        const CONTROL_BYTE: u8 = 0x53;

        // FFmpeg's HDCD scanner starts with a 32-sample read-ahead, then
        // advances over digital silence in 31-sample windows. Choose the first
        // such boundary at or after 1.5 seconds so a one-second scan cannot
        // see the packet. PREFIX_WINDOW maps to the Format A sync word
        // 0x7e0fa005; CONTROL_BYTE maps to a valid peak-extend control.
        let threshold = (SAMPLE_RATE as usize * 3) / 2;
        let silent_windows = (threshold.saturating_sub(32) + 30) / 31;
        let packet_start = 32 + 31 * silent_windows;

        let mut packet_bits = Vec::with_capacity(39);
        for bit in (0..31).rev() {
            packet_bits.push(((PREFIX_WINDOW >> bit) & 1) as i16);
        }
        for bit in (0..8).rev() {
            packet_bits.push(((CONTROL_BYTE >> bit) & 1) as i16);
        }

        let frame_count = SAMPLE_RATE as usize * SECONDS;
        let data_len = frame_count * CHANNELS as usize * std::mem::size_of::<i16>();
        let mut file = std::fs::File::create(path).expect("create synthetic HDCD wav");
        file.write_all(b"RIFF").expect("riff");
        file.write_all(&(36u32 + data_len as u32).to_le_bytes())
            .expect("riff size");
        file.write_all(b"WAVEfmt ").expect("wave fmt");
        file.write_all(&16u32.to_le_bytes()).expect("fmt size");
        file.write_all(&1u16.to_le_bytes()).expect("pcm format");
        file.write_all(&CHANNELS.to_le_bytes()).expect("channels");
        file.write_all(&SAMPLE_RATE.to_le_bytes())
            .expect("sample rate");
        file.write_all(&(SAMPLE_RATE * CHANNELS as u32 * 2).to_le_bytes())
            .expect("byte rate");
        file.write_all(&(CHANNELS * 2).to_le_bytes())
            .expect("block align");
        file.write_all(&16u16.to_le_bytes()).expect("bits");
        file.write_all(b"data").expect("data");
        file.write_all(&(data_len as u32).to_le_bytes())
            .expect("data size");

        for frame in 0..frame_count {
            let bit = if (packet_start..packet_start + packet_bits.len()).contains(&frame) {
                packet_bits[frame - packet_start]
            } else {
                0
            };
            let sample = 1_000i16 + bit;
            file.write_all(&sample.to_le_bytes()).expect("left sample");
            file.write_all(&sample.to_le_bytes()).expect("right sample");
        }
    }

    #[tokio::test]
    async fn whole_file_hdcd_scan_detects_code_after_first_second() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_err()
        {
            return;
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("late-hdcd.wav");
        write_late_hdcd_packet_wav(&path);

        let first_second = detect_hdcd(&path, None, Some(1.0))
            .await
            .expect("bounded HDCD scan");
        assert!(!first_second.detected);

        let whole = detect_hdcd(&path, None, None)
            .await
            .expect("whole-file HDCD scan");
        assert!(whole.detected);
        assert!(whole.peak_extend);
        assert!(whole.total_packets > 0);
    }

    #[test]
    fn unknown_bit_depth_is_not_renderable_or_hdcd_eligible() {
        assert_eq!(known_bit_depth_values(0, None), None);
        assert_eq!(
            bit_depth_display_values(0, None),
            "unknown / not applicable"
        );
        assert!(!hdcd_eligible_depth(None, 0));
        assert!(hdcd_eligible_depth(None, 16));
        assert!(hdcd_eligible_depth(Some(16), 0));
        assert!(!hdcd_eligible_depth(Some(24), 16));
    }

    #[test]
    fn synthetic_analysis_rows_are_detected_for_mutation_refusal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("album.wav");
        let silence = vec![0i16; 44_100];
        write_pcm16_mono_wav(&source, 44_100, &silence);
        let direct = analyze_file(&source, None, None).expect("direct analysis");
        assert!(!has_synthetic_source_rows(std::slice::from_ref(&direct)));

        let mut cue_row = direct;
        cue_row.path = temp.path().join("01 - Track One.flac");
        assert!(has_synthetic_source_rows(std::slice::from_ref(&cue_row)));
        assert_eq!(cue_row.source_path, source);
    }

    #[test]
    fn synthetic_cue_sort_uses_display_identity_without_filesystem_path_semantics() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("album.wav");
        let silence = vec![0i16; 44_100];
        write_pcm16_mono_wav(&source, 44_100, &silence);
        let base = analyze_file(&source, None, None).expect("direct analysis");

        let mut track_two = base.clone();
        track_two.path = temp.path().join("02 - AC/DC.flac");
        let mut track_one = base;
        track_one.path = temp.path().join("01 - First.flac");
        let mut rows = vec![track_two, track_one];

        sort_results_for_display(&mut rows);

        assert_eq!(display_name(&rows[0]), "01 - First.flac");
        assert_eq!(display_name(&rows[1]), "02 - AC/DC.flac");
        assert!(!temp.path().join("02 - AC").exists());
    }

    #[test]
    fn analyze_accepts_id3_wrapped_flac_fixture() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/regression/id3_wrapped_flac/id3v2_id3v1_48000.flac");
        let result = analyze_file(&path, None, None)
            .expect(":analyze must accept the verified ID3v2 + FLAC + ID3v1 envelope");
        assert_eq!(result.sample_rate, 48_000);
        assert_eq!(result.channels, 2);
        assert!((result.duration_secs - 1.0).abs() < 0.01);
        assert_eq!(
            result.flac_wrappers,
            Some(crate::flac_envelope::LegacyFlacWrappers {
                id3v2_prefix: true,
                id3v1_trailer: true,
            })
        );
        assert_eq!(result.flac_wrapper_repair_path.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn analyze_accepts_id3v1_trailer_only_stream_copy() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/regression/id3_wrapped_flac/id3v2_id3v1_48000.flac");
        let bytes = std::fs::read(&source).expect("read wrapped FLAC fixture");
        assert!(bytes.starts_with(b"ID3"));
        let temp = tempfile::tempdir().expect("temp dir");
        let staged = temp.path().join("stream-copy.flac");
        std::fs::write(&staged, &bytes[10..]).expect("write trailer-only staged FLAC");

        let result = analyze_file(&staged, None, None)
            .expect(":analyze must accept a verified trailer-only native FLAC stream copy");
        assert_eq!(result.sample_rate, 48_000);
        assert_eq!(result.channels, 2);
        assert!((result.duration_secs - 1.0).abs() < 0.01);
        assert_eq!(
            result.flac_wrappers,
            Some(crate::flac_envelope::LegacyFlacWrappers {
                id3v2_prefix: false,
                id3v1_trailer: true,
            })
        );
    }

    #[test]
    fn native_unavailability_maps_without_collapsing_reasons() {
        use crate::convert::replaygain::MathematicalUnavailability as Native;

        assert_eq!(
            LoudnessUnavailableReason::from(Native::TooShort {
                frames: 100,
                required_frames: 19_200,
            }),
            LoudnessUnavailableReason::TooShort {
                frames: 100,
                required_frames: 19_200,
            }
        );
        assert_eq!(
            LoudnessUnavailableReason::from(Native::BelowAbsoluteGate),
            LoudnessUnavailableReason::BelowAbsoluteGate
        );
        assert_eq!(
            LoudnessUnavailableReason::from(Native::BelowRelativeGate),
            LoudnessUnavailableReason::BelowRelativeGate
        );
        assert_eq!(
            LoudnessUnavailableReason::from(Native::NoEligibleBlocks),
            LoudnessUnavailableReason::NoEligibleBlocks
        );
    }
}
