use tonepoet_true_peak::{
    CertifiedPeakMeter, CertifiedReconstruction, EdgePolicy, PeakLevel, PeakTier, SearchStatus,
    TruePeakError, FAST_ALGORITHM_REVISION, FAST_WALL_NANOS_PER_PROGRAMME_MINUTE,
};

fn run_fast(samples: &[f64], sample_rate_hz: u32, channels: usize, edge: EdgePolicy) -> tonepoet_true_peak::PeakCertificate {
    let mut meter = CertifiedPeakMeter::new(sample_rate_hz, channels, edge, PeakTier::Fast).unwrap();
    meter.push_interleaved(samples).unwrap();
    meter.finalize().unwrap()
}

/// Commissioning-only stage timing (`fast-stage-timing` feature) is wall-clock
/// and non-deterministic; it is not part of the logical certificate. Transactional
/// equality assertions must exclude it. No-op unless the feature is built.
#[allow(unused_mut)]
fn strip_timing(mut certificate: tonepoet_true_peak::PeakCertificate) -> tonepoet_true_peak::PeakCertificate {
    #[cfg(feature = "fast-stage-timing")]
    {
        let d = &mut certificate.diagnostics;
        d.fast_stage_prefix_block_ingest_nanos = 0;
        d.fast_stage_midpoint_survey_nanos = 0;
        d.fast_stage_flat_envelope_nanos = 0;
        d.fast_stage_candidate_refinement_nanos = 0;
        d.fast_stage_nomination_nanos = 0;
        d.fast_stage_proposal_nanos = 0;
        d.fast_stage_finishing_nanos = 0;
        d.fast_stage_finalize_nanos = 0;
    }
    certificate
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

fn assert_fast_accuracy(certificate: &tonepoet_true_peak::PeakCertificate) {
    for (channel, interval) in certificate.channel_intervals.iter().copied().enumerate() {
        let width = interval.width_db().unwrap_or(0.0);
        assert!(width <= 0.01 + 1.0e-12, "channel {channel} width {width:.12} dB");
    }
}

fn aligned_windowed_multitone(frames: usize, frequencies: &[f64]) -> Vec<f64> {
    let center = (frames as f64 - 1.0) * 0.5;
    let half_span = center;
    (0..frames)
        .map(|frame| {
            let relative = frame as f64 - center;
            let envelope = if half_span == 0.0 {
                1.0
            } else {
                (std::f64::consts::PI * relative / (2.0 * half_span)).cos().powi(2)
            };
            let carrier = frequencies
                .iter()
                .copied()
                .map(|frequency| (2.0 * std::f64::consts::PI * frequency * relative).cos())
                .sum::<f64>()
                / frequencies.len() as f64;
            envelope * carrier
        })
        .collect()
}

#[test]
fn fast066_public_contract_remains_frozen() {
    assert_eq!(FAST_ALGORITHM_REVISION, "Fast066V2");
    assert_eq!(FAST_WALL_NANOS_PER_PROGRAMME_MINUTE, 660_000_000);

    let samples = deterministic_noise(1025, 2);
    let certificate = run_fast(&samples, 192_000, 2, EdgePolicy::RepeatEndpoints);
    assert_eq!(certificate.reconstruction, CertifiedReconstruction::Hq1024V1);
    assert_eq!(certificate.tier, PeakTier::Fast);
    assert_ne!(certificate.status, SearchStatus::TimeLimited);
    assert_eq!(certificate.diagnostics.time_bounded_prefix_blocks_skipped, 0);
    assert_eq!(certificate.diagnostics.time_limited_tiles, 0);
    assert_eq!(certificate.diagnostics.work_credits_consumed, 0);
    assert_eq!(certificate.diagnostics.work_limited_tiles, 0);
    assert_fast_accuracy(&certificate);
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
    }
}

