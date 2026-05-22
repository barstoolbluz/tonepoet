//! Test the spectral scorer directly (bypassing metadata detection).
//!
//! This tests whether the M0/M1/M2 model comparison can distinguish
//! PE audio from non-PE audio based purely on spectral shape.

use std::path::PathBuf;

/// Run spectral scoring on a single file, bypassing metadata.
/// Returns (LLR, alpha, frames_scored, confidence_label).
fn score_file_spectral(path: &PathBuf) -> Result<(f64, f64, usize, String), String> {
    use tonepoet::tui::preemphasis::{corpus, frame_select, models, scoring, stft};

    let info = tonepoet::tui::probe::probe_audio(path).map_err(|e| format!("probe: {}", e))?;

    let corpus_model = corpus::load_corpus()?;

    let stft_result =
        stft::compute_band_spectra(path, info.sample_rate).map_err(|e| format!("stft: {}", e))?;

    let selected = frame_select::select_frames(&stft_result);
    if selected.frames.is_empty() {
        return Err("no qualifying frames".into());
    }

    let model_scores = models::score_models(&selected, &stft_result, &corpus_model);

    let deemph_delta =
        scoring::virtual_deemphasis_score(&stft_result, &selected, &corpus_model, info.sample_rate);

    let verdict = scoring::compute_verdict(&model_scores, deemph_delta, &selected, &corpus_model);

    Ok((
        verdict.llr,
        model_scores.alpha,
        selected.frames.len(),
        format!("{:?}", verdict.confidence),
    ))
}

#[tokio::test]
async fn test_spectral_pe_vs_non_pe() {
    // Ensure corpus exists.
    if tonepoet::tui::preemphasis::corpus::load_corpus().is_err() {
        eprintln!("SKIP: no corpus model (run test_corpus_training first)");
        return;
    }

    // Collect PE files.
    let pe_dir = dirs::home_dir().unwrap().join("preemph-dev/preemph");
    let non_pe_dir = dirs::home_dir().unwrap().join("preemph-dev/non-preemph");

    if !pe_dir.is_dir() || !non_pe_dir.is_dir() {
        eprintln!("SKIP: test directories not found");
        return;
    }

    let pe_files: Vec<PathBuf> = walkdir::WalkDir::new(&pe_dir)
        .into_iter()
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("flac"))
        .map(|e| e.path().to_path_buf())
        .take(5)
        .collect();

    let non_pe_files: Vec<PathBuf> = walkdir::WalkDir::new(&non_pe_dir)
        .into_iter()
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("flac"))
        .map(|e| e.path().to_path_buf())
        .take(5)
        .collect();

    println!("\n=== PE FILES (spectral only, metadata bypassed) ===");
    let mut pe_llrs = Vec::new();
    for path in &pe_files {
        let name = path.file_name().unwrap().to_string_lossy();
        match tokio::task::spawn_blocking({
            let p = path.clone();
            move || score_file_spectral(&p)
        })
        .await
        .unwrap()
        {
            Ok((llr, alpha, frames, conf)) => {
                println!(
                    "  {:40} LLR={:+7.2} α={:+.3} frames={:4} → {}",
                    &name[..name.len().min(40)],
                    llr,
                    alpha,
                    frames,
                    conf
                );
                pe_llrs.push(llr);
            }
            Err(e) => println!("  {:40} ERROR: {}", &name[..name.len().min(40)], e),
        }
    }

    println!("\n=== NON-PE FILES (spectral only) ===");
    let mut non_pe_llrs = Vec::new();
    for path in &non_pe_files {
        let name = path.file_name().unwrap().to_string_lossy();
        match tokio::task::spawn_blocking({
            let p = path.clone();
            move || score_file_spectral(&p)
        })
        .await
        .unwrap()
        {
            Ok((llr, alpha, frames, conf)) => {
                println!(
                    "  {:40} LLR={:+7.2} α={:+.3} frames={:4} → {}",
                    &name[..name.len().min(40)],
                    llr,
                    alpha,
                    frames,
                    conf
                );
                non_pe_llrs.push(llr);
            }
            Err(e) => println!("  {:40} ERROR: {}", &name[..name.len().min(40)], e),
        }
    }

    println!("\n=== SUMMARY ===");
    if !pe_llrs.is_empty() {
        let pe_mean = pe_llrs.iter().sum::<f64>() / pe_llrs.len() as f64;
        println!(
            "  PE files mean LLR:     {:+.2} (range {:+.2} to {:+.2})",
            pe_mean,
            pe_llrs.iter().cloned().reduce(f64::min).unwrap(),
            pe_llrs.iter().cloned().reduce(f64::max).unwrap()
        );
    }
    if !non_pe_llrs.is_empty() {
        let non_pe_mean = non_pe_llrs.iter().sum::<f64>() / non_pe_llrs.len() as f64;
        println!(
            "  Non-PE files mean LLR: {:+.2} (range {:+.2} to {:+.2})",
            non_pe_mean,
            non_pe_llrs.iter().cloned().reduce(f64::min).unwrap(),
            non_pe_llrs.iter().cloned().reduce(f64::max).unwrap()
        );
    }

    // The key question: is there separation between PE and non-PE LLRs?
    if !pe_llrs.is_empty() && !non_pe_llrs.is_empty() {
        let pe_min = pe_llrs.iter().cloned().reduce(f64::min).unwrap();
        let non_pe_max = non_pe_llrs.iter().cloned().reduce(f64::max).unwrap();
        println!(
            "  Separation: PE min ({:+.2}) vs Non-PE max ({:+.2})",
            pe_min, non_pe_max
        );
        if pe_min > non_pe_max {
            println!("  ✓ CLEAN SEPARATION — no overlap");
        } else {
            println!("  ⚠ OVERLAP — thresholds need tuning");
        }
    }
}
