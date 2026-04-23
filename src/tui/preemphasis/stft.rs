//! STFT engine: Hann-windowed 4096-pt FFT with 1/3-octave band binning.
//!
//! Decodes audio via ffmpeg-next in-process and produces smoothed
//! log-magnitude spectra in 31 bands (100 Hz – 20 kHz).

use std::path::Path;

/// Number of 1/3-octave bands (100 Hz to ~20 kHz).
pub const NUM_BANDS: usize = 31;

/// FFT frame size.
pub const FFT_SIZE: usize = 4096;

/// Hop size (50% overlap).
pub const HOP_SIZE: usize = FFT_SIZE / 2;

/// Result of STFT analysis for a single file.
#[derive(Debug, Clone)]
pub struct StftResult {
    /// Per-frame 1/3-octave band spectra in dB (relative).
    /// Each inner array is [f64; NUM_BANDS].
    pub band_spectra: Vec<[f64; NUM_BANDS]>,
    /// Per-frame RMS level (linear, time-domain).
    pub frame_rms: Vec<f64>,
    /// Per-frame raw PCM samples (mono, for virtual de-emphasis).
    /// Each Vec has FFT_SIZE samples.
    pub frame_samples: Vec<Vec<f64>>,
    /// Sample rate.
    pub sample_rate: u32,
}

/// Compute 1/3-octave band center frequencies.
/// f_center[k] = 100 * 2^(k/3) for k = 0..NUM_BANDS
pub fn band_centers() -> [f64; NUM_BANDS] {
    let mut centers = [0.0; NUM_BANDS];
    for k in 0..NUM_BANDS {
        centers[k] = 100.0 * (2.0f64).powf(k as f64 / 3.0);
    }
    centers
}

/// Compute bin ranges for each 1/3-octave band given sample rate.
/// Returns (lo_bin, hi_bin) inclusive for each band.
pub fn compute_bin_ranges(sample_rate: u32) -> Vec<(usize, usize)> {
    let bin_width = sample_rate as f64 / FFT_SIZE as f64;
    let centers = band_centers();
    let factor = (2.0f64).powf(1.0 / 6.0); // half a 1/3 octave

    centers.iter().map(|&fc| {
        let lo_freq = fc / factor;
        let hi_freq = fc * factor;
        let lo_bin = (lo_freq / bin_width).ceil() as usize;
        let hi_bin = (hi_freq / bin_width).floor() as usize;
        let hi_bin = hi_bin.min(FFT_SIZE / 2 - 1);
        (lo_bin.max(1), hi_bin)
    }).collect()
}

/// Precompute Hann window of given size.
pub fn hann_window(size: usize) -> Vec<f64> {
    use std::f64::consts::PI;
    (0..size)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (size - 1) as f64).cos()))
        .collect()
}

