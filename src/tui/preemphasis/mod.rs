//! CD pre-emphasis detection via metadata evidence and spectral model comparison.
//!
//! Red Book (IEC 60908) pre-emphasis uses time constants τ₁ = 50 µs and
//! τ₂ = 15 µs, producing a first-order high-shelf boost of ~+9.5 dB at 20 kHz.
//!
//! Detection has two tiers:
//! 1. **Metadata evidence (authoritative):** Checks tags, CUE files, and EAC/XLD
//!    log files for explicit pre-emphasis indicators.
//! 2. **Spectral model comparison (supplementary):** Compares three models —
//!    null (M0), bright mastering (M1), and exact Red Book pre-emphasis (M2) —
//!    on smoothed log-spectra from low-information frames. Only flags when M2
//!    beats both M0 and M1 by a clear margin.

pub mod metadata;
pub mod catalog;
pub mod iir;
pub mod stft;
pub mod frame_select;
pub mod models;
pub mod scoring;
pub mod corpus;
pub mod diag;

use std::path::PathBuf;

// ── Public types ───────────────────────────────────────────────────

/// Confidence level for pre-emphasis detection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PreemphasisConfidence {
    /// Metadata evidence confirms pre-emphasis.
    Detected,
    /// Spectral analysis shows strong evidence across multiple cues.
    StrongCandidate,
    /// Some spectral evidence, but not conclusive.
    Possible,
    /// No evidence of pre-emphasis.
    NotDetected,
    /// Insufficient data to make a determination (e.g., no corpus model).
    Indeterminate,
}

/// Result of pre-emphasis detection for a single file.
#[derive(Debug, Clone)]
pub struct PreemphasisResult {
    pub path: PathBuf,
    pub confidence: PreemphasisConfidence,
    /// Whether a CUE file with FLAGS PRE or tag evidence was found.
    pub cue_confirmed: bool,
    /// Log-likelihood ratio: M2 vs M0.
    pub llr_m2_vs_m0: f64,
    /// Log-likelihood ratio: M2 vs best M1.
    pub llr_m2_vs_m1: f64,
    /// Fitted pre-emphasis amplitude (~1.0 for true PE).
    pub fitted_alpha: f64,
    /// Number of qualifying frames used for scoring.
    pub frames_scored: usize,
    /// Mean Mahalanobis distance improvement after virtual de-emphasis.
    pub deemph_distance_delta: f64,
    /// Which counterevidence gates fired (empty = none).
    pub gates_fired: Vec<String>,
    /// Human-readable detail string.
    pub detail: String,
    // Legacy fields for DB compatibility.
    pub spectral_rms_error: f64,
    pub crest_improvement: f64,
}

// ── Public API ─────────────────────────────────────────────────────

/// Detect pre-emphasis by checking metadata/CUE/log evidence first,
/// then running the spectral model comparison scorer.
pub async fn detect_preemphasis(path: PathBuf) -> PreemphasisResult {
    // Phase 1: Check tag and file evidence (fast, authoritative).
    let evidence = metadata::check_tag_evidence(&path)
        .or_else(|| metadata::check_file_evidence(&path));

    // If metadata confirms, return immediately with Detected.
    if let Some(ref ev) = evidence {
        return PreemphasisResult {
            path,
            confidence: PreemphasisConfidence::Detected,
            cue_confirmed: true,
            llr_m2_vs_m0: f64::NAN,
            llr_m2_vs_m1: f64::NAN,
            fitted_alpha: f64::NAN,
            frames_scored: 0,
            deemph_distance_delta: 0.0,
            gates_fired: vec![],
            detail: format!("{} confirmed", ev.label()),
            spectral_rms_error: 0.0,
            crest_improvement: 0.0,
        };
    }

    // Phase 2: Catalog-number matching (fast, semi-authoritative).
    if let Some(catalog_match) = catalog::check_catalog_evidence(&path) {
        return PreemphasisResult {
            path,
            confidence: catalog_match.confidence,
            cue_confirmed: catalog_match.confidence == PreemphasisConfidence::Detected,
            llr_m2_vs_m0: f64::NAN,
            llr_m2_vs_m1: f64::NAN,
            fitted_alpha: f64::NAN,
            frames_scored: 0,
            deemph_distance_delta: 0.0,
            gates_fired: vec![],
            detail: catalog_match.detail,
            spectral_rms_error: 0.0,
            crest_improvement: 0.0,
        };
    }

    // Phase 3: Spectral model comparison.
    match run_spectral_scorer(&path).await {
        Ok(result) => result,
        Err(e) => {
            PreemphasisResult {
                path,
                confidence: PreemphasisConfidence::Indeterminate,
                cue_confirmed: false,
                llr_m2_vs_m0: f64::NAN,
                llr_m2_vs_m1: f64::NAN,
                fitted_alpha: f64::NAN,
                frames_scored: 0,
                deemph_distance_delta: 0.0,
                gates_fired: vec![],
                detail: format!("spectral analysis failed: {}", e),
                spectral_rms_error: f64::NAN,
                crest_improvement: 0.0,
            }
        }
    }
}

