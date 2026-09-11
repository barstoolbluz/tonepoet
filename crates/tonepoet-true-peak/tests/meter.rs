use tonepoet_true_peak::{
    CertifiedPeakMeter, CertifiedReconstruction, EdgePolicy, PeakLevel, PeakTier,
    ReportingPeakMeter, SearchStatus, TruePeakError, FAST_ALGORITHM_REVISION,
    FAST_WALL_NANOS_PER_PROGRAMME_MINUTE, REFERENCE_INTERVAL_OBJECTIVE_DB,
    STANDARD_INTERVAL_OBJECTIVE_DB,
};

fn signal(frames: usize, channels: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames * channels);
    for frame in 0..frames {
        let x = frame as f64;
        for channel in 0..channels {
            let c = channel as f64;
            out.push(0.71 * (0.317 * x + c * 0.11).sin() + 0.19 * (1.07 * x - c * 0.07).cos());
        }
    }
    out
}

/// Commissioning-only stage timing (`fast-stage-timing` feature) is wall-clock
/// and non-deterministic; it is not part of the logical certificate. Chunk-
/// invariance / clone equality assertions must exclude it. No-op unless the
/// feature is built.
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

#[test]
fn public_surface_has_three_hq_tiers_with_explicit_contracts() {
    assert_eq!(PeakTier::default(), PeakTier::Standard);
    assert_eq!(PeakTier::Reference.interval_objective_db(), Some(REFERENCE_INTERVAL_OBJECTIVE_DB));
    assert_eq!(PeakTier::Standard.interval_objective_db(), Some(STANDARD_INTERVAL_OBJECTIVE_DB));
    assert_eq!(PeakTier::Fast.interval_objective_db(), None);
    assert_eq!(FAST_ALGORITHM_REVISION, "Fast066V2");
    assert_eq!(FAST_WALL_NANOS_PER_PROGRAMME_MINUTE, 660_000_000);
}

#[test]
fn every_public_ceiling_tier_reports_hq1024v1_and_contains_the_sample_peak() {
    let samples = signal(67, 2);
    let sample_peak = samples.iter().copied().map(f64::abs).fold(0.0_f64, f64::max);
    for tier in [PeakTier::Reference, PeakTier::Standard, PeakTier::Fast] {
        let mut meter = CertifiedPeakMeter::new(192_000, 2, EdgePolicy::RepeatEndpoints, tier).unwrap();
        meter.push_interleaved(&samples).unwrap();
        let certificate = meter.finalize().unwrap();
        assert_eq!(certificate.reconstruction, CertifiedReconstruction::Hq1024V1);
        assert_eq!(certificate.tier, tier);
        assert!(certificate.finite_interval.lower_linear >= sample_peak);
        assert!(certificate.finite_interval.upper_linear >= certificate.finite_interval.lower_linear);
        assert!(matches!(
            certificate.status,
            SearchStatus::Complete | SearchStatus::WorkLimited
        ));
    }
}

#[test]
fn reference_and_standard_are_chunk_invariant() {
    let samples = signal(53, 2);
    for tier in [PeakTier::Reference, PeakTier::Standard] {
        let mut whole = CertifiedPeakMeter::new(192_000, 2, EdgePolicy::ZeroExtend, tier).unwrap();
        whole.push_interleaved(&samples).unwrap();
        let whole = whole.finalize().unwrap();

        let mut chunked = CertifiedPeakMeter::new(192_000, 2, EdgePolicy::ZeroExtend, tier).unwrap();
        for chunk in samples.chunks(14) {
            chunked.push_interleaved(chunk).unwrap();
        }
        let chunked = chunked.finalize().unwrap();
        assert_eq!(whole, chunked, "tier={tier:?}");
    }
}

#[test]
fn fast066_is_chunk_invariant_including_exact_tile_and_fft_boundaries() {
    // 8193 frames contains exactly two 4096-interval canonical tiles while the
    // qualified-prefix stream also crosses its 6657-frame FFT block boundary
    // after the 777-frame leading halo. This forces tile ownership and prefix
    // block formation to remain independent of caller chunking in one test.
    const FRAMES: usize = 8193;
    let mut samples = signal(FRAMES, 2);
    // Put an unambiguous sample-aligned extremum exactly on the 4096-frame
    // tile boundary so the regression exercises deterministic ownership, not
    // merely equal output for a carrier whose winners are all interior.
    samples[4096 * 2] = 1.0;
    samples[4096 * 2 + 1] = -1.0;
    for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
        let run = |pattern: &[usize]| {
            let mut meter = CertifiedPeakMeter::new(192_000, 2, edge, PeakTier::Fast).unwrap();
            let mut frame = 0usize;
            let mut pattern_index = 0usize;
            while frame < FRAMES {
                let take = pattern[pattern_index % pattern.len()].min(FRAMES - frame);
                let first = frame * 2;
                let last = (frame + take) * 2;
                meter.push_interleaved(&samples[first..last]).unwrap();
                frame += take;
                pattern_index += 1;
            }
            meter.finalize().unwrap()
        };

        let whole = strip_timing(run(&[FRAMES]));
        let one = strip_timing(run(&[1]));
        let thirty_seven = strip_timing(run(&[37]));
        let irregular = strip_timing(run(&[5, 1, 5879, 3, 257, 17, 6657, 4096]));
        assert_eq!(whole, one, "{edge:?}: whole vs one-frame");
        assert_eq!(whole, thirty_seven, "{edge:?}: whole vs 37-frame");
        assert_eq!(whole, irregular, "{edge:?}: whole vs irregular");
    }
}

