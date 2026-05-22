//! Forensic error analysis: why do 23 PE albums get missed?
//! Compares detected vs missed PE albums on track-level feature distributions,
//! frame selection characteristics, and aggregation sensitivity.

use std::collections::BTreeMap;
use std::path::PathBuf;
use tonepoet::tui::preemphasis::{
    corpus, frame_select, models, scoring, stft, PreemphasisConfidence,
};

/// Per-track diagnostic record (richer than TrackSummary).
#[derive(Debug, Clone)]
struct TrackDiag {
    path: PathBuf,
    alpha: f64,
    pe_correlation: f64,
    deemph_delta: f64,
    frame_count: usize,
    alpha_stability: f64,
    /// Alpha computed from ALL frames (not just quiet ones).
    alpha_all_frames: f64,
    /// Alpha from top-quartile PE-like frames only.
    alpha_top_quartile: f64,
    /// Fraction of frames with positive alpha projection.
    fraction_positive_frames: f64,
    /// 75th percentile of per-frame alpha.
    alpha_p75: f64,
}

fn diagnose_track(path: &PathBuf, corpus_model: &corpus::CorpusModel) -> Option<TrackDiag> {
    let info = tonepoet::tui::probe::probe_audio(path).ok()?;
    if info.sample_rate > 48000 {
        return None;
    }

    let stft_result = stft::compute_band_spectra(path, info.sample_rate).ok()?;
    let selected = frame_select::select_frames(&stft_result);
    if selected.frames.is_empty() {
        return None;
    }

    // Standard scoring (quiet frames only).
    let model_scores = models::score_models(&selected, &stft_result, corpus_model);
    let deemph_delta =
        scoring::virtual_deemphasis_score(&stft_result, &selected, corpus_model, info.sample_rate);

    // Per-frame alpha distribution (quiet frames).
    let mask = models::usable_band_mask(stft_result.sample_rate);
    let pe_template = get_pe_template_masked(corpus_model, &mask);
    let per_frame_alphas = compute_per_frame_alphas(
        &selected.frames,
        &stft_result,
        corpus_model,
        &mask,
        &pe_template,
    );

    let fraction_positive = per_frame_alphas.iter().filter(|&&a| a > 0.0).count() as f64
        / per_frame_alphas.len().max(1) as f64;

    let mut sorted_alphas = per_frame_alphas.clone();
    sorted_alphas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted_alphas.len();
    let alpha_p75 = if n > 0 { sorted_alphas[3 * n / 4] } else { 0.0 };

    // Top quartile mean alpha.
    let top_q_start = 3 * n / 4;
    let alpha_top_quartile = if top_q_start < n {
        sorted_alphas[top_q_start..].iter().sum::<f64>() / (n - top_q_start) as f64
    } else {
        0.0
    };

    // All-frames alpha (not just quiet frames).
    let all_frame_indices: Vec<usize> = (0..stft_result.band_spectra.len()).collect();
    let all_frame_alphas = compute_per_frame_alphas(
        &all_frame_indices,
        &stft_result,
        corpus_model,
        &mask,
        &pe_template,
    );
    let alpha_all_frames = if !all_frame_alphas.is_empty() {
        let mut sorted = all_frame_alphas.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        sorted[sorted.len() / 2] // median
    } else {
        0.0
    };

    Some(TrackDiag {
        path: path.clone(),
        alpha: model_scores.alpha,
        pe_correlation: model_scores.pe_correlation,
        deemph_delta,
        frame_count: selected.frames.len(),
        alpha_stability: model_scores.alpha_stability,
        alpha_all_frames,
        alpha_top_quartile,
        fraction_positive_frames: fraction_positive,
        alpha_p75,
    })
}

/// Get PE template (empirical if available, else theoretical) masked to usable bands.
fn get_pe_template_masked(
    corpus_model: &corpus::CorpusModel,
    mask: &[bool; stft::NUM_BANDS],
) -> Vec<f64> {
    let pe = if let Some(ref emp) = corpus_model.empirical_pe_template {
        *emp
    } else {
        models::pe_curve()
    };
    pe.iter()
        .zip(mask.iter())
        .filter(|(_, &m)| m)
        .map(|(&v, _)| v)
        .collect()
}

