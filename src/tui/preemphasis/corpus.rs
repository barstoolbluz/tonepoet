//! Non-PE corpus model: training, storage, and loading.
//!
//! The corpus model captures the statistical distribution of "normal"
//! (non-pre-emphasized) CD spectral envelopes. It stores:
//! - Mean spectrum (31 bands)
//! - Shrinkage covariance matrix (31x31, Ledoit-Wolf regularized)
//! - PCA components (top eigenvectors of shrinkage covariance)
//!
//! Trained via `:preemph-train` command from the user's library.
//! Stored in the tonepoet SQLite database.
//!
//! ## Tuning guidance (from reasoning model analysis)
//!
//! - Corpus unit is tracks/albums, not frames (frames from same track correlate)
//! - Shrinkage covariance (Ledoit-Wolf) required for small-sample stability
//! - PCA component count: search k ∈ {0..6}, select via grouped held-out CV
//! - Minimum corpus: 50-100 tracks for first usable model, 100+ for calibration
//! - Validate with grouped splits by track/album, never random frame splits

use super::stft::NUM_BANDS;

/// Default PCA components — starting value, must be tuned via held-out CV.
/// Search k ∈ {0,1,2,3,4,5,6}; expected winner is 2-4.
const DEFAULT_PCA_COMPONENTS: usize = 3;

/// Minimum tracks needed for a usable corpus.
const MIN_TRACKS: u64 = 30;

/// Loaded corpus model for scoring.
#[derive(Debug, Clone)]
pub struct CorpusModel {
    /// Mean spectrum across all quiet frames from non-PE tracks (31 bands, dB).
    pub mean: [f64; NUM_BANDS],
    /// Shrinkage covariance matrix (31x31, row-major, Ledoit-Wolf regularized).
    pub covariance: Vec<f64>,
    /// Top PCA components (eigenvectors of covariance, unit-normalized).
    pub pca_components: Vec<[f64; NUM_BANDS]>,
    /// Empirical PE template: mean(x_PE - x_deemph) from paired files.
    /// If None, falls back to theoretical PE curve.
    pub empirical_pe_template: Option<[f64; NUM_BANDS]>,
    /// Number of frames used to build the model.
    pub n_frames: u64,
    /// Number of tracks sampled.
    pub n_tracks: u64,
}

impl CorpusModel {
    /// Compute Mahalanobis distance of a spectrum to the corpus distribution.
    /// Uses PCA-based approximation with regularized eigenvalues.
    pub fn mahalanobis_distance(&self, spectrum: &[f64; NUM_BANDS]) -> f64 {
        // Centered spectrum.
        let mut centered = [0.0; NUM_BANDS];
        for k in 0..NUM_BANDS {
            centered[k] = spectrum[k] - self.mean[k];
        }

        // Approximate Mahalanobis via PCA projection:
        // d² ≈ Σ (proj_i² / λ_i) + residual_norm² / λ_residual
        let mut d_sq = 0.0;
        let mut residual = centered;

        for pc in &self.pca_components {
            let proj: f64 = centered.iter().zip(pc.iter()).map(|(&c, &p)| c * p).sum();
            // Eigenvalue estimate: pc^T * Cov * pc (Rayleigh quotient).
            let cov_pc = mat_vec_dot_safe(&self.covariance, pc);
            let eigenval: f64 = pc.iter().zip(cov_pc.iter()).map(|(&p, &cp)| p * cp).sum();
            let eigenval = eigenval.max(1e-6);
            d_sq += proj * proj / eigenval;
            for k in 0..NUM_BANDS {
                residual[k] -= proj * pc[k];
            }
        }

        // Residual variance (average of diagonal covariance as proxy).
        let residual_var: f64 = (0..NUM_BANDS)
            .filter_map(|k| self.covariance.get(k * NUM_BANDS + k).copied())
            .sum::<f64>()
            / NUM_BANDS as f64;
        let residual_norm_sq: f64 = residual.iter().map(|&r| r * r).sum();
        d_sq += residual_norm_sq / residual_var.max(1e-6);

        d_sq.sqrt()
    }
}

