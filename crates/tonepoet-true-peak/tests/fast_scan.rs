use tonepoet_true_peak::{
    CertifiedPeakMeter, CertifiedReconstruction, EdgePolicy, PeakLevel, PeakTier, SearchStatus,
    TruePeakError, FAST_ALGORITHM_REVISION, FAST_WALL_NANOS_PER_PROGRAMME_MINUTE,
};

fn run_fast(samples: &[f64], sample_rate_hz: u32, channels: usize, edge: EdgePolicy) -> tonepoet_true_peak::PeakCertificate {
    let mut meter = CertifiedPeakMeter::new(sample_rate_hz, channels, edge, PeakTier::Fast).unwrap();
    meter.push_interleaved(samples).unwrap();
    meter.finalize().unwrap()
}

fn finite_linear(level: PeakLevel) -> f64 {
    match level {
        PeakLevel::Finite { linear, .. } => linear,
        PeakLevel::Silence => 0.0,
    }
}

fn finite_dbtp(level: PeakLevel) -> f64 {
    match level {
        PeakLevel::Finite { dbtp, .. } => dbtp,
        PeakLevel::Silence => f64::NEG_INFINITY,
    }
}

fn deterministic_noise(frames: usize, channels: usize) -> Vec<f64> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut out = Vec::with_capacity(frames * channels);
    for _ in 0..frames * channels {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let bits = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        let unit = ((bits >> 11) as f64) * (1.0 / ((1_u64 << 53) as f64));
        out.push(unit.mul_add(1.9, -0.95));
    }
    out
}

#[test]
fn fast066_public_contract_is_fixed_work_hq1024() {
    assert_eq!(FAST_ALGORITHM_REVISION, "Fast066V2");
    assert_eq!(FAST_WALL_NANOS_PER_PROGRAMME_MINUTE, 660_000_000);

    let samples = deterministic_noise(1025, 2);
    let certificate = run_fast(&samples, 192_000, 2, EdgePolicy::RepeatEndpoints);
    assert_eq!(certificate.reconstruction, CertifiedReconstruction::Hq1024V1);
    assert_eq!(certificate.tier, PeakTier::Fast);
    assert_ne!(certificate.status, SearchStatus::TimeLimited);
    assert_eq!(certificate.diagnostics.time_bounded_prefix_blocks_skipped, 0);
    assert_eq!(certificate.diagnostics.time_limited_tiles, 0);
    assert_eq!(certificate.diagnostics.accelerated_l1_groups_tested, 0);
    assert_eq!(certificate.diagnostics.accelerated_curvature_roots, 0);
}

#[test]
fn one_frame_target_is_exact_for_both_edge_policies() {
    for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
        let samples = [0.625, -0.25];
        let certificate = run_fast(&samples, 192_000, 2, edge);
        assert_eq!(certificate.status, SearchStatus::Complete);
        assert_eq!(certificate.finite_interval.lower_linear.to_bits(), 0.625_f64.to_bits());
        assert_eq!(certificate.finite_interval.upper_linear.to_bits(), 0.625_f64.to_bits());
        assert_eq!(certificate.reported_point_estimate.channel_linear_peaks[0].to_bits(), 0.625_f64.to_bits());
        assert_eq!(certificate.reported_point_estimate.channel_linear_peaks[1].to_bits(), 0.25_f64.to_bits());
        assert_eq!(certificate.diagnostics.fast_survey_knots, 2);
        assert_eq!(certificate.diagnostics.fast_flat_groups, 0);
        assert_eq!(certificate.diagnostics.fast_fine_knots_evaluated, 0);
    }
}

