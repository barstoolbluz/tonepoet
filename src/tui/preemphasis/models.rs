//! Pre-emphasis detection via protected-template whitened matched filter.
//!
//! Architecture (from reasoning model analysis):
//!   1. Protect the PE direction — do NOT orthogonalize it against a rich
//!      nuisance basis that could absorb the signal.
//!   2. Use an empirical PE template s_emp estimated from paired PE↔deemphasized
//!      files through the actual feature pipeline.
//!   3. Score with a whitened matched filter (GLS-style projection):
//!      z(x) = s^T Σ^{-1} (x - μ) / sqrt(s^T Σ^{-1} s)
//!   4. Nuisance handling: only intercept + tilt, projected into s⊥.
//!      The covariance Σ handles ordinary non-PE variability.
//!   5. Album pooling for the final "possible PE" flag.

use super::corpus::CorpusModel;
use super::frame_select::SelectedFrames;
use super::iir;
use super::stft::NUM_BANDS;

/// Result of the matched-filter scoring.
#[derive(Debug, Clone)]
pub struct ModelScores {
    /// Whitened matched-filter score (z-score along PE template).
    /// Positive and large = PE-like spectral shape after nuisance removal.
    pub z_score: f64,
    /// Raw projection onto PE template (before whitening).
    pub alpha: f64,
    /// Correlation between nuisance-residual and PE template.
    pub pe_correlation: f64,
    /// Alpha stability: std dev of per-frame z-scores.
    pub alpha_stability: f64,
    // Legacy fields for API compatibility.
    pub ll_m0: f64,
    pub ll_m1: f64,
    pub ll_m2: f64,
    pub llr: f64,
    pub best_m1_idx: usize,
}

/// The theoretical Red Book pre-emphasis curve at 1/3-octave band centers.
pub fn pe_curve() -> [f64; NUM_BANDS] {
    let centers = super::stft::band_centers();
    let mut curve = [0.0; NUM_BANDS];
    for (k, &fc) in centers.iter().enumerate() {
        curve[k] = iir::theoretical_gain_db(fc);
    }
    curve
}

/// Compute which bands are usable (below Nyquist) for a given sample rate.
pub fn usable_band_mask(sample_rate: u32) -> [bool; NUM_BANDS] {
    let nyquist = sample_rate as f64 / 2.0;
    let centers = super::stft::band_centers();
    let mut mask = [false; NUM_BANDS];
    for k in 0..NUM_BANDS {
        mask[k] = centers[k] < nyquist * 0.9;
    }
    mask
}

/// Extract only usable bands from a full-size array.
fn masked(values: &[f64; NUM_BANDS], mask: &[bool; NUM_BANDS]) -> Vec<f64> {
    values
        .iter()
        .zip(mask.iter())
        .filter(|(_, &m)| m)
        .map(|(&v, _)| v)
        .collect()
}

// ── Main scoring function ──────────────────────────────────────────

