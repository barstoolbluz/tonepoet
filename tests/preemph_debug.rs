//! Debug test: examine spectral shapes of PE vs non-PE vs corpus mean.

use std::path::PathBuf;

#[tokio::test]
async fn debug_spectral_shapes() {
    use tonepoet::tui::preemphasis::{stft, frame_select, corpus, iir};

    let corpus_model = match corpus::load_corpus() {
        Ok(m) => m,
        Err(e) => { eprintln!("SKIP: {}", e); return; }
    };

    // Get one PE file and one non-PE file.
    let pe_dir = dirs::home_dir().unwrap().join("preemph-dev/preemph");
    let non_pe_dir = dirs::home_dir().unwrap().join("preemph-dev/non-deemph");

    let pe_file: PathBuf = walkdir::WalkDir::new(&pe_dir)
        .into_iter().flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("flac"))
        .map(|e| e.path().to_path_buf())
        .next().unwrap();

    let non_pe_file: PathBuf = walkdir::WalkDir::new(&non_pe_dir)
        .into_iter().flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("flac"))
        .map(|e| e.path().to_path_buf())
        .next().unwrap();

    println!("\nPE file: {:?}", pe_file.file_name().unwrap());
    println!("Non-PE file: {:?}", non_pe_file.file_name().unwrap());

    // Compute median spectra for both.
    let pe_stft = tokio::task::spawn_blocking({
        let p = pe_file.clone();
        move || stft::compute_band_spectra(&p, 44100)
    }).await.unwrap().unwrap();

    let non_pe_stft = tokio::task::spawn_blocking({
        let p = non_pe_file.clone();
        move || stft::compute_band_spectra(&p, 44100)
    }).await.unwrap().unwrap();

    let pe_selected = frame_select::select_frames(&pe_stft);
    let non_pe_selected = frame_select::select_frames(&non_pe_stft);

    // Compute median spectrum for each.
    let pe_median = median_spectrum(&pe_selected, &pe_stft);
    let non_pe_median = median_spectrum(&non_pe_selected, &non_pe_stft);

    // Band centers.
    let centers = stft::band_centers();

    // Theoretical PE curve.
    let pe_curve: Vec<f64> = centers.iter().map(|&f| iir::theoretical_gain_db(f)).collect();

    println!("\n{:>8} {:>8} {:>8} {:>8} {:>8} {:>8}", "freq", "corpus", "PE_file", "nonPE", "PE-corp", "PE_theo");
    println!("{}", "-".repeat(56));
    for k in 0..stft::NUM_BANDS {
        let diff_pe = pe_median[k] - corpus_model.mean[k];
        println!("{:8.0} {:8.2} {:8.2} {:8.2} {:+8.2} {:8.2}",
            centers[k], corpus_model.mean[k], pe_median[k], non_pe_median[k],
            diff_pe, pe_curve[k]);
    }

    println!("\n=== Key comparison ===");
    println!("If PE file has emphasis baked in, PE_file - corpus should track PE_theoretical");
    println!("Correlation between (PE-corpus) and (PE_theoretical):");
    let diff: Vec<f64> = (0..stft::NUM_BANDS).map(|k| pe_median[k] - corpus_model.mean[k]).collect();
    let corr = pearson_correlation(&diff, &pe_curve);
    println!("  r = {:.4}", corr);
}

fn median_spectrum(
    selected: &tonepoet::tui::preemphasis::frame_select::SelectedFrames,
    stft: &tonepoet::tui::preemphasis::stft::StftResult,
) -> [f64; tonepoet::tui::preemphasis::stft::NUM_BANDS] {
    let n = selected.frames.len();
    let mut median = [0.0; tonepoet::tui::preemphasis::stft::NUM_BANDS];
    if n == 0 { return median; }
    for k in 0..tonepoet::tui::preemphasis::stft::NUM_BANDS {
        let mut values: Vec<f64> = selected.frames.iter()
            .map(|&idx| stft.band_spectra[idx][k])
            .collect();
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        median[k] = if n % 2 == 1 { values[n/2] } else { (values[n/2-1] + values[n/2]) / 2.0 };
    }
    median
}

fn pearson_correlation(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len() as f64;
    let mx = x.iter().sum::<f64>() / n;
    let my = y.iter().sum::<f64>() / n;
    let mut num = 0.0;
    let mut dx2 = 0.0;
    let mut dy2 = 0.0;
    for i in 0..x.len() {
        let dx = x[i] - mx;
        let dy = y[i] - my;
        num += dx * dy;
        dx2 += dx * dx;
        dy2 += dy * dy;
    }
    if dx2 * dy2 > 0.0 { num / (dx2 * dy2).sqrt() } else { 0.0 }
}