#[test]
fn fast066_v2_dense_fixture_exercises_dominance_pruning_with_aggregate_sanity_caps() {
    // This 4098-frame carrier deliberately includes a tiny final tile, so the
    // private one-tile regression in `src/fast_scan.rs` is the authoritative
    // per-channel/tile 64/8/104 falsification. Keep this integration fixture for
    // its independent public-diagnostics coverage of both dominance paths.
    let frames = 4098usize;
    let samples = deterministic_noise(frames, 1);
    let certificate = run_fast(&samples, 192_000, 1, EdgePolicy::RepeatEndpoints);
    let diagnostics = certificate.diagnostics;

    assert_eq!(diagnostics.fast_survey_knots, (4 * (frames - 1) + 1) as u64);
    assert_eq!(diagnostics.fast_flat_groups, ((frames - 1 + 255) / 256) as u64);
    assert!(diagnostics.fast_candidates_observed > 64, "fixture failed to saturate nominee discovery");
    assert!(diagnostics.fast_candidate_saturated_tiles >= 1, "fixture failed to saturate a channel-tile");
    assert!(diagnostics.fast_bound_pruned_channel_tiles >= 1, "fixture failed to exercise whole-tile dominance pruning");
    assert!(diagnostics.fast_bound_pruned_nominees >= 1, "fixture failed to exercise per-nominee dominance pruning");
    assert!(diagnostics.fast_candidates_selected <= diagnostics.tiles_processed * 64);
    assert!(diagnostics.fast_proposals_evaluated <= diagnostics.fast_candidates_selected);
    assert!(diagnostics.fast_finishing_candidates <= diagnostics.tiles_processed * 8);
    assert!(diagnostics.fast_proposal_fine_knots_evaluated <= diagnostics.fast_proposals_evaluated);
    assert!(diagnostics.fast_finishing_fine_knots_evaluated <= diagnostics.fast_finishing_candidates * 5);
    assert_eq!(
        diagnostics.fast_fine_knots_evaluated,
        diagnostics.fast_proposal_fine_knots_evaluated
            + diagnostics.fast_finishing_fine_knots_evaluated,
    );
    assert!(diagnostics.fast_fine_knots_evaluated <= diagnostics.tiles_processed * 104);
    assert_eq!(diagnostics.time_bounded_prefix_blocks_skipped, 0);
    assert_eq!(diagnostics.time_limited_tiles, 0);
}

#[test]
fn rejected_nonfinite_push_is_transactional() {
    let prefix = deterministic_noise(257, 2);
    let suffix = deterministic_noise(193, 2);
    let mut expected = CertifiedPeakMeter::new(48_000, 2, EdgePolicy::ZeroExtend, PeakTier::Fast).unwrap();
    expected.push_interleaved(&prefix).unwrap();

    let mut actual = expected.clone();
    let error = actual.push_interleaved(&[0.25, f64::NAN, 0.5, -0.5]).unwrap_err();
    assert!(matches!(error, TruePeakError::NonFiniteSample { sample_index: 1 }));

    expected.push_interleaved(&suffix).unwrap();
    actual.push_interleaved(&suffix).unwrap();
    assert_eq!(actual.finalize().unwrap(), expected.finalize().unwrap());
}

#[test]
fn signed_zero_input_remains_exact_digital_silence() {
    let samples = [0.0, -0.0, -0.0, 0.0];
    for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
        let certificate = run_fast(&samples, 48_000, 1, edge);
        assert_eq!(certificate.status, SearchStatus::Complete);
        assert_eq!(certificate.finite_interval.lower_linear.to_bits(), 0.0_f64.to_bits());
        assert_eq!(certificate.finite_interval.upper_linear.to_bits(), 0.0_f64.to_bits());
        assert_eq!(certificate.reported_point_estimate.overall, PeakLevel::Silence);
    }
}

#[test]
fn subnormal_input_is_not_collapsed_to_digital_silence() {
    let tiny = f64::from_bits(1);
    let certificate = run_fast(&[tiny, 0.0], 48_000, 1, EdgePolicy::ZeroExtend);
    assert!(certificate.finite_interval.lower_linear.to_bits() >= tiny.to_bits());
    assert!(certificate.finite_interval.upper_linear.to_bits() >= tiny.to_bits());
    assert_eq!(certificate.diagnostics.fast_input_sample_peak_linear.to_bits(), tiny.to_bits());
    assert_ne!(certificate.reported_point_estimate.overall, PeakLevel::Silence);
}

