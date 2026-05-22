//! Quick debug: check group distribution and fold assignment
//! without doing any audio processing.

#[tokio::test]
async fn debug_fold_assignment() {
    use tonepoet::tui::preemphasis::scoring;

    let pe_dir = dirs::home_dir().unwrap().join("preemph-dev/preemph");
    let non_pe_dir = dirs::home_dir().unwrap().join("preemph-dev/non-preemph");

    // Collect files and create dummy features.
    let mut rng_state: u64 = 42;
    let mut rng_f64 = || -> f64 {
        rng_state = rng_state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (rng_state >> 33) as f64 / (1u64 << 31) as f64 // 0.0 to 1.0
    };
    let dummy_features = |alpha: f64, rng: &mut dyn FnMut() -> f64| scoring::TrackFeatures {
        features: [
            alpha + rng() * 0.2 - 0.1,
            rng() * 0.3,
            rng() * 0.5 - 0.25,
            3.5 + rng(),
            0.05 + rng() * 0.1,
        ],
        alpha,
        alpha_stability_missing: false,
    };

    let mut samples: Vec<(scoring::TrackFeatures, bool, String)> = Vec::new();

    // PE files.
    for entry in walkdir::WalkDir::new(&pe_dir).into_iter().flatten() {
        if entry.path().extension().and_then(|x| x.to_str()) == Some("flac") {
            let group = scoring::album_group_id(entry.path());
            samples.push((dummy_features(0.3, &mut rng_f64), true, group));
        }
    }

    // Non-PE files.
    for entry in walkdir::WalkDir::new(&non_pe_dir).into_iter().flatten() {
        if entry.path().extension().and_then(|x| x.to_str()) == Some("flac") {
            let group = scoring::album_group_id(entry.path());
            samples.push((dummy_features(0.1, &mut rng_f64), false, group));
        }
    }

    // Count groups.
    let mut groups: std::collections::BTreeMap<String, (usize, usize)> =
        std::collections::BTreeMap::new();
    for (_, label, group) in &samples {
        let entry = groups.entry(group.clone()).or_insert((0, 0));
        if *label {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }

    println!(
        "\n=== {} total samples, {} groups ===",
        samples.len(),
        groups.len()
    );
    println!("\n{:60} {:>5} {:>5}", "Group", "PE", "nonPE");
    println!("{}", "-".repeat(72));
    for (group, (pe, non_pe)) in &groups {
        let short = group.rsplit('/').next().unwrap_or(group);
        println!("{:60} {:5} {:5}", &short[..short.len().min(60)], pe, non_pe);
    }

    let pe_groups = groups.values().filter(|(pe, _)| *pe > 0).count();
    let non_pe_groups = groups.values().filter(|(_, np)| *np > 0).count();
    println!(
        "\nPE groups: {}, non-PE groups: {}",
        pe_groups, non_pe_groups
    );

    // Check eligibility.
    let eligible = samples
        .iter()
        .filter(|(f, _, _)| {
            let alpha_ok = f.alpha.is_finite() && f.alpha >= 0.05;
            let frames_ok = f.features[3].is_finite() && f.features[3] >= (20.0f64).ln();
            let stab_ok = !f.alpha_stability_missing;
            let unstable = !f.alpha_stability_missing && (f.features[4] > f.alpha.abs() * 3.0);
            alpha_ok && frames_ok && stab_ok && !unstable
        })
        .count();
    println!("\nEligible samples: {} / {}", eligible, samples.len());

    // Manually test fold assignment for 3-fold.
    {
        use std::collections::BTreeMap;
        let mut by_group: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for (_, label, group) in &samples {
            let e = by_group.entry(group.clone()).or_default();
            if *label {
                e.0 += 1;
            } else {
                e.1 += 1;
            }
        }

        // Simple round-robin assignment by alternating PE/non-PE.
        let mut pe_g: Vec<(&str, usize)> = by_group
            .iter()
            .filter(|(_, (pe, _))| *pe > 0)
            .map(|(g, (pe, _))| (g.as_str(), *pe))
            .collect();
        let mut np_g: Vec<(&str, usize)> = by_group
            .iter()
            .filter(|(_, (_, np))| *np > 0)
            .map(|(g, (_, np))| (g.as_str(), *np))
            .collect();
        pe_g.sort_by(|a, b| b.1.cmp(&a.1));
        np_g.sort_by(|a, b| b.1.cmp(&a.1));

        let k = 3;
        let mut fold_pe = vec![0usize; k];
        let mut fold_np = vec![0usize; k];
        let mut fold_groups = vec![0usize; k];

        for (i, (_, count)) in pe_g.iter().enumerate() {
            let f = i % k;
            fold_pe[f] += count;
            fold_groups[f] += 1;
        }
        for (i, (_, count)) in np_g.iter().enumerate() {
            let f = i % k;
            fold_np[f] += count;
            fold_groups[f] += 1;
        }

        println!("\nManual round-robin 3-fold:");
        for f in 0..k {
            println!(
                "  Fold {}: {} PE, {} non-PE, {} groups",
                f, fold_pe[f], fold_np[f], fold_groups[f]
            );
        }
    }

    // Try CV with different fold counts.
    for k in [2, 3, 4, 5] {
        if groups.len() < k {
            continue;
        }
        match scoring::grouped_cv_train_with_calibration_report(&samples, k, 0.01) {
            Ok((classifier, report)) => {
                println!(
                    "\n{}-fold CV: OK — accuracy={:.1}%, FPR={:.1}%, threshold={:.4}",
                    k,
                    report.metrics.track_accuracy * 100.0,
                    report.metrics.track_fpr * 100.0,
                    report.metrics.final_model_threshold
                );
            }
            Err(e) => {
                println!("\n{}-fold CV: FAILED — {}", k, e);
            }
        }
    }
}
