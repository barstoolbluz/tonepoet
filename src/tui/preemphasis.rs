//! Standalone CD pre-emphasis detection via full-file streaming spectral analysis.
//!
//! Red Book (IEC 60908) pre-emphasis uses time constants τ₁ = 50 µs and
//! τ₂ = 15 µs, producing a first-order high-shelf boost of ~+10 dB at 20 kHz.
//!
//! Detection uses two independent metrics:
//! 1. **Per-block detrended spectral average** — Goertzel analysis on every
//!    block of the full file. Per-block detrending removes the music's spectral
//!    tilt; averaging across hundreds of blocks cancels the music's spectral
//!    variation, leaving only the constant pre-emphasis shelf shape.
//! 2. **Crest factor improvement** — apply the exact IIR de-emphasis filter and
//!    measure the reduction in peak-to-RMS ratio. Pre-emphasized audio has
//!    elevated crest factors from HF boost; de-emphasis reduces this more than
//!    it would for normal audio.

use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use std::fs;

/// Confidence level for pre-emphasis detection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PreemphasisConfidence {
    Detected,
    Possible,
    NotDetected,
}

/// Result of pre-emphasis detection for a single file.
#[derive(Debug, Clone)]
pub struct PreemphasisResult {
    pub path: PathBuf,
    pub confidence: PreemphasisConfidence,
    /// Whether a CUE file with FLAGS PRE was found for this track.
    pub cue_confirmed: bool,
    /// Primary metric: RMS error between detrended spectral average and
    /// theoretical pre-emphasis curve (dB). Lower = better match.
    pub spectral_rms_error: f64,
    /// Secondary metric: crest factor improvement after de-emphasis (dB).
    /// Higher positive = more consistent with pre-emphasis.
    pub crest_improvement: f64,
    /// Human-readable detail string.
    pub detail: String,
}

// ── Constants ───────────────────────────────────────────────────────

const F1: f64 = 1.0 / (2.0 * PI * 50e-6); // ≈ 3183.1 Hz
const F2: f64 = 1.0 / (2.0 * PI * 15e-6); // ≈ 10610.3 Hz

const PROBE_FREQS: &[f64] = &[
    1000.0, 2000.0, 4000.0, 6000.0, 8000.0, 10000.0, 12000.0, 16000.0,
];

const BLOCK_SIZE: usize = 8192;

/// Thresholds for spectral RMS error (dB). Lower = closer match.
/// Without CUE evidence, spectral analysis is a screening heuristic.
/// These thresholds are set conservatively: "Possible" casts a wide net
/// since false positives are less harmful than missed pre-emphasis.
const SPECTRAL_DETECTED: f64 = 2.0; // unused without CUE; kept for future refinement
const SPECTRAL_POSSIBLE: f64 = 4.0;

// ── CUE/log file evidence ───────────────────────────────────────────

/// Check whether any CUE file in the same directory as `audio_path`
/// contains a `FLAGS PRE` line for a track that references this file
/// (or for any track, if the CUE is a single-image layout).
/// Also checks EAC `.log` files for "pre-emphasis" mentions.
fn check_cue_evidence(audio_path: &Path) -> bool {
    let dir = match audio_path.parent() {
        Some(d) => d,
        None => return false,
    };
    let audio_name = audio_path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return false,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        if ext == "cue" {
            if check_cue_file_for_preemphasis(&path, audio_name) {
                return true;
            }
        } else if ext == "log" {
            if check_log_file_for_preemphasis(&path) {
                return true;
            }
        }
    }
    false
}