/// Compute per-frame alpha (projection onto PE template after intercept+tilt removal).
fn compute_per_frame_alphas(
    frame_indices: &[usize],
    stft_result: &stft::StftResult,
    corpus_model: &corpus::CorpusModel,
    mask: &[bool; stft::NUM_BANDS],
    pe_template: &[f64],
) -> Vec<f64> {
    let mut alphas = Vec::with_capacity(frame_indices.len());

    for &idx in frame_indices {
        let spectrum = &stft_result.band_spectra[idx];
        let mut diff = [0.0; stft::NUM_BANDS];
        for k in 0..stft::NUM_BANDS {
            diff[k] = spectrum[k] - corpus_model.mean[k];
        }
        let data: Vec<f64> = diff
            .iter()
            .zip(mask.iter())
            .filter(|(_, &m)| m)
            .map(|(&v, _)| v)
            .collect();

        // Remove intercept + tilt.
        let n = data.len() as f64;
        if n < 2.0 {
            continue;
        }
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
        let residual: Vec<f64> = data
            .iter()
            .enumerate()
            .map(|(i, &y)| y - slope * i as f64 - intercept)
            .collect();

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

fn album_name(path: &PathBuf) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

#[tokio::test]
async fn error_analysis() {
    let pe_dir = dirs::home_dir().unwrap().join("preemph-dev/preemph");
    if !pe_dir.is_dir() {
        eprintln!("SKIP");
        return;
    }

    let corpus_model = match corpus::load_corpus() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: {}", e);
            return;
        }
    };

    let collect_files = |dir: &std::path::Path| -> Vec<PathBuf> {
        walkdir::WalkDir::new(dir)
            .into_iter()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("flac"))
            .map(|e| e.path().to_path_buf())
            .collect()
    };

    // Score all PE tracks.
    println!("\n=== Scoring PE tracks for error analysis ===");
    let pe_files = collect_files(&pe_dir);
    let mut albums: BTreeMap<String, Vec<TrackDiag>> = BTreeMap::new();

    for path in &pe_files {
        let album = album_name(path);
        if let Some(diag) = tokio::task::spawn_blocking({
            let p = path.clone();
            let c = corpus_model.clone();
            move || diagnose_track(&p, &c)
        })
        .await
        .unwrap()
        {
            albums.entry(album).or_default().push(diag);
        }
    }

    // Classify albums as detected vs missed using soft rules.
    let mut detected: Vec<(String, Vec<TrackDiag>)> = Vec::new();
    let mut missed: Vec<(String, Vec<TrackDiag>)> = Vec::new();

    for (album, tracks) in &albums {
        let mut alphas: Vec<f64> = tracks.iter().map(|t| t.alpha).collect();
        alphas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median_alpha = if alphas.len() % 2 == 1 {
            alphas[alphas.len() / 2]
        } else {
            (alphas[alphas.len() / 2 - 1] + alphas[alphas.len() / 2]) / 2.0
        };
        let frac_pos = tracks.iter().filter(|t| t.alpha > 0.0).count() as f64 / tracks.len() as f64;

        let is_detected = median_alpha > 0.0 && frac_pos >= 0.60 && tracks.len() >= 3;

        if is_detected {
            detected.push((album.clone(), tracks.clone()));
        } else {
            missed.push((album.clone(), tracks.clone()));
        }
    }

    println!(
        "\n=== {} detected, {} missed PE albums ===",
        detected.len(),
        missed.len()
    );

    // Compare distributions.
    println!("\n=== DETECTED PE ALBUMS ===");
    println!(
        "{:55} {:>5} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "Album", "Trk", "α_med", "α_p75", "α_top", "α_all", "frac+", "pe_cor"
    );
    println!("{}", "-".repeat(110));
    for (album, tracks) in &detected {
        print_album_summary(album, tracks);
    }

    println!("\n=== MISSED PE ALBUMS ===");
    println!(
        "{:55} {:>5} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "Album", "Trk", "α_med", "α_p75", "α_top", "α_all", "frac+", "pe_cor"
    );
    println!("{}", "-".repeat(110));
    for (album, tracks) in &missed {
        print_album_summary(album, tracks);
    }

    // Aggregate comparison.
    println!("\n=== AGGREGATE COMPARISON ===");
    print_aggregate("Detected", &detected);
    print_aggregate("Missed", &missed);

    // Frame selection ablation: what if we used all frames instead of quiet-only?
    println!("\n=== FRAME SELECTION ABLATION ===");
    println!("How many missed albums would be recovered using all-frame alpha instead of quiet-frame alpha?");
    let mut recovered = 0;
    for (album, tracks) in &missed {
        let mut all_alphas: Vec<f64> = tracks.iter().map(|t| t.alpha_all_frames).collect();
        all_alphas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median_all = if all_alphas.len() % 2 == 1 {
            all_alphas[all_alphas.len() / 2]
        } else {
            (all_alphas[all_alphas.len() / 2 - 1] + all_alphas[all_alphas.len() / 2]) / 2.0
        };
        let frac_pos_all =
            tracks.iter().filter(|t| t.alpha_all_frames > 0.0).count() as f64 / tracks.len() as f64;
        let would_detect = median_all > 0.0 && frac_pos_all >= 0.60 && tracks.len() >= 3;
        if would_detect {
            recovered += 1;
            let short = &album[..album.len().min(55)];
            println!(
                "  RECOVERED: {:55} median_all={:+.3} frac+={:.0}%",
                short,
                median_all,
                frac_pos_all * 100.0
            );
        }
    }
    println!(
        "  {} of {} missed albums recovered with all-frame alpha",
        recovered,
        missed.len()
    );

    // Top-quartile ablation.
    println!("\n=== TOP-QUARTILE ABLATION ===");
    println!("How many missed albums would be recovered using p75/top-quartile alpha?");
    let mut recovered_tq = 0;
    for (album, tracks) in &missed {
        let mut tq_alphas: Vec<f64> = tracks.iter().map(|t| t.alpha_p75).collect();
        tq_alphas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median_tq = if tq_alphas.len() % 2 == 1 {
            tq_alphas[tq_alphas.len() / 2]
        } else {
            (tq_alphas[tq_alphas.len() / 2 - 1] + tq_alphas[tq_alphas.len() / 2]) / 2.0
        };
        let frac_pos_tq =
            tracks.iter().filter(|t| t.alpha_p75 > 0.0).count() as f64 / tracks.len() as f64;
        let would_detect = median_tq > 0.0 && frac_pos_tq >= 0.60 && tracks.len() >= 3;
        if would_detect {
            recovered_tq += 1;
            let short = &album[..album.len().min(55)];
            println!(
                "  RECOVERED: {:55} median_p75={:+.3} frac+={:.0}%",
                short,
                median_tq,
                frac_pos_tq * 100.0
            );
        }
    }
    println!(
        "  {} of {} missed albums recovered with p75 alpha",
        recovered_tq,
        missed.len()
    );
}

