//! Veto experiment: can a secondary model remove persistent false positives
//! from the main two-stage detector without losing PE recall?
//!
//! Systems compared:
//! A. deemph_delta-only album detector
//! B. current two-stage detector (track shape → pooled → album)
//! C. two-stage + veto model
//! D. 2-expert stack (deemph-only + two-stage → meta-model)
//!
//! All evaluation uses grouped CV by album with no leakage.

use std::collections::BTreeMap;
use std::path::PathBuf;
use tonepoet::tui::preemphasis::{corpus, frame_select, models, scoring, stft};

// ── Track-level data ───────────────────────────────────────────────

struct TrackData {
    deemph_delta: f64,
    alpha: f64,
    pe_correlation: f64,
    q75: f64,
    spread: f64,
    frac_pos: f64,
    shape: models::TrackShapeFeatures,
    frame_count: usize,
}

fn compute_track(path: &PathBuf, cm: &corpus::CorpusModel) -> Option<TrackData> {
    let info = tonepoet::tui::probe::probe_audio(path).ok()?;
    if info.sample_rate > 48000 {
        return None;
    }
    let sr = stft::compute_band_spectra(path, info.sample_rate).ok()?;
    let sel = frame_select::select_frames(&sr);
    if sel.frames.is_empty() {
        return None;
    }
    let ms = models::score_models(&sel, &sr, cm);
    let dd = scoring::virtual_deemphasis_score(&sr, &sel, cm, info.sample_rate);
    let ma = models::compute_multi_alpha(&sr, &sel, cm);
    let shape = models::TrackShapeFeatures::new(&ma, ms.pe_correlation, dd);
    Some(TrackData {
        deemph_delta: dd,
        alpha: ma.quiet_median,
        pe_correlation: ms.pe_correlation,
        q75: ma.quiet_p75,
        spread: ma.quiet_p75 - ma.quiet_median,
        frac_pos: ma.fraction_positive_quiet,
        shape,
        frame_count: sel.frames.len(),
    })
}

// ── Album-level data ───────────────────────────────────────────────

struct AlbumData {
    name: String,
    is_pe: bool,
    tracks: Vec<TrackData>,
}

impl AlbumData {
    fn deemph_median(&self) -> f64 {
        let mut v: Vec<f64> = self.tracks.iter().map(|t| t.deemph_delta).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if v.is_empty() {
            return f64::NAN;
        }
        v[v.len() / 2]
    }

    fn alpha_median(&self) -> f64 {
        let mut v: Vec<f64> = self.tracks.iter().map(|t| t.alpha).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if v.is_empty() {
            return f64::NAN;
        }
        v[v.len() / 2]
    }

    fn pe_corr_median(&self) -> f64 {
        let mut v: Vec<f64> = self.tracks.iter().map(|t| t.pe_correlation).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if v.is_empty() {
            return f64::NAN;
        }
        v[v.len() / 2]
    }

    fn frac_pos_alpha(&self) -> f64 {
        let n = self.tracks.len() as f64;
        if n == 0.0 {
            return 0.0;
        }
        self.tracks.iter().filter(|t| t.alpha > 0.0).count() as f64 / n
    }

    fn alpha_iqr(&self) -> f64 {
        let mut v: Vec<f64> = self.tracks.iter().map(|t| t.alpha).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if v.len() < 4 {
            return 0.0;
        }
        v[3 * v.len() / 4] - v[v.len() / 4]
    }

    fn usable(&self) -> bool {
        self.tracks.len() >= 3
    }
}

// ── Simple logistic regression ─────────────────────────────────────