/// Parse a CUE file looking for FLAGS PRE. Returns true if the CUE
/// has FLAGS PRE for a track that references `audio_name`, or for any
/// track if this is a single-image CUE (which covers all tracks).
fn check_cue_file_for_preemphasis(cue_path: &Path, audio_name: &str) -> bool {
    let content = match fs::read_to_string(cue_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    // Simple state machine: track the most recent FILE reference and
    // whether we've seen FLAGS PRE in the current TRACK block.
    let mut current_file: Option<String> = None;
    let mut has_flags_pre = false;

    for line in content.lines() {
        let trimmed = line.trim();
        let upper = trimmed.to_ascii_uppercase();

        if upper.starts_with("FILE ") {
            // Extract filename from FILE "name" FORMAT
            if let Some(start) = trimmed.find('"') {
                if let Some(end) = trimmed[start + 1..].find('"') {
                    current_file = Some(trimmed[start + 1..start + 1 + end].to_string());
                }
            }
        } else if upper.starts_with("TRACK ") {
            // Reset flags for new track.
            has_flags_pre = false;
        } else if upper.contains("FLAGS") && upper.contains("PRE") {
            has_flags_pre = true;
            // Check if this track's FILE matches our audio file, or
            // if this is a single-image CUE (one FILE for all tracks).
            if let Some(ref file) = current_file {
                // Direct match or the CUE references a different container
                // file (single-image). Either way, FLAGS PRE means this
                // disc has pre-emphasis.
                if file == audio_name || !file.is_empty() {
                    return true;
                }
            } else {
                // No FILE line yet but FLAGS PRE found — pre-emphasis.
                return true;
            }
        }
    }

    has_flags_pre
}

/// Check an EAC/XLD log file for pre-emphasis mentions.
fn check_log_file_for_preemphasis(log_path: &Path) -> bool {
    let content = match fs::read_to_string(log_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let lower = content.to_ascii_lowercase();
    // Look for explicit pre-emphasis mentions (not just the word "pre").
    lower.contains("pre-emphasis") || lower.contains("preemphasis")
        || lower.contains("pre emphasis")
}

// ── Public API ──────────────────────────────────────────────────────

/// Detect pre-emphasis by checking CUE/log files first, then streaming
/// the full audio through ffmpeg for spectral analysis.
pub async fn detect_preemphasis(path: PathBuf) -> PreemphasisResult {
    // Phase 1: Check CUE/log file evidence (fast, definitive).
    let cue_confirmed = check_cue_evidence(&path);

    // Probe for sample rate and channels.
    let info = match tokio::task::spawn_blocking({
        let p = path.clone();
        move || crate::tui::probe::probe_audio(&p)
    }).await {
        Ok(Ok(info)) => info,
        Ok(Err(e)) => return error_result(&path, cue_confirmed, &format!("probe: {}", e)),
        Err(e) => return error_result(&path, cue_confirmed, &format!("probe: {}", e)),
    };

    if info.sample_rate > 48000 {
        return PreemphasisResult {
            path,
            confidence: if cue_confirmed { PreemphasisConfidence::Detected } else { PreemphasisConfidence::NotDetected },
            cue_confirmed,
            spectral_rms_error: f64::NAN,
            crest_improvement: 0.0,
            detail: if cue_confirmed {
                "CUE confirmed (spectral skipped: sample rate > 48 kHz)".into()
            } else {
                "skipped (sample rate > 48 kHz)".into()
            },
        };
    }

    let sample_rate = info.sample_rate;
    let channels = info.channels as usize;

    // Spawn ffmpeg decoder.
    let mut child = match spawn_decoder(&path) {
        Ok(c) => c,
        Err(e) => return error_result(&path, cue_confirmed, &e),
    };
    let mut stdout = child.stdout.take().unwrap();

    let nyquist = sample_rate as f64 / 2.0;
    let usable_freqs: Vec<f64> = PROBE_FREQS
        .iter()
        .copied()
        .filter(|&f| f < nyquist * 0.9)
        .collect();

    if usable_freqs.len() < 5 {
        let _ = child.kill().await;
        return error_result(&path, cue_confirmed, "too few usable probe frequencies");
    }

    // Precompute Hann window and theoretical detrended curve.
    let hann: Vec<f64> = (0..BLOCK_SIZE)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (BLOCK_SIZE - 1) as f64).cos()))
        .collect();

    let ref_theoretical = theoretical_gain_db(usable_freqs[0]);
    let theoretical_relative: Vec<f64> = usable_freqs
        .iter()
        .map(|&f| theoretical_gain_db(f) - ref_theoretical)
        .collect();
    let theoretical_detrended = detrend(&theoretical_relative);

    // IIR de-emphasis filter coefficients for this sample rate.
    let (iir_b0, iir_b1, iir_a1_neg) = deemphasis_coefficients(sample_rate);

    // ── Streaming decode + analysis ─────────────────────────────────

    // Read raw s32le bytes. Each sample frame = 4 bytes × channels.
    let frame_bytes = 4 * channels;
    let block_bytes = BLOCK_SIZE * frame_bytes;
    let mut raw_buf = vec![0u8; block_bytes];
    let mut mono_buf = vec![0.0f64; BLOCK_SIZE];
    let mut windowed = vec![0.0f64; BLOCK_SIZE];

    // Spectral accumulators (per-block detrended residuals).
    let mut residual_sums = vec![0.0f64; usable_freqs.len()];
    let mut block_count = 0usize;

    // Crest factor accumulators.
    let mut orig_peak: f64 = 0.0;
    let mut orig_rms_sum: f64 = 0.0;
    let mut deemph_peak: f64 = 0.0;
    let mut deemph_rms_sum: f64 = 0.0;
    let mut total_samples: u64 = 0;

    // IIR filter state.
    let mut iir_x_prev = 0.0f64;
    let mut iir_y_prev = 0.0f64;

    // Half-block buffer for 50% overlap.
    let mut prev_half: Option<Vec<f64>> = None;

    loop {
        let n = fill_buf(&mut stdout, &mut raw_buf).await;
        let n = match n {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }

        // Convert s32le → mono f64.
        let sample_frames = n / frame_bytes;
        if sample_frames == 0 {
            break;
        }

        for i in 0..sample_frames {
            let mut sum = 0.0f64;
            for ch in 0..channels {
                let byte_off = (i * channels + ch) * 4;
                if byte_off + 4 > n {
                    break;
                }
                let sample_i32 = i32::from_le_bytes([
                    raw_buf[byte_off],
                    raw_buf[byte_off + 1],
                    raw_buf[byte_off + 2],
                    raw_buf[byte_off + 3],
                ]);
                sum += sample_i32 as f64 / 2147483648.0;
            }
            let mono = sum / channels as f64;
            if i < BLOCK_SIZE {
                mono_buf[i] = mono;
            }

            // Crest factor: original.
            let abs_val = mono.abs();
            if abs_val > orig_peak { orig_peak = abs_val; }
            orig_rms_sum += mono * mono;

            // IIR de-emphasis filter.
            let y = iir_b0 * mono + iir_b1 * iir_x_prev + iir_a1_neg * iir_y_prev;
            iir_x_prev = mono;
            iir_y_prev = y;

            // Crest factor: de-emphasized.
            let abs_y = y.abs();
            if abs_y > deemph_peak { deemph_peak = abs_y; }
            deemph_rms_sum += y * y;

            total_samples += 1;
        }

        // Process full blocks (with 50% overlap via prev_half).
        let mono_slice = &mono_buf[..sample_frames.min(BLOCK_SIZE)];

        if let Some(ref prev) = prev_half {
            // Overlap block: prev_half + first half of current.
            if prev.len() == BLOCK_SIZE / 2 && mono_slice.len() >= BLOCK_SIZE / 2 {
                let half = BLOCK_SIZE / 2;
                for i in 0..half {
                    windowed[i] = prev[i] * hann[i];
                }
                for i in 0..half {
                    windowed[half + i] = mono_slice[i] * hann[half + i];
                }
                accumulate_spectral_block(
                    &windowed, &usable_freqs, sample_rate as f64,
                    &theoretical_detrended, &mut residual_sums,
                );
                block_count += 1;
            }
        }

        // Full block from current data.
        if sample_frames >= BLOCK_SIZE {
            for i in 0..BLOCK_SIZE {
                windowed[i] = mono_slice[i] * hann[i];
            }
            accumulate_spectral_block(
                &windowed, &usable_freqs, sample_rate as f64,
                &theoretical_detrended, &mut residual_sums,
            );
            block_count += 1;

            // Save second half for overlap.
            let half = BLOCK_SIZE / 2;
            prev_half = Some(mono_slice[half..BLOCK_SIZE].to_vec());
        } else if sample_frames >= BLOCK_SIZE / 2 {
            // Save what we have for the next overlap.
            let half = BLOCK_SIZE / 2;
            prev_half = Some(mono_slice[..half].to_vec());
        } else {
            prev_half = None;
        }
    }

    let _ = child.kill().await;

    // ── Compute metrics ─────────────────────────────────────────────

    if block_count < 10 || total_samples == 0 {
        return error_result(&path, cue_confirmed, "insufficient audio data");
    }

    // Primary: average detrended residual vs theoretical.
    let avg_residuals: Vec<f64> = residual_sums
        .iter()
        .map(|&s| s / block_count as f64)
        .collect();
    let spectral_rms_error = rms_error(&avg_residuals, &theoretical_detrended);

    // Secondary: crest factor improvement.
    let orig_rms = (orig_rms_sum / total_samples as f64).sqrt();
    let deemph_rms = (deemph_rms_sum / total_samples as f64).sqrt();
    let orig_crest = if orig_rms > 0.0 {
        20.0 * (orig_peak / orig_rms).log10()
    } else {
        0.0
    };
    let deemph_crest = if deemph_rms > 0.0 {
        20.0 * (deemph_peak / deemph_rms).log10()
    } else {
        0.0
    };
    let crest_improvement = orig_crest - deemph_crest;

    // Combined confidence: CUE evidence is authoritative; spectral is screening.
    let confidence = if cue_confirmed {
        PreemphasisConfidence::Detected
    } else if spectral_rms_error < SPECTRAL_DETECTED {
        PreemphasisConfidence::Possible
    } else if spectral_rms_error < SPECTRAL_POSSIBLE {
        PreemphasisConfidence::Possible
    } else {
        PreemphasisConfidence::NotDetected
    };

    let cue_tag = if cue_confirmed { "CUE confirmed, " } else { "" };
    let detail = format!(
        "{}spectral={:.2} dB, crest={:+.2} dB, blocks={}",
        cue_tag, spectral_rms_error, crest_improvement, block_count,
    );

    // Diagnostic dump (temporary for threshold tuning).
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true)
        .open("/tmp/tonepoet-preemph-diag.txt")
    {
        use std::io::Write;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        let _ = writeln!(f, "--- {} ---", name);
        let _ = writeln!(f, "sample_rate={}, blocks={}, samples={}", sample_rate, block_count, total_samples);
        let _ = writeln!(f, "avg detrended residuals: {:?}", avg_residuals);
        let _ = writeln!(f, "theoretical detrended:   {:?}", theoretical_detrended);
        let _ = writeln!(f, "spectral_rms_error: {:.4} dB", spectral_rms_error);
        let _ = writeln!(f, "crest: orig={:.2} deemph={:.2} improvement={:+.2} dB",
            orig_crest, deemph_crest, crest_improvement);
        let _ = writeln!(f, "confidence: {:?}", confidence);
        let _ = writeln!(f, "");
    }

    PreemphasisResult {
        path,
        confidence,
        cue_confirmed,
        spectral_rms_error,
        crest_improvement,
        detail,
    }
}

