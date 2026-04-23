//! Full calibration test: train corpus, score all PE and non-PE files,
//! report separation and matched-pair comparisons.

use std::path::PathBuf;
use std::collections::HashMap;

fn score_file_spectral(path: &PathBuf) -> Result<(f64, f64, usize, String), String> {
    use tonepoet::tui::preemphasis::{stft, frame_select, models, scoring, corpus};

    let info = tonepoet::tui::probe::probe_audio(path)
        .map_err(|e| format!("probe: {}", e))?;

    if info.sample_rate > 48000 {
        return Err("sample rate > 48 kHz".into());
    }

    let corpus_model = corpus::load_corpus()?;

    let stft_result = stft::compute_band_spectra(path, info.sample_rate)
        .map_err(|e| format!("stft: {}", e))?;

    let selected = frame_select::select_frames(&stft_result);
    if selected.frames.is_empty() {
        return Err("no qualifying frames".into());
    }

    let model_scores = models::score_models(&selected, &stft_result, &corpus_model);

    let deemph_delta = scoring::virtual_deemphasis_score(
        &stft_result, &selected, &corpus_model, info.sample_rate,
    );

    let verdict = scoring::compute_verdict(
        &model_scores, deemph_delta, &selected, &corpus_model,
    );

    Ok((verdict.llr, model_scores.alpha, selected.frames.len(), format!("{:?}", verdict.confidence)))
}

/// Extract album name from path for matching pairs.
/// Strips the pressing info in braces to get base album name.
fn album_key(path: &PathBuf) -> String {
    let parent = path.parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    // Extract "Artist - Album" before the year/format info.
    // Pattern: "Artist - Album (Year) [Format] {Pressing}"
    if let Some(paren_pos) = parent.find('(') {
        parent[..paren_pos].trim().to_string()
    } else {
        parent.to_string()
    }
}