/// Load the corpus model from the tonepoet database.
pub fn load_corpus() -> Result<CorpusModel, String> {
    let db = crate::db::Database::open().map_err(|e| format!("db open: {}", e))?;

    let model = db.load_preemph_corpus()?;

    if model.n_tracks < MIN_TRACKS {
        return Err(format!(
            "corpus has {} tracks (need {}+); run :preemph-train on more files",
            model.n_tracks, MIN_TRACKS
        ));
    }

    Ok(model)
}

/// Training accumulator using Welford's online algorithm.
pub struct CorpusTrainer {
    n: u64,
    mean: [f64; NUM_BANDS],
    m2: Vec<f64>, // Running sum of (x - mean_old)(x - mean_new), flattened 31x31.
    n_tracks: u64,
    num_pca: usize,
}

impl CorpusTrainer {
    pub fn new() -> Self {
        Self {
            n: 0,
            mean: [0.0; NUM_BANDS],
            m2: vec![0.0; NUM_BANDS * NUM_BANDS],
            n_tracks: 0,
            num_pca: DEFAULT_PCA_COMPONENTS,
        }
    }

    /// Set the number of PCA components to extract (for tuning via CV).
    pub fn set_num_pca(&mut self, k: usize) {
        self.num_pca = k.min(NUM_BANDS - 1);
    }

    /// Increment track count.
    pub fn add_track(&mut self) {
        self.n_tracks += 1;
    }

    /// Add a single frame (31-band spectrum) to the running statistics.
    pub fn add_frame(&mut self, spectrum: &[f64; NUM_BANDS]) {
        self.n += 1;
        let n = self.n as f64;

        let mut delta = [0.0; NUM_BANDS];
        for k in 0..NUM_BANDS {
            delta[k] = spectrum[k] - self.mean[k];
            self.mean[k] += delta[k] / n;
        }

        // Update covariance running sums (outer product of delta and delta2).
        for i in 0..NUM_BANDS {
            let delta2_i = spectrum[i] - self.mean[i]; // delta from new mean
            for j in 0..NUM_BANDS {
                self.m2[i * NUM_BANDS + j] += delta[j] * delta2_i;
            }
        }
    }

    /// Finalize and produce the corpus model with Ledoit-Wolf shrinkage.
    pub fn finalize(self) -> Result<CorpusModel, String> {
        if self.n < 100 {
            return Err(format!("too few frames ({}) for stable corpus", self.n));
        }
        if self.n_tracks < MIN_TRACKS {
            return Err(format!(
                "too few tracks ({}) — need at least {} for stable corpus",
                self.n_tracks, MIN_TRACKS
            ));
        }

        let n = self.n as f64;

        // Sample covariance.
        let mut sample_cov = self.m2.clone();
        for v in sample_cov.iter_mut() {
            *v /= n - 1.0;
        }

        // Apply Ledoit-Wolf shrinkage: Σ_shrunk = (1-α)·S + α·μ·I
        // where μ = trace(S)/p and α is the optimal shrinkage intensity.
        let covariance = ledoit_wolf_shrinkage(&sample_cov, self.n);

        // Compute PCA via power iteration on shrinkage covariance.
        let pca_components = compute_pca(&covariance, self.num_pca);

        Ok(CorpusModel {
            mean: self.mean,
            covariance,
            pca_components,
            empirical_pe_template: None, // Set later via train_empirical_template().
            n_frames: self.n,
            n_tracks: self.n_tracks,
        })
    }

    /// Get current frame count.
    pub fn frame_count(&self) -> u64 {
        self.n
    }

    /// Get current track count.
    pub fn track_count(&self) -> u64 {
        self.n_tracks
    }
}

// ── Ledoit-Wolf shrinkage covariance ───────────────────────────────