fn train_logreg(samples: &[(&[f64], bool)], nf: usize, lambda: f64) -> (Vec<f64>, f64) {
    let n = samples.len() as f64;
    if n < 2.0 {
        return (vec![0.0; nf], 0.0);
    }
    let pos = samples.iter().filter(|(_, l)| *l).count() as f64;
    let neg = n - pos;
    let wp = n / (2.0 * pos.max(1.0));
    let wn = n / (2.0 * neg.max(1.0));

    let mut means = vec![0.0; nf];
    let mut stds = vec![0.0; nf];
    for (x, _) in samples {
        for i in 0..nf {
            means[i] += x[i];
        }
    }
    for m in means.iter_mut() {
        *m /= n;
    }
    for (x, _) in samples {
        for i in 0..nf {
            stds[i] += (x[i] - means[i]).powi(2);
        }
    }
    for s in stds.iter_mut() {
        *s = (*s / (n - 1.0)).sqrt().max(1e-10);
    }

    let zs: Vec<(Vec<f64>, f64)> = samples
        .iter()
        .map(|(x, l)| {
            (
                (0..nf).map(|i| (x[i] - means[i]) / stds[i]).collect(),
                if *l { 1.0 } else { 0.0 },
            )
        })
        .collect();

    let mut w = vec![0.0; nf];
    let mut b = 0.0;
    for _ in 0..500 {
        let mut gw = vec![0.0; nf];
        let mut gb = 0.0;
        for (x, y) in &zs {
            let logit: f64 = b + (0..nf).map(|i| w[i] * x[i]).sum::<f64>();
            let p = 1.0 / (1.0 + (-logit).exp());
            let cw = if *y > 0.5 { wp } else { wn };
            let e = (p - y) * cw;
            for i in 0..nf {
                gw[i] += e * x[i];
            }
            gb += e;
        }
        for i in 0..nf {
            w[i] -= 0.1 * (gw[i] / n + lambda * w[i]);
        }
        b -= 0.1 * gb / n;
    }

    let mut wo = vec![0.0; nf];
    let mut bo = b;
    for i in 0..nf {
        wo[i] = w[i] / stds[i];
        bo -= w[i] * means[i] / stds[i];
    }
    (wo, bo)
}

fn score_lr(x: &[f64], w: &[f64], b: f64) -> f64 {
    b + x
        .iter()
        .zip(w.iter())
        .map(|(&xi, &wi)| xi * wi)
        .sum::<f64>()
}

// ── Detector A: deemph_delta only ──────────────────────────────────

fn detector_a_score(album: &AlbumData) -> f64 {
    album.deemph_median()
}

// ── Detector B: two-stage (track shape → pooled) ───────────────────

fn detector_b_train(train_albums: &[&AlbumData]) -> (Vec<f64>, f64) {
    let mut samples: Vec<(Vec<f64>, bool)> = Vec::new();
    for a in train_albums {
        for t in &a.tracks {
            samples.push((t.shape.features.to_vec(), a.is_pe));
        }
    }
    let refs: Vec<(&[f64], bool)> = samples.iter().map(|(x, l)| (x.as_slice(), *l)).collect();
    train_logreg(&refs, models::NUM_SHAPE_FEATURES, 0.1)
}

fn detector_b_album_score(album: &AlbumData, w: &[f64], b: f64) -> f64 {
    let mut scores: Vec<f64> = album
        .tracks
        .iter()
        .map(|t| score_lr(&t.shape.features, w, b))
        .collect();
    scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if scores.is_empty() {
        return f64::NAN;
    }
    scores[scores.len() / 2] // median track score
}

// ── Veto model ─────────────────────────────────────────────────────

/// Veto features for an album that the main detector flagged.
/// Designed to separate "real PE" from "bright non-PE that fooled the main detector."
fn veto_features(album: &AlbumData, main_score: f64, deemph_score: f64) -> Vec<f64> {
    vec![
        main_score,                // how confident the main detector was
        deemph_score,              // deemph-only score
        album.alpha_median(),      // alpha evidence
        album.frac_pos_alpha(),    // consistency of alpha
        album.pe_corr_median(),    // spectral shape match
        main_score - deemph_score, // agreement between detectors
    ]
}

const VETO_NF: usize = 6;

// ── Threshold tuning ───────────────────────────────────────────────

fn tune_threshold(scores: &[(f64, bool)], target_fpr: f64) -> f64 {
    let mut cands: Vec<f64> = scores.iter().map(|(s, _)| *s).collect();
    cands.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    cands.dedup();
    let neg = scores.iter().filter(|(_, l)| !l).count();
    let mut best_t = cands.last().copied().unwrap_or(0.0) + 1.0;
    let mut best_tp = 0;
    for &t in &cands {
        let tp = scores.iter().filter(|(s, l)| *l && *s >= t).count();
        let fp = scores.iter().filter(|(s, l)| !l && *s >= t).count();
        let fpr = if neg > 0 { fp as f64 / neg as f64 } else { 0.0 };
        if fpr <= target_fpr && tp > best_tp {
            best_tp = tp;
            best_t = t;
        }
    }
    best_t
}

// ── Evaluation ─────────────────────────────────────────────────────

struct FoldResult {
    // Per album: (name, is_pe, score_a, score_b, flagged_b, vetoed, score_stack)
    albums: Vec<AlbumResult>,
}

