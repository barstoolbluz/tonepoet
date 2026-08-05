//! CD pre-emphasis detection helpers.
//!
//! Red Book (IEC 60908) pre-emphasis uses time constants τ₁ = 50 µs and
//! τ₂ = 15 µs, producing a first-order high-shelf boost of ~+9.5 dB at 20 kHz.
//!
//! The Phase 2 metadata editor deliberately uses only metadata/CUE PRE flags
//! and catalog-number matches. Spectral scoring remains in this module for
//! unrelated diagnostics and future experiments, but the metadata editor and
//! `:preemph` scan path must not call it.

pub mod catalog;
pub mod corpus;
pub mod diag;
pub mod frame_select;
pub mod iir;
pub mod metadata;
pub mod models;
pub mod scoring;
pub mod stft;

use std::path::PathBuf;

// ── Public types ───────────────────────────────────────────────────

/// Confidence level for pre-emphasis detection.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PreemphasisConfidence {
    /// Metadata evidence confirms pre-emphasis.
    Detected,
    /// Strong non-explicit evidence, such as an exact authoritative catalog tag.
    StrongCandidate,
    /// Weaker evidence, such as an exact catalog ID inferred from a folder name.
    Possible,
    /// No evidence of pre-emphasis.
    NotDetected,
    /// Insufficient data to make a determination (e.g., no corpus model).
    Indeterminate,
}

