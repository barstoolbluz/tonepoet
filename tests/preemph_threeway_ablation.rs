//! Three-way summary ablation: compute multiple track-level summaries
//! (median, P75, top-quartile, all-frame) for PE, deemphasized, and non-PE albums.
//! Measure separation for each summary type.

use std::collections::BTreeMap;
use std::path::PathBuf;
use tonepoet::tui::preemphasis::{stft, frame_select, models, corpus};

/// Per-track multi-summary record.
#[derive(Debug, Clone)]
struct TrackMultiSummary {
    quiet_median_alpha: f64,
    quiet_p75_alpha: f64,
    quiet_top_q_alpha: f64,
    all_median_alpha: f64,
    all_p75_alpha: f64,
    fraction_positive_quiet: f64,
    fraction_positive_all: f64,
    pe_correlation: f64,
    deemph_delta: f64,
    frame_count: usize,
}

fn compute_multi_summary(
    path: &PathBuf,
    corpus_model: &corpus::CorpusModel,
) -> Option<TrackMultiSummary> {
    let info = tonepoet::tui::probe::probe_audio(path).ok()?;
    if info.sample_rate > 48000 { return None; }

    let stft_result = stft::compute_band_spectra(path, info.sample_rate).ok()?;
    let selected = frame_select::select_frames(&stft_result);
    if selected.frames.is_empty() { return None; }

    let model_scores = models::score_models(&selected, &stft_result, corpus_model);
    let deemph_delta = tonepoet::tui::preemphasis::scoring::virtual_deemphasis_score(
        &stft_result, &selected, corpus_model, info.sample_rate,
    );

    let mask = models::usable_band_mask(stft_result.sample_rate);
    let pe_template = get_pe_template_masked(corpus_model, &mask);

    // Quiet-frame per-frame alphas.
    let quiet_alphas = compute_per_frame_alphas(&selected.frames, &stft_result, corpus_model, &mask, &pe_template);
    let quiet_stats = compute_alpha_stats(&quiet_alphas);

    // All-frame per-frame alphas.
    let all_indices: Vec<usize> = (0..stft_result.band_spectra.len()).collect();
    let all_alphas = compute_per_frame_alphas(&all_indices, &stft_result, corpus_model, &mask, &pe_template);
    let all_stats = compute_alpha_stats(&all_alphas);

    Some(TrackMultiSummary {
        quiet_median_alpha: quiet_stats.0,
        quiet_p75_alpha: quiet_stats.1,
        quiet_top_q_alpha: quiet_stats.2,
        all_median_alpha: all_stats.0,
        all_p75_alpha: all_stats.1,
        fraction_positive_quiet: quiet_stats.3,
        fraction_positive_all: all_stats.3,
        pe_correlation: model_scores.pe_correlation,
        deemph_delta,
        frame_count: selected.frames.len(),
    })
}

/// Returns (median, p75, top_quartile_mean, fraction_positive).
fn compute_alpha_stats(alphas: &[f64]) -> (f64, f64, f64, f64) {
    if alphas.is_empty() { return (0.0, 0.0, 0.0, 0.0); }
    let mut sorted = alphas.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    let median = if n % 2 == 1 { sorted[n / 2] } else { (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0 };
    let p75 = sorted[3 * n / 4];
    let top_q_start = 3 * n / 4;
    let top_q_mean = sorted[top_q_start..].iter().sum::<f64>() / (n - top_q_start).max(1) as f64;
    let frac_pos = alphas.iter().filter(|&&a| a > 0.0).count() as f64 / n as f64;
    (median, p75, top_q_mean, frac_pos)
}

fn get_pe_template_masked(corpus_model: &corpus::CorpusModel, mask: &[bool; stft::NUM_BANDS]) -> Vec<f64> {
    let pe = if let Some(ref emp) = corpus_model.empirical_pe_template { *emp } else { models::pe_curve() };
    pe.iter().zip(mask.iter()).filter(|(_, &m)| m).map(|(&v, _)| v).collect()
}

fn compute_per_frame_alphas(
    frame_indices: &[usize], stft_result: &stft::StftResult,
    corpus_model: &corpus::CorpusModel, mask: &[bool; stft::NUM_BANDS], pe_template: &[f64],
) -> Vec<f64> {
    let mut alphas = Vec::with_capacity(frame_indices.len());
    for &idx in frame_indices {
        let spectrum = &stft_result.band_spectra[idx];
        let mut diff = [0.0; stft::NUM_BANDS];
        for k in 0..stft::NUM_BANDS { diff[k] = spectrum[k] - corpus_model.mean[k]; }
        let data: Vec<f64> = diff.iter().zip(mask.iter()).filter(|(_, &m)| m).map(|(&v, _)| v).collect();
        let n = data.len() as f64;
        if n < 2.0 { continue; }
        let mean_x = (n - 1.0) / 2.0;
        let mean_y = data.iter().sum::<f64>() / n;
        let mut num = 0.0; let mut den = 0.0;
        for (i, &y) in data.iter().enumerate() {
            let dx = i as f64 - mean_x; num += dx * (y - mean_y); den += dx * dx;
        }
        let slope = if den.abs() > 1e-12 { num / den } else { 0.0 };
        let intercept = mean_y - slope * mean_x;
        let residual: Vec<f64> = data.iter().enumerate().map(|(i, &y)| y - slope * i as f64 - intercept).collect();
        let dot_rp: f64 = residual.iter().zip(pe_template.iter()).map(|(&r, &p)| r * p).sum();
        let dot_pp: f64 = pe_template.iter().map(|&p| p * p).sum();
        alphas.push(if dot_pp > 1e-10 { dot_rp / dot_pp } else { 0.0 });
    }
    alphas
}

fn album_name(path: &PathBuf) -> String {
    path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("?").to_string()
}

fn collect_files(dir: &std::path::Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir).into_iter().flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("flac"))
        .map(|e| e.path().to_path_buf()).collect()
}