#[test]
fn fast066_partial_clone_is_independent_of_completion_chunking() {
    const FRAMES: usize = 5003;
    const PREFIX: usize = 911;
    let samples = signal(FRAMES, 2);
    let mut base = CertifiedPeakMeter::new(
        192_000,
        2,
        EdgePolicy::RepeatEndpoints,
        PeakTier::Fast,
    )
    .unwrap();
    base.push_interleaved(&samples[..PREFIX * 2]).unwrap();

    let mut whole_tail = base.clone();
    whole_tail.push_interleaved(&samples[PREFIX * 2..]).unwrap();

    let mut prime_tail = base;
    for chunk in samples[PREFIX * 2..].chunks(74) {
        // 74 interleaved samples = 37 complete stereo frames.
        prime_tail.push_interleaved(chunk).unwrap();
    }
    assert_eq!(strip_timing(whole_tail.finalize().unwrap()), strip_timing(prime_tail.finalize().unwrap()));
}

#[test]
fn fast066_short_and_tile_boundary_lengths_are_chunk_invariant() {
    for frames in [1usize, 2, 31, 32, 33, 255, 256, 257, 4095, 4096, 4097] {
        let mut samples = vec![0.0_f64; frames];
        samples[0] = 0.75;
        if frames > 1 {
            samples[frames - 1] = -0.125;
        }
        let run = |chunk_frames: usize| {
            let mut meter = CertifiedPeakMeter::new(
                192_000,
                1,
                EdgePolicy::ZeroExtend,
                PeakTier::Fast,
            )
            .unwrap();
            for chunk in samples.chunks(chunk_frames) {
                meter.push_interleaved(chunk).unwrap();
            }
            meter.finalize().unwrap()
        };
        assert_eq!(strip_timing(run(frames)), strip_timing(run(37)), "frames={frames}");
    }
}

#[test]
fn fast066_pruning_ignores_future_validation_peak_in_the_same_caller_push() {
    // Put the decisive sample after tile 0 has enough native right context.
    // A whole-push validator has seen it before tile 0 runs; one-frame streaming
    // has not. Any accidental use of that future validation maximum as an
    // earlier tile's lower witness therefore changes deterministic work.
    const FRAMES: usize = 8193;
    const FUTURE_PEAK_FRAME: usize = 7000;
    let mut samples = Vec::with_capacity(FRAMES);
    for frame in 0..FRAMES {
        let x = frame as f64;
        samples.push(0.035 * (0.91 * x + 0.17).sin() + 0.01 * (2.73 * x - 0.4).cos());
    }
    samples[FUTURE_PEAK_FRAME] = 1.0;

    let run = |pattern: &[usize]| {
        let mut meter = CertifiedPeakMeter::new(
            192_000,
            1,
            EdgePolicy::RepeatEndpoints,
            PeakTier::Fast,
        )
        .unwrap();
        let mut frame = 0usize;
        let mut pattern_index = 0usize;
        while frame < FRAMES {
            let take = pattern[pattern_index % pattern.len()].min(FRAMES - frame);
            meter.push_interleaved(&samples[frame..frame + take]).unwrap();
            frame += take;
            pattern_index += 1;
        }
        meter.finalize().unwrap()
    };

    let whole = strip_timing(run(&[FRAMES]));
    let one = strip_timing(run(&[1]));
    let irregular = strip_timing(run(&[5, 37, 5777, 3, 257, 19, 911]));
    assert!(whole.diagnostics.groups_expanded > 0);
    assert!(whole.diagnostics.phase_evaluations > 0);
    assert_eq!(whole, one, "whole push vs one-frame push");
    assert_eq!(whole, irregular, "whole push vs irregular pushes");
}