struct AlbumResult {
    name: String,
    is_pe: bool,
    score_a: f64,
    score_b: f64,
    flagged_b: bool,
    vetoed: bool,
    score_stack: f64,
}

fn collect_files(dir: &std::path::Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("flac"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

fn album_name(path: &PathBuf) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string()
}

fn album_key(name: &str) -> String {
    if let Some(pos) = name.find('(') {
        name[..pos].trim().to_string()
    } else {
        name.to_string()
    }
}

#[tokio::test]
async fn veto_experiment() {
    let pe_dir = dirs::home_dir().unwrap().join("preemph-dev/preemph");
    let np_dir = dirs::home_dir().unwrap().join("preemph-dev/non-preemph");
    if !pe_dir.is_dir() || !np_dir.is_dir() {
        eprintln!("SKIP");
        return;
    }

    let cm = match corpus::load_corpus() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: {}", e);
            return;
        }
    };

    // Score all tracks.
    println!("\n=== Scoring tracks ===");
    let mut albums: Vec<AlbumData> = Vec::new();

    for (is_pe, dir) in [(true, pe_dir.as_path()), (false, np_dir.as_path())] {
        let files = collect_files(dir);
        let label = if is_pe { "PE" } else { "Non-PE" };
        println!("  {} {} tracks...", label, files.len());

        let mut by_album: BTreeMap<String, Vec<TrackData>> = BTreeMap::new();
        for path in &files {
            let name = album_name(path);
            if let Some(td) = tokio::task::spawn_blocking({
                let p = path.clone();
                let c = cm.clone();
                move || compute_track(&p, &c)
            })
            .await
            .unwrap()
            {
                by_album.entry(name).or_default().push(td);
            }
        }
        for (name, tracks) in by_album {
            albums.push(AlbumData {
                name,
                is_pe,
                tracks,
            });
        }
    }

    let usable: Vec<&AlbumData> = albums.iter().filter(|a| a.usable()).collect();
    let pe_count = usable.iter().filter(|a| a.is_pe).count();
    let np_count = usable.iter().filter(|a| !a.is_pe).count();
    println!(
        "\n  {} usable albums: {} PE, {} non-PE",
        usable.len(),
        pe_count,
        np_count
    );

    // ── 3-fold grouped CV ──────────────────────────────────────────

    let target_fpr = 0.05;
    let k_folds = 3;

    // Round-robin fold assignment by class.
    let mut pe_albums: Vec<&AlbumData> = usable.iter().filter(|a| a.is_pe).copied().collect();
    let mut np_albums: Vec<&AlbumData> = usable.iter().filter(|a| !a.is_pe).copied().collect();
    pe_albums.sort_by(|a, b| a.name.cmp(&b.name));
    np_albums.sort_by(|a, b| a.name.cmp(&b.name));

    let mut fold_map: BTreeMap<String, usize> = BTreeMap::new();
    for (i, a) in pe_albums.iter().enumerate() {
        fold_map.insert(a.name.clone(), i % k_folds);
    }
    for (i, a) in np_albums.iter().enumerate() {
        fold_map.insert(a.name.clone(), i % k_folds);
    }

    // Collect OOF results.
    let mut all_results: Vec<AlbumResult> = Vec::new();

    for fold in 0..k_folds {
        let train: Vec<&AlbumData> = usable
            .iter()
            .filter(|a| fold_map.get(&a.name) != Some(&fold))
            .copied()
            .collect();
        let test: Vec<&AlbumData> = usable
            .iter()
            .filter(|a| fold_map.get(&a.name) == Some(&fold))
            .copied()
            .collect();

        // Train detector A threshold on train.
        let a_train_scores: Vec<(f64, bool)> = train
            .iter()
            .map(|a| (detector_a_score(a), a.is_pe))
            .collect();
        let a_thresh = tune_threshold(&a_train_scores, target_fpr);

        // Train detector B on train.
        let (b_weights, b_bias) = detector_b_train(&train);
        let b_train_scores: Vec<(f64, bool)> = train
            .iter()
            .map(|a| (detector_b_album_score(a, &b_weights, b_bias), a.is_pe))
            .collect();
        let b_thresh = tune_threshold(&b_train_scores, target_fpr);

        // Build veto training set: train albums flagged by detector B.
        let mut veto_train: Vec<(Vec<f64>, bool)> = Vec::new();
        for a in &train {
            let b_score = detector_b_album_score(a, &b_weights, b_bias);
            if b_score >= b_thresh {
                let a_score = detector_a_score(a);
                veto_train.push((veto_features(a, b_score, a_score), a.is_pe));
            }
        }

        let veto_pos = veto_train.iter().filter(|(_, l)| *l).count();
        let veto_neg = veto_train.iter().filter(|(_, l)| !l).count();

        // Train veto model (if enough data).
        let veto_model: Option<(Vec<f64>, f64, f64)> = if veto_pos >= 3 && veto_neg >= 1 {
            let refs: Vec<(&[f64], bool)> =
                veto_train.iter().map(|(x, l)| (x.as_slice(), *l)).collect();
            let (vw, vb) = train_logreg(&refs, VETO_NF, 0.3); // Higher lambda for tiny dataset.
                                                              // Veto threshold: reject if veto score < 0 (predicts non-PE).
            Some((vw, vb, 0.0))
        } else {
            None
        };

        // Train stack meta-model on OOF base scores from train.
        let stack_samples: Vec<(Vec<f64>, bool)> = train
            .iter()
            .map(|a| {
                let sa = detector_a_score(a);
                let sb = detector_b_album_score(a, &b_weights, b_bias);
                (vec![sa, sb], a.is_pe)
            })
            .collect();
        let stack_refs: Vec<(&[f64], bool)> = stack_samples
            .iter()
            .map(|(x, l)| (x.as_slice(), *l))
            .collect();
        let (sw, sb_stack) = train_logreg(&stack_refs, 2, 0.1);
        let stack_train_scores: Vec<(f64, bool)> = train
            .iter()
            .map(|a| {
                let sa = detector_a_score(a);
                let s_b = detector_b_album_score(a, &b_weights, b_bias);
                (score_lr(&[sa, s_b], &sw, sb_stack), a.is_pe)
            })
            .collect();
        let stack_thresh = tune_threshold(&stack_train_scores, target_fpr);

        // Score test albums.
        for a in &test {
            let score_a = detector_a_score(a);
            let score_b = detector_b_album_score(a, &b_weights, b_bias);
            let flagged_b = score_b >= b_thresh;

            let vetoed = if flagged_b {
                if let Some((ref vw, vb, vt)) = veto_model {
                    let vf = veto_features(a, score_b, score_a);
                    score_lr(&vf, vw, vb) < vt // Veto if score below threshold.
                } else {
                    false
                }
            } else {
                false
            };

            let score_stack = score_lr(&[score_a, score_b], &sw, sb_stack);

            all_results.push(AlbumResult {
                name: a.name.clone(),
                is_pe: a.is_pe,
                score_a,
                score_b,
                flagged_b,
                vetoed,
                score_stack,
            });
        }
    }

    // ── Evaluate all systems ───────────────────────────────────────

    // Tune global thresholds on OOF scores for systems A and D.
    let a_oof: Vec<(f64, bool)> = all_results.iter().map(|r| (r.score_a, r.is_pe)).collect();
    let a_global_thresh = tune_threshold(&a_oof, target_fpr);
    let d_oof: Vec<(f64, bool)> = all_results
        .iter()
        .map(|r| (r.score_stack, r.is_pe))
        .collect();
    let d_global_thresh = tune_threshold(&d_oof, target_fpr);

    println!(
        "\n=== RESULTS (target album FPR = {:.0}%) ===\n",
        target_fpr * 100.0
    );
    println!(
        "{:8} {:>8} {:>8} {:>8} {:>8}",
        "System", "PE det", "Recall", "FP", "FPR"
    );
    println!("{}", "-".repeat(45));

    // A: deemph-only.
    let a_tp = all_results
        .iter()
        .filter(|r| r.is_pe && r.score_a >= a_global_thresh)
        .count();
    let a_fp = all_results
        .iter()
        .filter(|r| !r.is_pe && r.score_a >= a_global_thresh)
        .count();
    println!(
        "{:8} {:>5}/{} {:>7.1}% {:>5}/{} {:>7.1}%",
        "A",
        a_tp,
        pe_count,
        a_tp as f64 / pe_count as f64 * 100.0,
        a_fp,
        np_count,
        a_fp as f64 / np_count as f64 * 100.0
    );

    // B: two-stage (per-fold threshold, OOF evaluation).
    let b_tp = all_results
        .iter()
        .filter(|r| r.is_pe && r.flagged_b)
        .count();
    let b_fp = all_results
        .iter()
        .filter(|r| !r.is_pe && r.flagged_b)
        .count();
    println!(
        "{:8} {:>5}/{} {:>7.1}% {:>5}/{} {:>7.1}%",
        "B",
        b_tp,
        pe_count,
        b_tp as f64 / pe_count as f64 * 100.0,
        b_fp,
        np_count,
        b_fp as f64 / np_count as f64 * 100.0
    );

    // C: two-stage + veto.
    let c_tp = all_results
        .iter()
        .filter(|r| r.is_pe && r.flagged_b && !r.vetoed)
        .count();
    let c_fp = all_results
        .iter()
        .filter(|r| !r.is_pe && r.flagged_b && !r.vetoed)
        .count();
    let c_vetoed_pe = all_results
        .iter()
        .filter(|r| r.is_pe && r.flagged_b && r.vetoed)
        .count();
    let c_vetoed_np = all_results
        .iter()
        .filter(|r| !r.is_pe && r.flagged_b && r.vetoed)
        .count();
    println!(
        "{:8} {:>5}/{} {:>7.1}% {:>5}/{} {:>7.1}%   (vetoed: {} PE, {} FP)",
        "C",
        c_tp,
        pe_count,
        c_tp as f64 / pe_count as f64 * 100.0,
        c_fp,
        np_count,
        c_fp as f64 / np_count as f64 * 100.0,
        c_vetoed_pe,
        c_vetoed_np
    );

    // D: 2-expert stack.
    let d_tp = all_results
        .iter()
        .filter(|r| r.is_pe && r.score_stack >= d_global_thresh)
        .count();
    let d_fp = all_results
        .iter()
        .filter(|r| !r.is_pe && r.score_stack >= d_global_thresh)
        .count();
    println!(
        "{:8} {:>5}/{} {:>7.1}% {:>5}/{} {:>7.1}%",
        "D",
        d_tp,
        pe_count,
        d_tp as f64 / pe_count as f64 * 100.0,
        d_fp,
        np_count,
        d_fp as f64 / np_count as f64 * 100.0
    );

    // Detail: which FPs were removed by veto?
    println!("\n=== FALSE POSITIVE DETAIL ===");
    let b_fps: Vec<&AlbumResult> = all_results
        .iter()
        .filter(|r| !r.is_pe && r.flagged_b)
        .collect();
    for fp in &b_fps {
        let status = if fp.vetoed { "VETOED" } else { "kept" };
        println!(
            "  {:55} B_score={:+.3} veto={}",
            fp.name, fp.score_b, status
        );
    }

    // Matched pairs.
    println!("\n=== MATCHED PAIRS ===");
    println!("{:45} {:>5} {:>5} {:>5} {:>5}", "Album", "A", "B", "C", "D");
    let pe_results: Vec<&AlbumResult> = all_results.iter().filter(|r| r.is_pe).collect();
    let np_results: Vec<&AlbumResult> = all_results.iter().filter(|r| !r.is_pe).collect();

    for pe_r in &pe_results {
        let pe_key = album_key(&pe_r.name);
        for np_r in &np_results {
            if album_key(&np_r.name) == pe_key {
                let a_pe = if pe_r.score_a >= a_global_thresh {
                    "+"
                } else {
                    "-"
                };
                let a_np = if np_r.score_a >= a_global_thresh {
                    "FP"
                } else {
                    "-"
                };
                let b_pe = if pe_r.flagged_b { "+" } else { "-" };
                let b_np = if np_r.flagged_b { "FP" } else { "-" };
                let c_pe = if pe_r.flagged_b && !pe_r.vetoed {
                    "+"
                } else {
                    "-"
                };
                let c_np = if np_r.flagged_b && !np_r.vetoed {
                    "FP"
                } else {
                    "-"
                };
                let d_pe = if pe_r.score_stack >= d_global_thresh {
                    "+"
                } else {
                    "-"
                };
                let d_np = if np_r.score_stack >= d_global_thresh {
                    "FP"
                } else {
                    "-"
                };
                println!(
                    "  {:43} {}/{} {}/{} {}/{} {}/{}",
                    &pe_key[..pe_key.len().min(43)],
                    a_pe,
                    a_np,
                    b_pe,
                    b_np,
                    c_pe,
                    c_np,
                    d_pe,
                    d_np
                );
            }
        }
    }

    // Recommendation.
    println!("\n=== RECOMMENDATION ===");
    if c_fp < b_fp && c_tp >= b_tp - 1 {
        println!("  Ship C (two-stage + veto): removes FPs with minimal recall cost.");
    } else if d_tp > b_tp && d_fp <= b_fp {
        println!("  Ship D (2-expert stack): better recall at same FPR.");
    } else {
        println!("  Ship B (two-stage): veto/stack didn't help enough.");
    }
}
