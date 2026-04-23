//! Decision layer: track-level LDA classifier + album-level pooling.
//!
//! Scope:
//! - Track-level model: 5-feature LDA with shrinkage and explicit threshold tuning.
//! - Cross-validation: grouped by album, with greedy balancing over both raw class counts
//!   and gate-eligible training support.
//! - Album pooling: conservative, low false-positive rules that ignore very low-info tracks.
//! - The CV guarantees in this file apply to the classifier over the supplied
//!   `TrackFeatures`. Any upstream learned transforms that produced those features
//!   must be frozen or rebuilt outside this module.

use super::corpus::CorpusModel;
use super::frame_select::SelectedFrames;
use super::iir::DeemphasisFilter;
use super::models::ModelScores;
use super::stft::{compute_bin_ranges, hann_window};
use super::stft::{StftResult, NUM_BANDS};
use super::PreemphasisConfidence;

/// Number of features in the track-level classifier.
pub const NUM_FEATURES: usize = 5;

/// Default target false-positive rate used for threshold tuning.
pub const DEFAULT_TARGET_TRACK_FPR: f64 = 0.01;

/// Tracks with fewer qualifying frames than this are treated as low-info for pooling.
pub const MIN_RELIABLE_FRAMES: usize = 20;

/// Lowest class count accepted by the trainer.
pub const MIN_CLASS_SAMPLES: usize = 10;

/// Small margin used when placing thresholds between adjacent scores.
const SCORE_EPSILON: f64 = 1e-9;

/// Margin above the tuned threshold required for a strong per-track verdict.
const TRACK_STRONG_MARGIN: f64 = 0.5;

/// Album pooling uses threshold-relative score cutoffs so the rules stay aligned
/// with the fitted classifier's score scale.
const ALBUM_POSSIBLE_MEDIAN_MARGIN: f64 = 0.25;
const ALBUM_STRONG_MEDIAN_MARGIN: f64 = 0.5;
const ALBUM_STRONG_TRACK_MARGIN: f64 = 0.5;
const ALBUM_STRONG_NEGATIVE_MARGIN: f64 = 0.5;

// ── Track-level feature vector ─────────────────────────────────────

/// Track-level feature vector for the LDA classifier.
#[derive(Debug, Clone)]
pub struct TrackFeatures {
    /// Features: [alpha, pe_correlation, deemph_delta, log_frame_count, alpha_stability]
    ///
    /// alpha_stability may be NaN when unavailable. Tracks that fail any
    /// deployment hard gate are excluded from model fitting, but the classifier
    /// still stores an imputation value so it can score such tracks for
    /// diagnostics before the gate forces rejection.
    pub features: [f64; NUM_FEATURES],
    /// Raw alpha value (for display/diagnostics).
    pub alpha: f64,
    /// Whether alpha_stability was missing at feature extraction time.
    pub alpha_stability_missing: bool,
}

impl TrackFeatures {
    pub fn from_scores(scores: &ModelScores, deemph_delta: f64, frame_count: usize) -> Self {
        let alpha_stability_missing = !scores.alpha_stability.is_finite();
        let alpha_stability = if alpha_stability_missing {
            f64::NAN
        } else {
            scores.alpha_stability.max(0.0)
        };

        Self {
            features: [
                scores.alpha,
                scores.pe_correlation,
                deemph_delta,
                (frame_count.max(1) as f64).ln(),
                alpha_stability,
            ],
            alpha: scores.alpha,
            alpha_stability_missing,
        }
    }
}

// ── LDA Classifier ─────────────────────────────────────────────────

/// Two-class LDA classifier with per-fold standardization and tuned threshold.
///
/// Features are standardized (z-scored) using training-fold statistics before
/// fitting, so the ridge penalty is scale-neutral across mixed-unit features.
///
/// Raw score: score = w · standardize(x) + bias
/// Prediction: score >= threshold
#[derive(Debug, Clone)]
pub struct LdaClassifier {
    /// Weight vector (NUM_FEATURES) in standardized feature space.
    pub weights: [f64; NUM_FEATURES],
    /// Bias term for the raw linear score.
    pub bias: f64,
    /// Decision threshold on this classifier's own score scale.
    ///
    /// For CV folds, this is tuned on that fold's training partition. For the
    /// final deployed model, this is re-tuned in-sample after retraining on all
    /// available data so the decision boundary matches the deployed score scale.
    pub threshold: f64,
    /// Per-feature imputation values (training-fold medians) for missing/non-finite features.
    pub feature_impute: [f64; NUM_FEATURES],
    /// Per-feature means from training fold (for standardization).
    pub feature_means: [f64; NUM_FEATURES],
    /// Per-feature std devs from training fold (for standardization).
    pub feature_stds: [f64; NUM_FEATURES],
}

impl LdaClassifier {
    /// Fingerprint the fitted score scale so downstream consumers can reject
    /// pooled scores from a different classifier instance.
    pub fn score_scale_fingerprint(&self) -> u64 {
        fingerprint_classifier_score_scale(self)
    }

    /// Train LDA from labeled feature vectors.
    /// Labels: true = PE, false = non-PE.
    ///
    /// Steps:
    /// 1. Exclude samples that fail the shipped hard gates for deployment
    /// 2. Compute per-feature imputation values (medians) for remaining missing data
    /// 3. Impute all remaining non-finite features (consistent train/inference policy)
    /// 4. Standardize features (z-score) so ridge penalty is scale-neutral
    /// 5. Fit LDA with shrinkage ladder
    pub fn train(samples: &[(TrackFeatures, bool)]) -> Result<Self, String> {
        // Step 1: Exclude samples that fail the shipped hard gates so the
        // classifier is trained on the same support that can ever be accepted at
        // deployment time.
        let fit_samples: Vec<(TrackFeatures, bool)> = samples
            .iter()
            .filter(|(features, _)| training_sample_is_eligible(features))
            .map(|(features, label)| (features.clone(), *label))
            .collect();

        let excluded = samples.len().saturating_sub(fit_samples.len());
        if excluded > 0 {
            log::debug!(
                "LDA train: excluded {} deployment-ineligible samples before fitting",
                excluded
            );
        }

        // Step 2: Compute per-feature imputation values (medians of finite values).
        let feature_impute = compute_feature_impute(&fit_samples);

        // Step 3: Impute non-finite features. Log how many values were imputed.
        let mut n_imputed = 0usize;
        let valid: Vec<([f64; NUM_FEATURES], bool)> = fit_samples
            .iter()
            .map(|(features, label)| {
                let mut cleaned = features.features;
                for i in 0..NUM_FEATURES {
                    if !cleaned[i].is_finite() {
                        cleaned[i] = feature_impute[i];
                        n_imputed += 1;
                    }
                }
                (cleaned, *label)
            })
            .collect();

        if n_imputed > 0 {
            log::debug!(
                "LDA train: imputed {} non-finite feature values across {} fit samples",
                n_imputed,
                valid.len()
            );
        }

        let pe: Vec<&[f64; NUM_FEATURES]> = valid
            .iter()
            .filter(|(_, label)| *label)
            .map(|(f, _)| f)
            .collect();
        let non_pe: Vec<&[f64; NUM_FEATURES]> = valid
            .iter()
            .filter(|(_, label)| !*label)
            .map(|(f, _)| f)
            .collect();

        if pe.len() < MIN_CLASS_SAMPLES || non_pe.len() < MIN_CLASS_SAMPLES {
            return Err(format!(
                "too few samples: {} PE, {} non-PE (need {}+ each)",
                pe.len(),
                non_pe.len(),
                MIN_CLASS_SAMPLES
            ));
        }

        // Step 3: Compute per-feature mean and std for standardization.
        let all_features: Vec<&[f64; NUM_FEATURES]> = valid.iter().map(|(f, _)| f).collect();
        let feature_means = class_mean(&all_features);
        let mut feature_stds = [0.0f64; NUM_FEATURES];
        let n = all_features.len() as f64;
        for i in 0..NUM_FEATURES {
            let var: f64 = all_features.iter()
                .map(|f| (f[i] - feature_means[i]).powi(2))
                .sum::<f64>() / (n - 1.0);
            feature_stds[i] = var.sqrt().max(1e-10); // Floor to avoid div-by-zero.
        }

        // Standardize all features.
        let standardized: Vec<([f64; NUM_FEATURES], bool)> = valid.iter()
            .map(|(f, label)| {
                let mut z = [0.0; NUM_FEATURES];
                for i in 0..NUM_FEATURES {
                    z[i] = (f[i] - feature_means[i]) / feature_stds[i];
                }
                (z, *label)
            })
            .collect();

        let pe_std: Vec<&[f64; NUM_FEATURES]> = standardized
            .iter().filter(|(_, l)| *l).map(|(f, _)| f).collect();
        let non_pe_std: Vec<&[f64; NUM_FEATURES]> = standardized
            .iter().filter(|(_, l)| !*l).map(|(f, _)| f).collect();

        // Step 4: LDA on standardized features.
        let mu_pe = class_mean(&pe_std);
        let mu_non_pe = class_mean(&non_pe_std);

        let cov_pe = class_covariance(&pe_std, &mu_pe);
        let cov_non_pe = class_covariance(&non_pe_std, &mu_non_pe);

        let n_pe = pe_std.len() as f64;
        let n_non = non_pe_std.len() as f64;
        let n_total = n_pe + n_non;

        let mut pooled_cov = [0.0f64; NUM_FEATURES * NUM_FEATURES];
        for i in 0..NUM_FEATURES * NUM_FEATURES {
            pooled_cov[i] = ((n_pe - 1.0) * cov_pe[i] + (n_non - 1.0) * cov_non_pe[i])
                / (n_total - 2.0);
        }

        let mut mu_diff = [0.0f64; NUM_FEATURES];
        for i in 0..NUM_FEATURES {
            mu_diff[i] = mu_pe[i] - mu_non_pe[i];
        }

        // Shrinkage ladder on standardized covariance.
        // After standardization, trace ≈ NUM_FEATURES, so base_ridge ≈ 1.0.
        let trace: f64 = (0..NUM_FEATURES)
            .map(|i| pooled_cov[i * NUM_FEATURES + i])
            .sum();
        let base_ridge = if trace.is_finite() && trace > 0.0 {
            trace / NUM_FEATURES as f64
        } else {
            1.0
        };

        let ridge_scales = [0.01, 0.03, 0.1, 0.3, 1.0];
        let mut solved_weights = None;
        let mut last_error = String::from("solver did not run");
        for scale in ridge_scales {
            let mut regularized = pooled_cov;
            let ridge = base_ridge * scale;
            for i in 0..NUM_FEATURES {
                regularized[i * NUM_FEATURES + i] += ridge;
            }
            match gauss_jordan_solve_5(&regularized, &mu_diff) {
                Ok(w) => {
                    solved_weights = Some(w);
                    break;
                }
                Err(err) => last_error = err,
            }
        }

        let weights = solved_weights.ok_or_else(|| {
            format!("failed to solve pooled covariance system after shrinkage ladder: {last_error}")
        })?;

        // Equal-prior boundary on standardized scores.
        let proj_pe: f64 = mu_pe.iter().zip(weights.iter()).map(|(&m, &w)| m * w).sum();
        let proj_non: f64 = mu_non_pe.iter().zip(weights.iter()).map(|(&m, &w)| m * w).sum();
        let bias = -(proj_pe + proj_non) / 2.0;

        Ok(Self {
            weights,
            bias,
            threshold: 0.0,
            feature_impute,
            feature_means,
            feature_stds,
        })
    }