/// Score a track using the whitened matched filter.
///
/// Steps:
/// 1. Compute median quiet-frame spectrum
/// 2. Subtract corpus mean
/// 3. Remove intercept + tilt (minimal nuisance, protected PE direction)
/// 4. Project onto whitened PE template: z = s^T Σ^{-1} r / sqrt(s^T Σ^{-1} s)
pub fn score_models(
    selected: &SelectedFrames,
    stft: &super::stft::StftResult,
    corpus: &CorpusModel,
) -> ModelScores {
    let mask = usable_band_mask(stft.sample_rate);
    let n_bands = mask.iter().filter(|&&m| m).count();

    // Compute median spectrum.
    let median_spectrum = compute_median_spectrum(selected, stft);

    // Subtract corpus mean.
    let mut diff = [0.0; NUM_BANDS];
    for k in 0..NUM_BANDS {
        diff[k] = median_spectrum[k] - corpus.mean[k];
    }
    let data = masked(&diff, &mask);

    // Remove intercept + tilt only (minimal nuisance).
    let residual = remove_intercept_tilt(&data);

    // Get the PE template (empirical if available, else theoretical).
    let pe_template = get_pe_template(corpus, &mask);

    // Also remove intercept + tilt from the template so they're in the same space.
    let pe_residual = remove_intercept_tilt(&pe_template);

    // Whitened matched filter: z = s^T Σ^{-1} r / sqrt(s^T Σ^{-1} s)
    // where Σ is the non-PE corpus covariance (masked and in residual space).
    let cov_masked = mask_covariance(&corpus.covariance, &mask);
    let z_score = whitened_matched_filter(&residual, &pe_residual, &cov_masked, n_bands);

    // Raw (unwhitened) projection for alpha.
    let alpha = dot(&residual, &pe_residual) / dot(&pe_residual, &pe_residual).max(1e-10);

    // PE correlation.
    let pe_correlation = pearson_corr(&residual, &pe_residual);

    // Alpha stability from per-frame scores.
    let alpha_stability =
        compute_z_stability(selected, stft, corpus, &mask, &pe_residual, &cov_masked);

    ModelScores {
        z_score,
        alpha,
        pe_correlation,
        alpha_stability,
        // Legacy.
        ll_m0: 0.0,
        ll_m1: 0.0,
        ll_m2: 0.0,
        llr: z_score, // Map z_score to llr for compatibility.
        best_m1_idx: 0,
    }
}

// ── Whitened matched filter ────────────────────────────────────────

/// Compute the whitened matched-filter score.
///
/// z = s^T Σ^{-1} r / sqrt(s^T Σ^{-1} s)
///
/// Uses regularized pseudo-inverse via eigendecomposition for stability.
fn whitened_matched_filter(
    residual: &[f64],
    template: &[f64],
    covariance: &[f64], // n×n row-major
    n: usize,
) -> f64 {
    // Compute Σ^{-1} s via solving the linear system.
    // For a small matrix (24×24), direct regularized inverse is fine.
    let sigma_inv_s = solve_regularized(covariance, template, n);
    let sigma_inv_r = solve_regularized(covariance, residual, n);

    let s_sigma_inv_r: f64 = template
        .iter()
        .zip(sigma_inv_r.iter())
        .map(|(&a, &b)| a * b)
        .sum();
    let s_sigma_inv_s: f64 = template
        .iter()
        .zip(sigma_inv_s.iter())
        .map(|(&a, &b)| a * b)
        .sum();

    if s_sigma_inv_s > 1e-10 {
        s_sigma_inv_r / s_sigma_inv_s.sqrt()
    } else {
        0.0
    }
}

/// Solve Σx = b via regularized Cholesky-like approach.
/// Adds a small ridge to the diagonal for numerical stability.
fn solve_regularized(covariance: &[f64], rhs: &[f64], n: usize) -> Vec<f64> {
    // Add ridge regularization: Σ_reg = Σ + λI
    let trace: f64 = (0..n).map(|i| covariance[i * n + i]).sum();
    let lambda = trace / n as f64 * 0.01; // 1% of mean variance

    let mut reg = covariance.to_vec();
    for i in 0..n {
        reg[i * n + i] += lambda;
    }

    // Solve via Gauss-Jordan elimination (fine for n ≈ 24).
    gauss_solve(&reg, rhs, n)
}