#[test]
fn native_screen_avoids_physical_hq4_work_and_retired_optional_policy_stays_zero() {
    const FRAMES: usize = 12_289;
    let mut samples = vec![0.0_f64; FRAMES];
    samples[0] = 1.0;
    samples[17] = -0.2;
    let certificate = run_fast(&samples, 192_000, 1, EdgePolicy::ZeroExtend);
    let d = certificate.diagnostics;

    assert!(d.groups_rejected > 0, "fixture must exercise native-domain rejection");
    assert!(d.fast_survey_knots < (4 * (FRAMES - 1) + 1) as u64);
    assert_eq!(d.fast_candidates_observed, 0);
    assert_eq!(d.fast_candidates_selected, 0);
    assert_eq!(d.fast_candidate_saturated_tiles, 0);
    assert_eq!(d.fast_proposals_evaluated, 0);
    assert_eq!(d.fast_proposal_fine_knots_evaluated, 0);
    assert_eq!(d.fast_finishing_candidates, 0);
    assert_eq!(d.fast_finishing_fine_knots_evaluated, 0);
    assert_eq!(d.fast_fine_knots_evaluated, 0);
    assert_eq!(d.fast_bound_pruned_channel_tiles, 0);
    assert_eq!(d.fast_bound_pruned_nominees, 0);
    assert_eq!(d.fast_bound_pruned_finishers, 0);
    assert_eq!(d.fast_invalid_proposal_fits, 0);
    assert_eq!(d.fast_unbracketed_finishers, 0);
    assert_eq!(d.fast_invalid_finishing_fits, 0);
    assert_fast_accuracy(&certificate);
}

#[test]
fn rejected_nonfinite_and_incomplete_pushes_are_transactional() {
    let prefix = deterministic_noise(257, 2);
    let suffix = deterministic_noise(193, 2);
    let mut expected = CertifiedPeakMeter::new(48_000, 2, EdgePolicy::ZeroExtend, PeakTier::Fast).unwrap();
    expected.push_interleaved(&prefix).unwrap();

    let mut nonfinite = expected.clone();
    let error = nonfinite.push_interleaved(&[0.25, f64::NAN, 0.5, -0.5]).unwrap_err();
    assert!(matches!(error, TruePeakError::NonFiniteSample { sample_index: 1 }));

    let mut incomplete = expected.clone();
    let error = incomplete.push_interleaved(&[0.25]).unwrap_err();
    assert!(matches!(error, TruePeakError::IncompleteFrame { samples: 1, channels: 2 }));

    expected.push_interleaved(&suffix).unwrap();
    nonfinite.push_interleaved(&suffix).unwrap();
    incomplete.push_interleaved(&suffix).unwrap();
    let expected = strip_timing(expected.finalize().unwrap());
    assert_eq!(strip_timing(nonfinite.finalize().unwrap()), expected);
    assert_eq!(strip_timing(incomplete.finalize().unwrap()), expected);
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
fn very_large_finite_input_fails_closed_after_scale_aware_resolution() {
    let huge = f64::MAX / 2.0;
    let mut meter = CertifiedPeakMeter::new(48_000, 1, EdgePolicy::RepeatEndpoints, PeakTier::Fast).unwrap();
    meter.push_interleaved(&[huge, -huge, huge]).unwrap();
    assert_eq!(meter.finalize().unwrap_err(), TruePeakError::NumericalOverflow);
}

#[test]
fn fast_interval_contains_reference_point_and_meets_width_on_short_hostile_cases() {
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
            assert_fast_accuracy(&fast);
        }
    }
}

#[test]
fn fast_point_meets_001_db_on_independently_known_windowed_multitone_peaks() {
    // Every cosine and the cosine-squared envelope reaches +1 at the common
    // half-sample center, while each factor is magnitude-bounded by 1. The
    // continuous-time peak of each constructed waveform is therefore exactly
    // 1.0 (0 dBTP), independent of the HQ1024 implementation. The endpoints
    // are zero so ZeroExtend does not introduce an artificial boundary step.
    let cases: &[&[f64]] = &[
        &[0.30, 0.35, 0.40],
        &[0.4850, 0.4875, 0.4900, 0.4925, 0.4945],
    ];
    for frequencies in cases {
        let samples = aligned_windowed_multitone(4096, frequencies);
        let certificate = run_fast(&samples, 192_000, 1, EdgePolicy::ZeroExtend);
        let measured = finite_dbtp(certificate.reported_point_estimate.overall);
        assert!(
            measured.abs() <= 0.01,
            "analytical point error exceeded 0.01 dB: {measured:.12} dB for {frequencies:?}",
        );
        assert_fast_accuracy(&certificate);
    }
}