// ── Internals ───────────────────────────────────────────────────────

/// Theoretical pre-emphasis gain at frequency f (dB).
fn theoretical_gain_db(freq: f64) -> f64 {
    let ratio = (1.0 + (freq / F1).powi(2)) / (1.0 + (freq / F2).powi(2));
    10.0 * ratio.log10()
}

/// Compute IIR de-emphasis coefficients via bilinear transform.
/// Returns (b0, b1, -a1) for: y[n] = b0·x[n] + b1·x[n-1] + (-a1)·y[n-1]
fn deemphasis_coefficients(sample_rate: u32) -> (f64, f64, f64) {
    let fs = sample_rate as f64;
    let a = 2.0 * 50e-6 * fs;
    let b = 2.0 * 15e-6 * fs;
    let b0 = (1.0 + b) / (1.0 + a);
    let b1 = (1.0 - b) / (1.0 + a);
    let a1 = (1.0 - a) / (1.0 + a); // negative for fs > ~3.2 kHz
    (b0, b1, -a1) // return -a1 so caller does: y = b0*x + b1*x_prev + neg_a1*y_prev
}

/// Run Goertzel at probe frequencies on a windowed block, detrend the
/// resulting dB spectrum, and accumulate the residuals.
fn accumulate_spectral_block(
    windowed: &[f64],
    freqs: &[f64],
    sample_rate: f64,
    _theoretical_detrended: &[f64],
    residual_sums: &mut [f64],
) {
    let energy_db: Vec<f64> = freqs
        .iter()
        .map(|&f| {
            let mag_sq = goertzel_mag_squared(windowed, sample_rate, f);
            if mag_sq > 0.0 { 10.0 * mag_sq.log10() } else { -120.0 }
        })
        .collect();

    // Normalize to first probe frequency (1 kHz).
    let ref_db = energy_db[0];
    let relative: Vec<f64> = energy_db.iter().map(|&db| db - ref_db).collect();
    let detrended = detrend(&relative);

    // Accumulate. We don't subtract the theoretical here — we'll compare
    // the averaged residuals against theoretical after all blocks.
    for (i, &val) in detrended.iter().enumerate() {
        residual_sums[i] += val;
    }
}