    /// Return the raw linear score. Positive values are more PE-like.
    /// Applies the same imputation + standardization used during training.
    pub fn score(&self, features: &TrackFeatures) -> f64 {
        let imputed = self.impute_features(features);
        let standardized = self.standardize(&imputed);
        let mut s = self.bias;
        for i in 0..NUM_FEATURES {
            s += self.weights[i] * standardized[i];
        }
        s
    }

    /// Return the threshold-relative margin on this classifier's own score scale.
    ///
    /// This is the quantity that should be calibrated later; unlike raw scores,
    /// a margin is aligned to the decision boundary actually carried by this
    /// classifier instance.
    pub fn margin(&self, features: &TrackFeatures) -> f64 {
        self.score(features) - self.threshold
    }

    /// Whether the feature vector passes the shipped conservative track gates.
    pub fn passes_track_gates(&self, features: &TrackFeatures) -> bool {
        passes_track_gates(features)
    }

    /// Return the calibration margin for tracks that survive the shipped gates.
    ///
    /// Hard-gated tracks should usually be handled by policy (for example, map
    /// to probability 0) rather than being mixed into a monotone calibrator fit.
    pub fn calibration_margin(&self, features: &TrackFeatures) -> Option<f64> {
        if self.passes_track_gates(features) {
            Some(self.margin(features))
        } else {
            None
        }
    }

    /// Impute non-finite features using training-fold medians.
    fn impute_features(&self, features: &TrackFeatures) -> [f64; NUM_FEATURES] {
        let mut out = features.features;
        for i in 0..NUM_FEATURES {
            if !out[i].is_finite() {
                out[i] = self.feature_impute[i];
            }
        }
        out
    }

    /// Standardize features using training-fold mean/std.
    fn standardize(&self, features: &[f64; NUM_FEATURES]) -> [f64; NUM_FEATURES] {
        let mut z = [0.0; NUM_FEATURES];
        for i in 0..NUM_FEATURES {
            z[i] = (features[i] - self.feature_means[i]) / self.feature_stds[i];
        }
        z
    }

    /// Apply the shipped track-level decision rule: threshold plus conservative gates.
    pub fn predict(&self, features: &TrackFeatures) -> bool {
        gated_predict(self.score(features), self.threshold, features)
    }

    /// Apply only the tuned numeric threshold, without conservative gates.
    pub fn raw_predict(&self, features: &TrackFeatures) -> bool {
        self.score(features) >= self.threshold
    }

    /// Return a copy with a tuned threshold.
    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold;
        self
    }
}

fn fingerprint_classifier_score_scale(classifier: &LdaClassifier) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for value in classifier.weights {
        value.to_bits().hash(&mut hasher);
    }
    classifier.bias.to_bits().hash(&mut hasher);
    for value in classifier.feature_impute {
        value.to_bits().hash(&mut hasher);
    }
    for value in classifier.feature_means {
        value.to_bits().hash(&mut hasher);
    }
    for value in classifier.feature_stds {
        value.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

/// Compute per-feature imputation values: median of finite values for each feature.
/// Used consistently in both training and inference.
fn compute_feature_impute(samples: &[(TrackFeatures, bool)]) -> [f64; NUM_FEATURES] {
    let mut impute = [0.0f64; NUM_FEATURES];
    for i in 0..NUM_FEATURES {
        let mut values: Vec<f64> = samples
            .iter()
            .map(|(f, _)| f.features[i])
            .filter(|v| v.is_finite())
            .collect();
        if values.is_empty() {
            impute[i] = 0.0;
            continue;
        }
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = values.len();
        impute[i] = if n % 2 == 1 { values[n / 2] } else { (values[n / 2 - 1] + values[n / 2]) / 2.0 };
    }
    impute
}

fn class_mean(samples: &[&[f64; NUM_FEATURES]]) -> [f64; NUM_FEATURES] {
    let mut mean = [0.0; NUM_FEATURES];
    let n = samples.len() as f64;
    for sample in samples {
        for i in 0..NUM_FEATURES {
            mean[i] += sample[i];
        }
    }
    for value in &mut mean {
        *value /= n;
    }
    mean
}

fn class_covariance(
    samples: &[&[f64; NUM_FEATURES]],
    mean: &[f64; NUM_FEATURES],
) -> [f64; NUM_FEATURES * NUM_FEATURES] {
    let mut cov = [0.0; NUM_FEATURES * NUM_FEATURES];
    let n = samples.len() as f64;
    for sample in samples {
        for i in 0..NUM_FEATURES {
            for j in 0..NUM_FEATURES {
                cov[i * NUM_FEATURES + j] += (sample[i] - mean[i]) * (sample[j] - mean[j]);
            }
        }
    }
    for value in &mut cov {
        *value /= n - 1.0;
    }
    cov
}

/// Full Gauss-Jordan solve for a 5×5 system with partial pivoting.
fn gauss_jordan_solve_5(
    matrix: &[f64; NUM_FEATURES * NUM_FEATURES],
    rhs: &[f64; NUM_FEATURES],
) -> Result<[f64; NUM_FEATURES], String> {
    let n = NUM_FEATURES;
    let mut aug = [0.0f64; NUM_FEATURES * (NUM_FEATURES + 1)];
    for i in 0..n {
        for j in 0..n {
            aug[i * (n + 1) + j] = matrix[i * n + j];
        }
        aug[i * (n + 1) + n] = rhs[i];
    }

    for col in 0..n {
        let mut pivot_row = col;
        let mut pivot_abs = aug[col * (n + 1) + col].abs();
        for row in (col + 1)..n {
            let value = aug[row * (n + 1) + col].abs();
            if value > pivot_abs {
                pivot_abs = value;
                pivot_row = row;
            }
        }

        if pivot_abs < 1e-12 {
            return Err(format!("near-singular matrix at column {col}"));
        }

        if pivot_row != col {
            for j in 0..=n {
                aug.swap(col * (n + 1) + j, pivot_row * (n + 1) + j);
            }
        }

        let pivot = aug[col * (n + 1) + col];
        for j in col..=n {
            aug[col * (n + 1) + j] /= pivot;
        }

        for row in 0..n {
            if row == col {
                continue;
            }
            let factor = aug[row * (n + 1) + col];
            if factor == 0.0 {
                continue;
            }
            for j in col..=n {
                aug[row * (n + 1) + j] -= factor * aug[col * (n + 1) + j];
            }
        }
    }

    let mut solution = [0.0f64; NUM_FEATURES];
    for i in 0..n {
        solution[i] = aug[i * (n + 1) + n];
    }
    Ok(solution)
}

// ── Grouped K-Fold Cross-Validation ────────────────────────────────

/// Assign group IDs based on the full parent directory path.
pub fn album_group_id(path: &std::path::Path) -> String {
    path.parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("unknown"))
}

#[derive(Debug, Clone)]
struct GroupStats {
    group_id: String,
    pe_count: usize,
    non_pe_count: usize,
    eligible_pe_count: usize,
    eligible_non_pe_count: usize,
}

#[derive(Debug, Clone, Default)]
struct FoldStats {
    groups: Vec<String>,
    pe_count: usize,
    non_pe_count: usize,
    eligible_pe_count: usize,
    eligible_non_pe_count: usize,
}

impl FoldStats {
    fn total(&self) -> usize {
        self.pe_count + self.non_pe_count
    }

    fn total_eligible(&self) -> usize {
        self.eligible_pe_count + self.eligible_non_pe_count
    }
}

impl GroupStats {
    fn total(&self) -> usize {
        self.pe_count + self.non_pe_count
    }

    fn total_eligible(&self) -> usize {
        self.eligible_pe_count + self.eligible_non_pe_count
    }
}

/// Cross-validation summary returned by the detailed trainer.
///
/// The main `track_*` metrics are aligned with the shipped track decision rule:
/// thresholded classifier score plus the same conservative gates used at inference.
///
/// During CV, each held-out fold is evaluated with a threshold tuned only on that
/// fold's training partition, so the reported metrics do not mix raw scores from
/// different fold-specific models onto a single threshold scale.
#[derive(Debug, Clone)]
pub struct CvMetrics {
    /// Deployment-aligned metrics: held-out OOF predictions scored with the
    /// shipped gates and each fold's own threshold tuned on that fold's training
    /// partition. These are the unbiased CV metrics.
    pub track_accuracy: f64,
    pub track_fpr: f64,
    pub track_precision: f64,
    pub track_tp: usize,
    pub track_tn: usize,
    pub track_fp: usize,
    pub track_fn_count: usize,
    /// Threshold-only metrics (diagnostic only). These apply the per-record
    /// threshold but skip the shipped hard gates.
    pub threshold_only_track_accuracy: f64,
    pub threshold_only_track_fpr: f64,
    pub threshold_only_track_precision: f64,
    pub threshold_only_track_tp: usize,
    pub threshold_only_track_tn: usize,
    pub threshold_only_track_fp: usize,
    pub threshold_only_track_fn_count: usize,
    /// Backward-compatible aliases for `threshold_only_track_*`.
    pub ungated_track_accuracy: f64,
    pub ungated_track_fpr: f64,
    pub ungated_track_precision: f64,
    /// Older aliases preserved for compatibility. These are thresholded-but-
    /// ungated metrics, not pooled raw score summaries.
    pub raw_track_accuracy: f64,
    pub raw_track_fpr: f64,
    pub raw_track_precision: f64,
    /// Whether the OOF metrics above were computed with varying fold-specific
    /// thresholds rather than the final deployment threshold.
    pub oof_thresholds_vary_by_fold: bool,
    /// Number of distinct tuned thresholds that appeared across the OOF folds.
    pub oof_unique_thresholds: usize,
    /// Final full-data threshold on the retrained deployment model's score scale.
    /// This threshold is for deployment and final-model diagnostics, not the
    /// source of the OOF metrics above.
    pub final_model_threshold: f64,
    /// Backward-compatible alias for `final_model_threshold`.
    ///
    /// Deprecated by convention: callers should prefer `final_model_threshold`
    /// or `deployment_threshold()` so they do not confuse the deployed full-data
    /// threshold with the fold-specific thresholds used to produce OOF metrics.
    pub tuned_threshold: f64,
    pub folds_used: usize,
    pub total_test_tracks: usize,
}

impl CvMetrics {
    /// Final full-data threshold on the retrained deployment model's score scale.
    pub fn deployment_threshold(&self) -> f64 {
        self.final_model_threshold
    }

    /// Final full-data threshold on the retrained deployment model's score scale.
    pub fn final_model_threshold(&self) -> f64 {
        self.final_model_threshold
    }