fn print_album_summary(album: &str, tracks: &[TrackDiag]) {
    let n = tracks.len();
    let mut alphas: Vec<f64> = tracks.iter().map(|t| t.alpha).collect();
    alphas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_alpha = if n % 2 == 1 {
        alphas[n / 2]
    } else {
        (alphas[n / 2 - 1] + alphas[n / 2]) / 2.0
    };

    let mean_p75 = tracks.iter().map(|t| t.alpha_p75).sum::<f64>() / n as f64;
    let mean_top = tracks.iter().map(|t| t.alpha_top_quartile).sum::<f64>() / n as f64;
    let mean_all = tracks.iter().map(|t| t.alpha_all_frames).sum::<f64>() / n as f64;
    let frac_pos = tracks.iter().filter(|t| t.alpha > 0.0).count() as f64 / n as f64;
    let mean_pe_corr = tracks.iter().map(|t| t.pe_correlation).sum::<f64>() / n as f64;

    let short = &album[..album.len().min(53)];
    println!(
        "{:55} {:>5} {:>+7.3} {:>+7.3} {:>+7.3} {:>+7.3} {:>6.0}% {:>+7.3}",
        short,
        n,
        median_alpha,
        mean_p75,
        mean_top,
        mean_all,
        frac_pos * 100.0,
        mean_pe_corr
    );
}

fn print_aggregate(label: &str, albums: &[(String, Vec<TrackDiag>)]) {
    let all_tracks: Vec<&TrackDiag> = albums.iter().flat_map(|(_, t)| t.iter()).collect();
    let n = all_tracks.len();
    if n == 0 {
        return;
    }

    let mean_alpha = all_tracks.iter().map(|t| t.alpha).sum::<f64>() / n as f64;
    let mean_p75 = all_tracks.iter().map(|t| t.alpha_p75).sum::<f64>() / n as f64;
    let mean_top = all_tracks.iter().map(|t| t.alpha_top_quartile).sum::<f64>() / n as f64;
    let mean_all = all_tracks.iter().map(|t| t.alpha_all_frames).sum::<f64>() / n as f64;
    let frac_pos = all_tracks.iter().filter(|t| t.alpha > 0.0).count() as f64 / n as f64;
    let mean_pe_corr = all_tracks.iter().map(|t| t.pe_correlation).sum::<f64>() / n as f64;
    let mean_frames = all_tracks.iter().map(|t| t.frame_count).sum::<usize>() as f64 / n as f64;

    println!(
        "  {:12}: {} albums, {} tracks, mean_frames={:.0}",
        label,
        albums.len(),
        n,
        mean_frames
    );
    println!(
        "    alpha: quiet_med={:+.3}, p75={:+.3}, top_q={:+.3}, all_frames={:+.3}, frac+={:.0}%",
        mean_alpha,
        mean_p75,
        mean_top,
        mean_all,
        frac_pos * 100.0
    );
    println!("    pe_corr={:+.3}", mean_pe_corr);
}