#[test]
fn public_reference_constant_carrier_is_chunk_invariant_across_edges() {
    // Public counterpart of the internal tie-heavy regression. Thirty-eight
    // frames is the smallest constant carrier that preserves the former
    // 1-versus-37 caller boundary while keeping this regression cheap.
    let samples = vec![0.75_f64; 38];
    for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
        let run = |pattern: &[usize]| {
            let mut meter =
                CertifiedPeakMeter::new(192_000, 1, edge, PeakTier::Reference).unwrap();
            let mut frame = 0usize;
            let mut pattern_index = 0usize;
            while frame < samples.len() {
                let take = pattern[pattern_index % pattern.len()].min(samples.len() - frame);
                meter.push_interleaved(&samples[frame..frame + take]).unwrap();
                frame += take;
                pattern_index += 1;
            }
            meter.finalize().unwrap()
        };

        let whole = run(&[samples.len()]);
        let one = run(&[1]);
        let thirty_seven = run(&[37]);
        let irregular = run(&[5, 1, 17, 3, 12]);
        assert_eq!(whole, one, "{edge:?}: whole vs 1-frame");
        assert_eq!(whole, thirty_seven, "{edge:?}: whole vs 37-frame");
        assert_eq!(whole, irregular, "{edge:?}: whole vs irregular");
        assert_eq!(whole.status, SearchStatus::Complete);
    }
}

#[test]
fn fast066_never_reports_time_limited_or_executes_retired_fast_machinery() {
    let samples = signal(257, 1);
    let mut meter = CertifiedPeakMeter::new(192_000, 1, EdgePolicy::RepeatEndpoints, PeakTier::Fast).unwrap();
    meter.push_interleaved(&samples).unwrap();
    let certificate = meter.finalize().unwrap();
    assert!(matches!(certificate.status, SearchStatus::Complete | SearchStatus::WorkLimited));
    assert_eq!(certificate.diagnostics.time_limited_tiles, 0);
    assert_eq!(certificate.diagnostics.time_bounded_prefix_blocks_skipped, 0);
    assert_eq!(certificate.diagnostics.accelerated_l1_groups_tested, 0);
    assert_eq!(certificate.diagnostics.accelerated_l1_groups_rejected, 0);
    assert_eq!(certificate.diagnostics.accelerated_curvature_roots, 0);
    assert!(certificate.diagnostics.fast_survey_knots > 0);
    assert!(certificate.diagnostics.fast_flat_groups > 0);
    assert!(certificate.diagnostics.fast_hq4_peak_linear >= certificate.diagnostics.fast_input_sample_peak_linear);
    assert!(certificate.reported_point_estimate.overall.linear() >= certificate.diagnostics.fast_hq4_peak_linear);
}

#[test]
fn reporting_profile_is_separate_and_rate_dependent() {
    assert_eq!(ReportingPeakMeter::new(44_100, 1).unwrap().oversample_factor(), 4);
    assert_eq!(ReportingPeakMeter::new(96_000, 1).unwrap().oversample_factor(), 2);
    assert_eq!(ReportingPeakMeter::new(192_000, 1).unwrap().oversample_factor(), 1);
}

#[test]
fn reporting_and_certified_input_validation_is_transactional() {
    let mut reporting = ReportingPeakMeter::new(48_000, 2).unwrap();
    reporting.push_interleaved(&[0.4, -0.3]).unwrap();
    let before = reporting.clone();
    assert!(matches!(reporting.push_interleaved(&[0.1]), Err(TruePeakError::IncompleteFrame { .. })));
    assert_eq!(reporting.finalize().unwrap(), before.finalize().unwrap());

    let mut certified = CertifiedPeakMeter::new(192_000, 1, EdgePolicy::RepeatEndpoints, PeakTier::Standard).unwrap();
    certified.push_interleaved(&[0.4]).unwrap();
    let before = certified.clone();
    assert!(matches!(certified.push_interleaved(&[f64::NAN]), Err(TruePeakError::NonFiniteSample { .. })));
    assert_eq!(certified.finalize().unwrap(), before.finalize().unwrap());
}

#[test]
fn exact_reporting_silence_remains_silence() {
    let mut meter = ReportingPeakMeter::new(48_000, 2).unwrap();
    meter.push_interleaved(&[0.0; 32]).unwrap();
    let result = meter.finalize().unwrap();
    assert_eq!(result.overall, PeakLevel::Silence);
}

fn finite_dbtp(level: PeakLevel) -> f64 {
    match level {
        PeakLevel::Finite { dbtp, .. } => dbtp,
        PeakLevel::Silence => panic!("expected finite peak"),
    }
}

fn reporting_mono_at_rate(samples: &[f64], sample_rate_hz: u32) -> PeakLevel {
    let mut meter = ReportingPeakMeter::new(sample_rate_hz, 1).unwrap();
    meter.push_interleaved(samples).unwrap();
    meter.finalize().unwrap().overall
}