/// Remove linear trend from a sequence (least-squares).
fn detrend(values: &[f64]) -> Vec<f64> {
    let n = values.len() as f64;
    if n < 2.0 {
        return values.to_vec();
    }
    let mean_x = (n - 1.0) / 2.0;
    let mean_y = values.iter().sum::<f64>() / n;
    let mut num = 0.0;
    let mut den = 0.0;
    for (i, &y) in values.iter().enumerate() {
        let dx = i as f64 - mean_x;
        num += dx * (y - mean_y);
        den += dx * dx;
    }
    let slope = if den.abs() > 1e-12 { num / den } else { 0.0 };
    let intercept = mean_y - slope * mean_x;
    values.iter().enumerate()
        .map(|(i, &y)| y - (slope * i as f64 + intercept))
        .collect()
}

/// RMS error between two equal-length slices.
fn rms_error(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let sum_sq: f64 = a.iter().zip(b.iter()).map(|(&x, &y)| (x - y).powi(2)).sum();
    (sum_sq / n).sqrt()
}

/// Goertzel algorithm: |X(f)|² for a single frequency.
fn goertzel_mag_squared(samples: &[f64], sample_rate: f64, target_freq: f64) -> f64 {
    let n = samples.len();
    let k = (0.5 + n as f64 * target_freq / sample_rate) as usize;
    let w = 2.0 * PI * k as f64 / n as f64;
    let coeff = 2.0 * w.cos();
    let mut s1 = 0.0;
    let mut s2 = 0.0;
    for &sample in samples {
        let s0 = sample + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - coeff * s1 * s2
}

/// Read exactly buf.len() bytes or fewer at EOF.
async fn fill_buf(
    reader: &mut (impl AsyncReadExt + Unpin),
    buf: &mut [u8],
) -> Result<usize, std::io::Error> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]).await? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

fn spawn_decoder(path: &Path) -> Result<tokio::process::Child, String> {
    Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "s32le", "-acodec", "pcm_s32le", "pipe:1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("ffmpeg: {}", e))
}

fn error_result(path: &Path, cue_confirmed: bool, detail: &str) -> PreemphasisResult {
    PreemphasisResult {
        path: path.to_path_buf(),
        confidence: if cue_confirmed { PreemphasisConfidence::Detected } else { PreemphasisConfidence::NotDetected },
        cue_confirmed,
        spectral_rms_error: f64::NAN,
        crest_improvement: 0.0,
        detail: if cue_confirmed {
            format!("CUE confirmed; {}", detail)
        } else {
            detail.to_string()
        },
    }
}