/// Gauss-Jordan elimination for Ax = b.
fn gauss_solve(matrix: &[f64], rhs: &[f64], n: usize) -> Vec<f64> {
    // Augmented matrix [A | b].
    let mut aug = vec![0.0; n * (n + 1)];
    for i in 0..n {
        for j in 0..n {
            aug[i * (n + 1) + j] = matrix[i * n + j];
        }
        aug[i * (n + 1) + n] = rhs[i];
    }

    // Forward elimination with partial pivoting.
    for col in 0..n {
        // Find pivot.
        let mut max_row = col;
        let mut max_val = aug[col * (n + 1) + col].abs();
        for row in (col + 1)..n {
            let val = aug[row * (n + 1) + col].abs();
            if val > max_val {
                max_val = val;
                max_row = row;
            }
        }
        if max_val < 1e-15 {
            continue;
        } // Skip singular column.

        // Swap rows.
        if max_row != col {
            for j in 0..=n {
                let tmp = aug[col * (n + 1) + j];
                aug[col * (n + 1) + j] = aug[max_row * (n + 1) + j];
                aug[max_row * (n + 1) + j] = tmp;
            }
        }

        // Eliminate below.
        let pivot = aug[col * (n + 1) + col];
        for row in (col + 1)..n {
            let factor = aug[row * (n + 1) + col] / pivot;
            for j in col..=n {
                aug[row * (n + 1) + j] -= factor * aug[col * (n + 1) + j];
            }
        }
    }

    // Back substitution.
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let mut sum = aug[i * (n + 1) + n];
        for j in (i + 1)..n {
            sum -= aug[i * (n + 1) + j] * x[j];
        }
        let diag = aug[i * (n + 1) + i];
        x[i] = if diag.abs() > 1e-15 { sum / diag } else { 0.0 };
    }
    x
}

// ── Nuisance removal (intercept + tilt only) ──────────────────────

/// Remove intercept and linear tilt from a vector.
/// This is the ONLY nuisance removal — no shelf dictionary, no PCA.
/// The covariance handles ordinary spectral variation.
fn remove_intercept_tilt(data: &[f64]) -> Vec<f64> {
    let n = data.len() as f64;
    if n < 2.0 {
        return data.to_vec();
    }

    // Fit y = a + b*x via least squares.
    let mean_x = (n - 1.0) / 2.0;
    let mean_y = data.iter().sum::<f64>() / n;
    let mut num = 0.0;
    let mut den = 0.0;
    for (i, &y) in data.iter().enumerate() {
        let dx = i as f64 - mean_x;
        num += dx * (y - mean_y);
        den += dx * dx;
    }
    let slope = if den.abs() > 1e-12 { num / den } else { 0.0 };
    let intercept = mean_y - slope * mean_x;

    data.iter()
        .enumerate()
        .map(|(i, &y)| y - slope * i as f64 - intercept)
        .collect()
}

// ── PE template ────────────────────────────────────────────────────

/// Get the PE template: empirical if stored in corpus, else theoretical.
fn get_pe_template(corpus: &CorpusModel, mask: &[bool; NUM_BANDS]) -> Vec<f64> {
    if let Some(ref emp) = corpus.empirical_pe_template {
        masked(emp, mask)
    } else {
        // Fallback: theoretical PE curve.
        let pe = pe_curve();
        masked(&pe, mask)
    }
}

// ── Covariance masking ─────────────────────────────────────────────

/// Extract the submatrix of covariance for usable bands only.
fn mask_covariance(covariance: &[f64], mask: &[bool; NUM_BANDS]) -> Vec<f64> {
    let indices: Vec<usize> = mask
        .iter()
        .enumerate()
        .filter(|(_, &m)| m)
        .map(|(i, _)| i)
        .collect();
    let n = indices.len();
    let mut sub = vec![0.0; n * n];
    for (si, &i) in indices.iter().enumerate() {
        for (sj, &j) in indices.iter().enumerate() {
            let idx = i * NUM_BANDS + j;
            sub[si * n + sj] = covariance.get(idx).copied().unwrap_or(0.0);
        }
    }
    sub
}

// ── Per-frame stability ────────────────────────────────────────────