    /// Threshold-only OOF metrics that ignore hard gates.
    pub fn threshold_only_metrics(&self) -> ThresholdMetrics {
        ThresholdMetrics {
            accuracy: self.threshold_only_track_accuracy,
            fpr: self.threshold_only_track_fpr,
            precision: self.threshold_only_track_precision,
            tp: self.threshold_only_track_tp,
            tn: self.threshold_only_track_tn,
            fp: self.threshold_only_track_fp,
            fn_count: self.threshold_only_track_fn_count,
        }
    }

    /// Deployment-aligned OOF metrics that apply the shipped hard gates.
    pub fn deployment_aligned_metrics(&self) -> ThresholdMetrics {
        ThresholdMetrics {
            accuracy: self.track_accuracy,
            fpr: self.track_fpr,
            precision: self.track_precision,
            tp: self.track_tp,
            tn: self.track_tn,
            fp: self.track_fp,
            fn_count: self.track_fn_count,
        }
    }
}

/// Confusion-matrix-backed metrics for a single thresholded evaluation.
#[derive(Debug, Clone)]
pub struct ThresholdMetrics {
    pub accuracy: f64,
    pub fpr: f64,
    pub precision: f64,
    pub tp: usize,
    pub tn: usize,
    pub fp: usize,
    pub fn_count: usize,
}

/// Per-track record for downstream calibration auditing.
///
/// This type keeps the raw per-fold `score` and `threshold` only for audit and
/// debugging. Calibrator fitting should use `CalibrationFitRecord`, which
/// exposes only threshold-relative held-out margins from gate-passing tracks.
#[derive(Debug, Clone)]
pub struct CalibrationRecord {
    fold_index: usize,
    label: bool,
    score: f64,
    threshold: f64,
    margin: f64,
    gated_out: bool,
    gates_fired: Vec<String>,
}

impl CalibrationRecord {
    pub fn fold_index(&self) -> usize {
        self.fold_index
    }

    pub fn label(&self) -> bool {
        self.label
    }

    /// Raw held-out score kept for audit only. Do not pool this across folds for calibration.
    pub fn audit_score(&self) -> f64 {
        self.score
    }

    /// Fold-local tuned threshold kept for audit only. Pair with `audit_score` only within-record.
    pub fn audit_threshold(&self) -> f64 {
        self.threshold
    }

    pub fn margin(&self) -> f64 {
        self.margin
    }

    pub fn gated_out(&self) -> bool {
        self.gated_out
    }

    pub fn gates_fired(&self) -> &[String] {
        &self.gates_fired
    }
}

/// Fit-safe held-out record for downstream calibration.
#[derive(Debug, Clone)]
pub struct CalibrationFitRecord {
    pub fold_index: usize,
    pub label: bool,
    pub margin: f64,
}

/// Detailed CV report for downstream calibration.
///
/// This report intentionally keeps unbiased OOF evaluation, deployment threshold
/// selection, and final-model in-sample diagnostics separate.
#[derive(Debug, Clone)]
pub struct CvCalibrationReport {
    pub metrics: CvMetrics,
    /// Held-out per-track records.
    ///
    /// Fit the calibrator on `margin`, usually after filtering to
    /// `!gated_out`. Do not assume pooled raw `score` is comparable across
    /// folds just because the records are held out. Tracks marked
    /// `gated_out = true` remain here for auditing and policy handling, not for
    /// monotone calibration fitting.
    calibration_records: Vec<CalibrationRecord>,
    /// Thresholded performance of the final retrained model evaluated in-sample.
    /// Useful for deployment alignment diagnostics only, not unbiased validation.
    pub final_model_in_sample: ThresholdMetrics,
}

impl CvCalibrationReport {
    /// Audit-oriented access to the full held-out record table.
    pub fn calibration_records(&self) -> &[CalibrationRecord] {
        &self.calibration_records
    }

    /// Fit-safe held-out records for calibration.
    ///
    /// This export drops raw scores, drops fold-local thresholds, and excludes
    /// hard-gated tracks so downstream calibration cannot silently fit on
    /// incomparable fold scores or policy-rejected support.
    pub fn fit_calibration_records(&self) -> Vec<CalibrationFitRecord> {
        self.calibration_records
            .iter()
            .filter(|record| !record.gated_out())
            .map(|record| CalibrationFitRecord {
                fold_index: record.fold_index(),
                label: record.label(),
                margin: record.margin(),
            })
            .collect()
    }

    /// Return only the held-out audit records that survive the shipped track gates.
    ///
    /// Despite the older API naming, these are gate-passing records, not
    /// records generated without gating.
    pub fn gate_passing_calibration_records(&self) -> Vec<&CalibrationRecord> {
        self.calibration_records
            .iter()
            .filter(|record| !record.gated_out())
            .collect()
    }

    /// Backward-compatible alias that preserves the historic "ungated" meaning:
    /// return the full held-out audit table without filtering on deployment
    /// gates.
    #[deprecated(note = "use calibration_records for the full audit table, fit_calibration_records for calibrator fitting, or gate_passing_calibration_records for gate-filtered audit")]
    pub fn ungated_calibration_records(&self) -> Vec<&CalibrationRecord> {
        self.calibration_records.iter().collect()
    }
}

#[derive(Debug, Clone)]
struct OofPrediction {
    features: TrackFeatures,
    score: f64,
    label: bool,
    threshold: f64,
    fold_index: usize,
}

/// Backward-compatible wrapper. Returns the final classifier plus held-out
/// deployment-aligned OOF accuracy/FPR.
#[deprecated(
    note = "prefer grouped_cv_train_detailed or grouped_cv_train_with_calibration_report; this legacy tuple omits the deployment threshold and threshold-only diagnostics"
)]
pub fn grouped_cv_train(
    samples: &[(TrackFeatures, bool, String)],
    k_folds: usize,
) -> Result<(LdaClassifier, f64, f64), String> {
    let (classifier, metrics) = grouped_cv_train_detailed(samples, k_folds, DEFAULT_TARGET_TRACK_FPR)?;
    Ok((classifier, metrics.track_accuracy, metrics.track_fpr))
}

/// Run grouped k-fold CV with greedy fold assignment over both raw class counts
/// and gate-eligible training support, tune a low-FPR threshold independently
/// inside each training fold, and return the final classifier trained on all samples.
///
/// The returned `CvMetrics` separate deployment-aligned OOF metrics from the
/// final full-data deployment threshold and from threshold-only diagnostics.
pub fn grouped_cv_train_detailed(
    samples: &[(TrackFeatures, bool, String)],
    k_folds: usize,
    target_track_fpr: f64,
) -> Result<(LdaClassifier, CvMetrics), String> {
    let (classifier, report) = grouped_cv_train_with_calibration_report(
        samples,
        k_folds,
        target_track_fpr,
    )?;
    Ok((classifier, report.metrics))
}

/// Run grouped k-fold CV over precomputed `TrackFeatures` and return
/// calibration-ready held-out records.
///
/// The returned report exposes fit-safe threshold-relative margins via
/// `fit_calibration_records()`. Full held-out audit records remain available via
/// `calibration_records()`. The OOF guarantees here cover threshold selection
/// and evaluation for the classifier defined in this file on the supplied
/// feature vectors; upstream feature-building must satisfy its own fold-local
/// training contract outside this module.
pub fn grouped_cv_train_with_calibration_report(
    samples: &[(TrackFeatures, bool, String)],
    k_folds: usize,
    target_track_fpr: f64,
) -> Result<(LdaClassifier, CvCalibrationReport), String> {
    if k_folds < 2 {
        return Err(String::from("k_folds must be at least 2"));
    }
    if !(0.0..1.0).contains(&target_track_fpr) {
        return Err(format!(
            "target_track_fpr must be in [0, 1), got {target_track_fpr}"
        ));
    }

    let groups = build_group_stats(samples);
    if groups.len() < k_folds {
        return Err(format!("only {} groups for {} folds", groups.len(), k_folds));
    }

    let folds = assign_groups_to_folds(&groups, k_folds);
    if folds.iter().any(|fold| fold.groups.is_empty()) {
        return Err(String::from("at least one fold ended up empty; need more balanced groups"));
    }
    validate_assigned_folds(samples, &folds)?;

    let mut oof_predictions: Vec<OofPrediction> = Vec::new();

    for (fold_index, fold) in folds.iter().enumerate() {
        let test_groups: std::collections::HashSet<&str> =
            fold.groups.iter().map(|group| group.as_str()).collect();

        let train: Vec<(TrackFeatures, bool)> = samples
            .iter()
            .filter(|sample| !test_groups.contains(sample.2.as_str()))
            .map(|sample| (sample.0.clone(), sample.1))
            .collect();
        let test: Vec<(TrackFeatures, bool)> = samples
            .iter()
            .filter(|sample| test_groups.contains(sample.2.as_str()))
            .map(|sample| (sample.0.clone(), sample.1))
            .collect();

        if train.is_empty() || test.is_empty() {
            return Err(String::from("encountered empty train/test partition during CV"));
        }

        let train_pos = train.iter().filter(|(_, label)| *label).count();
        let train_neg = train.len() - train_pos;
        let test_pos = test.iter().filter(|(_, label)| *label).count();
        let test_neg = test.len() - test_pos;
        if train_pos == 0 || train_neg == 0 || test_pos == 0 || test_neg == 0 {
            return Err(String::from(
                "grouped split produced a fold without both classes in train and test",
            ));
        }

        let classifier = LdaClassifier::train(&train)?;

        // Tune this fold's threshold on the gate-passing subset of the training
        // partition only, then evaluate that threshold on the held-out fold.
        // Threshold selection therefore lives on the same support as model
        // fitting, while held-out evaluation still applies the shipped decision
        // rule to every test record.
        let train_predictions = threshold_tuning_predictions(&classifier, &train, fold_index)?;
        let fold_threshold = tune_threshold_for_target_fpr(&train_predictions, target_track_fpr);

        for (features, label) in &test {
            let score = classifier.score(features);
            oof_predictions.push(OofPrediction {
                features: features.clone(),
                score,
                label: *label,
                threshold: fold_threshold,
                fold_index,
            });
        }
    }

    if oof_predictions.is_empty() {
        return Err(String::from("no held-out scores produced during CV"));
    }

    // Train final classifier on all data.
    let all_samples: Vec<(TrackFeatures, bool)> = samples
        .iter()
        .map(|sample| (sample.0.clone(), sample.1))
        .collect();
    let final_classifier = LdaClassifier::train(&all_samples)?;

    // Re-tune threshold on the final classifier's score scale for deployment,
    // again using only the gate-passing support that can ever be accepted by
    // the shipped decision rule. Keep a separate all-track score table for
    // in-sample deployment diagnostics.
    let final_threshold_tuning_scores =
        threshold_tuning_predictions(&final_classifier, &all_samples, usize::MAX)?;
    let final_scores: Vec<OofPrediction> = all_samples
        .iter()
        .map(|(features, label)| OofPrediction {
            features: features.clone(),
            score: final_classifier.score(features),
            label: *label,
            threshold: 0.0,
            fold_index: usize::MAX,
        })
        .collect();
    let final_threshold =
        tune_threshold_for_target_fpr(&final_threshold_tuning_scores, target_track_fpr);
    let final_classifier = final_classifier.with_threshold(final_threshold);

    let metrics = evaluate_oof_predictions(&oof_predictions, folds.len(), final_threshold);
    let final_model_in_sample = threshold_metrics(&final_scores, final_threshold, true);
    let calibration_records = build_calibration_records(&oof_predictions);

    Ok((
        final_classifier,
        CvCalibrationReport {
            metrics,
            calibration_records,
            final_model_in_sample,
        },
    ))
}