#[tokio::test]
async fn full_calibration() {
    let pe_dir = dirs::home_dir().unwrap().join("preemph-dev/preemph");
    let non_pe_dir = dirs::home_dir().unwrap().join("preemph-dev/non-preemph");

    if !pe_dir.is_dir() || !non_pe_dir.is_dir() {
        eprintln!("SKIP: test directories not found");
        return;
    }

    // Step 1: Train corpus from non-PE files.
    println!("\n=== STEP 1: Training corpus from {} ===", non_pe_dir.display());
    let train_result = tonepoet::tui::preemphasis::corpus::train_corpus_from_dir(&non_pe_dir).await;
    match &train_result {
        Ok(model) => println!("  Corpus: {} tracks, {} frames, {} PCA components",
            model.n_tracks, model.n_frames, model.pca_components.len()),
        Err(e) => { panic!("Corpus training failed: {}", e); }
    }

    // Step 2: Score all PE files.
    println!("\n=== STEP 2: Scoring PE files ===");
    let pe_files: Vec<PathBuf> = walkdir::WalkDir::new(&pe_dir)
        .into_iter().flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("flac"))
        .map(|e| e.path().to_path_buf())
        .collect();

    let mut pe_results: Vec<(PathBuf, f64, f64, usize)> = Vec::new();
    let mut pe_errors = 0;

    for (i, path) in pe_files.iter().enumerate() {
        let name = path.file_name().unwrap().to_string_lossy();
        let parent = path.parent().and_then(|p| p.file_name())
            .and_then(|n| n.to_str()).unwrap_or("?");
        match tokio::task::spawn_blocking({
            let p = path.clone();
            move || score_file_spectral(&p)
        }).await.unwrap() {
            Ok((llr, alpha, frames, _conf)) => {
                if i < 20 || llr > -8.0 {
                    println!("  {:50} LLR={:+7.2} α={:+.3} fr={:4}",
                        &format!("{}/{}", parent, name)[..80.min(parent.len()+name.len()+1)],
                        llr, alpha, frames);
                }
                pe_results.push((path.clone(), llr, alpha, frames));
            }
            Err(e) => {
                pe_errors += 1;
                if pe_errors <= 5 {
                    println!("  ERR: {}: {}", name, e);
                }
            }
        }
    }

    // Step 3: Score all non-PE files.
    println!("\n=== STEP 3: Scoring non-PE files ===");
    let non_pe_files: Vec<PathBuf> = walkdir::WalkDir::new(&non_pe_dir)
        .into_iter().flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("flac"))
        .map(|e| e.path().to_path_buf())
        .collect();

    let mut non_pe_results: Vec<(PathBuf, f64, f64, usize)> = Vec::new();
    let mut non_pe_errors = 0;

    for (i, path) in non_pe_files.iter().enumerate() {
        let name = path.file_name().unwrap().to_string_lossy();
        let parent = path.parent().and_then(|p| p.file_name())
            .and_then(|n| n.to_str()).unwrap_or("?");
        match tokio::task::spawn_blocking({
            let p = path.clone();
            move || score_file_spectral(&p)
        }).await.unwrap() {
            Ok((llr, alpha, frames, _conf)) => {
                if i < 20 || llr > -12.0 {
                    println!("  {:50} LLR={:+7.2} α={:+.3} fr={:4}",
                        &format!("{}/{}", parent, name)[..80.min(parent.len()+name.len()+1)],
                        llr, alpha, frames);
                }
                non_pe_results.push((path.clone(), llr, alpha, frames));
            }
            Err(e) => {
                non_pe_errors += 1;
                if non_pe_errors <= 5 {
                    println!("  ERR: {}: {}", name, e);
                }
            }
        }
    }

    // Step 4: Summary statistics.
    println!("\n=== STEP 4: SUMMARY ===");
    let pe_llrs: Vec<f64> = pe_results.iter().map(|r| r.1).collect();
    let non_pe_llrs: Vec<f64> = non_pe_results.iter().map(|r| r.1).collect();
    let pe_alphas: Vec<f64> = pe_results.iter().map(|r| r.2).collect();
    let non_pe_alphas: Vec<f64> = non_pe_results.iter().map(|r| r.2).collect();

    if !pe_llrs.is_empty() {
        let mean = pe_llrs.iter().sum::<f64>() / pe_llrs.len() as f64;
        let min = pe_llrs.iter().cloned().reduce(f64::min).unwrap();
        let max = pe_llrs.iter().cloned().reduce(f64::max).unwrap();
        let alpha_mean = pe_alphas.iter().sum::<f64>() / pe_alphas.len() as f64;
        println!("  PE files ({} scored, {} errors):", pe_llrs.len(), pe_errors);
        println!("    LLR:   mean={:+.2}, min={:+.2}, max={:+.2}", mean, min, max);
        println!("    Alpha: mean={:+.3}", alpha_mean);
    }

    if !non_pe_llrs.is_empty() {
        let mean = non_pe_llrs.iter().sum::<f64>() / non_pe_llrs.len() as f64;
        let min = non_pe_llrs.iter().cloned().reduce(f64::min).unwrap();
        let max = non_pe_llrs.iter().cloned().reduce(f64::max).unwrap();
        let alpha_mean = non_pe_alphas.iter().sum::<f64>() / non_pe_alphas.len() as f64;
        println!("  Non-PE files ({} scored, {} errors):", non_pe_llrs.len(), non_pe_errors);
        println!("    LLR:   mean={:+.2}, min={:+.2}, max={:+.2}", mean, min, max);
        println!("    Alpha: mean={:+.3}", alpha_mean);
    }

    if !pe_llrs.is_empty() && !non_pe_llrs.is_empty() {
        let pe_min = pe_llrs.iter().cloned().reduce(f64::min).unwrap();
        let non_pe_max = non_pe_llrs.iter().cloned().reduce(f64::max).unwrap();
        let gap = pe_min - non_pe_max;
        println!("\n  Separation gap: {:.2} dB (PE min={:+.2}, non-PE max={:+.2})",
            gap, pe_min, non_pe_max);
        if gap > 0.0 {
            println!("  ✓ CLEAN SEPARATION");
        } else {
            println!("  ⚠ OVERLAP of {:.2} dB", -gap);
        }
    }

    // Step 5: Matched-pair comparison.
    println!("\n=== STEP 5: MATCHED PAIRS (same album, PE vs non-PE) ===");
    let mut pe_by_album: HashMap<String, Vec<f64>> = HashMap::new();
    for (path, llr, _, _) in &pe_results {
        pe_by_album.entry(album_key(path)).or_default().push(*llr);
    }
    let mut non_pe_by_album: HashMap<String, Vec<f64>> = HashMap::new();
    for (path, llr, _, _) in &non_pe_results {
        non_pe_by_album.entry(album_key(path)).or_default().push(*llr);
    }

    for (album, pe_llrs) in &pe_by_album {
        if let Some(non_pe_llrs) = non_pe_by_album.get(album) {
            let pe_mean = pe_llrs.iter().sum::<f64>() / pe_llrs.len() as f64;
            let non_pe_mean = non_pe_llrs.iter().sum::<f64>() / non_pe_llrs.len() as f64;
            let delta = pe_mean - non_pe_mean;
            println!("  {:45} PE={:+7.2} ({:2}t)  non-PE={:+7.2} ({:2}t)  Δ={:+.2}",
                album, pe_mean, pe_llrs.len(), non_pe_mean, non_pe_llrs.len(), delta);
        }
    }
}
