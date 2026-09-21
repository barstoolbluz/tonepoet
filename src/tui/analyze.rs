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
    pub path: PathBuf,
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
        () => {
            let n = decoded.samples();
            if n == 0 {
                continue;
            }

            // ── Extract per-channel samples + accumulate global stats ──
            for ch in 0..channels as usize {
                match sample_fmt {
                    Sample::I16(SampleType::Planar) => {
                        let plane = decoded.plane::<i16>(ch);
                        let floats: Vec<f64> = plane.iter().map(|&s| s as f64 / 32768.0).collect();
                        let i32s: Vec<i32> = plane.iter().map(|&s| (s as i32) << 16).collect();
                        accumulate_global!(floats, Some(&i32s));
                        ch_floats[ch] = floats;
                    }
                    Sample::I32(SampleType::Planar) => {
                        let plane = decoded.plane::<i32>(ch);
                        let floats: Vec<f64> =
                            plane.iter().map(|&s| s as f64 / 2147483648.0).collect();
                        accumulate_global!(floats, Some(plane));
                        ch_floats[ch] = floats;
                    }
                    Sample::F32(SampleType::Planar) => {
                        let plane = decoded.plane::<f32>(ch);
                        let floats: Vec<f64> = plane.iter().map(|&s| s as f64).collect();
                        accumulate_global!(floats, None::<&[i32]>);
                        ch_floats[ch] = floats;
                    }
                    Sample::F64(SampleType::Planar) => {
                        let plane = decoded.plane::<f64>(ch);
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
                                n * channels as usize,
                            )
                        };
                        let floats: Vec<f64> = full
                            .iter()
                            .skip(ch)
                            .step_by(channels as usize)
                            .map(|&s| s as f64 / 32768.0)
                            .collect();
                        let i32s: Vec<i32> = full
                            .iter()
                            .skip(ch)
                            .step_by(channels as usize)
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
                                n * channels as usize,
                            )
                        };
                        let floats: Vec<f64> = full
                            .iter()
                            .skip(ch)
                            .step_by(channels as usize)
                            .map(|&s| s as f64 / 2147483648.0)
                            .collect();
                        let i32_ch: Vec<i32> = full
                            .iter()
                            .skip(ch)
                            .step_by(channels as usize)
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
                                n * channels as usize,
                            )
                        };
                        let floats: Vec<f64> = full
                            .iter()
                            .skip(ch)
                            .step_by(channels as usize)
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
                                n * channels as usize,
                            )
                        };
                        let floats: Vec<f64> = full
                            .iter()
                            .skip(ch)
                            .step_by(channels as usize)
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

    // Seek to start position if specified.
    if let Some(start) = start_sample {
        // For most lossless formats, time_base = 1/sample_rate,
        // so the timestamp in time_base units IS the sample number.
        let ts = start as i64;
        // Seek to the nearest keyframe at or before the target.
        ictx.seek(ts, ..ts)
            .map_err(|e| format!("seek failed: {}", e))?;
    }

    let sample_limit = max_samples.unwrap_or(u64::MAX);
    let mut total_decoded: u64 = 0;
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
                    let frame_samples = decoded.samples() as u64;
                    let next_decoded = wrapped_flac
                        .checked_advance(total_decoded, frame_samples)
                        .map_err(|error| error.to_string())?;
                    process_frame!();
                    total_decoded = next_decoded;
                    if total_decoded >= sample_limit {
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
                    let frame_samples = decoded.samples() as u64;
                    let next_decoded = wrapped_flac
                        .checked_advance(total_decoded, frame_samples)
                        .map_err(|error| error.to_string())?;
                    process_frame!();
                    total_decoded = next_decoded;
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

    Ok(AnalysisResult {
        path: path.to_path_buf(),
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
    cmd.args(["-hide_banner", "-nostats", "-y", "-v", "info"]);
    if let Some(ss) = seek_secs {
        cmd.args(["-ss", &format!("{:.6}", ss)]);
    }
    cmd.args(["-t", &format!("{:.6}", duration_secs.unwrap_or(1.0))]);
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

    #[test]
    fn analyze_accepts_id3_wrapped_flac_fixture() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/regression/id3_wrapped_flac/id3v2_id3v1_48000.flac");
        let result = analyze_file(&path, None, None)
            .expect(":analyze must accept the verified ID3v2 + FLAC + ID3v1 envelope");
        assert_eq!(result.sample_rate, 48_000);
        assert_eq!(result.channels, 2);
        assert!((result.duration_secs - 1.0).abs() < 0.01);
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