/// Compute Ledoit-Wolf shrinkage covariance estimate.
///
/// Shrinks the sample covariance toward a scaled identity matrix:
///   Σ_shrunk = (1 - alpha) * S + alpha * mu * I
///
/// where mu = trace(S)/p and alpha is the optimal shrinkage intensity
/// estimated analytically (Oracle Approximating Shrinkage).
fn ledoit_wolf_shrinkage(sample_cov: &[f64], n_samples: u64) -> Vec<f64> {
    let p = NUM_BANDS;
    let n = n_samples as f64;

    // Target: scaled identity with mu = trace(S) / p.
    let trace: f64 = (0..p).map(|i| sample_cov[i * p + i]).sum();
    let mu = trace / p as f64;

    // Frobenius norm of (S - mu*I).
    let mut delta_frob_sq = 0.0;
    for i in 0..p {
        for j in 0..p {
            let s_ij = sample_cov[i * p + j];
            let target = if i == j { mu } else { 0.0 };
            delta_frob_sq += (s_ij - target).powi(2);
        }
    }

    // Simplified OAS shrinkage intensity estimate.
    // alpha = min(1, (delta_frob_sq * (n-1)) / (n * (trace_sq - trace²/p + delta_frob_sq)))
    // Simplified form: alpha ≈ min(1, delta_frob_sq / (n * delta_frob_sq + trace² - trace²/p))
    // We use the simpler Ledoit-Wolf (2004) formula:
    let trace_sq: f64 = (0..p)
        .flat_map(|i| (0..p).map(move |j| sample_cov[i * p + j] * sample_cov[j * p + i]))
        .sum();

    let numerator = delta_frob_sq;
    let denominator = (n + 1.0 - 2.0 / p as f64) * (trace_sq - trace * trace / p as f64);

    let alpha = if denominator > 1e-10 {
        (numerator / denominator).min(1.0).max(0.0)
    } else {
        1.0 // Full shrinkage if denominator is degenerate.
    };

    // Apply shrinkage.
    let mut shrunk = sample_cov.to_vec();
    for i in 0..p {
        for j in 0..p {
            let target = if i == j { mu } else { 0.0 };
            shrunk[i * p + j] = (1.0 - alpha) * sample_cov[i * p + j] + alpha * target;
        }
    }

    shrunk
}

// ── PCA via power iteration ────────────────────────────────────────

/// Compute top-k principal components via deflated power iteration.
fn compute_pca(covariance: &[f64], k: usize) -> Vec<[f64; NUM_BANDS]> {
    let mut components = Vec::with_capacity(k);
    let mut deflated = covariance.to_vec();

    for _ in 0..k {
        let pc = power_iteration(&deflated, 200);

        // Check convergence: if the vector is near-zero, stop extracting.
        let norm: f64 = pc.iter().map(|&x| x * x).sum::<f64>().sqrt();
        if norm < 0.5 {
            break;
        } // Degenerate eigenvector, stop.

        // Compute eigenvalue: lambda = pc^T * C * pc.
        let cpc = mat_vec_dot(&deflated, &pc);
        let lambda: f64 = pc.iter().zip(cpc.iter()).map(|(&p, &cp)| p * cp).sum();

        // Only keep components with meaningful eigenvalue.
        if lambda < 1e-4 {
            break;
        }

        // Deflate: C = C - lambda * pc * pc^T.
        for i in 0..NUM_BANDS {
            for j in 0..NUM_BANDS {
                deflated[i * NUM_BANDS + j] -= lambda * pc[i] * pc[j];
            }
        }
        components.push(pc);
    }

    components
}

/// Power iteration to find dominant eigenvector of a symmetric matrix.
fn power_iteration(matrix: &[f64], max_iter: usize) -> [f64; NUM_BANDS] {
    let mut v = [0.0; NUM_BANDS];
    // Initialize with uniform vector.
    let init = 1.0 / (NUM_BANDS as f64).sqrt();
    for x in v.iter_mut() {
        *x = init;
    }

    for _ in 0..max_iter {
        let mv = mat_vec_dot(matrix, &v);
        let norm: f64 = mv.iter().map(|&x| x * x).sum::<f64>().sqrt();
        if norm < 1e-15 {
            break;
        }
        for k in 0..NUM_BANDS {
            v[k] = mv[k] / norm;
        }
    }
    v
}

/// Matrix-vector multiply for NUM_BANDS x NUM_BANDS row-major matrix.
fn mat_vec_dot(matrix: &[f64], vec: &[f64; NUM_BANDS]) -> [f64; NUM_BANDS] {
    let mut result = [0.0; NUM_BANDS];
    for i in 0..NUM_BANDS {
        for j in 0..NUM_BANDS {
            result[i] += matrix[i * NUM_BANDS + j] * vec[j];
        }
    }
    result
}