/// Decode an audio file and compute per-frame 1/3-octave band spectra.
///
/// Uses ffmpeg-next for in-process decoding. Returns band spectra in dB
/// along with per-frame RMS and raw samples for downstream processing.
pub fn compute_band_spectra(path: &Path, sample_rate: u32) -> Result<StftResult, String> {
    use ffmpeg_next as ffmpeg;
    use ffmpeg_next::media::Type;
    use rustfft::{FftPlanner, num_complex::Complex};

    crate::tui::probe::ensure_ffmpeg_init_pub();

    let mut ictx = ffmpeg::format::input(&path)
        .map_err(|e| format!("open failed: {}", e))?;

    let audio_stream = ictx.streams().best(Type::Audio)
        .ok_or("no audio stream")?;
    let stream_idx = audio_stream.index();

    let codec_params = audio_stream.parameters();
    let codec_ctx = ffmpeg::codec::context::Context::from_parameters(codec_params)
        .map_err(|e| format!("codec params: {}", e))?;
    let mut decoder = codec_ctx.decoder().audio()
        .map_err(|e| format!("decoder: {}", e))?;

    let channels = decoder.channels() as usize;
    let actual_rate = decoder.rate();
    let _ = sample_rate; // Use actual_rate from decoder instead.

    // Precompute window and bin ranges.
    let window = hann_window(FFT_SIZE);
    let bin_ranges = compute_bin_ranges(actual_rate);

    // FFT planner.
    let mut planner = FftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);

    // Cap decode at 10 minutes to bound memory usage.
    // Pre-emphasis is a global property — any representative portion suffices.
    let max_samples = (actual_rate as usize) * 600; // 10 minutes

    // Accumulate decoded samples into a mono buffer (capped).
    let mut mono_samples: Vec<f64> = Vec::with_capacity(max_samples.min(actual_rate as usize * 300));

    let mut decoded_frame = ffmpeg::util::frame::Audio::empty();

    // Decode audio into mono buffer (up to cap).
    'decode: for (stream, packet) in ictx.packets() {
        if stream.index() != stream_idx {
            continue;
        }
        decoder.send_packet(&packet).map_err(|e| format!("send packet: {}", e))?;

        while decoder.receive_frame(&mut decoded_frame).is_ok() {
            let n = decoded_frame.samples();
            if n == 0 { continue; }

            let fmt = decoder.format();
            for i in 0..n {
                if mono_samples.len() >= max_samples { break 'decode; }
                let mut sum = 0.0f64;
                for ch in 0..channels {
                    let sample = extract_sample(&decoded_frame, fmt, ch, i);
                    sum += sample;
                }
                mono_samples.push(sum / channels as f64);
            }
        }
    }

    // Flush decoder (only if we haven't hit the cap).
    if mono_samples.len() < max_samples {
        decoder.send_eof().ok();
        while decoder.receive_frame(&mut decoded_frame).is_ok() {
            let n = decoded_frame.samples();
            if n == 0 { continue; }
            let fmt = decoder.format();
            for i in 0..n {
                if mono_samples.len() >= max_samples { break; }
                let mut sum = 0.0f64;
                for ch in 0..channels {
                    let sample = extract_sample(&decoded_frame, fmt, ch, i);
                    sum += sample;
                }
                mono_samples.push(sum / channels as f64);
            }
        }
    }

    if mono_samples.len() < FFT_SIZE {
        return Err("audio too short for analysis".into());
    }

    // Process frames with hop.
    let num_frames = (mono_samples.len() - FFT_SIZE) / HOP_SIZE + 1;
    let mut band_spectra = Vec::with_capacity(num_frames);
    let mut frame_rms = Vec::with_capacity(num_frames);
    let mut frame_samples_out = Vec::with_capacity(num_frames);

    let mut fft_buf = vec![Complex::new(0.0, 0.0); FFT_SIZE];

    for frame_idx in 0..num_frames {
        let start = frame_idx * HOP_SIZE;
        let frame_slice = &mono_samples[start..start + FFT_SIZE];

        // Compute RMS.
        let rms = (frame_slice.iter().map(|&x| x * x).sum::<f64>() / FFT_SIZE as f64).sqrt();
        frame_rms.push(rms);

        // Store raw samples for later virtual de-emphasis.
        frame_samples_out.push(frame_slice.to_vec());

        // Apply window and compute FFT.
        for (i, &s) in frame_slice.iter().enumerate() {
            fft_buf[i] = Complex::new(s * window[i], 0.0);
        }
        fft.process(&mut fft_buf);

        // Compute power spectrum (first half only, symmetric).
        let power: Vec<f64> = fft_buf[..FFT_SIZE / 2]
            .iter()
            .map(|c| c.norm_sqr())
            .collect();

        // Bin into 1/3-octave bands.
        let mut bands = [0.0f64; NUM_BANDS];
        for (k, &(lo, hi)) in bin_ranges.iter().enumerate() {
            if lo > hi || hi >= power.len() {
                bands[k] = -120.0;
                continue;
            }
            let sum: f64 = power[lo..=hi].iter().sum();
            bands[k] = if sum > 0.0 {
                10.0 * sum.log10()
            } else {
                -120.0
            };
        }
        band_spectra.push(bands);
    }

    Ok(StftResult {
        band_spectra,
        frame_rms,
        frame_samples: frame_samples_out,
        sample_rate: actual_rate,
    })
}

/// Extract a single sample as f64 from a decoded frame.
/// Returns 0.0 if the index is out of bounds (safety for edge-case frame sizes).
fn extract_sample(
    frame: &ffmpeg_next::util::frame::Audio,
    fmt: ffmpeg_next::util::format::sample::Sample,
    channel: usize,
    index: usize,
) -> f64 {
    use ffmpeg_next::util::format::sample::{Sample, Type as SampleType};

    match fmt {
        Sample::I16(SampleType::Planar) => {
            let plane = frame.plane::<i16>(channel);
            plane.get(index).map(|&s| s as f64 / 32768.0).unwrap_or(0.0)
        }
        Sample::I16(SampleType::Packed) => {
            let plane = frame.plane::<i16>(0);
            let channels = frame.channels() as usize;
            let idx = index * channels + channel;
            plane.get(idx).map(|&s| s as f64 / 32768.0).unwrap_or(0.0)
        }
        Sample::I32(SampleType::Planar) => {
            let plane = frame.plane::<i32>(channel);
            plane.get(index).map(|&s| s as f64 / 2147483648.0).unwrap_or(0.0)
        }
        Sample::I32(SampleType::Packed) => {
            let plane = frame.plane::<i32>(0);
            let channels = frame.channels() as usize;
            let idx = index * channels + channel;
            plane.get(idx).map(|&s| s as f64 / 2147483648.0).unwrap_or(0.0)
        }
        Sample::F32(SampleType::Planar) => {
            let plane = frame.plane::<f32>(channel);
            plane.get(index).map(|&s| s as f64).unwrap_or(0.0)
        }
        Sample::F32(SampleType::Packed) => {
            let plane = frame.plane::<f32>(0);
            let channels = frame.channels() as usize;
            let idx = index * channels + channel;
            plane.get(idx).map(|&s| s as f64).unwrap_or(0.0)
        }
        Sample::F64(SampleType::Planar) => {
            let plane = frame.plane::<f64>(channel);
            plane.get(index).copied().unwrap_or(0.0)
        }
        Sample::F64(SampleType::Packed) => {
            let plane = frame.plane::<f64>(0);
            let channels = frame.channels() as usize;
            let idx = index * channels + channel;
            plane.get(idx).copied().unwrap_or(0.0)
        }
        _ => 0.0,
    }
}
