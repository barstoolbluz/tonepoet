//! Ablation ladder: compare detector variants at the same album-level FPR.
//!
//! A. deemph_delta only
//! B. deemph_delta + median(alpha)
//! C. deemph_delta + pe_correlation
//! D. deemph_delta + spread (q75 - q50)
//! E. current full model (6 track features → pooled)

use std::collections::BTreeMap;
use std::path::PathBuf;
use tonepoet::tui::preemphasis::{stft, frame_select, models, scoring, corpus};

struct TrackData {
    deemph_delta: f64,
    alpha: f64,        // quiet median
    pe_correlation: f64,
    q75: f64,
    spread: f64,       // q75 - q50
    frac_pos: f64,
    shape: models::TrackShapeFeatures,
    frame_count: usize,
}

fn compute_track(
    path: &PathBuf,
    corpus_model: &corpus::CorpusModel,
) -> Option<TrackData> {
    let info = tonepoet::tui::probe::probe_audio(path).ok()?;
    if info.sample_rate > 48000 { return None; }
    let stft_result = stft::compute_band_spectra(path, info.sample_rate).ok()?;
    let selected = frame_select::select_frames(&stft_result);
    if selected.frames.is_empty() { return None; }

    let model_scores = models::score_models(&selected, &stft_result, corpus_model);
    let deemph_delta = scoring::virtual_deemphasis_score(
        &stft_result, &selected, corpus_model, info.sample_rate,
    );
    let multi_alpha = models::compute_multi_alpha(&stft_result, &selected, corpus_model);
    let shape = models::TrackShapeFeatures::new(&multi_alpha, model_scores.pe_correlation, deemph_delta);

    Some(TrackData {
        deemph_delta,
        alpha: multi_alpha.quiet_median,
        pe_correlation: model_scores.pe_correlation,
        q75: multi_alpha.quiet_p75,
        spread: multi_alpha.quiet_p75 - multi_alpha.quiet_median,
        frac_pos: multi_alpha.fraction_positive_quiet,
        shape,
        frame_count: selected.frames.len(),
    })
}

fn album_name(path: &PathBuf) -> String {
    path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str())
        .unwrap_or("?").to_string()
}

fn collect_files(dir: &std::path::Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir).into_iter().flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("flac"))
        .map(|e| e.path().to_path_buf()).collect()
}

/// Simple logistic regression on arbitrary-dimension features.
/// Returns (weights, bias) after training.
fn train_logreg(
    samples: &[(&[f64], bool)],
    n_features: usize,
    lambda: f64,
) -> (Vec<f64>, f64) {
    let n = samples.len() as f64;
    let pos = samples.iter().filter(|(_, l)| *l).count() as f64;
    let neg = n - pos;
    let w_pos = n / (2.0 * pos.max(1.0));
    let w_neg = n / (2.0 * neg.max(1.0));

    // Standardize.
    let mut means = vec![0.0f64; n_features];
    let mut stds = vec![0.0f64; n_features];
    for (x, _) in samples { for i in 0..n_features { means[i] += x[i]; } }
    for m in means.iter_mut() { *m /= n; }
    for (x, _) in samples { for i in 0..n_features { stds[i] += (x[i] - means[i]).powi(2); } }
    for s in stds.iter_mut() { *s = (*s / (n - 1.0)).sqrt().max(1e-10); }

    let std_samples: Vec<(Vec<f64>, f64)> = samples.iter()
        .map(|(x, l)| {
            let z: Vec<f64> = (0..n_features).map(|i| (x[i] - means[i]) / stds[i]).collect();
            (z, if *l { 1.0 } else { 0.0 })
        }).collect();

    let mut w = vec![0.0f64; n_features];
    let mut b = 0.0f64;
    for _ in 0..500 {
        let mut gw = vec![0.0f64; n_features];
        let mut gb = 0.0f64;
        for (x, y) in &std_samples {
            let logit: f64 = b + (0..n_features).map(|i| w[i] * x[i]).sum::<f64>();
            let p = 1.0 / (1.0 + (-logit).exp());
            let cw = if *y > 0.5 { w_pos } else { w_neg };
            let err = (p - y) * cw;
            for i in 0..n_features { gw[i] += err * x[i]; }
            gb += err;
        }
        for i in 0..n_features { w[i] -= 0.1 * (gw[i] / n + lambda * w[i]); }
        b -= 0.1 * gb / n;
    }

    // Return in original scale for scoring.
    let mut w_orig = vec![0.0f64; n_features];
    let mut b_orig = b;
    for i in 0..n_features {
        w_orig[i] = w[i] / stds[i];
        b_orig -= w[i] * means[i] / stds[i];
    }
    (w_orig, b_orig)
}