/// Bounds-safe matrix-vector multiply (for corpus loaded from DB).
fn mat_vec_dot_safe(matrix: &[f64], vec: &[f64; NUM_BANDS]) -> [f64; NUM_BANDS] {
    let mut result = [0.0; NUM_BANDS];
    for i in 0..NUM_BANDS {
        for j in 0..NUM_BANDS {
            let idx = i * NUM_BANDS + j;
            if let Some(&m) = matrix.get(idx) {
                result[i] += m * vec[j];
            }
        }
    }
    result
}

/// Save corpus model to database.
pub fn save_corpus(model: &CorpusModel) -> Result<(), String> {
    let db = crate::db::Database::open().map_err(|e| format!("db open: {}", e))?;

    db.store_preemph_corpus(model)
}

// ── Corpus training from directory ─────────────────────────────────

/// Train a corpus model from a directory of non-PE audio files.
///
/// Walks the directory recursively, skips files with PE indicators,
/// decodes a 30-second segment from each qualifying track, computes
/// STFT, selects quiet frames, and accumulates into the model.
///
/// All blocking I/O (ffmpeg decode, STFT computation) runs inside
/// `spawn_blocking` to avoid stalling the tokio runtime.
pub async fn train_corpus_from_dir(dir: &std::path::Path) -> Result<CorpusModel, String> {
    let dir = dir.to_path_buf();

    tokio::task::spawn_blocking(move || train_corpus_blocking(&dir))
        .await
        .map_err(|e| format!("training task panicked: {}", e))?
}

/// Synchronous corpus training (runs inside spawn_blocking).
fn train_corpus_blocking(dir: &std::path::Path) -> Result<CorpusModel, String> {
    use walkdir::WalkDir;

    // Collect audio files.
    let audio_extensions = ["flac", "wav", "aiff", "aif", "wv", "ape", "dsf", "dff"];
    let mut audio_files: Vec<std::path::PathBuf> = Vec::new();

    for entry in WalkDir::new(dir).follow_links(true).into_iter().flatten() {
        let path = entry.path().to_path_buf();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if audio_extensions.contains(&ext.as_str()) {
            audio_files.push(path);
        }
    }

    if audio_files.is_empty() {
        return Err(format!("no audio files found in {}", dir.display()));
    }

    // Filter out files with pre-emphasis indicators.
    let mut non_pe_files: Vec<std::path::PathBuf> = Vec::new();
    for path in &audio_files {
        let has_pe = super::metadata::check_tag_evidence(path).is_some()
            || super::metadata::check_file_evidence(path).is_some();
        if !has_pe {
            non_pe_files.push(path.clone());
        }
    }

    if non_pe_files.is_empty() {
        return Err("all files have pre-emphasis indicators".into());
    }

    log::info!(
        "Corpus training: {} non-PE files from {} total in {}",
        non_pe_files.len(),
        audio_files.len(),
        dir.display()
    );

    // Train: decode each file, compute STFT, select quiet frames.
    let mut trainer = CorpusTrainer::new();

    for path in &non_pe_files {
        match train_single_track(path, &mut trainer) {
            Ok(frames_added) => {
                if frames_added > 0 {
                    trainer.add_track();
                    log::debug!(
                        "  {} frames from {:?}",
                        frames_added,
                        path.file_name().unwrap_or_default()
                    );
                }
            }
            Err(e) => {
                log::warn!("  skip {:?}: {}", path.file_name().unwrap_or_default(), e);
            }
        }
    }

    log::info!(
        "Corpus training complete: {} tracks, {} frames",
        trainer.track_count(),
        trainer.frame_count()
    );

    // Finalize model.
    let model = trainer.finalize()?;

    // Save to database.
    save_corpus(&model)?;

    Ok(model)
}

/// Train from a single track: decode middle 30 seconds, compute STFT,
/// select quiet frames, add to trainer. Returns number of frames added.
fn train_single_track(
    path: &std::path::Path,
    trainer: &mut CorpusTrainer,
) -> Result<usize, String> {
    use super::frame_select;

    // Probe to get duration and sample rate.
    let info = crate::tui::probe::probe_audio(path).map_err(|e| format!("probe: {}", e))?;

    // Skip hi-res (>48 kHz) — corpus should represent CD-quality audio.
    if info.sample_rate > 48000 {
        return Err("sample rate > 48 kHz".into());
    }

    // Compute STFT for the middle 30 seconds.
    // We use the full compute_band_spectra which caps at 10 minutes anyway.
    // For training efficiency, we'll just use whatever it produces.
    let stft_result = super::stft::compute_band_spectra(path, info.sample_rate)
        .map_err(|e| format!("stft: {}", e))?;

    // Select quiet frames.
    let selected = frame_select::select_frames(&stft_result);

    // Add selected frames to trainer.
    let mut added = 0;
    for &idx in &selected.frames {
        trainer.add_frame(&stft_result.band_spectra[idx]);
        added += 1;
    }

    Ok(added)
}