#[test]
fn very_large_finite_input_fails_closed_instead_of_falling_back_to_unbounded_work() {
    let huge = f64::MAX / 2.0;
    let mut meter = CertifiedPeakMeter::new(48_000, 1, EdgePolicy::RepeatEndpoints, PeakTier::Fast).unwrap();
    meter.push_interleaved(&[huge, -huge, huge]).unwrap();
    assert_eq!(meter.finalize().unwrap_err(), TruePeakError::NumericalOverflow);
}

#[test]
fn fast_interval_contains_reference_point_on_short_hostile_cases() {
    let mut cases = Vec::new();
    cases.push(vec![0.0, 0.9, -0.9, 0.2, -0.3, 0.7, -0.1]);
    cases.push((0..33).map(|i| if i % 2 == 0 { 0.95 } else { -0.95 }).collect());
    cases.push(deterministic_noise(47, 1));

    for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
        for samples in &cases {
            let fast = run_fast(samples, 48_000, 1, edge);
            let mut reference = CertifiedPeakMeter::new(48_000, 1, edge, PeakTier::Reference).unwrap();
            reference.push_interleaved(samples).unwrap();
            let reference = reference.finalize().unwrap();
            let reference_point = finite_linear(reference.reported_point_estimate.overall);
            assert!(fast.finite_interval.lower_linear <= reference_point);
            assert!(fast.finite_interval.upper_linear >= reference_point);
        }
    }
}

#[test]
fn channel_processing_does_not_cross_nominate_candidates() {
    let frames = 1200usize;
    let left = deterministic_noise(frames, 1);
    let mut right = deterministic_noise(frames + 31, 1);
    right.drain(..31);
    let mut stereo = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        stereo.push(left[i]);
        stereo.push(0.5 * right[i]);
    }

    let stereo_result = run_fast(&stereo, 96_000, 2, EdgePolicy::RepeatEndpoints);
    let left_result = run_fast(&left, 96_000, 1, EdgePolicy::RepeatEndpoints);
    let scaled_right: Vec<f64> = right.iter().map(|value| 0.5 * value).collect();
    let right_result = run_fast(&scaled_right, 96_000, 1, EdgePolicy::RepeatEndpoints);

    // Packed-prefix arithmetic has a joint numerical enclosure, so compare the
    // point values with a tight floating tolerance rather than requiring a
    // bitwise identity between one- and two-channel FFT arithmetic graphs.
    let tolerance = 2.0e-11;
    assert!((stereo_result.reported_point_estimate.channel_linear_peaks[0]
        - left_result.reported_point_estimate.channel_linear_peaks[0]).abs() <= tolerance);
    assert!((stereo_result.reported_point_estimate.channel_linear_peaks[1]
        - right_result.reported_point_estimate.channel_linear_peaks[0]).abs() <= tolerance);
}

#[test]
fn bundled_recording_fast_point_regresses_to_dense_hq1024_target() {
    const BYTES: &[u8] = include_bytes!("fixtures/real_reference_48k_stereo.f64le");
    let samples: Vec<f64> = BYTES
        .chunks_exact(8)
        .map(|chunk| f64::from_le_bytes(chunk.try_into().unwrap()))
        .collect();
    let certificate = run_fast(&samples, 48_000, 2, EdgePolicy::RepeatEndpoints);
    let measured = finite_dbtp(certificate.reported_point_estimate.overall);
    let expected_dense_hq1024 = -0.112_284_234_518_646_16_f64;
    assert!((measured - expected_dense_hq1024).abs() <= 1.0e-8, "{measured}");
    assert!(certificate.finite_interval.lower_linear <= 0.987_155_997_134_972_3);
    assert!(certificate.finite_interval.upper_linear >= 0.987_155_997_134_972_3);
}
