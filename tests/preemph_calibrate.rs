//! End-to-end calibration test: train corpus, compute empirical template,
//! calibrate LDA classifier, then test detection on sample files.

#[tokio::test]
async fn full_calibration_pipeline() {
    let pe_dir = dirs::home_dir().unwrap().join("preemph-dev/preemph");
    let non_pe_dir = dirs::home_dir().unwrap().join("preemph-dev/non-preemph");
    let deemph_dir = dirs::home_dir().unwrap().join("preemph-dev/deemphasized");

    if !pe_dir.is_dir() || !non_pe_dir.is_dir() {
        eprintln!("SKIP: test directories not found");
        return;
    }

    // Step 1: Ensure corpus is trained.
    println!("\n=== Step 1: Corpus ===");
    if tonepoet::tui::preemphasis::corpus::load_corpus().is_err() {
        println!("  Training corpus from non-PE files...");
        tonepoet::tui::preemphasis::corpus::train_corpus_from_dir(&non_pe_dir)
            .await
            .expect("corpus training failed");
    }
    let corpus = tonepoet::tui::preemphasis::corpus::load_corpus().unwrap();
    println!(
        "  Corpus: {} tracks, {} frames",
        corpus.n_tracks, corpus.n_frames
    );

    // Step 2: Ensure empirical template exists.
    println!("\n=== Step 2: Empirical template ===");
    if corpus.empirical_pe_template.is_none() && deemph_dir.is_dir() {
        println!("  Computing empirical template from paired files...");
        tonepoet::tui::preemphasis::corpus::train_empirical_template(&pe_dir, &deemph_dir)
            .await
            .expect("template training failed");
    }
    let corpus = tonepoet::tui::preemphasis::corpus::load_corpus().unwrap();
    println!(
        "  Empirical template: {}",
        if corpus.empirical_pe_template.is_some() {
            "yes"
        } else {
            "no (using theoretical)"
        }
    );

    // Step 3: Calibrate.
    println!("\n=== Step 3: Calibrate LDA classifier ===");
    let result = tonepoet::tui::preemphasis::corpus::calibrate(&pe_dir, &non_pe_dir)
        .await
        .expect("calibration failed");

    println!("  Samples: {} PE, {} non-PE", result.n_pe, result.n_non_pe);
    println!("  CV accuracy: {:.1}%", result.cv_accuracy * 100.0);
    println!("  CV FPR: {:.1}%", result.cv_fpr * 100.0);
    println!("  CV precision: {:.1}%", result.cv_precision * 100.0);
    println!("  Threshold: {:.4}", result.threshold);
    println!("  Weights: {:?}", result.classifier.weights);
    println!("  Bias: {:.4}", result.classifier.bias);

    // Step 4: Test detection with trained classifier.
    println!("\n=== Step 4: Test detection ===");

    // Find one PE file and one non-PE file.
    let pe_file: std::path::PathBuf = walkdir::WalkDir::new(&pe_dir)
        .into_iter()
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("flac"))
        .map(|e| e.path().to_path_buf())
        .next()
        .unwrap();
    let non_pe_file: std::path::PathBuf = walkdir::WalkDir::new(&non_pe_dir)
        .into_iter()
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("flac"))
        .map(|e| e.path().to_path_buf())
        .next()
        .unwrap();

    println!("  PE file: {:?}", pe_file.file_name().unwrap());
    let pe_result = tonepoet::tui::preemphasis::detect_preemphasis(pe_file).await;
    println!("    confidence: {:?}", pe_result.confidence);
    println!("    detail: {}", pe_result.detail);

    println!("  Non-PE file: {:?}", non_pe_file.file_name().unwrap());
    let non_pe_result = tonepoet::tui::preemphasis::detect_preemphasis(non_pe_file).await;
    println!("    confidence: {:?}", non_pe_result.confidence);
    println!("    detail: {}", non_pe_result.detail);
}