/// Run the full spectral scoring pipeline.
async fn run_spectral_scorer(path: &PathBuf) -> Result<PreemphasisResult, String> {
    // Probe for sample rate and channels.
    let info = tokio::task::spawn_blocking({
        let p = path.clone();
        move || crate::tui::probe::probe_audio(&p)
    }).await
        .map_err(|e| format!("probe task: {}", e))?
        .map_err(|e| format!("probe: {}", e))?;

    if info.sample_rate > 48000 {
        return Ok(PreemphasisResult {
            path: path.clone(),
            confidence: PreemphasisConfidence::Indeterminate,
            cue_confirmed: false,
            llr_m2_vs_m0: f64::NAN,
            llr_m2_vs_m1: f64::NAN,
            fitted_alpha: f64::NAN,
            frames_scored: 0,
            deemph_distance_delta: 0.0,
            gates_fired: vec![],
            detail: "skipped (sample rate > 48 kHz)".into(),
            spectral_rms_error: f64::NAN,
            crest_improvement: 0.0,
        });
    }

    let sample_rate = info.sample_rate;

    // Load corpus model.
    let corpus = match corpus::load_corpus() {
        Ok(c) => c,
        Err(_) => {
            return Ok(PreemphasisResult {
                path: path.clone(),
                confidence: PreemphasisConfidence::Indeterminate,
                cue_confirmed: false,
                llr_m2_vs_m0: f64::NAN,
                llr_m2_vs_m1: f64::NAN,
                fitted_alpha: f64::NAN,
                frames_scored: 0,
                deemph_distance_delta: 0.0,
                gates_fired: vec![],
                detail: "no corpus model (run :preemph-train first)".into(),
                spectral_rms_error: f64::NAN,
                crest_improvement: 0.0,
            });
        }
    };

    // Decode and compute STFT band spectra.
    let stft_result = tokio::task::spawn_blocking({
        let p = path.clone();
        move || stft::compute_band_spectra(&p, sample_rate)
    }).await
        .map_err(|e| format!("stft task: {}", e))?
        .map_err(|e| format!("stft: {}", e))?;

    // Select low-information frames.
    let selected = frame_select::select_frames(&stft_result);

    if selected.frames.is_empty() {
        return Ok(PreemphasisResult {
            path: path.clone(),
            confidence: PreemphasisConfidence::Indeterminate,
            cue_confirmed: false,
            llr_m2_vs_m0: f64::NAN,
            llr_m2_vs_m1: f64::NAN,
            fitted_alpha: f64::NAN,
            frames_scored: 0,
            deemph_distance_delta: 0.0,
            gates_fired: vec![],
            detail: "insufficient qualifying frames".into(),
            spectral_rms_error: f64::NAN,
            crest_improvement: 0.0,
        });
    }

    // Score with M0/M1/M2 models.
    let model_scores = models::score_models(&selected, &stft_result, &corpus);

    // Virtual de-emphasis test.
    let deemph_delta = scoring::virtual_deemphasis_score(
        &stft_result, &selected, &corpus, sample_rate,
    );

    // Try to load trained classifier; fall back to legacy alpha if unavailable.
    let classifier = crate::db::Database::open()
        .ok()
        .and_then(|db| db.load_preemph_classifier().ok());

    let verdict = if let Some(ref clf) = classifier {
        scoring::compute_verdict_with_classifier(
            &model_scores, deemph_delta, &selected, &corpus, clf,
        )
    } else {
        #[allow(deprecated)]
        scoring::compute_verdict_legacy_alpha(
            &model_scores, deemph_delta, &selected, &corpus,
        )
    };

    // Diagnostic dump.
    diag::write_diag(
        path,
        sample_rate,
        stft_result.band_spectra.len(),
        selected.frames.len(),
        model_scores.z_score,
        model_scores.alpha,
        model_scores.pe_correlation,
        verdict.score,
        model_scores.alpha,
        deemph_delta,
        &verdict.gates_fired,
        &format!("{:?}", verdict.confidence),
    );

    let detail = format!(
        "z={:.2}, α={:.3}, r={:.3}, Δd={:.2}, frames={}, gates=[{}]",
        model_scores.z_score, model_scores.alpha, model_scores.pe_correlation,
        deemph_delta, selected.frames.len(),
        verdict.gates_fired.join(", "),
    );

    Ok(PreemphasisResult {
        path: path.clone(),
        confidence: verdict.confidence,
        cue_confirmed: false,
        llr_m2_vs_m0: model_scores.z_score,
        llr_m2_vs_m1: model_scores.z_score,
        fitted_alpha: model_scores.alpha,
        frames_scored: selected.frames.len(),
        deemph_distance_delta: deemph_delta,
        gates_fired: verdict.gates_fired,
        detail,
        spectral_rms_error: 0.0,
        crest_improvement: 0.0,
    })
}