/// Compute z-score stability across individual frames.
fn compute_z_stability(
    selected: &SelectedFrames,
    stft: &super::stft::StftResult,
    corpus: &CorpusModel,
    mask: &[bool; NUM_BANDS],
    pe_template: &[f64],
    covariance: &[f64],
) -> f64 {
    if selected.frames.len() < 10 {
        return f64::NAN;
    }

    let n_bands = mask.iter().filter(|&&m| m).count();
    let step = (selected.frames.len() / 30).max(1);
    let mut z_scores = Vec::new();

    for &idx in selected.frames.iter().step_by(step).take(30) {
        let spectrum = &stft.band_spectra[idx];
        let mut diff = [0.0; NUM_BANDS];
        for k in 0..NUM_BANDS {
            diff[k] = spectrum[k] - corpus.mean[k];
        }
        let data = masked(&diff, mask);
        let residual = remove_intercept_tilt(&data);

        let z = whitened_matched_filter(&residual, pe_template, covariance, n_bands);
        z_scores.push(z);
    }

    if z_scores.is_empty() {
        return f64::NAN;
    }
    let mean = z_scores.iter().sum::<f64>() / z_scores.len() as f64;
    let var = z_scores.iter().map(|&z| (z - mean).powi(2)).sum::<f64>() / z_scores.len() as f64;
    var.sqrt()
}

// ── Utilities ──────────────────────────────────────────────────────

fn compute_median_spectrum(
    selected: &SelectedFrames,
    stft: &super::stft::StftResult,
) -> [f64; NUM_BANDS] {
    let n = selected.frames.len();
    let mut median = [0.0; NUM_BANDS];
    if n == 0 {
        return median;
    }

    for k in 0..NUM_BANDS {
        let mut values: Vec<f64> = selected
            .frames
            .iter()
            .map(|&idx| stft.band_spectra[idx][k])
            .collect();
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        median[k] = if n % 2 == 1 {
            values[n / 2]
        } else {
            (values[n / 2 - 1] + values[n / 2]) / 2.0
        };
    }
    median
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b.iter()).map(|(&x, &y)| x * y).sum()
}

fn pearson_corr(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let mx = x.iter().sum::<f64>() / n;
    let my = y.iter().sum::<f64>() / n;
    let mut num = 0.0;
    let mut dx2 = 0.0;
    let mut dy2 = 0.0;
    for (xi, yi) in x.iter().zip(y.iter()) {
        let dx = xi - mx;
        let dy = yi - my;
        num += dx * dy;
        dx2 += dx * dx;
        dy2 += dy * dy;
    }
    if dx2 * dy2 > 0.0 {
        num / (dx2 * dy2).sqrt()
    } else {
        0.0
    }
}

// ── Multi-summary per-frame alpha computation ──────────────────────

/// Multi-alpha summary for a single track, capturing both median and
/// upper-quantile PE evidence from quiet and all frames.
#[derive(Debug, Clone)]
pub struct TrackMultiAlpha {
    /// Median alpha from quiet frames (current default pipeline).
    pub quiet_median: f64,
    /// 75th percentile alpha from quiet frames (captures sparse PE).
    pub quiet_p75: f64,
    /// Median alpha from all frames.
    pub all_median: f64,
    /// 75th percentile alpha from all frames.
    pub all_p75: f64,
    /// Fraction of quiet frames with positive alpha projection.
    pub fraction_positive_quiet: f64,
}

/// Compute multi-alpha summary for a track.
///
/// Projects each frame (both quiet-selected and all) onto the PE template
/// after intercept + tilt removal, then computes distributional summaries.
pub fn compute_multi_alpha(
    stft_result: &super::stft::StftResult,
    selected: &SelectedFrames,
    corpus: &CorpusModel,
) -> TrackMultiAlpha {
    let mask = usable_band_mask(stft_result.sample_rate);
    let pe_template = get_pe_template(corpus, &mask);

    // Quiet-frame alphas.
    let quiet_alphas = per_frame_alphas(&selected.frames, stft_result, corpus, &mask, &pe_template);
    let quiet_stats = alpha_distribution_stats(&quiet_alphas);

    // All-frame alphas.
    let all_indices: Vec<usize> = (0..stft_result.band_spectra.len()).collect();
    let all_alphas = per_frame_alphas(&all_indices, stft_result, corpus, &mask, &pe_template);
    let all_stats = alpha_distribution_stats(&all_alphas);

    TrackMultiAlpha {
        quiet_median: quiet_stats.0,
        quiet_p75: quiet_stats.1,
        all_median: all_stats.0,
        all_p75: all_stats.1,
        fraction_positive_quiet: quiet_stats.2,
    }
}

