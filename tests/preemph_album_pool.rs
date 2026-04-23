//! A/B ablation: current album classifier vs track-level shape model → pooled scores.

use std::collections::BTreeMap;
use std::path::PathBuf;
use tonepoet::tui::preemphasis::{stft, frame_select, models, scoring, corpus, PreemphasisConfidence};

/// Full per-track computation: features + multi-alpha + shape features.
struct TrackResult {
    features: scoring::TrackFeatures,
    multi_alpha: models::TrackMultiAlpha,
    shape: models::TrackShapeFeatures,
    summary: scoring::TrackSummary,
    pe_correlation: f64,
    deemph_delta: f64,
    frame_count: usize,
}

fn score_track_full(
    path: &PathBuf,
    corpus_model: &corpus::CorpusModel,
    classifier: &scoring::LdaClassifier,
) -> Option<TrackResult> {
    let info = tonepoet::tui::probe::probe_audio(path).ok()?;
    if info.sample_rate > 48000 { return None; }

    let stft_result = stft::compute_band_spectra(path, info.sample_rate).ok()?;
    let selected = frame_select::select_frames(&stft_result);
    if selected.frames.is_empty() { return None; }

    let model_scores = models::score_models(&selected, &stft_result, corpus_model);
    let deemph_delta = scoring::virtual_deemphasis_score(
        &stft_result, &selected, corpus_model, info.sample_rate,
    );
    let multi_alpha = models::compute_multi_alpha(&stft_result, &selected, corpus_model);

    let features = scoring::TrackFeatures::from_scores(
        &model_scores, deemph_delta, selected.frames.len(),
    );
    let shape = models::TrackShapeFeatures::new(&multi_alpha, model_scores.pe_correlation, deemph_delta);
    let summary = scoring::TrackSummary::from_classifier_with_multi_alpha(
        classifier, &features, &multi_alpha, selected.frames.len(), path.clone(),
    );

    Some(TrackResult {
        features, multi_alpha, shape, summary,
        pe_correlation: model_scores.pe_correlation,
        deemph_delta,
        frame_count: selected.frames.len(),
    })
}

fn album_name(path: &PathBuf) -> String {
    path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str())
        .unwrap_or("?").to_string()
}

fn album_key(name: &str) -> String {
    if let Some(pos) = name.find('(') { name[..pos].trim().to_string() }
    else { name.to_string() }
}

fn collect_files(dir: &std::path::Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir).into_iter().flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("flac"))
        .map(|e| e.path().to_path_buf()).collect()
}