#[test]
fn channel_processing_is_independent_within_certificate_error() {
    let frames = 1200usize;
    let left = deterministic_noise(frames, 1);
    let mut right = deterministic_noise(frames + 31, 1);
    right.drain(..31);
    let mut stereo = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        stereo.push(left[i]);
        stereo.push(1.0e-6 * right[i]);
    }

    let stereo_result = run_fast(&stereo, 96_000, 2, EdgePolicy::RepeatEndpoints);
    let left_result = run_fast(&left, 96_000, 1, EdgePolicy::RepeatEndpoints);
    let scaled_right: Vec<f64> = right.iter().map(|value| 1.0e-6 * value).collect();
    let right_result = run_fast(&scaled_right, 96_000, 1, EdgePolicy::RepeatEndpoints);

    // Packing context may change the floating point estimate: the dense path
    // packs a channel pair into one complex FFT while a mono scan does not.
    // The design requires each arithmetic graph to return a truthful narrow
    // certificate, not bitwise-identical (or fixed-absolute-tolerance) points.
    // For the same channel data the independently valid certificates must be
    // mutually consistent, so their intervals must overlap.
    for (label, stereo_channel, mono) in [
        ("loud", 0usize, &left_result),
        ("quiet", 1usize, &right_result),
    ] {
        let stereo_interval = stereo_result.channel_intervals[stereo_channel];
        let mono_interval = mono.channel_intervals[0];
        assert!(
            stereo_interval.lower_linear <= mono_interval.upper_linear
                && mono_interval.lower_linear <= stereo_interval.upper_linear,
            "{label} channel certificates disagree across packing context: stereo={stereo_interval:?}, mono={mono_interval:?}",
        );
    }
    assert!(stereo_result.diagnostics.dense_regions > 0, "fixture must exercise packed dense first-stage work");
    assert!(
        stereo_result.diagnostics.direct_rescore_evaluations > 0,
        "quiet channel must tighten packed-FFT uncertainty directly when needed",
    );
    assert_fast_accuracy(&stereo_result);
    assert_fast_accuracy(&left_result);
    assert_fast_accuracy(&right_result);
}

#[test]
fn mandatory_resolution_uses_live_generic_counters_not_retired_v2_counters() {
    let samples = deterministic_noise(257, 1);
    let certificate = run_fast(&samples, 192_000, 1, EdgePolicy::ZeroExtend);
    assert!(
        certificate.diagnostics.refined_cells > 0,
        "fixture must exercise mandatory dyadic refinement",
    );
    assert!(certificate.diagnostics.phase_evaluations >= certificate.diagnostics.refined_cells);
    assert_eq!(certificate.diagnostics.fast_fine_knots_evaluated, 0);
    assert_fast_accuracy(&certificate);
}

#[test]
fn bundled_recording_meets_accuracy_floor_and_contains_dense_hq1024_target() {
    const BYTES: &[u8] = include_bytes!("fixtures/real_reference_48k_stereo.f64le");
    let samples: Vec<f64> = BYTES
        .chunks_exact(8)
        .map(|chunk| f64::from_le_bytes(chunk.try_into().unwrap()))
        .collect();
    let certificate = run_fast(&samples, 48_000, 2, EdgePolicy::RepeatEndpoints);
    let measured = finite_dbtp(certificate.reported_point_estimate.overall);
    let expected_dense_hq1024_dbtp = -0.112_284_234_518_646_16_f64;
    let expected_dense_hq1024_linear = 0.987_155_997_134_972_3_f64;
    assert!((measured - expected_dense_hq1024_dbtp).abs() <= 0.01, "{measured}");
    assert!(certificate.finite_interval.lower_linear <= expected_dense_hq1024_linear);
    assert!(certificate.finite_interval.upper_linear >= expected_dense_hq1024_linear);
    assert_fast_accuracy(&certificate);
}