fn build_group_stats(samples: &[(TrackFeatures, bool, String)]) -> Vec<GroupStats> {
    let mut by_group: std::collections::BTreeMap<String, GroupStats> = std::collections::BTreeMap::new();
    for (features, label, group_id) in samples {
        let entry = by_group.entry(group_id.clone()).or_insert_with(|| GroupStats {
            group_id: group_id.clone(),
            pe_count: 0,
            non_pe_count: 0,
            eligible_pe_count: 0,
            eligible_non_pe_count: 0,
        });
        if *label {
            entry.pe_count += 1;
        } else {
            entry.non_pe_count += 1;
        }

        if training_sample_is_eligible(features) {
            if *label {
                entry.eligible_pe_count += 1;
            } else {
                entry.eligible_non_pe_count += 1;
            }
        }
    }
    let mut groups: Vec<GroupStats> = by_group.into_values().collect();
    groups.sort_by(|a, b| {
        b.total_eligible()
            .cmp(&a.total_eligible())
            .then_with(|| b.total().cmp(&a.total()))
            .then_with(|| b.eligible_pe_count.cmp(&a.eligible_pe_count))
            .then_with(|| b.pe_count.cmp(&a.pe_count))
            .then_with(|| a.group_id.cmp(&b.group_id))
    });
    groups
}

fn assign_groups_to_folds(groups: &[GroupStats], k_folds: usize) -> Vec<FoldStats> {
    let mut folds = vec![FoldStats::default(); k_folds];

    // Split groups by majority class, sort each by size descending,
    // then round-robin assign within each class. This guarantees both
    // classes are distributed across all folds, which greedy assignment
    // can fail to do when groups are single-class (PE-only or non-PE-only).
    let mut pe_groups: Vec<&GroupStats> = groups.iter()
        .filter(|g| g.pe_count > g.non_pe_count)
        .collect();
    let mut non_pe_groups: Vec<&GroupStats> = groups.iter()
        .filter(|g| g.non_pe_count >= g.pe_count)
        .collect();

    pe_groups.sort_by(|a, b| b.total().cmp(&a.total()));
    non_pe_groups.sort_by(|a, b| b.total().cmp(&a.total()));

    // Round-robin PE groups across folds.
    for (i, group) in pe_groups.iter().enumerate() {
        let fold = &mut folds[i % k_folds];
        fold.pe_count += group.pe_count;
        fold.non_pe_count += group.non_pe_count;
        fold.eligible_pe_count += group.eligible_pe_count;
        fold.eligible_non_pe_count += group.eligible_non_pe_count;
        fold.groups.push(group.group_id.clone());
    }

    // Round-robin non-PE groups across folds.
    for (i, group) in non_pe_groups.iter().enumerate() {
        let fold = &mut folds[i % k_folds];
        fold.pe_count += group.pe_count;
        fold.non_pe_count += group.non_pe_count;
        fold.eligible_pe_count += group.eligible_pe_count;
        fold.eligible_non_pe_count += group.eligible_non_pe_count;
        fold.groups.push(group.group_id.clone());
    }

    folds
}

fn validate_assigned_folds(
    samples: &[(TrackFeatures, bool, String)],
    folds: &[FoldStats],
) -> Result<(), String> {
    for (fold_index, fold) in folds.iter().enumerate() {
        let test_groups: std::collections::HashSet<&str> =
            fold.groups.iter().map(|group| group.as_str()).collect();

        let mut train_pos = 0usize;
        let mut train_neg = 0usize;
        let mut test_pos = 0usize;
        let mut test_neg = 0usize;
        let mut eligible_train_pos = 0usize;
        let mut eligible_train_neg = 0usize;

        for (features, label, group_id) in samples {
            if test_groups.contains(group_id.as_str()) {
                if *label {
                    test_pos += 1;
                } else {
                    test_neg += 1;
                }
            } else {
                if *label {
                    train_pos += 1;
                } else {
                    train_neg += 1;
                }

                if training_sample_is_eligible(features) {
                    if *label {
                        eligible_train_pos += 1;
                    } else {
                        eligible_train_neg += 1;
                    }
                }
            }
        }

        if train_pos == 0 || train_neg == 0 || test_pos == 0 || test_neg == 0 {
            return Err(format!(
                "grouped split produced fold {} without both classes in train/test (train: {} PE, {} non-PE; test: {} PE, {} non-PE)",
                fold_index,
                train_pos,
                train_neg,
                test_pos,
                test_neg
            ));
        }

        if eligible_train_pos < MIN_CLASS_SAMPLES || eligible_train_neg < MIN_CLASS_SAMPLES {
            return Err(format!(
                "grouped split produced fold {} with too few gate-eligible threshold-tuning samples in train ({} PE, {} non-PE; need {}+ each)",
                fold_index,
                eligible_train_pos,
                eligible_train_neg,
                MIN_CLASS_SAMPLES
            ));
        }
    }

    Ok(())
}

fn threshold_tuning_predictions(
    classifier: &LdaClassifier,
    samples: &[(TrackFeatures, bool)],
    fold_index: usize,
) -> Result<Vec<OofPrediction>, String> {
    let eligible: Vec<(TrackFeatures, bool)> = samples
        .iter()
        .filter(|(features, _)| training_sample_is_eligible(features))
        .map(|(features, label)| (features.clone(), *label))
        .collect();

    let eligible_pos = eligible.iter().filter(|(_, label)| *label).count();
    let eligible_neg = eligible.len().saturating_sub(eligible_pos);
    if eligible_pos < MIN_CLASS_SAMPLES || eligible_neg < MIN_CLASS_SAMPLES {
        return Err(format!(
            "too few threshold-tuning samples after gating: {} PE, {} non-PE (need {}+ each)",
            eligible_pos,
            eligible_neg,
            MIN_CLASS_SAMPLES
        ));
    }

    Ok(eligible
        .iter()
        .map(|(features, label)| OofPrediction {
            features: features.clone(),
            score: classifier.score(features),
            label: *label,
            threshold: 0.0,
            fold_index,
        })
        .collect())
}

fn tune_threshold_for_target_fpr(predictions: &[OofPrediction], target_fpr: f64) -> f64 {
    let eligible_predictions: Vec<&OofPrediction> = predictions
        .iter()
        .filter(|record| passes_track_gates(&record.features))
        .collect();

    debug_assert_eq!(
        eligible_predictions.len(),
        predictions.len(),
        "threshold tuning should receive only gate-passing records"
    );

    if eligible_predictions.is_empty() {
        log::warn!("threshold tuning received no eligible predictions; falling back to 0.0");
        return 0.0;
    }

    let mut candidates: Vec<f64> = eligible_predictions.iter().map(|record| record.score).collect();
    candidates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    candidates.dedup_by(|a, b| (*a - *b).abs() <= SCORE_EPSILON);

    let min_score = candidates.first().copied().unwrap_or(0.0);
    let max_score = candidates.last().copied().unwrap_or(0.0);

    let mut threshold_candidates = Vec::with_capacity(candidates.len() + 2);
    threshold_candidates.push(max_score + 1.0);
    for window in candidates.windows(2) {
        threshold_candidates.push((window[0] + window[1]) / 2.0);
    }
    threshold_candidates.push(min_score - 1.0);

    let mut best_threshold = max_score + 1.0;
    let mut best_tp = 0usize;
    let mut best_fp = usize::MAX;

    for threshold in threshold_candidates {
        let mut tp = 0usize;
        let mut fp = 0usize;
        let mut neg_total = 0usize;
        for record in &eligible_predictions {
            let record = *record;
            let predicted = record.score >= threshold;
            if record.label {
                if predicted {
                    tp += 1;
                }
            } else {
                neg_total += 1;
                if predicted {
                    fp += 1;
                }
            }
        }
        let fpr = if neg_total > 0 {
            fp as f64 / neg_total as f64
        } else {
            0.0
        };
        if fpr <= target_fpr + SCORE_EPSILON {
            let better = tp > best_tp
                || (tp == best_tp && fp < best_fp)
                || (tp == best_tp && fp == best_fp && threshold > best_threshold);
            if better {
                best_threshold = threshold;
                best_tp = tp;
                best_fp = fp;
            }
        }
    }

    best_threshold
}

fn evaluate_oof_predictions(
    predictions: &[OofPrediction],
    folds_used: usize,
    deployment_threshold: f64,
) -> CvMetrics {
    let threshold_only_metrics = threshold_metrics_from_records(predictions, false);
    let gated_metrics = threshold_metrics_from_records(predictions, true);

    let mut unique_thresholds: Vec<f64> = predictions.iter().map(|record| record.threshold).collect();
    unique_thresholds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    unique_thresholds.dedup_by(|a, b| (*a - *b).abs() <= SCORE_EPSILON);
    let oof_unique_thresholds = unique_thresholds.len();

    CvMetrics {
        track_accuracy: gated_metrics.accuracy,
        track_fpr: gated_metrics.fpr,
        track_precision: gated_metrics.precision,
        track_tp: gated_metrics.tp,
        track_tn: gated_metrics.tn,
        track_fp: gated_metrics.fp,
        track_fn_count: gated_metrics.fn_count,
        threshold_only_track_accuracy: threshold_only_metrics.accuracy,
        threshold_only_track_fpr: threshold_only_metrics.fpr,
        threshold_only_track_precision: threshold_only_metrics.precision,
        threshold_only_track_tp: threshold_only_metrics.tp,
        threshold_only_track_tn: threshold_only_metrics.tn,
        threshold_only_track_fp: threshold_only_metrics.fp,
        threshold_only_track_fn_count: threshold_only_metrics.fn_count,
        ungated_track_accuracy: threshold_only_metrics.accuracy,
        ungated_track_fpr: threshold_only_metrics.fpr,
        ungated_track_precision: threshold_only_metrics.precision,
        raw_track_accuracy: threshold_only_metrics.accuracy,
        raw_track_fpr: threshold_only_metrics.fpr,
        raw_track_precision: threshold_only_metrics.precision,
        oof_thresholds_vary_by_fold: oof_unique_thresholds > 1,
        oof_unique_thresholds,
        final_model_threshold: deployment_threshold,
        tuned_threshold: deployment_threshold,
        folds_used,
        total_test_tracks: gated_metrics.tp
            + gated_metrics.tn
            + gated_metrics.fp
            + gated_metrics.fn_count,
    }
}