fn aligned_multitone(
    frames: usize,
    aligned_time: f64,
    components: &[(f64, f64)],
) -> Vec<f64> {
    use std::f64::consts::PI;
    (0..frames)
        .map(|frame| {
            components
                .iter()
                .map(|(frequency, amplitude)| {
                    amplitude * (2.0 * PI * frequency * (frame as f64 - aligned_time)).cos()
                })
                .sum::<f64>()
        })
        .collect()
}

#[test]
fn reporting_is_bit_identical_across_irregular_push_sizes() {
    let mut samples = Vec::new();
    for frame in 0..4097 {
        samples.push((frame as f64 * 0.017).sin() * 0.81);
        samples.push((frame as f64 * 0.031).cos() * 0.63);
    }
    let mut one = ReportingPeakMeter::new(48_000, 2).unwrap();
    one.push_interleaved(&samples).unwrap();
    let one = one.finalize().unwrap();

    let mut chunked = ReportingPeakMeter::new(48_000, 2).unwrap();
    let frame_chunks = [1usize, 7, 29, 3, 257, 11, 64];
    let mut frame = 0usize;
    let mut which = 0usize;
    while frame < samples.len() / 2 {
        let count = frame_chunks[which % frame_chunks.len()].min(samples.len() / 2 - frame);
        chunked
            .push_interleaved(&samples[frame * 2..(frame + count) * 2])
            .unwrap();
        frame += count;
        which += 1;
    }
    let chunked = chunked.finalize().unwrap();
    assert_eq!(one, chunked);
}

#[test]
fn reporting_matches_frozen_libebur128_1_2_6_across_sample_rates() {
    let signal = aligned_multitone(
        4096,
        2000.5,
        &[(0.30, 1.0 / 6.0), (0.35, 1.0 / 6.0), (0.40, 1.0 / 6.0)],
    );
    let references = [
        (44_100, -6.273_137_129_108_395),
        (48_000, -6.273_137_129_108_395),
        (96_000, -6.007_520_784_414_950),
        (192_000, -6.783_443_214_035_310),
    ];
    for (sample_rate_hz, expected_dbtp) in references {
        let measured = finite_dbtp(reporting_mono_at_rate(&signal, sample_rate_hz));
        assert!(
            (measured - expected_dbtp).abs() < 0.000_01,
            "{sample_rate_hz} Hz: measured {measured:.12}, libebur128 1.2.6 {expected_dbtp:.12}",
        );
    }
}

#[test]
fn reporting_matches_frozen_libebur128_1_2_6_at_finite_stream_boundaries() {
    use std::f64::consts::PI;
    let rate = 48_000u32;
    let cases = [
        (
            "one_second_440_hz_nonzero_phase",
            (0..rate as usize)
                .map(|frame| 0.5 * (2.0 * PI * 440.0 * frame as f64 / rate as f64 + 0.3).cos())
                .collect::<Vec<_>>(),
            -5.446_101_414_459_087,
        ),
        ("constant_nonzero", vec![0.25; 128], -11.008_690_159_967_02),
        ("one_frame", vec![0.5], -6.020_599_913_279_624),
        (
            "start_impulse",
            std::iter::once(1.0)
                .chain(std::iter::repeat_n(0.0, 63))
                .collect::<Vec<_>>(),
            0.0,
        ),
        (
            "end_impulse",
            std::iter::repeat_n(0.0, 63)
                .chain(std::iter::once(1.0))
                .collect::<Vec<_>>(),
            0.0,
        ),
    ];
    for (name, signal, reference) in cases {
        let measured = finite_dbtp(reporting_mono_at_rate(&signal, rate));
        assert!(
            (measured - reference).abs() < 0.000_01,
            "{name}: measured {measured:.12}, libebur128 1.2.6 {reference:.12}",
        );
    }
}

#[test]
fn reporting_matches_real_material_frozen_libebur128_reference() {
    const BYTES: &[u8] = include_bytes!("fixtures/real_reference_48k_stereo.f64le");
    const FRAMES: usize = 48_000;
    const CHANNELS: usize = 2;
    const LIBEBUR128_1_2_6_DBTP: f64 = -0.108_161_099_781_057_48;
    assert_eq!(BYTES.len(), FRAMES * CHANNELS * 8);
    let samples = BYTES
        .chunks_exact(8)
        .map(|chunk| f64::from_le_bytes(chunk.try_into().expect("exact f64 fixture chunk")))
        .collect::<Vec<_>>();
    let mut meter = ReportingPeakMeter::new(48_000, CHANNELS).unwrap();
    meter.push_interleaved(&samples).unwrap();
    let measured = finite_dbtp(meter.finalize().unwrap().overall);
    assert!(
        (measured - LIBEBUR128_1_2_6_DBTP).abs() <= 0.01,
        "real material Reporting4x: measured {measured:.12}, libebur128 1.2.6 {LIBEBUR128_1_2_6_DBTP:.12}",
    );
}
