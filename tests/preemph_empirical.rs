//! Test with empirical PE template computed from paired PE↔deemphasized files.
//! Also runs the diagnostic comparisons the reasoning model requested.

use std::path::PathBuf;
use tonepoet::tui::preemphasis::stft::NUM_BANDS;
use tonepoet::tui::preemphasis::{corpus, frame_select, models, scoring, stft};

fn score_file(path: &PathBuf) -> Result<(f64, f64, f64, f64, usize), String> {
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

    let scores = models::score_models(&selected, &stft_result, &corpus_model);
    let deemph_delta =
        scoring::virtual_deemphasis_score(&stft_result, &selected, &corpus_model, info.sample_rate);

    Ok((
        scores.z_score,
        scores.alpha,
        scores.pe_correlation,
        deemph_delta,
        selected.frames.len(),
    ))
}

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
async fn empirical_template_test() {
    let pe_dir = dirs::home_dir().unwrap().join("preemph-dev/preemph");
    let deemph_dir = dirs::home_dir().unwrap().join("preemph-dev/deemphasized");
    let non_pe_dir = dirs::home_dir().unwrap().join("preemph-dev/non-preemph");

    if !pe_dir.is_dir() || !deemph_dir.is_dir() || !non_pe_dir.is_dir() {
        eprintln!("SKIP: directories not found");
        return;
    }

    // Step 1: Train corpus if needed.
    println!("\n=== Step 1: Ensure corpus exists ===");
    if corpus::load_corpus().is_err() {
        println!("  Training corpus...");
        corpus::train_corpus_from_dir(&non_pe_dir)
            .await
            .expect("corpus training failed");
    }
    let corpus_model = corpus::load_corpus().unwrap();
    println!(
        "  Corpus: {} tracks, {} frames",
        corpus_model.n_tracks, corpus_model.n_frames
    );

    // Step 2: Compute empirical PE template.
    println!("\n=== Step 2: Computing empirical PE template ===");
    let s_emp = corpus::train_empirical_template(&pe_dir, &deemph_dir)
        .await
        .expect("empirical template failed");

    // Step 3: Diagnostic comparisons.
    println!("\n=== Step 3: Diagnostics ===");

    let s_theory = models::pe_curve();
    let mask = models::usable_band_mask(44100);
    let n_usable = mask.iter().filter(|&&m| m).count();

    // s_emp vs s_theory in usable bands.
    let emp_usable: Vec<f64> = s_emp
        .iter()
        .zip(mask.iter())
        .filter(|(_, &m)| m)
        .map(|(&v, _)| v)
        .collect();
    let theory_usable: Vec<f64> = s_theory
        .iter()
        .zip(mask.iter())
        .filter(|(_, &m)| m)
        .map(|(&v, _)| v)
        .collect();

    let corr = pearson(&emp_usable, &theory_usable);
    println!("  corr(s_theory, s_emp) = {:.4}", corr);

    // Print the templates side by side.
    let centers = stft::band_centers();
    println!("\n  {:>8} {:>10} {:>10}", "freq", "s_emp", "s_theory");
    for k in 0..NUM_BANDS {
        if !mask[k] {
            continue;
        }
        println!(
            "  {:8.0} {:10.3} {:10.3}",
            centers[k], s_emp[k], s_theory[k]
        );
    }

    // Covariance diagnostics.
    // s^T Sigma s for both templates (how much corpus variance lies along each).
    let cov = &corpus_model.covariance;
    let s_theory_sigma_s_theory = quad_form(cov, &theory_usable, &mask);
    let s_emp_sigma_s_emp = quad_form(cov, &emp_usable, &mask);
    println!("\n  s_theory^T Σ s_theory = {:.4}", s_theory_sigma_s_theory);
    println!("  s_emp^T Σ s_emp       = {:.4}", s_emp_sigma_s_emp);

    // Step 4: Score with empirical template.
    println!("\n=== Step 4: Three-way scoring with empirical template ===");

    let pe_sample = sample_files(&pe_dir, 50);
    let de_sample = sample_files(&deemph_dir, 50);
    let np_sample = sample_files(&non_pe_dir, 50);

    for (label, files) in [
        ("PE", &pe_sample),
        ("Deemphasized", &de_sample),
        ("Non-PE", &np_sample),
    ] {
        let mut zs = Vec::new();
        let mut alphas = Vec::new();
        let mut corrs = Vec::new();
        let mut deltas = Vec::new();

        for path in files.iter() {
            match tokio::task::spawn_blocking({
                let p = path.clone();
                move || score_file(&p)
            })
            .await
            .unwrap()
            {
                Ok((z, a, r, d, _)) => {
                    zs.push(z);
                    alphas.push(a);
                    corrs.push(r);
                    deltas.push(d);
                }
                Err(_) => {}
            }
        }

        let n = zs.len();
        if n == 0 {
            continue;
        }
        let z_mean = zs.iter().sum::<f64>() / n as f64;
        let a_mean = alphas.iter().sum::<f64>() / n as f64;
        let r_mean = corrs.iter().sum::<f64>() / n as f64;
        let d_mean = deltas.iter().sum::<f64>() / n as f64;
        let z_min = zs.iter().cloned().reduce(f64::min).unwrap();
        let z_max = zs.iter().cloned().reduce(f64::max).unwrap();

        println!(
            "  {:15} n={:3}  z={:+6.2} [{:+.2},{:+.2}]  α={:+.3}  r={:+.3}  Δd={:+.3}",
            label, n, z_mean, z_min, z_max, a_mean, r_mean, d_mean
        );
    }
}

fn pearson(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len() as f64;
    let mx = x.iter().sum::<f64>() / n;
    let my = y.iter().sum::<f64>() / n;
    let mut num = 0.0;
    let mut dx2 = 0.0;
    let mut dy2 = 0.0;
    for (xi, yi) in x.iter().zip(y.iter()) {
        let dx = xi - mx;
        let dy = yi - my;
        num += dx * dy;
        dx2 += dx * dx;
        dy2 += dy * dy;
    }
    if dx2 * dy2 > 0.0 {
        num / (dx2 * dy2).sqrt()
    } else {
        0.0
    }
}

/// Compute s^T Σ s using masked indices.
fn quad_form(cov: &[f64], s: &[f64], mask: &[bool; NUM_BANDS]) -> f64 {
    let indices: Vec<usize> = mask
        .iter()
        .enumerate()
        .filter(|(_, &m)| m)
        .map(|(i, _)| i)
        .collect();
    let mut result = 0.0;
    for (si, &i) in indices.iter().enumerate() {
        for (sj, &j) in indices.iter().enumerate() {
            let idx = i * NUM_BANDS + j;
            let cov_ij = cov.get(idx).copied().unwrap_or(0.0);
            result += s[si] * cov_ij * s[sj];
        }
    }
    result
}