// ── Empirical PE template from paired files ────────────────────────

/// Compute the empirical PE template from paired PE↔deemphasized files.
///
/// s_emp = mean(median_spectrum(PE_track) - median_spectrum(deemph_track))
///
/// Both spectra go through the same pipeline: STFT → 1/3-octave bands →
/// quiet-frame selection → median. This ensures the template matches
/// the actual feature space being scored.
///
/// Updates the corpus model in-place and saves to DB.
pub async fn train_empirical_template(
    pe_dir: &std::path::Path,
    deemph_dir: &std::path::Path,
) -> Result<[f64; NUM_BANDS], String> {
    let pe_dir = pe_dir.to_path_buf();
    let deemph_dir = deemph_dir.to_path_buf();

    tokio::task::spawn_blocking(move || compute_empirical_template_blocking(&pe_dir, &deemph_dir))
        .await
        .map_err(|e| format!("template task panicked: {}", e))?
}

/// Synchronous empirical template computation.
fn compute_empirical_template_blocking(
    pe_dir: &std::path::Path,
    deemph_dir: &std::path::Path,
) -> Result<[f64; NUM_BANDS], String> {
    use walkdir::WalkDir;

    // Collect PE FLAC files.
    let pe_files: Vec<std::path::PathBuf> = WalkDir::new(pe_dir)
        .follow_links(true)
        .into_iter()
        .flatten()
        .filter(|e| {
            e.path().is_file()
                && e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x.eq_ignore_ascii_case("flac"))
                    .unwrap_or(false)
        })
        .map(|e| e.path().to_path_buf())
        .collect();

    if pe_files.is_empty() {
        return Err("no PE files found".into());
    }

    let mut template_sum = [0.0f64; NUM_BANDS];
    let mut pair_count = 0u32;

    for pe_path in &pe_files {
        // Find corresponding deemphasized file (same relative path).
        let rel = pe_path
            .strip_prefix(pe_dir)
            .map_err(|e| format!("strip: {}", e))?;
        let deemph_path = deemph_dir.join(rel);

        if !deemph_path.exists() {
            continue; // No deemphasized counterpart.
        }

        // Compute median quiet-frame spectrum for both.
        let pe_median = match compute_track_median(pe_path) {
            Ok(m) => m,
            Err(e) => {
                log::warn!(
                    "  skip PE {:?}: {}",
                    pe_path.file_name().unwrap_or_default(),
                    e
                );
                continue;
            }
        };

        let deemph_median = match compute_track_median(&deemph_path) {
            Ok(m) => m,
            Err(e) => {
                log::warn!(
                    "  skip deemph {:?}: {}",
                    deemph_path.file_name().unwrap_or_default(),
                    e
                );
                continue;
            }
        };

        // Accumulate difference: PE - deemphasized.
        for k in 0..NUM_BANDS {
            template_sum[k] += pe_median[k] - deemph_median[k];
        }
        pair_count += 1;

        if pair_count % 20 == 0 {
            log::info!("  empirical template: {} pairs processed", pair_count);
        }
    }

    if pair_count < 10 {
        return Err(format!(
            "too few valid pairs ({}) for empirical template",
            pair_count
        ));
    }

    // Average.
    let mut s_emp = [0.0f64; NUM_BANDS];
    for k in 0..NUM_BANDS {
        s_emp[k] = template_sum[k] / pair_count as f64;
    }

    log::info!(
        "Empirical PE template from {} pairs. Peak gain: {:.2} dB at band {}",
        pair_count,
        s_emp.iter().cloned().reduce(f64::max).unwrap_or(0.0),
        s_emp
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap_or(0),
    );

    // Update corpus model and save.
    let mut corpus = load_corpus().map_err(|e| format!("load corpus: {}", e))?;
    corpus.empirical_pe_template = Some(s_emp);
    save_corpus(&corpus)?;

    Ok(s_emp)
}