fn build_calibration_records(predictions: &[OofPrediction]) -> Vec<CalibrationRecord> {
    predictions
        .iter()
        .map(|record| CalibrationRecord {
            fold_index: record.fold_index,
            label: record.label,
            score: record.score,
            threshold: record.threshold,
            margin: record.score - record.threshold,
            gated_out: !passes_track_gates(&record.features),
            gates_fired: track_gates_from_features(&record.features),
        })
        .collect()
}

fn threshold_metrics(
    predictions: &[OofPrediction],
    threshold: f64,
    apply_gates: bool,
) -> ThresholdMetrics {
    let normalized: Vec<OofPrediction> = predictions
        .iter()
        .map(|record| OofPrediction {
            features: record.features.clone(),
            score: record.score,
            label: record.label,
            threshold,
            fold_index: record.fold_index,
        })
        .collect();
    threshold_metrics_from_records(&normalized, apply_gates)
}

fn threshold_metrics_from_records(
    predictions: &[OofPrediction],
    apply_gates: bool,
) -> ThresholdMetrics {
    let (tp, tn, fp, fn_count) = confusion_counts(predictions, apply_gates);
    let total = tp + tn + fp + fn_count;
    ThresholdMetrics {
        accuracy: ratio(tp + tn, total),
        fpr: ratio(fp, fp + tn),
        precision: ratio(tp, tp + fp),
        tp,
        tn,
        fp,
        fn_count,
    }
}

fn confusion_counts(
    predictions: &[OofPrediction],
    apply_gates: bool,
) -> (usize, usize, usize, usize) {
    let mut tp = 0usize;
    let mut tn = 0usize;
    let mut fp = 0usize;
    let mut fn_count = 0usize;

    for record in predictions {
        let predicted = if apply_gates {
            gated_predict(record.score, record.threshold, &record.features)
        } else {
            record.score >= record.threshold
        };

        match (predicted, record.label) {
            (true, true) => tp += 1,
            (true, false) => fp += 1,
            (false, false) => tn += 1,
            (false, true) => fn_count += 1,
        }
    }

    (tp, tn, fp, fn_count)
}

fn ratio(num: usize, denom: usize) -> f64 {
    if denom > 0 {
        num as f64 / denom as f64
    } else {
        0.0
    }
}

// ── Virtual de-emphasis scoring ────────────────────────────────────

/// Compute virtual de-emphasis score: mean distance improvement.
pub fn virtual_deemphasis_score(
    stft: &StftResult,
    selected: &SelectedFrames,
    corpus: &CorpusModel,
    sample_rate: u32,
) -> f64 {
    use rustfft::{num_complex::Complex, FftPlanner};
    use super::stft::FFT_SIZE;

    let window = hann_window(FFT_SIZE);
    let bin_ranges = compute_bin_ranges(sample_rate);

    let mut planner = FftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);

    let mut deltas = Vec::with_capacity(selected.frames.len());
    let mut filter = DeemphasisFilter::new(sample_rate);

    for &frame_idx in &selected.frames {
        let d_orig = corpus.mahalanobis_distance(&stft.band_spectra[frame_idx]);

        filter.reset();
        let deemph_samples = filter.process_to_vec(&stft.frame_samples[frame_idx]);

        let mut fft_buf: Vec<Complex<f64>> = deemph_samples
            .iter()
            .enumerate()
            .map(|(i, &sample)| Complex::new(sample * window[i], 0.0))
            .collect();
        fft_buf.resize(FFT_SIZE, Complex::new(0.0, 0.0));
        fft.process(&mut fft_buf);

        let power: Vec<f64> = fft_buf[..FFT_SIZE / 2]
            .iter()
            .map(|bin| bin.norm_sqr())
            .collect();

        let mut bands = [0.0f64; NUM_BANDS];
        for (k, &(lo, hi)) in bin_ranges.iter().enumerate() {
            if lo > hi || hi >= power.len() {
                bands[k] = -120.0;
                continue;
            }
            let sum: f64 = power[lo..=hi].iter().sum();
            bands[k] = if sum > 0.0 { 10.0 * sum.log10() } else { -120.0 };
        }

        let d_deemph = corpus.mahalanobis_distance(&bands);
        deltas.push(d_orig - d_deemph);
    }

    if deltas.is_empty() {
        return 0.0;
    }
    deltas.iter().sum::<f64>() / deltas.len() as f64
}

fn training_sample_is_eligible(features: &TrackFeatures) -> bool {
    // Keep the model-fitting support identical to the support that can ever be
    // accepted by the deployed track-level decision rule.
    passes_track_gates(features)
}

fn track_gate_state(features: &TrackFeatures) -> (bool, bool, bool, bool) {
    let alpha_negative = !features.alpha.is_finite() || features.alpha < 0.05;
    let few_frames = !features.features[3].is_finite()
        || features.features[3] < (MIN_RELIABLE_FRAMES as f64).ln();
    let alpha_stability_missing = features.alpha_stability_missing;
    let alpha_unstable = !alpha_stability_missing
        && (!features.features[4].is_finite()
            || features.features[4] > features.alpha.abs() * 3.0);

    (
        alpha_negative,
        few_frames,
        alpha_stability_missing,
        alpha_unstable,
    )
}

fn track_gates_from_features(features: &TrackFeatures) -> Vec<String> {
    let (alpha_negative, few_frames, alpha_stability_missing, alpha_unstable) =
        track_gate_state(features);
    let mut gates_fired = Vec::new();

    if alpha_negative {
        gates_fired.push(String::from("alpha_negative"));
    }
    if few_frames {
        gates_fired.push(String::from("few_frames"));
    }
    if alpha_stability_missing {
        gates_fired.push(String::from("alpha_stability_missing"));
    }
    if alpha_unstable {
        gates_fired.push(String::from("alpha_unstable"));
    }

    gates_fired
}

fn passes_track_gates(features: &TrackFeatures) -> bool {
    let (alpha_negative, few_frames, alpha_stability_missing, alpha_unstable) =
        track_gate_state(features);
    !alpha_negative && !few_frames && !alpha_stability_missing && !alpha_unstable
}

fn gated_predict(score: f64, threshold: f64, features: &TrackFeatures) -> bool {
    score >= threshold && passes_track_gates(features)
}

// ── Verdict (using LDA when available) ─────────────────────────────

/// Final verdict after scoring.
#[derive(Debug, Clone)]
pub struct Verdict {
    pub confidence: PreemphasisConfidence,
    /// Linear discriminant score. Higher means more PE-like.
    pub score: f64,
    pub gates_fired: Vec<String>,
}

/// Compute verdict from track features using a trained classifier.
///
/// This is the deployment path. Callers that want the legacy raw-alpha heuristic
/// must opt in explicitly via `compute_verdict_legacy_alpha`.
pub fn compute_verdict_with_classifier(
    scores: &ModelScores,
    deemph_delta: f64,
    selected: &SelectedFrames,
    _corpus: &CorpusModel,
    classifier: &LdaClassifier,
) -> Verdict {
    let features = TrackFeatures::from_scores(scores, deemph_delta, selected.frames.len());
    let track_score = classifier.score(&features);
    let gates_fired = track_gates_from_features(&features);

    let confidence = if gated_predict(track_score, classifier.threshold, &features) {
        if track_score >= classifier.threshold + TRACK_STRONG_MARGIN {
            PreemphasisConfidence::StrongCandidate
        } else {
            PreemphasisConfidence::Possible
        }
    } else {
        PreemphasisConfidence::NotDetected
    };

    Verdict {
        confidence,
        score: track_score,
        gates_fired,
    }
}

/// Deployment-oriented wrapper around `compute_verdict_with_classifier`.
pub fn compute_verdict(
    scores: &ModelScores,
    deemph_delta: f64,
    selected: &SelectedFrames,
    corpus: &CorpusModel,
    classifier: &LdaClassifier,
) -> Verdict {
    compute_verdict_with_classifier(scores, deemph_delta, selected, corpus, classifier)
}

/// Explicit legacy fallback that ignores the trained classifier and thresholds on
/// raw alpha instead.
#[deprecated(
    note = "legacy raw-alpha path; prefer compute_verdict_with_classifier with a trained classifier"
)]
pub fn compute_verdict_legacy_alpha(
    scores: &ModelScores,
    deemph_delta: f64,
    selected: &SelectedFrames,
    _corpus: &CorpusModel,
) -> Verdict {
    let features = TrackFeatures::from_scores(scores, deemph_delta, selected.frames.len());
    let gates_fired = track_gates_from_features(&features);

    let confidence = if scores.alpha >= 0.5 && gates_fired.is_empty() {
        PreemphasisConfidence::Possible
    } else {
        PreemphasisConfidence::NotDetected
    };

    Verdict {
        confidence,
        score: scores.alpha,
        gates_fired,
    }
}

// ── Album-level pooling ────────────────────────────────────────────

/// Per-track summary for album pooling.
#[derive(Debug, Clone)]
pub struct TrackSummary {
    score: f64,
    alpha: f64,
    pe_correlation: f64,
    deemph_delta: f64,
    /// 75th percentile alpha from quiet frames (captures sparse PE).
    quiet_p75_alpha: f64,
    /// 75th percentile alpha from all frames.
    all_p75_alpha: f64,
    /// Fraction of quiet frames with positive alpha projection.
    fraction_positive_quiet: f64,
    frame_count: usize,
    score_scale_fingerprint: Option<u64>,
    /// Conservative gates fired by the track-level detector.
    gates_fired: Vec<String>,
    path: std::path::PathBuf,
}

impl TrackSummary {
    fn build(
        score: f64,
        features: &TrackFeatures,
        multi_alpha: Option<&super::models::TrackMultiAlpha>,
        frame_count: usize,
        path: std::path::PathBuf,
        score_scale_fingerprint: Option<u64>,
    ) -> Self {
        Self {
            score,
            alpha: features.alpha,
            pe_correlation: features.features[1],
            deemph_delta: features.features[2],
            quiet_p75_alpha: multi_alpha.map(|m| m.quiet_p75).unwrap_or(f64::NAN),
            all_p75_alpha: multi_alpha.map(|m| m.all_p75).unwrap_or(f64::NAN),
            fraction_positive_quiet: multi_alpha.map(|m| m.fraction_positive_quiet).unwrap_or(f64::NAN),
            frame_count,
            score_scale_fingerprint,
            gates_fired: track_gates_from_features(features),
            path,
        }
    }

    /// Construct a pooling summary by re-scoring the supplied features with the
    /// same classifier instance that will later anchor score-scale validation.
    ///
    /// The numeric `score` argument is accepted only for backward compatibility.
    /// It is ignored in favor of `classifier.score(features)` so callers cannot
    /// accidentally (or intentionally) stamp an unrelated score with a trusted
    /// classifier fingerprint.
    pub fn from_classifier_score(
        score: f64,
        classifier: &LdaClassifier,
        features: &TrackFeatures,
        frame_count: usize,
        path: std::path::PathBuf,
    ) -> Self {
        let derived_score = classifier.score(features);
        if score.is_finite() && (score - derived_score).abs() > 1e-12 {
            log::warn!(
                "TrackSummary::from_classifier_score ignored caller-supplied score that disagreed with classifier.score(features)"
            );
        }

        Self::build(
            derived_score,
            features,
            None,
            frame_count,
            path,
            Some(classifier.score_scale_fingerprint()),
        )
    }