fn score_logreg(x: &[f64], weights: &[f64], bias: f64) -> f64 {
    bias + x.iter().zip(weights.iter()).map(|(&xi, &wi)| xi * wi).sum::<f64>()
}

/// Evaluate a model at album level: returns (recall, fpr, n_pe_detected, n_fp).
fn evaluate_album_model(
    pe_albums: &BTreeMap<String, Vec<TrackData>>,
    np_albums: &BTreeMap<String, Vec<TrackData>>,
    track_feature_fn: &dyn Fn(&TrackData) -> Vec<f64>,
    n_features: usize,
    lambda: f64,
    target_album_fpr: f64,
) -> (f64, f64, usize, usize) {
    // Build track-level training data.
    let mut track_samples: Vec<(Vec<f64>, bool)> = Vec::new();
    for tracks in pe_albums.values() {
        for t in tracks { track_samples.push((track_feature_fn(t), true)); }
    }
    for tracks in np_albums.values() {
        for t in tracks { track_samples.push((track_feature_fn(t), false)); }
    }

    let refs: Vec<(&[f64], bool)> = track_samples.iter().map(|(x, l)| (x.as_slice(), *l)).collect();
    let (weights, bias) = train_logreg(&refs, n_features, lambda);

    // Score tracks → pool to albums → classify.
    let mut album_scores: Vec<(f64, bool)> = Vec::new(); // (median_track_score, is_pe)

    for (_, tracks) in pe_albums {
        let scores: Vec<f64> = tracks.iter()
            .map(|t| score_logreg(&track_feature_fn(t), &weights, bias)).collect();
        if scores.len() >= 3 {
            let mut s = scores.clone();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            album_scores.push((s[s.len() / 2], true));
        }
    }
    for (_, tracks) in np_albums {
        let scores: Vec<f64> = tracks.iter()
            .map(|t| score_logreg(&track_feature_fn(t), &weights, bias)).collect();
        if scores.len() >= 3 {
            let mut s = scores.clone();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            album_scores.push((s[s.len() / 2], false));
        }
    }

    // Find threshold for target album FPR.
    let mut candidates: Vec<f64> = album_scores.iter().map(|(s, _)| *s).collect();
    candidates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    candidates.dedup();

    let mut best_thresh = candidates.last().copied().unwrap_or(0.0) + 1.0;
    let mut best_tp = 0;
    let neg_total = album_scores.iter().filter(|(_, l)| !l).count();

    for &thresh in &candidates {
        let tp = album_scores.iter().filter(|(s, l)| *l && *s >= thresh).count();
        let fp = album_scores.iter().filter(|(s, l)| !l && *s >= thresh).count();
        let fpr = if neg_total > 0 { fp as f64 / neg_total as f64 } else { 0.0 };
        if fpr <= target_album_fpr && tp > best_tp {
            best_tp = tp;
            best_thresh = thresh;
        }
    }

    let tp = album_scores.iter().filter(|(s, l)| *l && *s >= best_thresh).count();
    let fp = album_scores.iter().filter(|(s, l)| !l && *s >= best_thresh).count();
    let pe_total = album_scores.iter().filter(|(_, l)| *l).count();
    let recall = if pe_total > 0 { tp as f64 / pe_total as f64 } else { 0.0 };
    let fpr = if neg_total > 0 { fp as f64 / neg_total as f64 } else { 0.0 };

    (recall, fpr, tp, fp)
}

