//! Frame selection: keep only low-information frames for scoring.
//!
//! Scores each STFT frame on three criteria:
//! - RMS level (must be quiet, < -30 dBFS)
//! - HF spectral flatness (Wiener entropy; high = noise-like = good)
//! - Tonalness (max/median in HF; reject highly tonal frames)
//!
//! If fewer than 50 frames pass, falls back to quietest 20% without
//! the flatness gate.

use super::stft::{StftResult, NUM_BANDS};

/// Minimum qualifying frames for a valid score.
const MIN_FRAMES: usize = 50;

/// RMS threshold in linear (corresponds to -30 dBFS).
const RMS_THRESHOLD: f64 = 0.0316228; // 10^(-30/20)

/// HF spectral flatness threshold (Wiener entropy).
/// 1.0 = white noise, <0.1 = tonal. We want noise-like frames.
const FLATNESS_THRESHOLD: f64 = 0.3;

/// Peak-to-median ratio threshold in dB for HF tonalness rejection.
const TONALNESS_THRESHOLD_DB: f64 = 12.0;

/// HF bands: indices 15..=30 (approximately 3.2 kHz to 20 kHz).
const HF_BAND_START: usize = 15;

/// Result of frame selection.
#[derive(Debug, Clone)]
pub struct SelectedFrames {
    /// Indices into StftResult.band_spectra of qualifying frames.
    pub frames: Vec<usize>,
    /// Per-frame tonalness (peak-to-median in HF, dB) for selected frames.
    pub tonalness: Vec<f64>,
    /// Per-frame RMS for selected frames (linear).
    pub rms_values: Vec<f64>,
}

/// Select low-information frames from STFT results.
pub fn select_frames(stft: &StftResult) -> SelectedFrames {
    let n = stft.band_spectra.len();
    if n == 0 {
        return SelectedFrames { frames: vec![], tonalness: vec![], rms_values: vec![] };
    }

    // Score each frame.
    let mut candidates: Vec<(usize, f64, f64)> = Vec::new(); // (index, rms, tonalness)

    for i in 0..n {
        let rms = stft.frame_rms[i];
        if rms < 1e-10 {
            continue; // Skip silence (no useful spectral info).
        }
        if rms > RMS_THRESHOLD {
            continue; // Too loud.
        }

        let bands = &stft.band_spectra[i];
        let (flatness, tonalness) = compute_hf_metrics(bands);

        if flatness >= FLATNESS_THRESHOLD && tonalness < TONALNESS_THRESHOLD_DB {
            candidates.push((i, rms, tonalness));
        }
    }

    // Fallback: if too few frames, take quietest 20% without flatness gate.
    if candidates.len() < MIN_FRAMES {
        candidates.clear();
        let mut all_quiet: Vec<(usize, f64, f64)> = (0..n)
            .filter_map(|i| {
                let rms = stft.frame_rms[i];
                if rms < 1e-10 || rms > RMS_THRESHOLD {
                    return None;
                }
                let bands = &stft.band_spectra[i];
                let (_, tonalness) = compute_hf_metrics(bands);
                Some((i, rms, tonalness))
            })
            .collect();

        // Sort by RMS ascending (quietest first).
        all_quiet.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        // Take quietest 20% of all frames.
        let target = (n as f64 * 0.2).ceil() as usize;
        let take = target.min(all_quiet.len());
        candidates = all_quiet.into_iter().take(take).collect();
    }

    SelectedFrames {
        frames: candidates.iter().map(|c| c.0).collect(),
        rms_values: candidates.iter().map(|c| c.1).collect(),
        tonalness: candidates.iter().map(|c| c.2).collect(),
    }
}

/// Compute HF spectral flatness (Wiener entropy) and tonalness.
///
/// Flatness = exp(mean(ln(S))) / mean(S) over HF bands.
/// Tonalness = max(S) - median(S) in dB over HF bands.
fn compute_hf_metrics(bands: &[f64; NUM_BANDS]) -> (f64, f64) {
    let hf = &bands[HF_BAND_START..];

    // Convert from dB to linear power for flatness computation.
    let linear: Vec<f64> = hf.iter().map(|&db| {
        if db <= -120.0 { 1e-12 } else { 10.0f64.powf(db / 10.0) }
    }).collect();

    let n = linear.len() as f64;

    // Geometric mean (exp of mean of logs).
    let log_sum: f64 = linear.iter().map(|&x| x.ln()).sum();
    let geo_mean = (log_sum / n).exp();

    // Arithmetic mean.
    let arith_mean = linear.iter().sum::<f64>() / n;

    let flatness = if arith_mean > 0.0 { geo_mean / arith_mean } else { 0.0 };

    // Tonalness: max - median in dB (on the original dB values).
    let mut sorted_db: Vec<f64> = hf.to_vec();
    sorted_db.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n_hf = sorted_db.len();
    let median_db = if n_hf % 2 == 1 {
        sorted_db[n_hf / 2]
    } else {
        (sorted_db[n_hf / 2 - 1] + sorted_db[n_hf / 2]) / 2.0
    };
    let max_db = sorted_db.last().copied().unwrap_or(-120.0);
    let tonalness = max_db - median_db;

    (flatness, tonalness)
}