/// Compute the median quiet-frame spectrum for a single track.
fn compute_track_median(path: &std::path::Path) -> Result<[f64; NUM_BANDS], String> {
    use super::frame_select;
    use super::stft;

    let info = crate::tui::probe::probe_audio(path).map_err(|e| format!("probe: {}", e))?;

    if info.sample_rate > 48000 {
        return Err("hi-res".into());
    }

    let stft_result =
        stft::compute_band_spectra(path, info.sample_rate).map_err(|e| format!("stft: {}", e))?;

    let selected = frame_select::select_frames(&stft_result);
    if selected.frames.is_empty() {
        return Err("no qualifying frames".into());
    }

    // Compute median spectrum.
    let n = selected.frames.len();
    let mut median = [0.0f64; NUM_BANDS];
    for k in 0..NUM_BANDS {
        let mut values: Vec<f64> = selected
            .frames
            .iter()
            .map(|&idx| stft_result.band_spectra[idx][k])
            .collect();
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        median[k] = if n % 2 == 1 {
            values[n / 2]
        } else {
            (values[n / 2 - 1] + values[n / 2]) / 2.0
        };
    }

    Ok(median)
}

// ── Calibration: train LDA classifier from labeled data ────────────

/// Calibration result returned to the TUI.
pub struct CalibrationResult {
    pub classifier: super::scoring::LdaClassifier,
    pub cv_accuracy: f64,
    pub cv_fpr: f64,
    pub cv_precision: f64,
    pub n_pe: usize,
    pub n_non_pe: usize,
    pub threshold: f64,
}

/// Run full calibration: compute features for PE and non-PE files,
/// train LDA via grouped CV, store the classifier to DB.
pub async fn calibrate(
    pe_dir: &std::path::Path,
    non_pe_dir: &std::path::Path,
) -> Result<CalibrationResult, String> {
    let pe_dir = pe_dir.to_path_buf();
    let non_pe_dir = non_pe_dir.to_path_buf();

    tokio::task::spawn_blocking(move || calibrate_blocking(&pe_dir, &non_pe_dir))
        .await
        .map_err(|e| format!("calibration task panicked: {}", e))?
}

