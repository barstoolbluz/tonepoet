//! End-to-end integration test for the pre-emphasis spectral scorer pipeline.
//!
//! Tests:
//! 1. Corpus training from ~/preemph-dev/non-deemph/
//! 2. Detection on a known PE file (should detect via metadata)
//! 3. Detection on a known non-PE file (should not flag)
//! 4. Spectral scoring produces reasonable values

use std::path::PathBuf;

/// Get a sample non-PE file from the dev directory.
fn find_non_pe_file() -> Option<PathBuf> {
    let dir = dirs::home_dir()?.join("preemph-dev/non-deemph");
    for entry in walkdir::WalkDir::new(&dir).into_iter().flatten() {
        let path = entry.path().to_path_buf();
        if path.extension().and_then(|e| e.to_str()) == Some("flac") {
            return Some(path);
        }
    }
    None
}

/// Get a sample PE file from the dev directory.
fn find_pe_file() -> Option<PathBuf> {
    let dir = dirs::home_dir()?.join("preemph-dev/preemph");
    for entry in walkdir::WalkDir::new(&dir).into_iter().flatten() {
        let path = entry.path().to_path_buf();
        if path.extension().and_then(|e| e.to_str()) == Some("flac") {
            return Some(path);
        }
    }
    None
}

#[tokio::test]
async fn test_corpus_training() {
    let dir = dirs::home_dir().unwrap().join("preemph-dev/non-deemph");
    if !dir.is_dir() {
        eprintln!("SKIP: ~/preemph-dev/non-deemph/ not found");
        return;
    }

    println!("=== Training corpus from {} ===", dir.display());
    let result = tonepoet::tui::preemphasis::corpus::train_corpus_from_dir(&dir).await;

    match result {
        Ok(model) => {
            println!("SUCCESS: corpus trained");
            println!("  tracks: {}", model.n_tracks);
            println!("  frames: {}", model.n_frames);
            println!("  PCA components: {}", model.pca_components.len());
            println!("  mean[0..5]: {:?}", &model.mean[..5]);
            assert!(
                model.n_tracks >= 30,
                "need at least 30 tracks, got {}",
                model.n_tracks
            );
            assert!(
                model.n_frames >= 100,
                "need at least 100 frames, got {}",
                model.n_frames
            );
            assert!(
                !model.pca_components.is_empty(),
                "should have PCA components"
            );
        }
        Err(e) => {
            panic!("Corpus training failed: {}", e);
        }
    }
}

#[tokio::test]
async fn test_detect_pe_file() {
    let pe_file = match find_pe_file() {
        Some(f) => f,
        None => {
            eprintln!("SKIP: no PE file found in ~/preemph-dev/preemph/");
            return;
        }
    };

    println!("=== Detecting PE on: {} ===", pe_file.display());
    let result = tonepoet::tui::preemphasis::detect_preemphasis(pe_file.clone()).await;

    println!("  confidence: {:?}", result.confidence);
    println!("  cue_confirmed: {}", result.cue_confirmed);
    println!("  detail: {}", result.detail);

    // PE files with tags should be Detected via metadata.
    assert_eq!(
        result.confidence,
        tonepoet::tui::preemphasis::PreemphasisConfidence::Detected,
        "PE file should be detected via metadata: {:?}",
        result.detail
    );
    assert!(
        result.cue_confirmed,
        "PE file should have metadata confirmation"
    );
}

#[tokio::test]
async fn test_detect_non_pe_file() {
    // Ensure corpus is trained first.
    let train_dir = dirs::home_dir().unwrap().join("preemph-dev/non-deemph");
    if !train_dir.is_dir() {
        eprintln!("SKIP: ~/preemph-dev/non-deemph/ not found");
        return;
    }

    // Check if corpus exists; train if not.
    if tonepoet::tui::preemphasis::corpus::load_corpus().is_err() {
        println!("Training corpus first...");
        tonepoet::tui::preemphasis::corpus::train_corpus_from_dir(&train_dir)
            .await
            .expect("corpus training failed");
    }

    let non_pe_file = match find_non_pe_file() {
        Some(f) => f,
        None => {
            eprintln!("SKIP: no non-PE file found");
            return;
        }
    };

    println!(
        "=== Detecting PE on non-PE file: {} ===",
        non_pe_file.display()
    );
    let result = tonepoet::tui::preemphasis::detect_preemphasis(non_pe_file.clone()).await;

    println!("  confidence: {:?}", result.confidence);
    println!("  cue_confirmed: {}", result.cue_confirmed);
    println!("  LLR M2 vs M0: {:.4}", result.llr_m2_vs_m0);
    println!("  LLR M2 vs M1: {:.4}", result.llr_m2_vs_m1);
    println!("  fitted alpha: {:.4}", result.fitted_alpha);
    println!("  frames scored: {}", result.frames_scored);
    println!("  deemph delta: {:.4}", result.deemph_distance_delta);
    println!("  gates: {:?}", result.gates_fired);
    println!("  detail: {}", result.detail);

    // Non-PE file should NOT be flagged as Detected or StrongCandidate.
    assert!(
        result.confidence != tonepoet::tui::preemphasis::PreemphasisConfidence::Detected
            && result.confidence
                != tonepoet::tui::preemphasis::PreemphasisConfidence::StrongCandidate,
        "Non-PE file should not be flagged as PE: {:?} — {}",
        result.confidence,
        result.detail
    );
}
