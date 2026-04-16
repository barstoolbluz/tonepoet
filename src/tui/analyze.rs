//! Audio analysis: DR meter, peak, RMS, clipping, DC bias, bit depth.
//!
//! Single-pass PCM decode via ffmpeg-next computes all metrics from one
//! read of the file. LUFS and true peak are obtained separately via the
//! loudgain subprocess.

use std::path::{Path, PathBuf};

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
    /// Integrated loudness in LUFS (from loudgain, None if unavailable).
    pub lufs: Option<f64>,
    /// True peak in dBTP (from loudgain, None if unavailable).
    pub true_peak_dbtp: Option<f64>,
}

/// Analyze an audio file: decode to PCM and compute all metrics in one pass.
pub fn analyze_file(path: &Path) -> Result<AnalysisResult, String> {
    use ffmpeg_next as ffmpeg;
    use ffmpeg_next::media::Type;
    use ffmpeg_next::util::format::sample::{Sample, Type as SampleType};

    crate::tui::probe::ensure_ffmpeg_init_pub();

    let mut ictx = ffmpeg::format::input(&path)
        .map_err(|e| format!("open failed: {}", e))?;

    let audio_stream = ictx
        .streams()
        .best(Type::Audio)
        .ok_or("no audio stream")?;
    let stream_idx = audio_stream.index();
    let time_base = audio_stream.time_base();

    let codec_params = audio_stream.parameters();
    let codec_ctx = ffmpeg::codec::context::Context::from_parameters(codec_params)
        .map_err(|e| format!("codec params: {}", e))?;
    let mut decoder = codec_ctx.decoder().audio()
        .map_err(|e| format!("decoder: {}", e))?;

    let sample_rate = decoder.rate();
    let channels = decoder.channels() as u32;
    let sample_fmt = decoder.format();

    // Declared bit depth from stream parameters.
    let declared_bit_depth = unsafe {
        let params = audio_stream.parameters().as_ptr();
        let raw = (*params).bits_per_raw_sample;
        if raw > 0 { Some(raw as u32) } else { None }
    };

    // Duration from stream.
    let duration_secs = audio_stream.duration() as f64 * f64::from(time_base);

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

    // Inline helper: accumulate samples for one channel. Does NOT
    // touch block_sample_count — that's handled per-frame below.
    macro_rules! accumulate_channel {
        ($samples:expr, $ch:expr, $raw_i32:expr) => {{
            let raw_opt: Option<&[i32]> = $raw_i32.map(|v| v as &[i32]);
            for (i, &s) in $samples.iter().enumerate() {
                let abs_val = s.abs();
                if abs_val > peak_abs { peak_abs = abs_val; }
                rms_sum += s * s;
                dc_sum += s;
                sample_count += 1;
                if abs_val >= 0.9999695 { clipping_count += 1; }

                if is_integer_fmt {
                    if let Some(raw) = raw_opt {
                        bit_or_mask |= (raw[i] as u32) | (raw[i].wrapping_neg() as u32);
                    }
                }

                if abs_val > block_peaks[$ch] { block_peaks[$ch] = abs_val; }
                block_rms_sums[$ch] += s * s;
            }
        }};
    }

    // ── Decode loop ──────────────────────────────────────────────

    let mut decoded = ffmpeg::util::frame::Audio::empty();

    // Process a single decoded audio frame: accumulate all channels
    // into the block and overall accumulators, then flush complete
    // 3-second blocks.
    macro_rules! process_frame {
        () => {
            let n = decoded.samples();
            if n == 0 { continue; }

            for ch in 0..channels as usize {
                match sample_fmt {
                    Sample::I16(SampleType::Planar) => {
                        let plane = decoded.plane::<i16>(ch);
                        let floats: Vec<f64> = plane.iter().map(|&s| s as f64 / 32768.0).collect();
                        let i32s: Vec<i32> = plane.iter().map(|&s| (s as i32) << 16).collect();
                        accumulate_channel!(floats, ch, Some(&i32s));
                    }
                    Sample::I32(SampleType::Planar) => {
                        let plane = decoded.plane::<i32>(ch);
                        let floats: Vec<f64> = plane.iter().map(|&s| s as f64 / 2147483648.0).collect();
                        accumulate_channel!(floats, ch, Some(plane));
                    }
                    Sample::F32(SampleType::Planar) => {
                        let plane = decoded.plane::<f32>(ch);
                        let floats: Vec<f64> = plane.iter().map(|&s| s as f64).collect();
                        accumulate_channel!(floats, ch, None::<&[i32]>);
                    }
                    Sample::F64(SampleType::Planar) => {
                        let plane = decoded.plane::<f64>(ch);
                        { let sl: &[f64] = plane; accumulate_channel!(sl, ch, None::<&[i32]>); }
                    }
                    // Packed formats: samples are interleaved in plane 0.
                    Sample::I16(SampleType::Packed) => {
                        let plane = decoded.plane::<i16>(0);
                        let ch_samples: Vec<f64> = plane.iter()
                            .skip(ch).step_by(channels as usize)
                            .map(|&s| s as f64 / 32768.0).collect();
                        let i32s: Vec<i32> = plane.iter()
                            .skip(ch).step_by(channels as usize)
                            .map(|&s| (s as i32) << 16).collect();
                        accumulate_channel!(ch_samples, ch, Some(&i32s));
                    }
                    Sample::I32(SampleType::Packed) => {
                        let plane = decoded.plane::<i32>(0);
                        let ch_samples: Vec<f64> = plane.iter()
                            .skip(ch).step_by(channels as usize)
                            .map(|&s| s as f64 / 2147483648.0).collect();
                        let i32_ch: Vec<i32> = plane.iter()
                            .skip(ch).step_by(channels as usize)
                            .copied().collect();
                        accumulate_channel!(ch_samples, ch, Some(&i32_ch));
                    }
                    Sample::F32(SampleType::Packed) => {
                        let plane = decoded.plane::<f32>(0);
                        let ch_samples: Vec<f64> = plane.iter()
                            .skip(ch).step_by(channels as usize)
                            .map(|&s| s as f64).collect();
                        accumulate_channel!(ch_samples, ch, None::<&[i32]>);
                    }
                    Sample::F64(SampleType::Packed) => {
                        let plane = decoded.plane::<f64>(0);
                        let ch_samples: Vec<f64> = plane.iter()
                            .skip(ch).step_by(channels as usize)
                            .copied().collect();
                        accumulate_channel!(ch_samples, ch, None::<&[i32]>);
                    }
                    _ => {
                        return Err(format!("unsupported sample format: {:?}", sample_fmt));
                    }
                }
            }
            // Flush DR blocks once per frame (all channels processed).
            block_sample_count += n;
            while block_sample_count >= block_size {
                for c in 0..channels as usize {
                    // AES-17 modified RMS: factor of 2 so full-scale sine = 0 dBFS.
                    let rms_linear = (2.0 * block_rms_sums[c] / block_size as f64).sqrt();
                    dr_block_rms[c].push(rms_linear);
                    dr_block_peak[c].push(block_peaks[c]);
                    block_rms_sums[c] = 0.0;
                    block_peaks[c] = 0.0;
                }
                block_sample_count -= block_size;
            }
        };
    }

    for (stream, packet) in ictx.packets() {
        if stream.index() != stream_idx {
            continue;
        }
        decoder.send_packet(&packet).map_err(|e| format!("send_packet: {}", e))?;

        while decoder.receive_frame(&mut decoded).is_ok() {
            process_frame!();
        }
    }

    // Flush decoder — process remaining buffered frames.
    decoder.send_eof().map_err(|e| format!("send_eof: {}", e))?;
    while decoder.receive_frame(&mut decoded).is_ok() {
        process_frame!();
    }

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

    let peak_db = if peak_abs > 0.0 { 20.0 * peak_abs.log10() } else { -120.0 };
    let rms_db = if sample_count > 0 {
        let rms = (rms_sum / sample_count as f64).sqrt();
        if rms > 0.0 { 20.0 * rms.log10() } else { -120.0 }
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
    })
}