/// Synchronous calibration (runs inside spawn_blocking).
fn calibrate_blocking(
    pe_dir: &std::path::Path,
    non_pe_dir: &std::path::Path,
) -> Result<CalibrationResult, String> {
    use super::{frame_select, models, scoring, stft};
    use walkdir::WalkDir;

    // Ensure corpus is loaded.
    let corpus = load_corpus()
        .map_err(|e| format!("corpus not available: {} (run :preemph-train first)", e))?;

    // Collect audio files from both directories.
    let audio_extensions = ["flac", "wav", "aiff", "aif", "wv"];

    let collect_audio = |dir: &std::path::Path| -> Vec<std::path::PathBuf> {
        WalkDir::new(dir)
            .follow_links(true)
            .into_iter()
            .flatten()
            .filter(|e| {
                e.path().is_file()
                    && e.path()
                        .extension()
                        .and_then(|x| x.to_str())
                        .map(|x| audio_extensions.contains(&x.to_ascii_lowercase().as_str()))
                        .unwrap_or(false)
            })
            .map(|e| e.path().to_path_buf())
            .collect()
    };

    let pe_files = collect_audio(pe_dir);
    let non_pe_files = collect_audio(non_pe_dir);

    if pe_files.is_empty() {
        return Err(format!("no audio files in PE dir: {}", pe_dir.display()));
    }
    if non_pe_files.is_empty() {
        return Err(format!(
            "no audio files in non-PE dir: {}",
            non_pe_dir.display()
        ));
    }

    log::info!(
        "Calibration: {} PE files, {} non-PE files",
        pe_files.len(),
        non_pe_files.len()
    );

    // Compute features for all files.
    let mut samples: Vec<(scoring::TrackFeatures, bool, String)> = Vec::new();

    let compute_features = |path: &std::path::Path,
                            corpus: &CorpusModel|
     -> Result<(scoring::TrackFeatures, usize), String> {
        let info = crate::tui::probe::probe_audio(path).map_err(|e| format!("probe: {}", e))?;
        if info.sample_rate > 48000 {
            return Err("hi-res".into());
        }

        let stft_result = stft::compute_band_spectra(path, info.sample_rate)
            .map_err(|e| format!("stft: {}", e))?;
        let selected = frame_select::select_frames(&stft_result);
        if selected.frames.is_empty() {
            return Err("no qualifying frames".into());
        }

        let model_scores = models::score_models(&selected, &stft_result, corpus);
        let deemph_delta =
            scoring::virtual_deemphasis_score(&stft_result, &selected, corpus, info.sample_rate);

        let features =
            scoring::TrackFeatures::from_scores(&model_scores, deemph_delta, selected.frames.len());
        Ok((features, selected.frames.len()))
    };

    // Process PE files.
    let mut pe_ok = 0usize;
    let mut pe_err = 0usize;
    for path in &pe_files {
        let group = scoring::album_group_id(path);
        match compute_features(path, &corpus) {
            Ok((features, _)) => {
                samples.push((features, true, group));
                pe_ok += 1;
            }
            Err(e) => {
                pe_err += 1;
                if pe_err <= 5 {
                    log::warn!(
                        "  skip PE {:?}: {}",
                        path.file_name().unwrap_or_default(),
                        e
                    );
                }
            }
        }
        if (pe_ok + pe_err) % 50 == 0 {
            log::info!("  PE progress: {}/{}", pe_ok + pe_err, pe_files.len());
        }
    }

    // Process non-PE files.
    let mut non_pe_ok = 0usize;
    let mut non_pe_err = 0usize;
    for path in &non_pe_files {
        let group = scoring::album_group_id(path);
        match compute_features(path, &corpus) {
            Ok((features, _)) => {
                samples.push((features, false, group));
                non_pe_ok += 1;
            }
            Err(e) => {
                non_pe_err += 1;
                if non_pe_err <= 5 {
                    log::warn!(
                        "  skip non-PE {:?}: {}",
                        path.file_name().unwrap_or_default(),
                        e
                    );
                }
            }
        }
        if (non_pe_ok + non_pe_err) % 50 == 0 {
            log::info!(
                "  non-PE progress: {}/{}",
                non_pe_ok + non_pe_err,
                non_pe_files.len()
            );
        }
    }

    log::info!(
        "Calibration features: {} PE ({} skipped), {} non-PE ({} skipped)",
        pe_ok,
        pe_err,
        non_pe_ok,
        non_pe_err
    );

    if pe_ok < 20 || non_pe_ok < 20 {
        return Err(format!(
            "too few usable files: {} PE, {} non-PE (need 20+ each)",
            pe_ok, non_pe_ok
        ));
    }

    // Train LDA via grouped CV.
    // Use min(5, n_groups/3) folds to ensure enough groups per fold.
    let n_groups = {
        let mut groups: Vec<String> = samples.iter().map(|s| s.2.clone()).collect();
        groups.sort();
        groups.dedup();
        groups.len()
    };
    let k_folds = 5.min(n_groups / 3).max(2);
    log::info!(
        "Training LDA classifier ({}-fold grouped CV, {} groups, target FPR=1%)...",
        k_folds,
        n_groups
    );
    let (classifier, report) = scoring::grouped_cv_train_with_calibration_report(
        &samples,
        k_folds,
        scoring::DEFAULT_TARGET_TRACK_FPR,
    )?;

    let metrics = &report.metrics;
    log::info!(
        "CV results: accuracy={:.1}%, FPR={:.1}%, precision={:.1}%, threshold={:.4}",
        metrics.track_accuracy * 100.0,
        metrics.track_fpr * 100.0,
        metrics.track_precision * 100.0,
        metrics.final_model_threshold,
    );

    // Store classifier to DB.
    let db = crate::db::Database::open().map_err(|e| format!("db open: {}", e))?;
    db.store_preemph_classifier(
        &classifier,
        metrics.track_accuracy,
        metrics.track_fpr,
        metrics.track_precision,
    )?;

    Ok(CalibrationResult {
        classifier,
        cv_accuracy: metrics.track_accuracy,
        cv_fpr: metrics.track_fpr,
        cv_precision: metrics.track_precision,
        n_pe: pe_ok,
        n_non_pe: non_pe_ok,
        threshold: metrics.final_model_threshold,
    })
}