/// Aggregate album-level stats from track summaries.
struct AlbumStats {
    n_tracks: usize,
    median_quiet_median: f64,
    median_quiet_p75: f64,
    median_all_p75: f64,
    frac_tracks_quiet_p75_positive: f64,
    median_pe_corr: f64,
    median_deemph: f64,
}

fn album_aggregate(tracks: &[TrackMultiSummary]) -> Option<AlbumStats> {
    if tracks.len() < 3 { return None; }
    let n = tracks.len();

    let med = |vals: &mut Vec<f64>| -> f64 {
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = vals.len();
        if n == 0 { return f64::NAN; }
        if n % 2 == 1 { vals[n / 2] } else { (vals[n / 2 - 1] + vals[n / 2]) / 2.0 }
    };

    Some(AlbumStats {
        n_tracks: n,
        median_quiet_median: med(&mut tracks.iter().map(|t| t.quiet_median_alpha).collect()),
        median_quiet_p75: med(&mut tracks.iter().map(|t| t.quiet_p75_alpha).collect()),
        median_all_p75: med(&mut tracks.iter().map(|t| t.all_p75_alpha).collect()),
        frac_tracks_quiet_p75_positive: tracks.iter().filter(|t| t.quiet_p75_alpha > 0.0).count() as f64 / n as f64,
        median_pe_corr: med(&mut tracks.iter().map(|t| t.pe_correlation).collect()),
        median_deemph: med(&mut tracks.iter().map(|t| t.deemph_delta).collect()),
    })
}