/// Compute the TT Dynamic Range value from per-channel block data.
///
/// Both `block_rms` and `block_peak` contain LINEAR amplitude values
/// (not dB). The block RMS values already include the AES-17 factor
/// of 2 (applied during accumulation).
fn compute_dr(
    block_rms: &[Vec<f64>],  // per-channel Vec of linear RMS values (with ×2)
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
        if rms.is_empty() { continue; }

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

    // For stereo/multi-channel, use the second-highest (i.e. lower for
    // stereo) per-channel DR — the channel with the least dynamic range
    // determines the track rating. For mono, use the only value.
    channel_drs.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let dr = if channel_drs.len() >= 2 {
        channel_drs[1]
    } else {
        channel_drs[0]
    };

    dr.round() as i32
}

/// Run loudgain in scan-only mode to get LUFS and true peak.
/// Returns (lufs, true_peak_dbtp) or None if loudgain fails.
pub async fn measure_loudness(path: &Path) -> Option<(f64, f64)> {
    use tokio::process::Command;

    let output = Command::new("loudgain")
        .args(["-s", "s", "-O", "-r", "-q"])
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Parse the -O format: File\tLoudness\tRange\tTrue_Peak\tTrue_Peak_dBTP\t...
    for line in stdout.lines() {
        if line.starts_with("File\t") { continue; } // Header.
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() >= 5 {
            let lufs = cols[1].trim().strip_suffix(" LUFS")
                .and_then(|s| s.parse::<f64>().ok())?;
            let true_peak = cols[4].trim().strip_suffix(" dBTP")
                .and_then(|s| s.parse::<f64>().ok())?;
            return Some((lufs, true_peak));
        }
    }
    None
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
