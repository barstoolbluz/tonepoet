//! Three-way calibration: PE vs deemphasized vs non-PE.
//!
//! Key hypothesis: deemphasized files should cluster with non-PE,
//! while PE files should show distinct spectral signatures.
//! The PE↔deemphasized difference IS the pure PE signal.

use std::path::PathBuf;

fn score_file(path: &PathBuf) -> Result<(f64, f64, f64, usize), String> {
    use tonepoet::tui::preemphasis::{corpus, frame_select, models, scoring, stft};

    let info = tonepoet::tui::probe::probe_audio(path).map_err(|e| format!("probe: {}", e))?;
    if info.sample_rate > 48000 {
        return Err("hi-res".into());
    }

    let corpus_model = corpus::load_corpus()?;
    let stft_result =
        stft::compute_band_spectra(path, info.sample_rate).map_err(|e| format!("stft: {}", e))?;
    let selected = frame_select::select_frames(&stft_result);
    if selected.frames.is_empty() {
        return Err("no frames".into());
    }

    let model_scores = models::score_models(&selected, &stft_result, &corpus_model);
    let deemph_delta =
        scoring::virtual_deemphasis_score(&stft_result, &selected, &corpus_model, info.sample_rate);
    let verdict = scoring::compute_verdict(&model_scores, deemph_delta, &selected, &corpus_model);

    Ok((
        model_scores.z_score,
        model_scores.alpha,
        model_scores.pe_correlation,
        selected.frames.len(),
    ))
}

/// Sample up to N files from a directory (evenly spaced).
fn sample_files(dir: &std::path::Path, max: usize) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("flac"))
        .map(|e| e.path().to_path_buf())
        .collect();
    files.sort();
    if files.len() <= max {
        return files;
    }
    let step = files.len() / max;
    files.into_iter().step_by(step).take(max).collect()
}

#[tokio::test]
async fn threeway_comparison() {
    let pe_dir = dirs::home_dir().unwrap().join("preemph-dev/preemph");
    let deemph_dir = dirs::home_dir().unwrap().join("preemph-dev/deemphasized");
    let non_pe_dir = dirs::home_dir().unwrap().join("preemph-dev/non-preemph");

    if !pe_dir.is_dir() || !deemph_dir.is_dir() || !non_pe_dir.is_dir() {
        eprintln!("SKIP: directories not found");
        return;
    }

    // Train corpus on non-PE files.
    println!("\n=== Training corpus ===");
    let train_result = tonepoet::tui::preemphasis::corpus::train_corpus_from_dir(&non_pe_dir).await;
    match &train_result {
        Ok(m) => println!("  Corpus: {} tracks, {} frames", m.n_tracks, m.n_frames),
        Err(e) => {
            panic!("Training failed: {}", e);
        }
    }

    // Sample files from each group (50 each for speed).
    let pe_sample = sample_files(&pe_dir, 50);
    let deemph_sample = sample_files(&deemph_dir, 50);
    let non_pe_sample = sample_files(&non_pe_dir, 50);

    println!("\n=== Scoring {} PE files ===", pe_sample.len());
    let mut pe_llrs = Vec::new();
    let mut pe_alphas = Vec::new();
    let mut pe_corrs = Vec::new();
    for path in &pe_sample {
        match tokio::task::spawn_blocking({
            let p = path.clone();
            move || score_file(&p)
        })
        .await
        .unwrap()
        {
            Ok((llr, alpha, corr, _)) => {
                pe_llrs.push(llr);
                pe_alphas.push(alpha);
                pe_corrs.push(corr);
            }
            Err(_) => {}
        }
    }

    println!("=== Scoring {} deemphasized files ===", deemph_sample.len());
    let mut de_llrs = Vec::new();
    let mut de_alphas = Vec::new();
    let mut de_corrs = Vec::new();
    for path in &deemph_sample {
        match tokio::task::spawn_blocking({
            let p = path.clone();
            move || score_file(&p)
        })
        .await
        .unwrap()
        {
            Ok((llr, alpha, corr, _)) => {
                de_llrs.push(llr);
                de_alphas.push(alpha);
                de_corrs.push(corr);
            }
            Err(_) => {}
        }
    }

    println!("=== Scoring {} non-PE files ===", non_pe_sample.len());
    let mut np_llrs = Vec::new();
    let mut np_alphas = Vec::new();
    let mut np_corrs = Vec::new();
    for path in &non_pe_sample {
        match tokio::task::spawn_blocking({
            let p = path.clone();
            move || score_file(&p)
        })
        .await
        .unwrap()
        {
            Ok((llr, alpha, corr, _)) => {
                np_llrs.push(llr);
                np_alphas.push(alpha);
                np_corrs.push(corr);
            }
            Err(_) => {}
        }
    }

    // Summary.
    println!("\n{}", "=".repeat(60));
    println!("=== THREE-WAY COMPARISON ===\n");
    println!(
        "{:20} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "Group", "N", "LLR_m", "LLR_mn", "LLR_mx", "α_mean", "α_mn", "α_mx", "r_mean", "r_mn"
    );
    println!("{}", "-".repeat(100));

    for (name, llrs, alphas, corrs) in [
        ("PE (emphasis in)", &pe_llrs, &pe_alphas, &pe_corrs),
        ("Deemphasized", &de_llrs, &de_alphas, &de_corrs),
        ("Non-PE", &np_llrs, &np_alphas, &np_corrs),
    ] {
        if llrs.is_empty() {
            continue;
        }
        let n = llrs.len();
        let l_mean = llrs.iter().sum::<f64>() / n as f64;
        let l_min = llrs.iter().cloned().reduce(f64::min).unwrap();
        let l_max = llrs.iter().cloned().reduce(f64::max).unwrap();
        let a_mean = alphas.iter().sum::<f64>() / n as f64;
        let a_min = alphas.iter().cloned().reduce(f64::min).unwrap();
        let a_max = alphas.iter().cloned().reduce(f64::max).unwrap();
        let r_mean = corrs.iter().sum::<f64>() / n as f64;
        let r_min = corrs.iter().cloned().reduce(f64::min).unwrap();
        println!(
            "{:20} {:>8} {:>8.2} {:>8.2} {:>8.2} {:>8.3} {:>8.3} {:>8.3} {:>8.3} {:>8.3}",
            name, n, l_mean, l_min, l_max, a_mean, a_min, a_max, r_mean, r_min
        );
    }

    println!("\n=== KEY QUESTION: Does deemphasized cluster with non-PE? ===");
    if !de_alphas.is_empty() && !np_alphas.is_empty() && !pe_alphas.is_empty() {
        let pe_a = pe_alphas.iter().sum::<f64>() / pe_alphas.len() as f64;
        let de_a = de_alphas.iter().sum::<f64>() / de_alphas.len() as f64;
        let np_a = np_alphas.iter().sum::<f64>() / np_alphas.len() as f64;
        println!(
            "  Alpha means: PE={:+.4}, Deemph={:+.4}, NonPE={:+.4}",
            pe_a, de_a, np_a
        );
        println!("  PE↔Deemph gap: {:.4}", pe_a - de_a);
        println!("  Deemph↔NonPE gap: {:.4}", (de_a - np_a).abs());
        if (de_a - np_a).abs() < (pe_a - de_a).abs() / 2.0 {
            println!("  ✓ Deemphasized clusters with non-PE (gap to non-PE < half of gap to PE)");
        } else {
            println!("  ⚠ Deemphasized does NOT cluster cleanly with non-PE");
        }
    }
}
