//! Diagnostic output for pre-emphasis detection threshold tuning.
//!
//! Appends per-file analysis details to /tmp/tonepoet-preemph-diag.txt.

use std::io::Write;
use std::path::Path;

const DIAG_PATH: &str = "/tmp/tonepoet-preemph-diag.txt";

/// Write a diagnostic entry for a single file analysis.
pub fn write_diag(
    path: &Path,
    sample_rate: u32,
    frames_total: usize,
    frames_scored: usize,
    ll_m0: f64,
    ll_m1: f64,
    ll_m2: f64,
    llr: f64,
    alpha: f64,
    delta_d: f64,
    gates: &[String],
    confidence: &str,
) {
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true)
        .open(DIAG_PATH) else { return };

    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    let _ = writeln!(f, "--- {} ---", name);
    let _ = writeln!(f, "sample_rate={}, frames_total={}, frames_scored={}", sample_rate, frames_total, frames_scored);
    let _ = writeln!(f, "LL_M0={:.4}, LL_M1={:.4}, LL_M2={:.4}", ll_m0, ll_m1, ll_m2);
    let _ = writeln!(f, "LLR={:.4}, alpha={:.4}, delta_d={:.4}", llr, alpha, delta_d);
    let _ = writeln!(f, "gates: [{}]", gates.join(", "));
    let _ = writeln!(f, "confidence: {}", confidence);
    let _ = writeln!(f, "");
}

/// Write a diagnostic entry for album-level pooling.
pub fn write_album_diag(
    album_path: &Path,
    album_score: f64,
    alpha_spread: f64,
    support_count: usize,
    total_tracks: usize,
) {
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true)
        .open(DIAG_PATH) else { return };

    let name = album_path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    let _ = writeln!(f, "=== ALBUM: {} ===", name);
    let _ = writeln!(f, "album_score={:.4}, alpha_spread={:.4}", album_score, alpha_spread);
    let _ = writeln!(f, "support: {}/{} tracks", support_count, total_tracks);
    let _ = writeln!(f, "");
}