#[tokio::test]
async fn ablation_ladder() {
    let pe_dir = dirs::home_dir().unwrap().join("preemph-dev/preemph");
    let non_pe_dir = dirs::home_dir().unwrap().join("preemph-dev/non-preemph");
    if !pe_dir.is_dir() || !non_pe_dir.is_dir() { eprintln!("SKIP"); return; }

    let corpus_model = match corpus::load_corpus() {
        Ok(c) => c, Err(e) => { eprintln!("SKIP: {}", e); return; }
    };

    // Score all tracks.
    println!("\n=== Scoring tracks ===");
    let mut pe_albums: BTreeMap<String, Vec<TrackData>> = BTreeMap::new();
    let mut np_albums: BTreeMap<String, Vec<TrackData>> = BTreeMap::new();

    for (label, dir, albums) in [
        ("PE", pe_dir.as_path(), &mut pe_albums),
        ("Non-PE", non_pe_dir.as_path(), &mut np_albums),
    ] {
        let files = collect_files(dir);
        println!("  {} {} tracks...", label, files.len());
        for path in &files {
            let album = album_name(path);
            if let Some(d) = tokio::task::spawn_blocking({
                let p = path.clone(); let c = corpus_model.clone();
                move || compute_track(&p, &c)
            }).await.unwrap() {
                albums.entry(album).or_default().push(d);
            }
        }
    }

    let pe_album_count = pe_albums.len();
    let np_album_count = np_albums.len();
    let target_fpr = 0.05;

    println!("\n=== ABLATION LADDER (target album FPR = {:.0}%) ===\n", target_fpr * 100.0);
    println!("{:45} {:>8} {:>8} {:>8} {:>8}", "Model", "PE det", "Recall", "FP", "FPR");
    println!("{}", "-".repeat(82));

    // A. deemph_delta only
    let (recall, fpr, tp, fp) = evaluate_album_model(
        &pe_albums, &np_albums,
        &|t: &TrackData| vec![t.deemph_delta],
        1, 0.1, target_fpr,
    );
    println!("{:45} {:>5}/{} {:>7.1}% {:>5}/{} {:>7.1}%",
        "A: deemph_delta only", tp, pe_album_count, recall * 100.0, fp, np_album_count, fpr * 100.0);

    // B. deemph_delta + median(alpha)
    let (recall, fpr, tp, fp) = evaluate_album_model(
        &pe_albums, &np_albums,
        &|t: &TrackData| vec![t.deemph_delta, t.alpha],
        2, 0.1, target_fpr,
    );
    println!("{:45} {:>5}/{} {:>7.1}% {:>5}/{} {:>7.1}%",
        "B: deemph_delta + alpha", tp, pe_album_count, recall * 100.0, fp, np_album_count, fpr * 100.0);

    // C. deemph_delta + pe_correlation
    let (recall, fpr, tp, fp) = evaluate_album_model(
        &pe_albums, &np_albums,
        &|t: &TrackData| vec![t.deemph_delta, t.pe_correlation],
        2, 0.1, target_fpr,
    );
    println!("{:45} {:>5}/{} {:>7.1}% {:>5}/{} {:>7.1}%",
        "C: deemph_delta + pe_correlation", tp, pe_album_count, recall * 100.0, fp, np_album_count, fpr * 100.0);

    // D. deemph_delta + spread (q75 - q50)
    let (recall, fpr, tp, fp) = evaluate_album_model(
        &pe_albums, &np_albums,
        &|t: &TrackData| vec![t.deemph_delta, t.spread],
        2, 0.1, target_fpr,
    );
    println!("{:45} {:>5}/{} {:>7.1}% {:>5}/{} {:>7.1}%",
        "D: deemph_delta + spread", tp, pe_album_count, recall * 100.0, fp, np_album_count, fpr * 100.0);

    // E. deemph_delta + frac_pos
    let (recall, fpr, tp, fp) = evaluate_album_model(
        &pe_albums, &np_albums,
        &|t: &TrackData| vec![t.deemph_delta, t.frac_pos],
        2, 0.1, target_fpr,
    );
    println!("{:45} {:>5}/{} {:>7.1}% {:>5}/{} {:>7.1}%",
        "E: deemph_delta + frac_pos", tp, pe_album_count, recall * 100.0, fp, np_album_count, fpr * 100.0);

    // F. Full shape model (6 features → pooled)
    let (recall, fpr, tp, fp) = evaluate_album_model(
        &pe_albums, &np_albums,
        &|t: &TrackData| t.shape.features.to_vec(),
        6, 0.1, target_fpr,
    );
    println!("{:45} {:>5}/{} {:>7.1}% {:>5}/{} {:>7.1}%",
        "F: full shape (6 features)", tp, pe_album_count, recall * 100.0, fp, np_album_count, fpr * 100.0);

    // G. deemph_delta + alpha + pe_correlation (3 features)
    let (recall, fpr, tp, fp) = evaluate_album_model(
        &pe_albums, &np_albums,
        &|t: &TrackData| vec![t.deemph_delta, t.alpha, t.pe_correlation],
        3, 0.1, target_fpr,
    );
    println!("{:45} {:>5}/{} {:>7.1}% {:>5}/{} {:>7.1}%",
        "G: deemph + alpha + pe_corr", tp, pe_album_count, recall * 100.0, fp, np_album_count, fpr * 100.0);
}