/// Compute per-frame alpha (projection onto PE template after intercept+tilt removal).
fn per_frame_alphas(
    frame_indices: &[usize],
    stft_result: &super::stft::StftResult,
    corpus: &CorpusModel,
    mask: &[bool; NUM_BANDS],
    pe_template: &[f64],
) -> Vec<f64> {
    let mut alphas = Vec::with_capacity(frame_indices.len());

    for &idx in frame_indices {
        if idx >= stft_result.band_spectra.len() {
            continue;
        }
        let spectrum = &stft_result.band_spectra[idx];

        // Subtract corpus mean, mask to usable bands.
        let mut diff = [0.0; NUM_BANDS];
        for k in 0..NUM_BANDS {
            diff[k] = spectrum[k] - corpus.mean[k];
        }
        let data: Vec<f64> = diff
            .iter()
            .zip(mask.iter())
            .filter(|(_, &m)| m)
            .map(|(&v, _)| v)
            .collect();

        // Remove intercept + tilt.
        let residual = remove_intercept_tilt(&data);

        // Project onto PE template.
        let dot_rp: f64 = residual
            .iter()
            .zip(pe_template.iter())
            .map(|(&r, &p)| r * p)
            .sum();
        let dot_pp: f64 = pe_template.iter().map(|&p| p * p).sum();
        let alpha = if dot_pp > 1e-10 { dot_rp / dot_pp } else { 0.0 };
        alphas.push(alpha);
    }

    alphas
}

/// Compute (median, p75, fraction_positive) from a vector of alpha values.
fn alpha_distribution_stats(alphas: &[f64]) -> (f64, f64, f64) {
    if alphas.is_empty() {
        return (0.0, 0.0, 0.0);
    }

    let mut sorted = alphas.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();

    let median = if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    };
    let p75 = sorted[3 * n / 4];
    let frac_pos = alphas.iter().filter(|&&a| a > 0.0).count() as f64 / n as f64;

    (median, p75, frac_pos)
}

// ── Track-level shape features ─────────────────────────────────────

/// Number of track-level shape features for the PE classifier.
pub const NUM_SHAPE_FEATURES: usize = 6;

/// Track-level distribution-shape features that capture the within-track
/// relationship between median and upper-tail alpha. The key insight:
/// PE tracks show "modest/negative median + positive upper tail" (intermittent PE),
/// while non-PE tracks shift both together.
#[derive(Debug, Clone)]
pub struct TrackShapeFeatures {
    /// Features array for classifier input.
    pub features: [f64; NUM_SHAPE_FEATURES],
}

impl TrackShapeFeatures {
    /// Construct shape features from multi-alpha summary + model scores.
    pub fn new(multi_alpha: &TrackMultiAlpha, pe_correlation: f64, deemph_delta: f64) -> Self {
        let q50 = multi_alpha.quiet_median;
        let q75 = multi_alpha.quiet_p75;
        let spread = q75 - q50; // Key signal: large spread = intermittent PE.
        let frac_pos = multi_alpha.fraction_positive_quiet;

        Self {
            features: [q50, q75, spread, frac_pos, pe_correlation, deemph_delta],
        }
    }

    pub fn q50(&self) -> f64 {
        self.features[0]
    }
    pub fn q75(&self) -> f64 {
        self.features[1]
    }
    pub fn spread(&self) -> f64 {
        self.features[2]
    }
    pub fn frac_pos(&self) -> f64 {
        self.features[3]
    }
    pub fn pe_correlation(&self) -> f64 {
        self.features[4]
    }
    pub fn deemph_delta(&self) -> f64 {
        self.features[5]
    }
}

// get_pe_template is defined above in the matched-filter section.