/// Typed metadata advisory retained across direct detection, caching, and UI
/// rendering. Free-form labels are not used as a transport format.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PreemphasisAdvisory {
    pub evidence: PreemphasisAdvisoryEvidence,
    pub confidence: PreemphasisConfidence,
    pub catalog: Option<CatalogAdvisory>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PreemphasisAdvisoryEvidence {
    ExplicitTag,
    CueFlag,
    Catalog,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CatalogAdvisory {
    pub catalog_number: String,
    pub quality: catalog::CatalogMatchQuality,
    pub source: catalog::CatalogMatchSource,
    pub source_row: usize,
    pub source_catalog_cell: String,
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

fn empty_result(path: PathBuf, confidence: PreemphasisConfidence, detail: String) -> PreemphasisResult {
    PreemphasisResult {
        path,
        confidence,
        cue_confirmed: false,
        llr_m2_vs_m0: f64::NAN,
        llr_m2_vs_m1: f64::NAN,
        fitted_alpha: f64::NAN,
        frames_scored: 0,
        deemph_distance_delta: 0.0,
        gates_fired: vec![],
        detail,
        spectral_rms_error: 0.0,
        crest_improvement: 0.0,
    }
}

/// Detect pre-emphasis for the Phase 2 metadata editor path.
///
/// This detector is intentionally metadata/catalog-only: it checks explicit PRE
/// evidence from explicit PRE tags, CUE FLAGS PRE sidecars, and the known catalog-number database.
/// It never runs spectral analysis and must remain the only detector used by
/// the metadata editor Details tab and the `:preemph` scan path that hydrates
/// that tab.
pub fn detect_preemphasis_metadata_catalog(path: PathBuf) -> PreemphasisResult {
    let advisory = detect_preemphasis_advisory(&path);
    if advisory.is_some() && source_is_known_non_red_book(&path) {
        return empty_result(path, PreemphasisConfidence::NotDetected, String::new());
    }
    result_from_advisory(path, advisory.as_ref())
}

/// Detect only the metadata/catalog evidence. This function deliberately does
/// not probe source format facts, making it suitable for metadata-read workers;
/// callers must apply `preemphasis_advisory_for_source` once `SourceInfo` is
/// available.
pub fn detect_preemphasis_advisory(path: &std::path::Path) -> Option<PreemphasisAdvisory> {
    if metadata::check_pre_flag_tag_evidence(path).is_some() {
        return Some(PreemphasisAdvisory {
            evidence: PreemphasisAdvisoryEvidence::ExplicitTag,
            confidence: PreemphasisConfidence::Detected,
            catalog: None,
            detail: "PRE tag".to_string(),
        });
    }
    if metadata::check_cue_evidence(path).is_some() {
        return Some(PreemphasisAdvisory {
            evidence: PreemphasisAdvisoryEvidence::CueFlag,
            confidence: PreemphasisConfidence::Detected,
            catalog: None,
            detail: "CUE FLAGS PRE".to_string(),
        });
    }
    let catalog_match = catalog::check_catalog_evidence(path)?;
    Some(PreemphasisAdvisory {
        evidence: PreemphasisAdvisoryEvidence::Catalog,
        confidence: catalog_match.confidence,
        catalog: Some(CatalogAdvisory {
            catalog_number: catalog_match.catalog_number.clone(),
            quality: catalog_match.quality,
            source: catalog_match.source,
            source_row: catalog_match.source_row,
            source_catalog_cell: catalog_match.source_catalog_cell,
        }),
        detail: catalog_match.detail,
    })
}

/// Suppress advisories for sources whose probed format cannot be Red Book CD
/// audio. Unknown bit depth remains conservative, but a known non-16-bit depth
/// or any non-44.1-kHz rate is inapplicable.
pub fn preemphasis_advisory_for_source(
    advisory: Option<PreemphasisAdvisory>,
    info: &crate::tui::probe::SourceInfo,
) -> Option<PreemphasisAdvisory> {
    advisory.filter(|_| source_info_is_red_book_compatible(info))
}

pub fn result_from_advisory(
    path: PathBuf,
    advisory: Option<&PreemphasisAdvisory>,
) -> PreemphasisResult {
    let Some(advisory) = advisory else {
        return empty_result(path, PreemphasisConfidence::NotDetected, String::new());
    };
    let detail = match advisory.evidence {
        PreemphasisAdvisoryEvidence::ExplicitTag | PreemphasisAdvisoryEvidence::CueFlag => {
            "PRE flag".to_string()
        }
        PreemphasisAdvisoryEvidence::Catalog => advisory
            .catalog
            .as_ref()
            .map(|catalog| format!("catalog match: {}", catalog.catalog_number))
            .unwrap_or_else(|| "catalog match".to_string()),
    };
    let mut result = empty_result(path, advisory.confidence, detail);
    result.cue_confirmed = matches!(
        advisory.evidence,
        PreemphasisAdvisoryEvidence::ExplicitTag | PreemphasisAdvisoryEvidence::CueFlag
    );
    result
}

fn source_is_known_non_red_book(path: &std::path::Path) -> bool {
    crate::tui::probe::probe_audio(path)
        .ok()
        .is_some_and(|info| !source_info_is_red_book_compatible(&info))
}

fn source_info_is_red_book_compatible(info: &crate::tui::probe::SourceInfo) -> bool {
    let combined = format!("{} {}", info.format_name, info.codec).to_ascii_lowercase();
    let known_non_pcm_or_lossy = [
        "dsd", "mp3", "aac", "opus", "vorbis", "ac-3", "e-ac-3", "dts",
    ]
    .iter()
    .any(|needle| combined.contains(needle));

    !known_non_pcm_or_lossy
        && info.sample_format_is_float != Some(true)
        && info.sample_rate == 44_100
        && info.bit_depth.map_or(true, |depth| depth == 16)
}

/// Async wrapper for metadata/catalog-only detection.
pub async fn detect_preemphasis_metadata_catalog_async(path: PathBuf) -> PreemphasisResult {
    let fallback_path = path.clone();
    tokio::task::spawn_blocking(move || detect_preemphasis_metadata_catalog(path))
        .await
        .unwrap_or_else(|err| {
            empty_result(
                fallback_path,
                PreemphasisConfidence::Indeterminate,
                format!("metadata/catalog detection task failed: {err}"),
            )
        })
}

/// Backwards-compatible public detector. It is metadata/catalog-only by
/// default so callers cannot accidentally route metadata-editor results through
/// spectral analysis. Use `detect_preemphasis_with_spectral_diagnostics` for
/// unrelated diagnostic workflows that explicitly need the spectral scorer.
pub async fn detect_preemphasis(path: PathBuf) -> PreemphasisResult {
    detect_preemphasis_metadata_catalog_async(path).await
}

/// Diagnostic-only detector that can run spectral analysis after metadata and
/// catalog checks fail. Do not call this from the metadata editor or Details tab.
pub async fn detect_preemphasis_with_spectral_diagnostics(path: PathBuf) -> PreemphasisResult {
    let primary = detect_preemphasis_metadata_catalog(path.clone());
    if primary.confidence != PreemphasisConfidence::NotDetected {
        return primary;
    }

    match run_spectral_scorer(&path).await {
        Ok(result) => result,
        Err(e) => empty_result(
            path,
            PreemphasisConfidence::Indeterminate,
            format!("spectral analysis failed: {e}"),
        ),
    }
}

/// Convert a lightweight metadata evidence label into a Phase 2-safe result.
/// This is used when the editor opens from an already-read SourceMetadata cache.
/// Only explicit PRE-flag labels and catalog labels are accepted. Broader
/// historical labels such as `comment tag` and `log file` are rejected so they
/// cannot leak into the metadata editor Details view as PRE-flag evidence.
pub fn result_from_metadata_label(path: PathBuf, label: &str) -> PreemphasisResult {
    let label = label.trim();
    let lower = label.to_ascii_lowercase();

    if preemphasis_detail_is_allowed_catalog(&lower) {
        let catalog = extract_catalog_from_detail(label);
        return empty_result(
            path,
            PreemphasisConfidence::Possible,
            catalog
                .map(|catalog| format!("catalog match: {catalog}"))
                .unwrap_or_else(|| "catalog match".to_string()),
        );
    }

    if preemphasis_detail_is_allowed_pre_flag(&lower) {
        let mut result = empty_result(path, PreemphasisConfidence::Detected, "PRE flag".to_string());
        result.cue_confirmed = true;
        return result;
    }

    empty_result(path, PreemphasisConfidence::NotDetected, String::new())
}

/// Normalize any externally supplied/cached pre-emphasis result before it is
/// consumed by the metadata editor. Spectral-only positives and legacy broad
/// metadata heuristics are deliberately reduced to `NotDetected` so only
/// explicit PRE metadata/CUE flags and catalog matches can influence Details
/// UI, mixed-value calculations, badges, or cached display state.
pub fn metadata_editor_safe_result(result: &PreemphasisResult) -> PreemphasisResult {
    let detail = result.detail.trim();
    let lower = detail.to_ascii_lowercase();

    if result.confidence == PreemphasisConfidence::Detected
        && preemphasis_detail_is_allowed_pre_flag(&lower)
    {
        let mut safe = empty_result(
            result.path.clone(),
            PreemphasisConfidence::Detected,
            "PRE flag".to_string(),
        );
        safe.cue_confirmed = true;
        return safe;
    }

    if matches!(
        result.confidence,
        PreemphasisConfidence::StrongCandidate | PreemphasisConfidence::Possible
    ) && preemphasis_detail_is_allowed_catalog(&lower)
    {
        let catalog = extract_catalog_from_detail(detail);
        return empty_result(
            result.path.clone(),
            result.confidence,
            catalog
                .map(|catalog| format!("catalog match: {catalog}"))
                .unwrap_or_else(|| "catalog match".to_string()),
        );
    }

    empty_result(
        result.path.clone(),
        PreemphasisConfidence::NotDetected,
        String::new(),
    )
}

/// Whitelist explicit PRE flag evidence for the Phase 2 metadata-editor path.
/// This intentionally rejects `comment tag`, `log file`, spectral detail text,
/// and any other legacy/free-text evidence.
pub fn metadata_editor_detail_is_pre_flag(detail: &str) -> bool {
    preemphasis_detail_is_allowed_pre_flag(&detail.trim().to_ascii_lowercase())
}

/// Whitelist catalog evidence for the Phase 2 metadata-editor path.
pub fn metadata_editor_detail_is_catalog(detail: &str) -> bool {
    preemphasis_detail_is_allowed_catalog(&detail.trim().to_ascii_lowercase())
}

fn preemphasis_detail_is_allowed_pre_flag(lower: &str) -> bool {
    let lower = lower.trim();
    lower == "pre flag"
        || lower == "flags pre"
        || lower == "cue file"
        || lower == "tag"
        || lower == "pre_emphasis tag"
        || lower == "pre-emphasis tag"
        || lower == "pre emphasis tag"
        || lower == "preemphasis tag"
        || lower == "pre_emphasis"
        || lower == "pre-emphasis"
        || lower == "pre emphasis"
        || lower == "preemphasis"
}

fn preemphasis_detail_is_allowed_catalog(lower: &str) -> bool {
    let lower = lower.trim();
    lower == "catalog match"
        || lower == "catalog exact"
        || lower.starts_with("catalog match:")
        || lower.starts_with("catalog (")
}

fn extract_catalog_from_detail(detail: &str) -> Option<String> {
    let trimmed = detail.trim();
    if let Some(rest) = trimmed.strip_prefix("catalog match:") {
        let value = rest.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    if let Some(rest) = trimmed.strip_prefix("catalog (") {
        if let Some(end) = rest.find(')') {
            let value = rest[..end].trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
static SPECTRAL_SCORER_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub fn reset_spectral_scorer_call_count_for_tests() {
    SPECTRAL_SCORER_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
pub fn spectral_scorer_call_count_for_tests() -> usize {
    SPECTRAL_SCORER_CALLS.load(std::sync::atomic::Ordering::SeqCst)
}

/// Run the full spectral scoring pipeline.
async fn run_spectral_scorer(path: &PathBuf) -> Result<PreemphasisResult, String> {
    #[cfg(test)]
    SPECTRAL_SCORER_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    // Probe for sample rate and channels.
    let info = tokio::task::spawn_blocking({
        let p = path.clone();
        move || crate::tui::probe::probe_audio(&p)
    })
    .await
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
    })
    .await
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
    let deemph_delta =
        scoring::virtual_deemphasis_score(&stft_result, &selected, &corpus, sample_rate);

    // Try to load trained classifier; fall back to legacy alpha if unavailable.
    let classifier = crate::db::Database::open()
        .ok()
        .and_then(|db| db.load_preemph_classifier().ok());

    let verdict = if let Some(ref clf) = classifier {
        scoring::compute_verdict_with_classifier(
            &model_scores,
            deemph_delta,
            &selected,
            &corpus,
            clf,
        )
    } else {
        #[allow(deprecated)]
        scoring::compute_verdict_legacy_alpha(&model_scores, deemph_delta, &selected, &corpus)
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

    // Log raw diagnostics for debugging (visible with RUST_LOG=debug).
    log::debug!(
        "spectral PE: z={:.2}, α={:.3}, r={:.3}, Δd={:.2}, frames={}, gates=[{}]",
        model_scores.z_score,
        model_scores.alpha,
        model_scores.pe_correlation,
        deemph_delta,
        selected.frames.len(),
        verdict.gates_fired.join(", "),
    );

    // User-facing detail: concise verdict without raw numbers.
    let detail = match verdict.confidence {
        PreemphasisConfidence::Possible | PreemphasisConfidence::StrongCandidate => {
            "spectral analysis suggests pre-emphasis boost may be present".to_string()
        }
        _ => "spectral analysis did not find pre-emphasis evidence".to_string(),
    };

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

#[cfg(test)]
mod tests {
    use super::*;

    fn spectral_positive_result(path: PathBuf) -> PreemphasisResult {
        PreemphasisResult {
            path,
            confidence: PreemphasisConfidence::Possible,
            cue_confirmed: false,
            llr_m2_vs_m0: 9.0,
            llr_m2_vs_m1: 8.0,
            fitted_alpha: 1.0,
            frames_scored: 128,
            deemph_distance_delta: 4.0,
            gates_fired: vec![],
            detail: "spectral analysis suggests pre-emphasis boost may be present".to_string(),
            spectral_rms_error: 0.0,
            crest_improvement: 0.0,
        }
    }

    #[test]
    fn metadata_editor_safe_result_rejects_spectral_positive() {
        let raw = spectral_positive_result(PathBuf::from("/tmp/spectral-only.flac"));
        let safe = metadata_editor_safe_result(&raw);
        assert_eq!(safe.confidence, PreemphasisConfidence::NotDetected);
        assert!(safe.detail.is_empty());
    }

    #[test]
    fn metadata_editor_safe_result_rejects_legacy_comment_and_log_evidence() {
        for detail in [
            "comment tag",
            "log file",
            "spectral analysis suggests pre-emphasis boost may be present",
        ] {
            let raw = PreemphasisResult {
                path: PathBuf::from(format!("/tmp/{detail}.flac")),
                confidence: PreemphasisConfidence::Detected,
                cue_confirmed: true,
                llr_m2_vs_m0: f64::NAN,
                llr_m2_vs_m1: f64::NAN,
                fitted_alpha: f64::NAN,
                frames_scored: 0,
                deemph_distance_delta: 0.0,
                gates_fired: vec![],
                detail: detail.to_string(),
                spectral_rms_error: 0.0,
                crest_improvement: 0.0,
            };
            let safe = metadata_editor_safe_result(&raw);
            assert_eq!(safe.confidence, PreemphasisConfidence::NotDetected, "{detail}");
            assert!(safe.detail.is_empty(), "{detail}");
        }
    }

    #[test]
    fn metadata_editor_safe_result_does_not_promote_contradictory_cached_labels() {
        for (confidence, detail) in [
            (PreemphasisConfidence::NotDetected, "tag"),
            (PreemphasisConfidence::Indeterminate, "PRE flag"),
            (PreemphasisConfidence::Detected, "catalog match: 35DP-4"),
        ] {
            let raw = PreemphasisResult {
                path: PathBuf::from(format!("/tmp/contradictory-{detail}.flac")),
                confidence,
                cue_confirmed: false,
                llr_m2_vs_m0: f64::NAN,
                llr_m2_vs_m1: f64::NAN,
                fitted_alpha: f64::NAN,
                frames_scored: 0,
                deemph_distance_delta: 0.0,
                gates_fired: vec![],
                detail: detail.to_string(),
                spectral_rms_error: 0.0,
                crest_improvement: 0.0,
            };
            let safe = metadata_editor_safe_result(&raw);
            assert_eq!(safe.confidence, PreemphasisConfidence::NotDetected, "{detail}");
            assert!(safe.detail.is_empty(), "{detail}");
        }
    }

    #[test]
    fn metadata_editor_safe_result_accepts_pre_flag_without_spectral() {
        let raw = PreemphasisResult {
            path: PathBuf::from("/tmp/pre-flag.flac"),
            confidence: PreemphasisConfidence::Detected,
            cue_confirmed: true,
            llr_m2_vs_m0: f64::NAN,
            llr_m2_vs_m1: f64::NAN,
            fitted_alpha: f64::NAN,
            frames_scored: 0,
            deemph_distance_delta: 0.0,
            gates_fired: vec![],
            detail: "PRE flag".to_string(),
            spectral_rms_error: 0.0,
            crest_improvement: 0.0,
        };
        let safe = metadata_editor_safe_result(&raw);
        assert_eq!(safe.confidence, PreemphasisConfidence::Detected);
        assert_eq!(safe.detail, "PRE flag");
    }

    #[test]
    fn metadata_label_catalog_becomes_candidate_without_spectral() {
        let safe = result_from_metadata_label(PathBuf::from("/tmp/catalog.flac"), "catalog (35DP-4)");
        assert_eq!(safe.confidence, PreemphasisConfidence::Possible);
        assert_eq!(safe.detail, "catalog match: 35DP-4");
    }

    #[test]
    fn folder_catalog_detection_retains_possible_source_and_exact_quality() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp
            .path()
            .join("Willie Nelson - Angel Eyes - 35DP-150");
        std::fs::create_dir(&album).expect("album directory");
        let path = album.join("track.flac");
        std::fs::write(&path, b"not real audio").expect("audio fixture");

        let advisory = detect_preemphasis_advisory(&path).expect("folder advisory");
        assert_eq!(advisory.evidence, PreemphasisAdvisoryEvidence::Catalog);
        assert_eq!(advisory.confidence, PreemphasisConfidence::Possible);
        let catalog = advisory.catalog.expect("catalog provenance");
        assert_eq!(catalog.catalog_number, "35DP-150");
        assert_eq!(catalog.quality, catalog::CatalogMatchQuality::Exact);
        assert_eq!(catalog.source, catalog::CatalogMatchSource::Folder);
    }

    #[test]
    fn metadata_editor_safe_result_rejects_legacy_catalog_series_labels() {
        let raw = empty_result(
            PathBuf::from("/tmp/legacy-series.flac"),
            PreemphasisConfidence::StrongCandidate,
            "catalog series".to_string(),
        );
        let safe = metadata_editor_safe_result(&raw);
        assert_eq!(safe.confidence, PreemphasisConfidence::NotDetected);
        assert!(safe.detail.is_empty());
    }

    #[test]
    fn metadata_editor_safe_result_preserves_catalog_match_confidence() {
        for confidence in [
            PreemphasisConfidence::StrongCandidate,
            PreemphasisConfidence::Possible,
        ] {
            let raw = empty_result(
                PathBuf::from("/tmp/catalog.flac"),
                confidence,
                "catalog match: 35DP-4".to_string(),
            );
            let safe = metadata_editor_safe_result(&raw);
            assert_eq!(safe.confidence, confidence);
            assert_eq!(safe.detail, "catalog match: 35DP-4");
        }
    }

    #[test]
    fn cached_advisory_gate_keeps_only_red_book_compatible_sources() {
        let advisory = PreemphasisAdvisory {
            evidence: PreemphasisAdvisoryEvidence::Catalog,
            confidence: PreemphasisConfidence::StrongCandidate,
            catalog: None,
            detail: "catalog".to_string(),
        };
        let mut info = crate::tui::probe::SourceInfo {
            format_name: "flac".to_string(),
            codec: "flac".to_string(),
            bit_depth: Some(16),
            sample_format_is_float: Some(false),
            sample_rate: 44_100,
            channels: 2,
            channel_layout: "stereo".to_string(),
            duration_secs: 1.0,
            file_size: 0,
        };
        assert!(preemphasis_advisory_for_source(Some(advisory.clone()), &info).is_some());

        for (depth, rate) in [(Some(24), 96_000), (Some(24), 44_100), (Some(16), 48_000)] {
            info.bit_depth = depth;
            info.sample_rate = rate;
            assert!(preemphasis_advisory_for_source(Some(advisory.clone()), &info).is_none());
        }
    }

    #[test]
    fn red_book_gate_rejects_known_high_resolution_sources() {
        let mut info = crate::tui::probe::SourceInfo {
            format_name: "flac".to_string(),
            codec: "flac".to_string(),
            bit_depth: Some(16),
            sample_format_is_float: Some(false),
            sample_rate: 44_100,
            channels: 2,
            channel_layout: "stereo".to_string(),
            duration_secs: 1.0,
            file_size: 0,
        };
        assert!(source_info_is_red_book_compatible(&info));

        info.sample_rate = 96_000;
        assert!(!source_info_is_red_book_compatible(&info));

        info.sample_rate = 44_100;
        info.bit_depth = Some(24);
        assert!(!source_info_is_red_book_compatible(&info));

        info.bit_depth = None;
        assert!(source_info_is_red_book_compatible(&info));

        info.codec = "mp3".to_string();
        info.format_name = "mp3".to_string();
        assert!(!source_info_is_red_book_compatible(&info));

        info.codec = "pcm_f32le".to_string();
        info.format_name = "wav".to_string();
        info.sample_format_is_float = Some(true);
        assert!(!source_info_is_red_book_compatible(&info));
    }

    #[test]
    fn metadata_label_accepts_only_explicit_pre_labels() {
        for label in ["tag", "PRE flag", "FLAGS PRE", "CUE file", "PRE_EMPHASIS"] {
            let safe = result_from_metadata_label(PathBuf::from("/tmp/pre-label.flac"), label);
            assert_eq!(safe.confidence, PreemphasisConfidence::Detected, "{label}");
            assert_eq!(safe.detail, "PRE flag", "{label}");
        }

        for label in ["comment tag", "log file", "spectral positive", "ordinary tag text", "unknown"] {
            let safe = result_from_metadata_label(PathBuf::from("/tmp/rejected-label.flac"), label);
            assert_eq!(safe.confidence, PreemphasisConfidence::NotDetected, "{label}");
            assert!(safe.detail.is_empty(), "{label}");
        }
    }

    #[test]
    fn metadata_catalog_detector_does_not_call_spectral_scorer() {
        reset_spectral_scorer_call_count_for_tests();
        let path = std::env::temp_dir().join(format!(
            "tonepoet-no-preemph-{}-{}.flac",
            std::process::id(),
            spectral_scorer_call_count_for_tests()
        ));
        std::fs::write(&path, b"not a real flac").expect("write test fixture");
        let result = detect_preemphasis_metadata_catalog(path.clone());
        let _ = std::fs::remove_file(path);

        assert_eq!(result.confidence, PreemphasisConfidence::NotDetected);
        assert_eq!(spectral_scorer_call_count_for_tests(), 0);
    }
}