    /// Construct a pooling summary directly from the classifier and features.
    pub fn from_classifier(
        classifier: &LdaClassifier,
        features: &TrackFeatures,
        frame_count: usize,
        path: std::path::PathBuf,
    ) -> Self {
        Self::build(
            classifier.score(features),
            features,
            None,
            frame_count,
            path,
            Some(classifier.score_scale_fingerprint()),
        )
    }

    /// Construct with multi-alpha summary (for enriched album pooling).
    pub fn from_classifier_with_multi_alpha(
        classifier: &LdaClassifier,
        features: &TrackFeatures,
        multi_alpha: &super::models::TrackMultiAlpha,
        frame_count: usize,
        path: std::path::PathBuf,
    ) -> Self {
        Self::build(
            classifier.score(features),
            features,
            Some(multi_alpha),
            frame_count,
            path,
            Some(classifier.score_scale_fingerprint()),
        )
    }

    /// Deprecated: this constructor cannot bind the score to a verified model
    /// scale, so album pooling will reject the resulting summary.
    #[deprecated(note = "use from_classifier or from_verdict_and_classifier so album pooling can verify score-scale compatibility")]
    pub fn from_score_and_features(
        score: f64,
        features: &TrackFeatures,
        frame_count: usize,
        path: std::path::PathBuf,
    ) -> Self {
        Self::build(score, features, None, frame_count, path, None)
    }

    /// Construct a pooling summary from a scored verdict and the same classifier
    /// that produced that verdict.
    ///
    /// The stored pooling score is always re-derived from `classifier` and
    /// `features`. The verdict is used only as a consistency check for gate state
    /// and for audit warnings when its score disagrees with the classifier.
    pub fn from_verdict_and_classifier(
        verdict: &Verdict,
        classifier: &LdaClassifier,
        features: &TrackFeatures,
        frame_count: usize,
        path: std::path::PathBuf,
    ) -> Self {
        let expected_gates = track_gates_from_features(features);
        if verdict.gates_fired != expected_gates {
            log::warn!(
                "TrackSummary::from_verdict_and_classifier received mismatched gate state; using gates re-derived from features"
            );
        }

        let derived_score = classifier.score(features);
        if verdict.score.is_finite() && (verdict.score - derived_score).abs() > 1e-12 {
            log::warn!(
                "TrackSummary::from_verdict_and_classifier ignored verdict.score that disagreed with classifier.score(features)"
            );
        }

        Self {
            score: derived_score,
            alpha: features.alpha,
            pe_correlation: features.features[1],
            deemph_delta: features.features[2],
            quiet_p75_alpha: f64::NAN,
            all_p75_alpha: f64::NAN,
            fraction_positive_quiet: f64::NAN,
            frame_count,
            score_scale_fingerprint: Some(classifier.score_scale_fingerprint()),
            gates_fired: expected_gates,
            path,
        }
    }

    /// Deprecated: this constructor cannot bind the verdict score to a verified model scale,
    /// so album pooling will reject the resulting summary.
    #[deprecated(note = "use from_verdict_and_classifier so album pooling can verify score-scale compatibility")]
    pub fn from_verdict_and_features(
        verdict: &Verdict,
        features: &TrackFeatures,
        frame_count: usize,
        path: std::path::PathBuf,
    ) -> Self {
        let expected_gates = track_gates_from_features(features);
        if verdict.gates_fired != expected_gates {
            log::warn!(
                "TrackSummary::from_verdict_and_features received mismatched gate state; using gates re-derived from features"
            );
        }

        Self {
            score: verdict.score,
            alpha: features.alpha,
            pe_correlation: features.features[1],
            deemph_delta: features.features[2],
            quiet_p75_alpha: f64::NAN,
            all_p75_alpha: f64::NAN,
            fraction_positive_quiet: f64::NAN,
            frame_count,
            score_scale_fingerprint: None,
            gates_fired: expected_gates,
            path,
        }
    }

    pub fn score(&self) -> f64 {
        self.score
    }

    pub fn alpha(&self) -> f64 {
        self.alpha
    }

    pub fn pe_correlation(&self) -> f64 {
        self.pe_correlation
    }

    pub fn deemph_delta(&self) -> f64 {
        self.deemph_delta
    }

    pub fn quiet_p75_alpha(&self) -> f64 {
        self.quiet_p75_alpha
    }

    pub fn all_p75_alpha(&self) -> f64 {
        self.all_p75_alpha
    }

    pub fn fraction_positive_quiet(&self) -> f64 {
        self.fraction_positive_quiet
    }

    pub fn frame_count(&self) -> usize {
        self.frame_count
    }

    pub fn score_scale_fingerprint(&self) -> Option<u64> {
        self.score_scale_fingerprint
    }

    pub fn gates_fired(&self) -> &[String] {
        &self.gates_fired
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    fn eligible_for_pooling(&self) -> bool {
        self.frame_count >= MIN_RELIABLE_FRAMES
            && self.score.is_finite()
            && self.gates_fired.is_empty()
    }
}

/// Album-level pooled verdict and diagnostics.
#[derive(Debug, Clone)]
pub struct AlbumPoolResult {
    pub confidence: PreemphasisConfidence,
    /// Median raw score among eligible tracks. NaN when too few tracks pass
    /// pooling eligibility to define a stable album summary.
    pub median_score: f64,
    /// Median score relative to the supplied track threshold. NaN when too few
    /// tracks pass pooling eligibility to define a stable album summary.
    pub median_margin: f64,
    pub eligible_track_count: usize,
    pub track_threshold: f64,
}

/// Pool album decisions using thresholds derived from the tuned track threshold.
pub fn album_pool(
    tracks: &[TrackSummary],
    classifier: &LdaClassifier,
) -> Result<AlbumPoolResult, String> {
    album_pool_with_threshold(tracks, classifier, classifier.threshold)
}

/// Explicit legacy wrapper that preserves the historic zero-threshold behavior.
///
/// This path intentionally skips score-scale fingerprint validation so legacy
/// summaries constructed without classifier binding remain poolable under the
/// old zero-threshold contract.
#[deprecated(
    note = "legacy zero-threshold pooling; prefer album_pool or album_pool_with_threshold"
)]
pub fn album_pool_legacy_zero_threshold(tracks: &[TrackSummary]) -> Result<AlbumPoolResult, String> {
    Ok(album_pool_checked(tracks, 0.0))
}

/// Pool album decisions using explicit threshold-relative score cutoffs.
///
/// The supplied threshold must exactly match `classifier.threshold`. This keeps
/// the score scale anchor and the numeric cutoff tied to the same classifier
/// instance. Callers that want a different cutoff on the same score scale should
/// call `classifier.clone().with_threshold(...)` first and then pass that tuned
/// classifier here.
pub fn album_pool_with_threshold(
    tracks: &[TrackSummary],
    classifier: &LdaClassifier,
    track_threshold: f64,
) -> Result<AlbumPoolResult, String> {
    if track_threshold.to_bits() != classifier.threshold.to_bits() {
        return Err(
            "album_pool_with_threshold requires a threshold taken from the supplied classifier; pass classifier.threshold or clone the classifier with the desired threshold first"
                .to_string(),
        );
    }

    validate_uniform_track_score_scale(tracks, Some(classifier.score_scale_fingerprint()))?;
    Ok(album_pool_checked(tracks, track_threshold))
}

fn validate_uniform_track_score_scale(
    tracks: &[TrackSummary],
    expected_fingerprint: Option<u64>,
) -> Result<Option<u64>, String> {
    let mut observed: Option<u64> = None;
    for track in tracks {
        let fingerprint = track.score_scale_fingerprint.ok_or_else(|| {
            format!(
                "album pooling requires track summaries bound to a classifier score scale; track {} was created without one",
                track.path().display()
            )
        })?;

        if let Some(expected) = expected_fingerprint {
            if fingerprint != expected {
                return Err(format!(
                    "album pooling mixed scores from a different classifier scale at {}",
                    track.path().display()
                ));
            }
        }

        if let Some(previous) = observed {
            if fingerprint != previous {
                return Err(format!(
                    "album pooling mixed incomparable score scales; track {} disagrees with earlier tracks",
                    track.path().display()
                ));
            }
        } else {
            observed = Some(fingerprint);
        }
    }

    Ok(observed)
}

// ── Soft-aggregation album pooling ──────────────────────────────────

/// Album-level features for the soft-aggregation pooler.
#[derive(Debug, Clone)]
pub struct AlbumFeatures {
    pub median_alpha: f64,
    pub trimmed_mean_alpha: f64,
    pub fraction_positive_alpha: f64,
    pub median_pe_correlation: f64,
    pub median_deemph_delta: f64,
    pub usable_track_count: usize,
    pub total_track_count: usize,
    pub fraction_missing_stability: f64,
    pub alpha_iqr: f64,
}