#[tokio::test]
async fn threeway_summary_ablation() {
    let pe_dir = dirs::home_dir().unwrap().join("preemph-dev/preemph");
    let deemph_dir = dirs::home_dir().unwrap().join("preemph-dev/deemphasized");
    let non_pe_dir = dirs::home_dir().unwrap().join("preemph-dev/non-preemph");

    if !pe_dir.is_dir() || !non_pe_dir.is_dir() { eprintln!("SKIP"); return; }

    let corpus_model = match corpus::load_corpus() {
        Ok(c) => c, Err(e) => { eprintln!("SKIP: {}", e); return; }
    };

    // Process all three groups.
    let mut pe_albums: BTreeMap<String, Vec<TrackMultiSummary>> = BTreeMap::new();
    let mut de_albums: BTreeMap<String, Vec<TrackMultiSummary>> = BTreeMap::new();
    let mut np_albums: BTreeMap<String, Vec<TrackMultiSummary>> = BTreeMap::new();

    for (label, dir, albums) in [
        ("PE", &pe_dir, &mut pe_albums),
        ("Deemph", &deemph_dir, &mut de_albums),
        ("Non-PE", &non_pe_dir, &mut np_albums),
    ] {
        if !dir.is_dir() { println!("  Skipping {} (dir not found)", label); continue; }
        let files = collect_files(dir);
        println!("  Scoring {} {} tracks...", files.len(), label);
        for path in &files {
            let album = album_name(path);
            if let Some(s) = tokio::task::spawn_blocking({
                let p = path.clone(); let c = corpus_model.clone();
                move || compute_multi_summary(&p, &c)
            }).await.unwrap() {
                albums.entry(album).or_default().push(s);
            }
        }
    }

    // Aggregate to album level.
    let pe_agg: Vec<AlbumStats> = pe_albums.values().filter_map(|t| album_aggregate(t)).collect();
    let de_agg: Vec<AlbumStats> = de_albums.values().filter_map(|t| album_aggregate(t)).collect();
    let np_agg: Vec<AlbumStats> = np_albums.values().filter_map(|t| album_aggregate(t)).collect();

    // Print summary for each candidate statistic.
    println!("\n=== ALBUM-LEVEL SUMMARY STATISTICS ===\n");
    println!("{:22} {:>8} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "Statistic", "Group", "Mean", "Median", "Min", "Max", "P25", "P75");
    println!("{}", "-".repeat(92));

    for (stat_name, extractor) in [
        ("quiet_median_alpha", (|a: &AlbumStats| a.median_quiet_median) as fn(&AlbumStats) -> f64),
        ("quiet_p75_alpha", |a: &AlbumStats| a.median_quiet_p75),
        ("all_p75_alpha", |a: &AlbumStats| a.median_all_p75),
        ("frac_p75_positive", |a: &AlbumStats| a.frac_tracks_quiet_p75_positive),
        ("pe_correlation", |a: &AlbumStats| a.median_pe_corr),
        ("deemph_delta", |a: &AlbumStats| a.median_deemph),
    ] {
        for (group_name, agg) in [("PE", &pe_agg), ("Deemph", &de_agg), ("Non-PE", &np_agg)] {
            let vals: Vec<f64> = agg.iter().map(|a| extractor(a)).filter(|v| v.is_finite()).collect();
            if vals.is_empty() { continue; }
            let n = vals.len() as f64;
            let mean = vals.iter().sum::<f64>() / n;
            let mut s = vals.clone();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = s[s.len() / 2];
            let min = s[0];
            let max = s[s.len() - 1];
            let p25 = s[s.len() / 4];
            let p75 = s[3 * s.len() / 4];
            println!("{:22} {:>8} {:>+10.3} {:>+10.3} {:>+10.3} {:>+10.3} {:>+10.3} {:>+10.3}",
                stat_name, group_name, mean, median, min, max, p25, p75);
        }
        println!();
    }

    // Separation analysis: for each candidate, measure PE-vs-nonPE separation.
    println!("=== SEPARATION ANALYSIS (PE vs Non-PE) ===\n");
    println!("{:22} {:>10} {:>10} {:>10} {:>10}", "Statistic", "PE_median", "NP_median", "Gap", "Overlap?");
    println!("{}", "-".repeat(66));

    for (stat_name, extractor) in [
        ("quiet_median_alpha", (|a: &AlbumStats| a.median_quiet_median) as fn(&AlbumStats) -> f64),
        ("quiet_p75_alpha", |a: &AlbumStats| a.median_quiet_p75),
        ("all_p75_alpha", |a: &AlbumStats| a.median_all_p75),
        ("frac_p75_positive", |a: &AlbumStats| a.frac_tracks_quiet_p75_positive),
    ] {
        let mut pe_vals: Vec<f64> = pe_agg.iter().map(|a| extractor(a)).filter(|v| v.is_finite()).collect();
        let mut np_vals: Vec<f64> = np_agg.iter().map(|a| extractor(a)).filter(|v| v.is_finite()).collect();
        pe_vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        np_vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        if pe_vals.is_empty() || np_vals.is_empty() { continue; }

        let pe_med = pe_vals[pe_vals.len() / 2];
        let np_med = np_vals[np_vals.len() / 2];
        let pe_min = pe_vals[0];
        let np_max = np_vals[np_vals.len() - 1];
        let overlap = if pe_min > np_max { "NO" } else { "YES" };

        println!("{:22} {:>+10.3} {:>+10.3} {:>+10.3} {:>10}",
            stat_name, pe_med, np_med, pe_med - np_med, overlap);
    }

    // Detection rates at simple thresholds.
    println!("\n=== DETECTION RATES (album median > 0 AND frac_positive >= 60%) ===\n");

    for (stat_name, med_ext, frac_ext) in [
        ("quiet_median",
            (|a: &AlbumStats| a.median_quiet_median) as fn(&AlbumStats) -> f64,
            (|_: &AlbumStats| 1.0) as fn(&AlbumStats) -> f64), // placeholder
        ("quiet_p75",
            (|a: &AlbumStats| a.median_quiet_p75) as fn(&AlbumStats) -> f64,
            (|a: &AlbumStats| a.frac_tracks_quiet_p75_positive) as fn(&AlbumStats) -> f64),
        ("all_p75",
            (|a: &AlbumStats| a.median_all_p75) as fn(&AlbumStats) -> f64,
            (|_: &AlbumStats| 1.0) as fn(&AlbumStats) -> f64),
    ] {
        let pe_det = pe_agg.iter().filter(|a| med_ext(a) > 0.0).count();
        let np_fp = np_agg.iter().filter(|a| med_ext(a) > 0.0).count();
        let de_det = de_agg.iter().filter(|a| med_ext(a) > 0.0).count();

        println!("  {}: PE={}/{} ({:.0}%), Non-PE FP={}/{} ({:.1}%), Deemph={}/{} ({:.0}%)",
            stat_name,
            pe_det, pe_agg.len(), pe_det as f64 / pe_agg.len().max(1) as f64 * 100.0,
            np_fp, np_agg.len(), np_fp as f64 / np_agg.len().max(1) as f64 * 100.0,
            de_det, de_agg.len(), de_det as f64 / de_agg.len().max(1) as f64 * 100.0);
    }
}