#[tokio::test]
async fn ab_ablation() {
    let pe_dir = dirs::home_dir().unwrap().join("preemph-dev/preemph");
    let non_pe_dir = dirs::home_dir().unwrap().join("preemph-dev/non-preemph");

    if !pe_dir.is_dir() || !non_pe_dir.is_dir() { eprintln!("SKIP"); return; }

    let corpus_model = match corpus::load_corpus() {
        Ok(c) => c, Err(e) => { eprintln!("SKIP: {}", e); return; }
    };
    let db = tonepoet::db::Database::open().expect("db");
    let classifier = match db.load_preemph_classifier() {
        Ok(c) => c, Err(e) => { eprintln!("SKIP: {}", e); return; }
    };

    // Score all tracks.
    println!("\n=== Scoring all tracks ===");
    let mut pe_albums: BTreeMap<String, Vec<TrackResult>> = BTreeMap::new();
    let mut np_albums: BTreeMap<String, Vec<TrackResult>> = BTreeMap::new();

    for (label, dir, albums) in [
        ("PE", pe_dir.as_path(), &mut pe_albums),
        ("Non-PE", non_pe_dir.as_path(), &mut np_albums),
    ] {
        let files = collect_files(dir);
        println!("  Scoring {} {} tracks...", files.len(), label);
        for path in &files {
            let album = album_name(path);
            if let Some(r) = tokio::task::spawn_blocking({
                let p = path.clone(); let c = corpus_model.clone(); let clf = classifier.clone();
                move || score_track_full(&p, &c, &clf)
            }).await.unwrap() {
                albums.entry(album).or_default().push(r);
            }
        }
    }

    // ── Pipeline A: Current album classifier on raw track features ──
    println!("\n=== Pipeline A: Album classifier on raw track features ===");
    let mut a_album_samples: Vec<([f64; scoring::NUM_ALBUM_FEATURES], bool, String)> = Vec::new();
    for (album, tracks) in &pe_albums {
        let summaries: Vec<scoring::TrackSummary> = tracks.iter().map(|t| t.summary.clone()).collect();
        if let Some(f) = scoring::extract_album_features(&summaries) {
            a_album_samples.push((f, true, album.clone()));
        }
    }
    for (album, tracks) in &np_albums {
        let summaries: Vec<scoring::TrackSummary> = tracks.iter().map(|t| t.summary.clone()).collect();
        if let Some(f) = scoring::extract_album_features(&summaries) {
            a_album_samples.push((f, false, album.clone()));
        }
    }

    let a_result = scoring::train_album_classifier_cv(&a_album_samples, 3, 0.1, 0.05);

    // ── Pipeline B: Track shape classifier → pooled scores → album ──
    println!("\n=== Pipeline B: Track shape model → pooled album scores ===");

    // Train track-level shape classifier.
    let mut track_samples: Vec<(models::TrackShapeFeatures, bool)> = Vec::new();
    for tracks in pe_albums.values() {
        for t in tracks {
            track_samples.push((t.shape.clone(), true));
        }
    }
    for tracks in np_albums.values() {
        for t in tracks {
            track_samples.push((t.shape.clone(), false));
        }
    }

    println!("  Training track shape classifier on {} tracks...", track_samples.len());
    let track_clf = match scoring::TrackPeClassifier::train(&track_samples, 0.1) {
        Ok(c) => c,
        Err(e) => { println!("  Track classifier failed: {}", e); return; }
    };
    println!("  Weights: {:?}", track_clf.weights);
    println!("  Bias: {:.4}", track_clf.bias);

    // Score all tracks with the shape classifier, pool to album level.
    let mut b_album_samples: Vec<([f64; scoring::NUM_POOLED_ALBUM_FEATURES], bool, String)> = Vec::new();

    for (album, tracks) in &pe_albums {
        let track_scores: Vec<f64> = tracks.iter().map(|t| track_clf.score(&t.shape)).collect();
        if let Some(f) = scoring::extract_pooled_album_features(&track_scores) {
            b_album_samples.push((f, true, album.clone()));
        }
    }
    for (album, tracks) in &np_albums {
        let track_scores: Vec<f64> = tracks.iter().map(|t| track_clf.score(&t.shape)).collect();
        if let Some(f) = scoring::extract_pooled_album_features(&track_scores) {
            b_album_samples.push((f, false, album.clone()));
        }
    }

    let b_result = scoring::train_album_classifier_cv(&b_album_samples, 3, 0.1, 0.05);

    // ── Compare A vs B ──
    println!("\n{}", "=".repeat(60));
    println!("=== A/B COMPARISON ===\n");

    match (&a_result, &b_result) {
        (Ok((a_clf, a_acc, a_fpr, a_prec)), Ok((b_clf, b_acc, b_fpr, b_prec))) => {
            println!("  Pipeline A (raw features):   accuracy={:.1}% FPR={:.1}% precision={:.1}%",
                a_acc * 100.0, a_fpr * 100.0, a_prec * 100.0);
            println!("  Pipeline B (shape → pooled):  accuracy={:.1}% FPR={:.1}% precision={:.1}%",
                b_acc * 100.0, b_fpr * 100.0, b_prec * 100.0);

            // Apply both to all albums.
            println!("\n=== DETECTION COMPARISON ===");
            println!("{:55} {:>6} {:>6}", "Album", "A", "B");
            println!("{}", "-".repeat(70));

            let mut a_pe_det = 0; let mut a_np_fp = 0;
            let mut b_pe_det = 0; let mut b_np_fp = 0;

            for (album, tracks) in &pe_albums {
                let summaries: Vec<scoring::TrackSummary> = tracks.iter().map(|t| t.summary.clone()).collect();
                let a_det = scoring::extract_album_features(&summaries)
                    .map(|f| a_clf.predict(&f)).unwrap_or(false);
                let track_scores: Vec<f64> = tracks.iter().map(|t| track_clf.score(&t.shape)).collect();
                let b_det = scoring::extract_pooled_album_features(&track_scores)
                    .map(|f| b_clf.predict(&f)).unwrap_or(false);

                if a_det { a_pe_det += 1; }
                if b_det { b_pe_det += 1; }

                let short = &album[..album.len().min(53)];
                let a_s = if a_det { "YES" } else { "-" };
                let b_s = if b_det { "YES" } else { "-" };
                if a_det || b_det {
                    println!("{:55} {:>6} {:>6}", short, a_s, b_s);
                }
            }

            println!("\n--- Non-PE false positives ---");
            for (album, tracks) in &np_albums {
                let summaries: Vec<scoring::TrackSummary> = tracks.iter().map(|t| t.summary.clone()).collect();
                let a_det = scoring::extract_album_features(&summaries)
                    .map(|f| a_clf.predict(&f)).unwrap_or(false);
                let track_scores: Vec<f64> = tracks.iter().map(|t| track_clf.score(&t.shape)).collect();
                let b_det = scoring::extract_pooled_album_features(&track_scores)
                    .map(|f| b_clf.predict(&f)).unwrap_or(false);

                if a_det { a_np_fp += 1; }
                if b_det { b_np_fp += 1; }

                if a_det || b_det {
                    let short = &album[..album.len().min(53)];
                    let a_s = if a_det { "FP!" } else { "-" };
                    let b_s = if b_det { "FP!" } else { "-" };
                    println!("{:55} {:>6} {:>6}", short, a_s, b_s);
                }
            }

            let pe_total = pe_albums.len();
            let np_total = np_albums.len();
            println!("\n=== FINAL SUMMARY ===");
            println!("  A: PE {}/{} ({:.0}%), FP {}/{} ({:.1}%)",
                a_pe_det, pe_total, a_pe_det as f64 / pe_total as f64 * 100.0,
                a_np_fp, np_total, a_np_fp as f64 / np_total as f64 * 100.0);
            println!("  B: PE {}/{} ({:.0}%), FP {}/{} ({:.1}%)",
                b_pe_det, pe_total, b_pe_det as f64 / pe_total as f64 * 100.0,
                b_np_fp, np_total, b_np_fp as f64 / np_total as f64 * 100.0);

            if b_pe_det > a_pe_det && b_np_fp <= a_np_fp + 1 {
                println!("  ✓ Pipeline B WINS: better recall at comparable FPR");
            } else if a_pe_det >= b_pe_det {
                println!("  Pipeline A still better or tied");
            }
        }
        (Err(e), _) => println!("  Pipeline A failed: {}", e),
        (_, Err(e)) => println!("  Pipeline B failed: {}", e),
    }

    // Matched pairs using soft rules.
    println!("\n=== MATCHED PAIRS (soft rules, for reference) ===");
    for (pe_album, pe_tracks) in &pe_albums {
        let pe_key = album_key(pe_album);
        for (np_album, np_tracks) in &np_albums {
            if album_key(np_album) == pe_key {
                let pe_summaries: Vec<scoring::TrackSummary> = pe_tracks.iter().map(|t| t.summary.clone()).collect();
                let np_summaries: Vec<scoring::TrackSummary> = np_tracks.iter().map(|t| t.summary.clone()).collect();
                let (pe_conf, _) = scoring::album_pool_soft(&pe_summaries);
                let (np_conf, _) = scoring::album_pool_soft(&np_summaries);
                println!("  {:45} PE={:?}  nonPE={:?}",
                    &pe_key[..pe_key.len().min(45)], pe_conf, np_conf);
            }
        }
    }
}