/// Soft-aggregation album pooling: uses continuous track scores with
/// soft stability weighting. No hard track threshold gate.
///
/// This is the recommended pooling path per reasoning model guidance.
/// Every track with usable frames contributes; tracks with missing/weak
/// stability are downweighted rather than excluded.
pub fn album_pool_soft(
    tracks: &[TrackSummary],
) -> (PreemphasisConfidence, AlbumFeatures) {
    let min_frames = MIN_RELIABLE_FRAMES;

    // Collect usable tracks (have enough frames and finite score).
    // Do NOT gate on alpha_stability_missing — downweight instead.
    let usable: Vec<&TrackSummary> = tracks.iter()
        .filter(|t| t.frame_count >= min_frames && t.score.is_finite())
        .collect();

    let total_count = tracks.len();

    if usable.len() < 3 {
        return (PreemphasisConfidence::Indeterminate, AlbumFeatures {
            median_alpha: f64::NAN, trimmed_mean_alpha: f64::NAN,
            fraction_positive_alpha: 0.0, median_pe_correlation: f64::NAN,
            median_deemph_delta: f64::NAN, usable_track_count: usable.len(),
            total_track_count: total_count, fraction_missing_stability: 1.0,
            alpha_iqr: f64::NAN,
        });
    }

    // Compute soft-weighted alpha values.
    // Stable tracks: weight 1.0. Missing/weak stability: weight 0.5.
    let mut weighted_alphas: Vec<f64> = Vec::with_capacity(usable.len());
    let mut stability_missing_count = 0usize;

    for t in &usable {
        let has_stability_issue = t.gates_fired().iter()
            .any(|g| g == "alpha_stability_missing" || g == "alpha_unstable");
        let weight = if has_stability_issue { 0.5 } else { 1.0 };
        if has_stability_issue { stability_missing_count += 1; }
        // Weight by repeating: weight=1.0 contributes once, weight=0.5 contributes half.
        // For median, we use the raw alpha but track the count for features.
        weighted_alphas.push(t.alpha() * weight);
    }

    // Raw alphas for statistics.
    let mut alphas: Vec<f64> = usable.iter().map(|t| t.alpha()).collect();
    alphas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = alphas.len();

    let median_alpha = median_f64(&alphas);

    // Trimmed mean (drop top/bottom 10%).
    let trim = (n as f64 * 0.1).ceil() as usize;
    let trimmed = &alphas[trim..n.saturating_sub(trim)];
    let trimmed_mean_alpha = if !trimmed.is_empty() {
        trimmed.iter().sum::<f64>() / trimmed.len() as f64
    } else {
        median_alpha
    };

    let fraction_positive_alpha = usable.iter()
        .filter(|t| t.alpha() > 0.0).count() as f64 / n as f64;

    // IQR of alpha.
    let q1 = alphas[n / 4];
    let q3 = alphas[3 * n / 4];
    let alpha_iqr = q3 - q1;

    // Other features (from the full score, not just alpha).
    // We don't have pe_correlation and deemph_delta on TrackSummary directly,
    // so use score as proxy. For a proper implementation, TrackSummary would
    // carry these fields.
    let mut scores: Vec<f64> = usable.iter().map(|t| t.score()).collect();
    scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let fraction_missing = stability_missing_count as f64 / n as f64;

    let features = AlbumFeatures {
        median_alpha,
        trimmed_mean_alpha,
        fraction_positive_alpha,
        median_pe_correlation: {
            let mut v: Vec<f64> = usable.iter().map(|t| t.pe_correlation).filter(|x| x.is_finite()).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            median_f64(&v)
        },
        median_deemph_delta: {
            let mut v: Vec<f64> = usable.iter().map(|t| t.deemph_delta).filter(|x| x.is_finite()).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            median_f64(&v)
        },
        usable_track_count: n,
        total_track_count: total_count,
        fraction_missing_stability: fraction_missing,
        alpha_iqr,
    };

    // Simple decision rules (reasoning model's "practical default"):
    // - median(alpha) > 0
    // - at least 60% of usable tracks with alpha > 0
    // - at least 3 usable tracks
    // These will be replaced by album-level CV tuning.
    let confidence = if median_alpha > 0.15
        && fraction_positive_alpha >= 0.70
        && n >= 5
        && alpha_iqr < median_alpha.abs() * 4.0
    {
        PreemphasisConfidence::StrongCandidate
    } else if median_alpha > 0.0
        && fraction_positive_alpha >= 0.60
        && n >= 3
    {
        PreemphasisConfidence::Possible
    } else {
        PreemphasisConfidence::NotDetected
    };

    (confidence, features)
}

fn median_f64(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n == 0 { return f64::NAN; }
    if n % 2 == 1 { sorted[n / 2] }
    else { (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0 }
}

// ── Album-level feature extraction ─────────────────────────────────

/// Number of album-level features.
pub const NUM_ALBUM_FEATURES: usize = 5;

/// Extract album-level features from track summaries.
///
/// Multi-summary features (per reasoning model guidance):
/// 1. median(quiet_median_alpha) — persistent PE evidence
/// 2. median(quiet_p75_alpha) — sparse PE evidence
/// 3. median(pe_correlation) — spectral shape match (best single discriminator)
/// 4. median(deemph_delta) — virtual de-emphasis evidence
/// 5. fraction of tracks with quiet_p75 > 0 — consistency
pub fn extract_album_features(tracks: &[TrackSummary]) -> Option<[f64; NUM_ALBUM_FEATURES]> {
    let usable: Vec<&TrackSummary> = tracks.iter()
        .filter(|t| t.frame_count >= MIN_RELIABLE_FRAMES && t.alpha.is_finite())
        .collect();

    if usable.len() < 3 { return None; }
    let n = usable.len();

    // Feature 1: median(quiet_median_alpha) — the alpha field IS the quiet-frame median.
    let mut alphas: Vec<f64> = usable.iter().map(|t| t.alpha).collect();
    alphas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_alpha = median_f64(&alphas);

    // Feature 2: median(quiet_p75_alpha) — captures sparse PE.
    let mut p75s: Vec<f64> = usable.iter().map(|t| t.quiet_p75_alpha)
        .filter(|x| x.is_finite()).collect();
    p75s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_p75 = if !p75s.is_empty() { median_f64(&p75s) } else { 0.0 };

    // Feature 3: median(pe_correlation) — best single discriminator (gap 0.396).
    let mut pe_corrs: Vec<f64> = usable.iter().map(|t| t.pe_correlation)
        .filter(|x| x.is_finite()).collect();
    pe_corrs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_pe_corr = if !pe_corrs.is_empty() { median_f64(&pe_corrs) } else { 0.0 };

    // Feature 4: median(deemph_delta) — strong discriminator (gap 0.262).
    let mut deltas: Vec<f64> = usable.iter().map(|t| t.deemph_delta)
        .filter(|x| x.is_finite()).collect();
    deltas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_deemph = if !deltas.is_empty() { median_f64(&deltas) } else { 0.0 };

    // Feature 5: fraction of tracks with quiet_p75 > 0.
    let frac_p75_positive = usable.iter()
        .filter(|t| t.quiet_p75_alpha.is_finite() && t.quiet_p75_alpha > 0.0)
        .count() as f64 / n as f64;

    Some([
        median_alpha,
        median_p75,
        median_pe_corr,
        median_deemph,
        frac_p75_positive,
    ])
}

// ── Album-level penalized logistic regression ──────────────────────

/// L2-regularized logistic regression for album-level classification.
#[derive(Debug, Clone)]
pub struct AlbumClassifier {
    pub weights: [f64; NUM_ALBUM_FEATURES],
    pub bias: f64,
    pub threshold: f64,
    pub feature_means: [f64; NUM_ALBUM_FEATURES],
    pub feature_stds: [f64; NUM_ALBUM_FEATURES],
}

impl AlbumClassifier {
    /// Train via gradient descent with L2 regularization.
    pub fn train(
        samples: &[([f64; NUM_ALBUM_FEATURES], bool)],
        lambda: f64,
    ) -> Result<Self, String> {
        if samples.len() < 10 {
            return Err(format!("too few album samples: {}", samples.len()));
        }

        let pos = samples.iter().filter(|(_, l)| *l).count();
        let neg = samples.len() - pos;
        if pos < 3 || neg < 3 {
            return Err(format!("too few per class: {} pos, {} neg", pos, neg));
        }

        // Standardize features.
        let n = samples.len() as f64;
        let mut means = [0.0f64; NUM_ALBUM_FEATURES];
        for (x, _) in samples {
            for i in 0..NUM_ALBUM_FEATURES { means[i] += x[i]; }
        }
        for m in means.iter_mut() { *m /= n; }

        let mut stds = [0.0f64; NUM_ALBUM_FEATURES];
        for (x, _) in samples {
            for i in 0..NUM_ALBUM_FEATURES {
                stds[i] += (x[i] - means[i]).powi(2);
            }
        }
        for s in stds.iter_mut() { *s = (*s / (n - 1.0)).sqrt().max(1e-10); }

        let standardized: Vec<([f64; NUM_ALBUM_FEATURES], f64)> = samples.iter()
            .map(|(x, label)| {
                let mut z = [0.0; NUM_ALBUM_FEATURES];
                for i in 0..NUM_ALBUM_FEATURES {
                    z[i] = (x[i] - means[i]) / stds[i];
                }
                (z, if *label { 1.0 } else { 0.0 })
            })
            .collect();

        // Gradient descent.
        let mut w = [0.0f64; NUM_ALBUM_FEATURES];
        let mut b = 0.0f64;
        let lr = 0.1;
        let max_iter = 500;

        // Class weights for imbalance.
        let w_pos = n / (2.0 * pos as f64);
        let w_neg = n / (2.0 * neg as f64);

        for _ in 0..max_iter {
            let mut grad_w = [0.0f64; NUM_ALBUM_FEATURES];
            let mut grad_b = 0.0f64;

            for (x, y) in &standardized {
                let logit: f64 = b + (0..NUM_ALBUM_FEATURES).map(|i| w[i] * x[i]).sum::<f64>();
                let p = sigmoid(logit);
                let class_weight = if *y > 0.5 { w_pos } else { w_neg };
                let err = (p - y) * class_weight;
                for i in 0..NUM_ALBUM_FEATURES {
                    grad_w[i] += err * x[i];
                }
                grad_b += err;
            }

            // L2 regularization on weights (not bias).
            for i in 0..NUM_ALBUM_FEATURES {
                grad_w[i] = grad_w[i] / n + lambda * w[i];
                w[i] -= lr * grad_w[i];
            }
            b -= lr * grad_b / n;
        }

        Ok(Self {
            weights: w,
            bias: b,
            threshold: 0.0,
            feature_means: means,
            feature_stds: stds,
        })
    }

    /// Score an album. Higher = more PE-like.
    pub fn score(&self, features: &[f64; NUM_ALBUM_FEATURES]) -> f64 {
        let mut z = [0.0; NUM_ALBUM_FEATURES];
        for i in 0..NUM_ALBUM_FEATURES {
            z[i] = (features[i] - self.feature_means[i]) / self.feature_stds[i];
        }
        let logit = self.bias + (0..NUM_ALBUM_FEATURES).map(|i| self.weights[i] * z[i]).sum::<f64>();
        logit
    }

    /// Probability estimate (sigmoid of score).
    pub fn probability(&self, features: &[f64; NUM_ALBUM_FEATURES]) -> f64 {
        sigmoid(self.score(features))
    }

    pub fn predict(&self, features: &[f64; NUM_ALBUM_FEATURES]) -> bool {
        self.score(features) >= self.threshold
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold;
        self
    }
}

fn sigmoid(x: f64) -> f64 {
    if x >= 0.0 {
        let ex = (-x).exp();
        1.0 / (1.0 + ex)
    } else {
        let ex = x.exp();
        ex / (1.0 + ex)
    }
}

/// Train album classifier with grouped CV. Returns classifier + album-level metrics.
pub fn train_album_classifier_cv(
    album_samples: &[([f64; NUM_ALBUM_FEATURES], bool, String)], // (features, label, group_id)
    k_folds: usize,
    lambda: f64,
    target_fpr: f64,
) -> Result<(AlbumClassifier, f64, f64, f64), String> { // (classifier, accuracy, fpr, precision)
    if k_folds < 2 { return Err("need at least 2 folds".into()); }

    // Split groups into PE and non-PE, round-robin assign.
    let mut groups: std::collections::BTreeMap<String, (usize, usize)> = std::collections::BTreeMap::new();
    for (_, label, group) in album_samples {
        let e = groups.entry(group.clone()).or_default();
        if *label { e.0 += 1; } else { e.1 += 1; }
    }

    let mut pe_groups: Vec<String> = groups.iter().filter(|(_, (p, _))| *p > 0).map(|(g, _)| g.clone()).collect();
    let mut np_groups: Vec<String> = groups.iter().filter(|(_, (_, n))| *n > 0).map(|(g, _)| g.clone()).collect();
    pe_groups.sort();
    np_groups.sort();

    if pe_groups.len() < k_folds || np_groups.len() < k_folds {
        return Err(format!("not enough groups: {} PE, {} non-PE for {} folds",
            pe_groups.len(), np_groups.len(), k_folds));
    }

    // Round-robin fold assignment.
    let mut fold_for_group: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, g) in pe_groups.iter().enumerate() { fold_for_group.insert(g.clone(), i % k_folds); }
    for (i, g) in np_groups.iter().enumerate() { fold_for_group.insert(g.clone(), i % k_folds); }

    let mut oof_scores: Vec<(f64, bool)> = Vec::new();

    for fold in 0..k_folds {
        let train: Vec<([f64; NUM_ALBUM_FEATURES], bool)> = album_samples.iter()
            .filter(|(_, _, g)| fold_for_group.get(g).copied() != Some(fold))
            .map(|(f, l, _)| (*f, *l))
            .collect();
        let test: Vec<([f64; NUM_ALBUM_FEATURES], bool)> = album_samples.iter()
            .filter(|(_, _, g)| fold_for_group.get(g).copied() == Some(fold))
            .map(|(f, l, _)| (*f, *l))
            .collect();

        if train.is_empty() || test.is_empty() { continue; }

        let clf = AlbumClassifier::train(&train, lambda)?;
        for (features, label) in &test {
            oof_scores.push((clf.score(features), *label));
        }
    }

    if oof_scores.is_empty() {
        return Err("no OOF scores produced".into());
    }

    // Tune threshold for target FPR.
    let mut candidates: Vec<f64> = oof_scores.iter().map(|(s, _)| *s).collect();
    candidates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    candidates.dedup();

    let mut best_threshold = candidates.last().copied().unwrap_or(0.0) + 1.0;
    let mut best_tp = 0usize;

    for &threshold in &candidates {
        let mut tp = 0; let mut fp = 0; let mut neg = 0;
        for &(score, label) in &oof_scores {
            if label { if score >= threshold { tp += 1; } }
            else { neg += 1; if score >= threshold { fp += 1; } }
        }
        let fpr = if neg > 0 { fp as f64 / neg as f64 } else { 0.0 };
        if fpr <= target_fpr && tp > best_tp {
            best_tp = tp;
            best_threshold = threshold;
        }
    }

    // Evaluate at chosen threshold.
    let mut tp = 0; let mut tn = 0; let mut fp = 0; let mut fn_c = 0;
    for &(score, label) in &oof_scores {
        let pred = score >= best_threshold;
        match (pred, label) {
            (true, true) => tp += 1,
            (true, false) => fp += 1,
            (false, false) => tn += 1,
            (false, true) => fn_c += 1,
        }
    }
    let total = (tp + tn + fp + fn_c) as f64;
    let accuracy = if total > 0.0 { (tp + tn) as f64 / total } else { 0.0 };
    let fpr = if fp + tn > 0 { fp as f64 / (fp + tn) as f64 } else { 0.0 };
    let precision = if tp + fp > 0 { tp as f64 / (tp + fp) as f64 } else { 0.0 };

    // Train final on all data.
    let all: Vec<([f64; NUM_ALBUM_FEATURES], bool)> = album_samples.iter()
        .map(|(f, l, _)| (*f, *l)).collect();
    let final_clf = AlbumClassifier::train(&all, lambda)?.with_threshold(best_threshold);

    Ok((final_clf, accuracy, fpr, precision))
}

// ── Track-level PE shape classifier ────────────────────────────────

use super::models::{TrackShapeFeatures, NUM_SHAPE_FEATURES};

/// L2-regularized logistic regression on track-level shape features.
/// Captures the within-track alpha distribution shape (intermittent PE signal).
#[derive(Debug, Clone)]
pub struct TrackPeClassifier {
    pub weights: Vec<f64>,
    pub bias: f64,
    pub threshold: f64,
    pub feature_means: Vec<f64>,
    pub feature_stds: Vec<f64>,
}

impl TrackPeClassifier {
    /// Train via gradient descent with L2 regularization.
    pub fn train(
        samples: &[(TrackShapeFeatures, bool)],
        lambda: f64,
    ) -> Result<Self, String> {
        let nf = NUM_SHAPE_FEATURES;
        if samples.len() < 20 {
            return Err(format!("too few track samples: {}", samples.len()));
        }
        let pos = samples.iter().filter(|(_, l)| *l).count();
        let neg = samples.len() - pos;
        if pos < 10 || neg < 10 {
            return Err(format!("too few per class: {} pos, {} neg", pos, neg));
        }

        // Standardize.
        let n = samples.len() as f64;
        let mut means = vec![0.0f64; nf];
        for (x, _) in samples {
            for i in 0..nf { means[i] += x.features[i]; }
        }
        for m in means.iter_mut() { *m /= n; }

        let mut stds = vec![0.0f64; nf];
        for (x, _) in samples {
            for i in 0..nf { stds[i] += (x.features[i] - means[i]).powi(2); }
        }
        for s in stds.iter_mut() { *s = (*s / (n - 1.0)).sqrt().max(1e-10); }

        let standardized: Vec<(Vec<f64>, f64)> = samples.iter()
            .map(|(x, label)| {
                let z: Vec<f64> = (0..nf).map(|i| (x.features[i] - means[i]) / stds[i]).collect();
                (z, if *label { 1.0 } else { 0.0 })
            })
            .collect();

        // Gradient descent with class weighting.
        let mut w = vec![0.0f64; nf];
        let mut b = 0.0f64;
        let lr = 0.1;
        let max_iter = 500;
        let w_pos = n / (2.0 * pos as f64);
        let w_neg = n / (2.0 * neg as f64);

        for _ in 0..max_iter {
            let mut grad_w = vec![0.0f64; nf];
            let mut grad_b = 0.0f64;

            for (x, y) in &standardized {
                let logit: f64 = b + (0..nf).map(|i| w[i] * x[i]).sum::<f64>();
                let p = sigmoid(logit);
                let cw = if *y > 0.5 { w_pos } else { w_neg };
                let err = (p - y) * cw;
                for i in 0..nf { grad_w[i] += err * x[i]; }
                grad_b += err;
            }

            for i in 0..nf {
                grad_w[i] = grad_w[i] / n + lambda * w[i];
                w[i] -= lr * grad_w[i];
            }
            b -= lr * grad_b / n;
        }

        Ok(Self { weights: w, bias: b, threshold: 0.0, feature_means: means, feature_stds: stds })
    }

    /// Score a track. Higher = more PE-like.
    pub fn score(&self, features: &TrackShapeFeatures) -> f64 {
        let nf = self.weights.len();
        let mut s = self.bias;
        for i in 0..nf {
            let z = (features.features[i] - self.feature_means[i]) / self.feature_stds[i];
            s += self.weights[i] * z;
        }
        s
    }

    pub fn probability(&self, features: &TrackShapeFeatures) -> f64 {
        sigmoid(self.score(features))
    }

    pub fn predict(&self, features: &TrackShapeFeatures) -> bool {
        self.score(features) >= self.threshold
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold;
        self
    }
}

// ── Pooled track-score album features ──────────────────────────────

/// Number of album features when using pooled track PE scores.
pub const NUM_POOLED_ALBUM_FEATURES: usize = 5;

/// Extract album features from pooled track PE scores.
pub fn extract_pooled_album_features(track_scores: &[f64]) -> Option<[f64; NUM_POOLED_ALBUM_FEATURES]> {
    let mut scores: Vec<f64> = track_scores.iter().copied()
        .filter(|s| s.is_finite())
        .collect();
    if scores.len() < 3 { return None; }

    scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = scores.len();

    // 1. median(track_score)
    let median = median_f64(&scores);

    // 2. mean of top quartile
    let top_q_start = 3 * n / 4;
    let top_q_mean = if top_q_start < n {
        scores[top_q_start..].iter().sum::<f64>() / (n - top_q_start) as f64
    } else { median };

    // 3. fraction above 0
    let frac_positive = scores.iter().filter(|&&s| s > 0.0).count() as f64 / n as f64;

    // 4. IQR
    let q1 = scores[n / 4];
    let q3 = scores[3 * n / 4];
    let iqr = q3 - q1;

    // 5. log(n_tracks)
    let log_n = (n as f64).ln();

    Some([median, top_q_mean, frac_positive, iqr, log_n])
}

/// Train album classifier on pooled track scores via grouped CV.
pub fn train_pooled_album_classifier_cv(
    album_samples: &[([f64; NUM_POOLED_ALBUM_FEATURES], bool, String)],
    k_folds: usize,
    lambda: f64,
    target_fpr: f64,
) -> Result<(AlbumClassifier, f64, f64, f64), String> {
    // Reuse existing train_album_classifier_cv but with pooled features.
    // The AlbumClassifier works on NUM_ALBUM_FEATURES = 5, same count.
    train_album_classifier_cv(album_samples, k_folds, lambda, target_fpr)
}

// ── Legacy threshold-based album pooling (kept for reference) ──────

fn album_pool_checked(
    tracks: &[TrackSummary],
    track_threshold: f64,
) -> AlbumPoolResult {
    let reliable: Vec<&TrackSummary> = tracks
        .iter()
        .filter(|track| track.eligible_for_pooling())
        .collect();

    if reliable.len() < 3 {
        return AlbumPoolResult {
            confidence: PreemphasisConfidence::Indeterminate,
            median_score: f64::NAN,
            median_margin: f64::NAN,
            eligible_track_count: reliable.len(),
            track_threshold,
        };
    }

    let mut scores: Vec<f64> = reliable.iter().map(|track| track.score).collect();
    scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let n = scores.len();
    let median_score = if n % 2 == 1 {
        scores[n / 2]
    } else {
        (scores[n / 2 - 1] + scores[n / 2]) / 2.0
    };
    let median_margin = median_score - track_threshold;

    let possible_median_threshold = track_threshold + ALBUM_POSSIBLE_MEDIAN_MARGIN;
    let strong_median_threshold = track_threshold + ALBUM_STRONG_MEDIAN_MARGIN;
    let strong_positive_threshold = track_threshold + ALBUM_STRONG_TRACK_MARGIN;
    let strong_negative_threshold = track_threshold - ALBUM_STRONG_NEGATIVE_MARGIN;

    let positive_count = reliable
        .iter()
        .filter(|track| track.score >= track_threshold)
        .count();
    let strong_positive_count = reliable
        .iter()
        .filter(|track| track.score >= strong_positive_threshold)
        .count();
    let strong_negative_count = reliable
        .iter()
        .filter(|track| track.score <= strong_negative_threshold)
        .count();

    let reliable_len = reliable.len() as f64;
    let positive_frac = positive_count as f64 / reliable_len;

    let confidence = if median_score >= strong_median_threshold
        && strong_positive_count >= 3
        && positive_frac >= 0.80
        && strong_negative_count <= 1
    {
        PreemphasisConfidence::StrongCandidate
    } else if median_score >= possible_median_threshold
        && positive_count >= 2
        && positive_frac >= 0.67
        && strong_negative_count <= 1
    {
        PreemphasisConfidence::Possible
    } else {
        PreemphasisConfidence::NotDetected
    };

    AlbumPoolResult {
        confidence,
        median_score,
        median_margin,
        eligible_track_count: reliable.len(),
        track_threshold,
    }
}
