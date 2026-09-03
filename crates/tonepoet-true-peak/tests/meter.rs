use std::f64::consts::PI;

use tonepoet_true_peak::{
    headroom64x_authority, EdgePolicy, HeadroomAuthorityError, PeakLevel, TruePeakConfig,
    TruePeakMeter, TruePeakMode, HEADROOM64X_GRID_MAX_UNDERREAD_DB,
    HEADROOM64X_MAX_UNDERREAD_DB, HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE,
};

fn meter_at_rate(mode: TruePeakMode, channels: usize, sample_rate_hz: u32) -> TruePeakMeter {
    TruePeakMeter::new(
        TruePeakConfig::new(sample_rate_hz, channels)
            .with_mode(mode)
            .with_edge_policy(EdgePolicy::RepeatEndpoints),
    )
    .expect("valid meter")
}

fn meter(mode: TruePeakMode, channels: usize) -> TruePeakMeter {
    meter_at_rate(mode, channels, 48_000)
}

fn finite_dbtp(level: PeakLevel) -> f64 {
    match level {
        PeakLevel::Finite { dbtp, .. } => dbtp,
        PeakLevel::Silence => panic!("expected finite peak"),
    }
}

fn tapered_sine(fraction_of_rate: f64, amplitude: f64, phase_degrees: f64) -> Vec<f64> {
    let frames = 48_000 / 2;
    let fade_frames = 480; // 10 ms at 48 kHz, matching EBU Tech 3341.
    let phase = phase_degrees.to_radians();
    let omega = 2.0 * PI * fraction_of_rate;
    (0..frames)
        .map(|frame| {
            let fade_in = if frame < fade_frames {
                0.5 - 0.5 * (PI * frame as f64 / fade_frames as f64).cos()
            } else {
                1.0
            };
            let tail = frames - 1 - frame;
            let fade_out = if tail < fade_frames {
                0.5 - 0.5 * (PI * tail as f64 / fade_frames as f64).cos()
            } else {
                1.0
            };
            amplitude * fade_in.min(fade_out) * (omega * frame as f64 + phase).sin()
        })
        .collect()
}

fn cosine_with_flat_center(
    frames: usize,
    fade_frames: usize,
    fraction_of_rate: f64,
    phase_cycles: f64,
) -> Vec<f64> {
    (0..frames)
        .map(|frame| {
            let edge = frame.min(frames - 1 - frame);
            let envelope = if edge < fade_frames {
                0.5 - 0.5 * (PI * edge as f64 / fade_frames as f64).cos()
            } else {
                1.0
            };
            envelope * (2.0 * PI * (fraction_of_rate * frame as f64 + phase_cycles)).cos()
        })
        .collect()
}

fn aligned_multitone(
    frames: usize,
    fade_frames: usize,
    aligned_time: f64,
    components: &[(f64, f64)],
) -> Vec<f64> {
    (0..frames)
        .map(|frame| {
            let edge = frame.min(frames - 1 - frame);
            let envelope = if edge < fade_frames {
                0.5 - 0.5 * (PI * edge as f64 / fade_frames as f64).cos()
            } else {
                1.0
            };
            envelope
                * components
                    .iter()
                    .map(|(frequency, amplitude)| {
                        amplitude * (2.0 * PI * frequency * (frame as f64 - aligned_time)).cos()
                    })
                    .sum::<f64>()
        })
        .collect()
}

fn bandlimited_enveloped_aligned_multitone(
    frames: usize,
    aligned_time: f64,
    frequencies: &[f64],
) -> Vec<f64> {
    let last = (frames - 1) as f64;
    assert!(aligned_time > 0.0 && aligned_time < last);
    let half_period = aligned_time.max(last - aligned_time);
    let envelope_frequency = 1.0 / (2.0 * half_period);
    assert!(
        frequencies.iter().copied().fold(0.0, f64::max) + envelope_frequency < 0.5,
        "envelope sideband crossed Nyquist"
    );

    (0..frames)
        .map(|frame| {
            let relative = frame as f64 - aligned_time;
            // cos^2 is in [0, 1] and is exactly 1 at the known aligned peak.
            // Choosing its half-period from the farther endpoint makes one end
            // exactly zero and the other extremely close to zero. Its spectrum
            // is only DC plus +/- envelope_frequency, so the stated assertion
            // proves every carrier sideband remains below original Nyquist.
            let envelope = (PI * relative / (2.0 * half_period)).cos().powi(2);
            envelope
                * frequencies
                    .iter()
                    .map(|frequency| (2.0 * PI * frequency * relative).cos())
                    .sum::<f64>()
                / frequencies.len() as f64
        })
        .collect()
}

fn measure_mono_at_rate(samples: &[f64], mode: TruePeakMode, sample_rate_hz: u32) -> PeakLevel {
    let mut meter = meter_at_rate(mode, 1, sample_rate_hz);
    meter.push_interleaved(samples).unwrap();
    meter.finalize().unwrap().overall
}

fn measure_mono(samples: &[f64], mode: TruePeakMode) -> PeakLevel {
    measure_mono_at_rate(samples, mode, 48_000)
}

#[test]
fn identical_frames_one_block_or_irregular_blocks_are_bit_identical() {
    let mut samples = Vec::new();
    for frame in 0..4097 {
        samples.push((frame as f64 * 0.017).sin() * 0.81);
        samples.push((frame as f64 * 0.031).cos() * 0.63);
    }

    for mode in [TruePeakMode::Reporting4x, TruePeakMode::Headroom64x] {
        let mut one = meter(mode, 2);
        one.push_interleaved(&samples).unwrap();
        let one = one.finalize().unwrap();

        let mut chunked = meter(mode, 2);
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

        assert_eq!(one.frames, chunked.frames, "mode={mode:?}");
        assert_eq!(
            one.channel_linear_peaks.len(),
            chunked.channel_linear_peaks.len(),
            "mode={mode:?}"
        );
        assert_eq!(
            one.overall.linear().to_bits(),
            chunked.overall.linear().to_bits(),
            "mode={mode:?}"
        );
        for (left, right) in one
            .channel_linear_peaks
            .iter()
            .zip(&chunked.channel_linear_peaks)
        {
            assert_eq!(left.to_bits(), right.to_bits(), "mode={mode:?}");
        }
    }
}

#[test]
fn silence_is_negative_infinity_and_near_silence_is_finite() {
    for mode in [TruePeakMode::Reporting4x, TruePeakMode::Headroom64x] {
        let silence = measure_mono(&[0.0; 64], mode);
        assert_eq!(silence, PeakLevel::Silence, "mode={mode:?}");
        assert!(
            silence.dbtp().is_infinite() && silence.dbtp().is_sign_negative(),
            "mode={mode:?}"
        );

        let near = measure_mono(&[1.0e-12; 64], mode);
        let dbtp = finite_dbtp(near);
        assert!(dbtp.is_finite(), "mode={mode:?}, dbtp={dbtp}");
        assert!(dbtp < -230.0, "mode={mode:?}, near-silence dbtp={dbtp}");
    }
}

#[test]
fn one_frame_short_stream_and_boundaries_finalize_without_underreading_samples() {
    let one = measure_mono(&[0.5], TruePeakMode::Reporting4x);
    assert!(one.linear() >= 0.5);
    assert!(finite_dbtp(one) > -6.03 && finite_dbtp(one) < -6.00);

    let short = measure_mono(&[0.25, 0.25, 0.25], TruePeakMode::Headroom64x);
    assert!(short.linear() >= 0.25);

    let first_impulse = measure_mono(&[1.0, 0.0, 0.0, 0.0, 0.0], TruePeakMode::Reporting4x);
    let last_impulse = measure_mono(&[0.0, 0.0, 0.0, 0.0, 1.0], TruePeakMode::Reporting4x);
    assert!(first_impulse.linear() >= 1.0);
    assert!(last_impulse.linear() >= 1.0);
}

#[test]
fn headroom64x_short_stream_start_and_end_peaks_cover_both_edge_policies() {
    for edge_policy in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
        for signal in [
            vec![0.5],
            vec![0.25, 0.25, 0.25],
            vec![1.0, 0.0, 0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0, 0.0, 1.0],
        ] {
            let sample_peak = signal.iter().copied().map(f64::abs).fold(0.0, f64::max);
            let mut meter = TruePeakMeter::new(
                TruePeakConfig::new(48_000, 1)
                    .with_mode(TruePeakMode::Headroom64x)
                    .with_edge_policy(edge_policy),
            )
            .unwrap();
            meter.push_interleaved(&signal).unwrap();
            let measured = meter.finalize().unwrap().overall.linear();
            assert!(
                measured >= sample_peak,
                "edge_policy={edge_policy:?}, signal={signal:?}, measured={measured}, sample_peak={sample_peak}"
            );
        }
    }
}

#[test]
fn above_full_scale_is_reported_not_clamped() {
    let peak = measure_mono(&[1.5], TruePeakMode::Headroom64x);
    assert!(peak.linear() >= 1.5, "above-full-scale input was under-read: {peak:?}");
    assert!(finite_dbtp(peak) > 3.52, "above-full-scale input was clamped: {peak:?}");
    assert!(finite_dbtp(peak) < 3.54, "unexpected interpolation ripple: {peak:?}");
}

#[test]
fn multichannel_result_is_maximum_across_channels() {
    // Fade the block in and out. `Reporting4x` deliberately follows libebur128's
    // finite-stream contract -- zero-initialized state, no synthetic pre-roll --
    // so an abrupt onset is a step edge and the interpolator rings on it: the
    // same constants presented as a hard 64-frame block measure 0.900981 rather
    // than 0.8, a +1.03 dB onset overshoot that is correct for this mode and has
    // nothing to do with the across-channel maximum under test. Measured with a
    // 256-frame fade the flat centre reads 0.800991, inside the tolerance below.
    const FRAMES: usize = 1024;
    const FADE: usize = 256;
    let mut interleaved = Vec::new();
    for frame in 0..FRAMES {
        let edge = frame.min(FRAMES - 1 - frame);
        let envelope = if edge < FADE {
            0.5 - 0.5 * (PI * edge as f64 / FADE as f64).cos()
        } else {
            1.0
        };
        for level in [0.1, -0.25, 0.8, -0.4] {
            interleaved.push(envelope * level);
        }
    }
    let mut meter = meter(TruePeakMode::Reporting4x, 4);
    meter.push_interleaved(&interleaved).unwrap();
    let result = meter.finalize().unwrap();
    assert_eq!(result.channel_linear_peaks.len(), 4);
    assert!((result.channel_linear_peaks[2] - 0.8).abs() < 0.002);
    assert_eq!(result.overall.linear().to_bits(), result.channel_linear_peaks[2].to_bits());
}

#[test]
fn deterministic_repeated_measurements_match_bit_for_bit() {
    let signal = tapered_sine(1.0 / 6.0, 0.73, 37.0);
    let first = measure_mono(&signal, TruePeakMode::Headroom64x);
    let second = measure_mono(&signal, TruePeakMode::Headroom64x);
    assert_eq!(first.linear().to_bits(), second.linear().to_bits());
    assert_eq!(first.dbtp().to_bits(), second.dbtp().to_bits());
}

#[test]
fn invalid_blocks_are_rejected_before_state_is_mutated() {
    let mut meter = meter(TruePeakMode::Reporting4x, 2);
    assert!(meter.push_interleaved(&[0.0]).is_err());
    assert!(meter.push_interleaved(&[0.0, f64::NAN]).is_err());
    meter.push_interleaved(&[0.25, -0.25]).unwrap();
    assert_eq!(meter.finalize().unwrap().frames, 1);
}

// EBU Tech 3341 v4.0 (2023), minimum-requirement true-peak tests 16-19.
// Published expected windows are -6.0 dBTP +0.2/-0.4 for tests 16-18 and
// +3.0 dBTP +0.2/-0.4 for test 19. These are standards-derived expected
// values, not values generated by this implementation.
#[test]
fn ebu_tech_3341_true_peak_synthetic_vectors_16_to_19_are_in_tolerance() {
    let cases = [
        ("16", 1.0 / 4.0, 0.50, 45.0, -6.0),
        ("17", 1.0 / 6.0, 0.50, 60.0, -6.0),
        ("18", 1.0 / 8.0, 0.50, 67.5, -6.0),
        ("19", 1.0 / 4.0, 1.41, 45.0, 3.0),
    ];
    for (name, frequency, amplitude, phase, expected) in cases {
        let signal = tapered_sine(frequency, amplitude, phase);
        let dbtp = finite_dbtp(measure_mono(&signal, TruePeakMode::Reporting4x));
        assert!(
            dbtp >= expected - 0.4 && dbtp <= expected + 0.2,
            "EBU Tech 3341 test {name}: measured {dbtp:.6} dBTP, expected {expected:.1} +0.2/-0.4 dB"
        );
    }
}

#[test]
fn real_program_material_matches_frozen_independent_references() {
    // Genuine recorded programme material, not a generated carrier. The fixture is a
    // one-second excerpt (source frames 384000..432000) of Gradio's checked-in
    // gradio/media_assets/audio/sax.wav, duplicated from its original 48 kHz mono
    // channel to stereo and stored as interleaved little-endian f64 PCM.
    //
    // Upstream sax.wav SHA-256:
    // 12ee32c66257e1c98ed0f2f7b708a1eab638ec09f4c69dda3ec1d78047a7be4d
    // Fixture SHA-256:
    // b6ba8b041ebd87543f04f92267487937128acc9905fc743323567682ef77fd20
    // License/provenance and the exact derivation command are in tests/fixtures/README.md.
    //
    // Frozen independent references, measured from these exact fixture bytes before
    // check-in (neither tool runs during cargo test):
    // - Reporting4x: libebur128 1.2.6, ebur128_add_frames_double/ebur128_true_peak.
    // - Headroom64x cross-check: FFmpeg 7.1.5 + libsoxr 0.1.3 at 256x,
    //   precision=33, cheby=1, cutoff=1.0, with the maximum read from f64 output.
    // The libsoxr observation is deliberately not mathematical proof of the published
    // 0.030 dB Headroom64x bound; the analytical tests remain the authority for it.
    const BYTES: &[u8] = include_bytes!("fixtures/real_reference_48k_stereo.f64le");
    const FRAMES: usize = 48_000;
    const CHANNELS: usize = 2;
    const LIBEBUR128_1_2_6_DBTP: f64 = -0.108_161_099_781_057_48;
    const LIBEBUR128_TOLERANCE_DB: f64 = 0.01;
    const SOXR_256X_DBTP: f64 = -0.112_265_386_047_262_47;
    const SOXR_OBSERVATION_TOLERANCE_DB: f64 = 0.10;

    assert_eq!(BYTES.len(), FRAMES * CHANNELS * 8);
    let samples = BYTES
        .chunks_exact(8)
        .map(|chunk| {
            let bytes: [u8; 8] = chunk.try_into().expect("exact f64 fixture chunk");
            f64::from_le_bytes(bytes)
        })
        .collect::<Vec<_>>();

    let mut reporting = meter_at_rate(TruePeakMode::Reporting4x, CHANNELS, 48_000);
    reporting.push_interleaved(&samples).unwrap();
    let reporting_dbtp = finite_dbtp(reporting.finalize().unwrap().overall);
    assert!(
        (reporting_dbtp - LIBEBUR128_1_2_6_DBTP).abs() <= LIBEBUR128_TOLERANCE_DB,
        "real material Reporting4x: measured {reporting_dbtp:.12}, libebur128 1.2.6 {LIBEBUR128_1_2_6_DBTP:.12}"
    );

    let mut headroom = meter_at_rate(TruePeakMode::Headroom64x, CHANNELS, 48_000);
    headroom.push_interleaved(&samples).unwrap();
    let headroom_dbtp = finite_dbtp(headroom.finalize().unwrap().overall);
    assert!(
        (headroom_dbtp - SOXR_256X_DBTP).abs() <= SOXR_OBSERVATION_TOLERANCE_DB,
        "real material Headroom64x: measured {headroom_dbtp:.12}, 256x libsoxr {SOXR_256X_DBTP:.12}"
    );
}

#[test]
fn reporting_profile_matches_libebur128_1_2_6_across_sample_rates() {
    // Independent reference values were produced by libebur128 1.2.6 using
    // ebur128_add_frames_double/ebur128_true_peak on this exact carrier.
    let signal = aligned_multitone(
        4096,
        0,
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
        let measured = finite_dbtp(measure_mono_at_rate(
            &signal,
            TruePeakMode::Reporting4x,
            sample_rate_hz,
        ));
        assert!(
            (measured - expected_dbtp).abs() < 0.000_01,
            "{sample_rate_hz} Hz: measured {measured:.12}, libebur128 1.2.6 {expected_dbtp:.12}"
        );
    }
}

#[test]
fn reporting_profile_matches_libebur128_1_2_6_at_finite_stream_boundaries() {
    // Direct libebur128 1.2.6 references using add_frames_double/true_peak.
    // These deliberately exercise startup/finalization rather than only a
    // steady-state region where endpoint synthesis can accidentally agree.
    let rate = 48_000u32;
    let cases = [
        (
            "one_second_440_hz_nonzero_phase",
            (0..rate as usize)
                .map(|frame| {
                    0.5 * (2.0 * PI * 440.0 * frame as f64 / rate as f64 + 0.3).cos()
                })
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
        let measured = finite_dbtp(measure_mono_at_rate(
            &signal,
            TruePeakMode::Reporting4x,
            rate,
        ));
        assert!(
            (measured - reference).abs() < 0.000_01,
            "{name}: measured {measured:.12}, libebur128 1.2.6 {reference:.12}"
        );
    }
}

#[test]
fn reporting_profile_has_fixed_libebur128_boundary_semantics() {
    let signal = (0..48_000)
        .map(|frame| 0.5 * (2.0 * PI * 440.0 * frame as f64 / 48_000.0 + 0.3).cos())
        .collect::<Vec<_>>();

    let measure = |edge_policy| {
        let mut meter = TruePeakMeter::new(
            TruePeakConfig::new(48_000, 1)
                .with_mode(TruePeakMode::Reporting4x)
                .with_edge_policy(edge_policy),
        )
        .unwrap();
        meter.push_interleaved(&signal).unwrap();
        meter.finalize().unwrap().overall.linear()
    };

    assert_eq!(
        measure(EdgePolicy::RepeatEndpoints).to_bits(),
        measure(EdgePolicy::ZeroExtend).to_bits(),
        "Reporting4x edge behavior is fixed by the libebur128 compatibility contract"
    );
}

#[test]
fn reporting_profile_exposes_its_rate_dependent_factor() {
    assert_eq!(
        TruePeakMode::Reporting4x.oversample_factor_for_sample_rate(44_100),
        4
    );
    assert_eq!(
        TruePeakMode::Reporting4x.oversample_factor_for_sample_rate(96_000),
        2
    );
    assert_eq!(
        TruePeakMode::Reporting4x.oversample_factor_for_sample_rate(192_000),
        1
    );
    assert_eq!(
        TruePeakMode::Headroom64x.oversample_factor_for_sample_rate(384_000),
        64
    );
}

#[test]
fn headroom_declared_one_sided_bound_meets_hard_authority_contract() {
    let grid_formula_db = 20.0 * (1.0 / (PI / 128.0).cos()).log10();
    assert!(
        (HEADROOM64X_GRID_MAX_UNDERREAD_DB - grid_formula_db).abs() < 1.0e-15,
        "published grid bound {} disagrees with formula {}",
        HEADROOM64X_GRID_MAX_UNDERREAD_DB,
        grid_formula_db,
    );
    assert!(HEADROOM64X_MAX_UNDERREAD_DB > HEADROOM64X_GRID_MAX_UNDERREAD_DB);
    assert!(HEADROOM64X_MAX_UNDERREAD_DB <= 0.05);
    assert_eq!(HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE, 0.495);
}

#[test]
fn reviewer_aligned_three_tone_is_inside_headroom_accuracy_contract() {
    // Continuous-time truth is exact: at t=2000.5 every cosine is 1 and the
    // triangle inequality proves no larger magnitude exists. Therefore peak =
    // 0.5 exactly (-6.020599913 dBTP) although the decoded sample peak is only
    // about 0.457960309 (-6.783443214 dBFS).
    let signal = aligned_multitone(
        4096,
        0,
        2000.5,
        &[(0.30, 1.0 / 6.0), (0.35, 1.0 / 6.0), (0.40, 1.0 / 6.0)],
    );
    let sample_peak = signal.iter().copied().map(f64::abs).fold(0.0, f64::max);
    let measured = measure_mono_at_rate(&signal, TruePeakMode::Headroom64x, 44_100);
    let true_dbtp = 20.0 * 0.5_f64.log10();
    let error_db = finite_dbtp(measured) - true_dbtp;

    assert!((sample_peak - 0.457_960_309).abs() < 1.0e-9, "sample={sample_peak}");
    assert!(
        error_db.abs() <= 0.05,
        "Headroom64x analytical three-tone error {error_db:.9} dB"
    );
    assert!(
        -error_db <= HEADROOM64X_MAX_UNDERREAD_DB,
        "Headroom64x under-read exceeded declared bound: {error_db:.9} dB"
    );
}

#[test]
fn reviewer_aligned_five_tone_upper_band_regression_meets_total_accuracy_contract() {
    // Exact continuous-time truth: every component is <= 1 and all five are
    // exactly 1 at t=511.5, so the weighted sum peaks at exactly 1.0 (0 dBTP).
    // The decoded sample peak is only ~0.868611568 (-1.223488 dBFS).
    let frequencies = [0.4850, 0.4875, 0.4900, 0.4925, 0.4950];
    let signal = (0..1024)
        .map(|frame| {
            frequencies
                .iter()
                .map(|frequency| {
                    (2.0 * PI * frequency * (frame as f64 - 511.5)).cos()
                })
                .sum::<f64>()
                / frequencies.len() as f64
        })
        .collect::<Vec<_>>();
    let sample_peak = signal.iter().copied().map(f64::abs).fold(0.0, f64::max);
    let measured = finite_dbtp(measure_mono_at_rate(
        &signal,
        TruePeakMode::Headroom64x,
        44_100,
    ));

    assert!((sample_peak - 0.868_611_568).abs() < 1.0e-9, "sample={sample_peak}");
    assert!(
        -measured <= HEADROOM64X_MAX_UNDERREAD_DB,
        "Headroom64x under-read exceeded declared bound: {measured:.9} dB"
    );
    assert!(
        measured.abs() <= 0.05,
        "Headroom64x total point-estimate error exceeded 0.05 dB: {measured:.9} dB"
    );
}

#[test]
fn headroom64x_normalized_measurement_is_sample_rate_independent() {
    let frequencies = [0.4850, 0.4875, 0.4900, 0.4925, 0.4950];
    let signal = (0..1024)
        .map(|frame| {
            frequencies
                .iter()
                .map(|frequency| {
                    (2.0 * PI * frequency * (frame as f64 - 511.5)).cos()
                })
                .sum::<f64>()
                / frequencies.len() as f64
        })
        .collect::<Vec<_>>();
    let baseline = measure_mono_at_rate(&signal, TruePeakMode::Headroom64x, 44_100).linear();
    for rate in [48_000, 96_000, 192_000, 384_000] {
        let measured = measure_mono_at_rate(&signal, TruePeakMode::Headroom64x, rate).linear();
        assert_eq!(baseline.to_bits(), measured.to_bits(), "sample_rate_hz={rate}");
    }
}

#[test]
fn headroom64x_qualified_upper_band_multitone_family_meets_declared_bound() {
    // Exact continuous-time peak is 1.0 by component alignment and the
    // triangle inequality. The low-rate cos^2 envelope keeps every sideband
    // inside the frozen 0.495 * Fs authority domain.
    let bands: &[&[f64]] = &[
        &[0.45, 0.46, 0.47, 0.48, 0.49],
        &[0.475, 0.480, 0.485, 0.490, 0.493],
        &[0.4850, 0.4870, 0.4890, 0.4910, 0.4930],
    ];
    let alignments = [511.25, 511.5, 511.75];
    for aligned_time in alignments {
        for frequencies in bands {
            let signal = bandlimited_enveloped_aligned_multitone(1024, aligned_time, frequencies);
            let measured = measure_mono_at_rate(&signal, TruePeakMode::Headroom64x, 44_100);
            let measured_db = finite_dbtp(measured);
            let half_period = aligned_time.max(1023.0 - aligned_time);
            let envelope_frequency = 1.0 / (2.0 * half_period);
            let max_frequency = frequencies.iter().copied().fold(0.0, f64::max)
                + envelope_frequency;
            assert!(max_frequency <= HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE);
            let authority = headroom64x_authority(measured, max_frequency).unwrap();
            assert!(
                -measured_db <= HEADROOM64X_MAX_UNDERREAD_DB,
                "alignment={aligned_time}, frequencies={frequencies:?}, under-read={:.9} dB",
                -measured_db
            );
            assert!(
                measured_db.abs() <= 0.05,
                "alignment={aligned_time}, frequencies={frequencies:?}, point-estimate error={measured_db:.9} dB"
            );
            assert!(authority.dbtp() >= 0.0);
        }
    }
}

#[test]
fn headroom64x_dense_multitone_family_reaches_the_qualified_band_edge() {
    let frames = 4096usize;
    let center = (frames - 1) as f64 / 2.0;
    for offset in [-0.375, -0.125, 0.0, 0.125, 0.375] {
        let aligned_time = center + offset;
        let half_period = aligned_time.max((frames - 1) as f64 - aligned_time);
        let envelope_frequency = 1.0 / (2.0 * half_period);
        let high = HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE
            - envelope_frequency
            - 1.0e-6;
        let low = high - 0.0045;
        let frequencies = (0..9)
            .map(|index| low + (high - low) * index as f64 / 8.0)
            .collect::<Vec<_>>();
        let signal = bandlimited_enveloped_aligned_multitone(frames, aligned_time, &frequencies);
        let point = measure_mono_at_rate(&signal, TruePeakMode::Headroom64x, 96_000);
        let point_db = finite_dbtp(point);
        let support = high + envelope_frequency;
        assert!(support <= HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE);
        assert!(
            -point_db <= HEADROOM64X_MAX_UNDERREAD_DB,
            "edge-family alignment={aligned_time:.6}, under-read={:.9} dB",
            -point_db
        );
        assert!(
            point_db.abs() <= 0.05,
            "edge-family alignment={aligned_time:.6}, point error={point_db:.9} dB"
        );
        assert!(headroom64x_authority(point, support).unwrap().dbtp() >= 0.0);
    }
}

#[test]
fn frozen_adversarial_case_0712_stays_inside_headroom64_authority_contract() {
    // Worst one-sided under-read found by the frozen 4000-case deterministic
    // analytical search. The cos^2 envelope and both carriers equal one at
    // aligned_time, so the continuous peak is exactly 1.0; the complete
    // carrier+envelope support remains below the qualified 0.495 * Fs band.
    let aligned_time = 639.826_807_719_896_6;
    let frequencies = [0.474_675_584_167_041_6, 0.492_117_256_039_306_06];
    let signal = bandlimited_enveloped_aligned_multitone(1280, aligned_time, &frequencies);
    let point = measure_mono_at_rate(&signal, TruePeakMode::Headroom64x, 48_000);
    let point_db = finite_dbtp(point);
    let envelope_frequency = 1.0 / (2.0 * aligned_time.max(1279.0 - aligned_time));
    let support = frequencies.iter().copied().fold(0.0, f64::max) + envelope_frequency;
    assert!(support <= HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE);
    assert!(
        -point_db <= HEADROOM64X_MAX_UNDERREAD_DB,
        "frozen adversarial under-read={:.9} dB",
        -point_db
    );
    assert!(
        point_db.abs() <= 0.05,
        "frozen adversarial point error={point_db:.9} dB"
    );
    assert!(headroom64x_authority(point, support).unwrap().dbtp() >= 0.0);
}

#[test]
fn frozen_independent_adversarial_case_4701_stays_inside_headroom64_authority_contract() {
    // Strongest one-sided under-read found by the independent 12,000-case
    // upper-band-biased analytical search (seed 0xBAD5EED64). The cos^2
    // envelope and all four carriers equal one at aligned_time, so the
    // continuous peak is exactly 1.0 by the triangle inequality.
    let aligned_time = 255.075_924_466_840_8;
    let frequencies = [
        0.477_486_748_032_391_55,
        0.484_269_849_528_041_53,
        0.487_700_211_455_845_2,
        0.489_453_684_821_182_95,
    ];
    let signal = bandlimited_enveloped_aligned_multitone(512, aligned_time, &frequencies);
    let point = measure_mono_at_rate(&signal, TruePeakMode::Headroom64x, 44_100);
    let point_db = finite_dbtp(point);
    let envelope_frequency = 1.0 / (2.0 * aligned_time.max(511.0 - aligned_time));
    let support = frequencies.iter().copied().fold(0.0, f64::max) + envelope_frequency;
    assert!(support <= HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE);
    assert!(
        -point_db <= HEADROOM64X_MAX_UNDERREAD_DB,
        "independent frozen adversarial under-read={:.9} dB",
        -point_db
    );
    assert!(
        point_db.abs() <= 0.05,
        "independent frozen adversarial point error={point_db:.9} dB"
    );
    assert!(headroom64x_authority(point, support).unwrap().dbtp() >= 0.0);
}

#[test]
fn headroom64x_multichannel_uses_loudest_channel_without_changing_authority() {
    let aligned_time = 511.5;
    let frequencies = [0.4850, 0.4875, 0.4900, 0.4925, 0.4930];
    let loud = bandlimited_enveloped_aligned_multitone(1024, aligned_time, &frequencies);
    let mut interleaved = Vec::with_capacity(loud.len() * 3);
    for sample in &loud {
        interleaved.extend_from_slice(&[0.05 * sample, 0.25 * sample, *sample]);
    }
    let mut meter = meter_at_rate(TruePeakMode::Headroom64x, 3, 48_000);
    meter.push_interleaved(&interleaved).unwrap();
    let result = meter.finalize().unwrap();
    // `channel_linear_peaks` holds linear magnitudes; `overall` is the PeakLevel.
    // Channel 2 is the loudest here, so the assertion below establishes that
    // `overall` is exactly channel 2's peak and its dBTP is the value under test.
    let loud_db = finite_dbtp(result.overall);
    assert_eq!(
        result.overall.linear().to_bits(),
        result.channel_linear_peaks[2].to_bits()
    );
    assert!(
        -loud_db <= HEADROOM64X_MAX_UNDERREAD_DB,
        "non-first-channel under-read={:.9} dB",
        -loud_db
    );
    assert!(loud_db.abs() <= 0.05, "non-first-channel point error={loud_db:.9} dB");
    assert!(headroom64x_authority(result.overall, 0.494).unwrap().dbtp() >= 0.0);
}

#[test]
fn r3_near_nyquist_envelope_is_retained_but_refused_as_unqualified_authority() {
    // This is the R3 reviewer family around 0.4980..0.4988 * Fs. Its generating
    // continuous formula has a 1.0 aligned peak, but the finite 1024-sample
    // record plus a selectable edge convention does not uniquely determine
    // that generator arbitrarily close to critical Nyquist. Its cos^2
    // sidebands reach ~0.49978 * Fs, beyond the frozen authority domain.
    let frequencies = [0.4980, 0.4982, 0.4984, 0.4986, 0.4988];
    let aligned_time: f64 = 511.5;
    let signal = bandlimited_enveloped_aligned_multitone(1024, aligned_time, &frequencies);
    let point = measure_mono_at_rate(&signal, TruePeakMode::Headroom64x, 44_100);
    let envelope_frequency = 1.0 / (2.0 * aligned_time.max(1023.0 - aligned_time));
    let max_frequency = frequencies.iter().copied().fold(0.0, f64::max) + envelope_frequency;
    assert!(max_frequency > HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE);
    assert_eq!(
        headroom64x_authority(point, max_frequency),
        Err(HeadroomAuthorityError::OutsideQualifiedBand)
    );
}

#[test]
fn headroom64x_authority_rejects_invalid_or_unqualified_band_declarations() {
    let point = PeakLevel::Finite { linear: 1.0, dbtp: 0.0 };
    assert!(headroom64x_authority(point, 0.495).is_ok());
    assert_eq!(
        headroom64x_authority(point, 0.495_000_1),
        Err(HeadroomAuthorityError::OutsideQualifiedBand)
    );
    assert_eq!(
        headroom64x_authority(point, f64::NAN),
        Err(HeadroomAuthorityError::InvalidBandLimit)
    );
}

#[test]
fn headroom64x_authority_adds_only_the_frozen_qualified_reserve() {
    let point = PeakLevel::Finite { linear: 1.0, dbtp: 0.0 };
    let authority = headroom64x_authority(point, 0.495).unwrap();
    let PeakLevel::Finite { linear, dbtp } = authority else {
        panic!("finite point must produce finite authority");
    };
    assert!((dbtp - HEADROOM64X_MAX_UNDERREAD_DB).abs() < 1.0e-15);
    assert!((20.0 * linear.log10() - dbtp).abs() < 1.0e-12);
}

#[test]
fn headroom_edge_policy_remains_explicit_and_selectable() {
    let signal = [0.5, 0.5, 0.5];
    let measure = |edge_policy| {
        let mut meter = TruePeakMeter::new(
            TruePeakConfig::new(48_000, 1)
                .with_mode(TruePeakMode::Headroom64x)
                .with_edge_policy(edge_policy),
        )
        .unwrap();
        meter.push_interleaved(&signal).unwrap();
        meter.finalize().unwrap().overall.linear()
    };
    assert_ne!(
        measure(EdgePolicy::RepeatEndpoints).to_bits(),
        measure(EdgePolicy::ZeroExtend).to_bits()
    );
}

#[test]
fn headroom64x_frequency_phase_sweep_stays_inside_declared_bound() {
    let frequencies = [
        0.05, 0.125, 0.25, 0.30, 0.35, 0.40, 0.45, 0.475, 0.49, 0.494,
    ];
    let phases = [0.0, 1.0 / 16.0, 3.0 / 16.0, 7.0 / 16.0];

    // The fade length sets the envelope's spectral skirt, and that is what
    // decides whether these signals are actually inside the band this mode
    // qualifies. A 128-frame fade carries a skirt of roughly 1/128 = 0.0078 of
    // the sample rate, so a 0.494 carrier reaches ~0.5018 -- past Nyquist, and
    // outside the 0.495 qualified maximum. Such a signal does not test the
    // meter: its own exact band-limited peak is +0.300 dBTP while the meter
    // reads +0.104, so a correct meter fails the point-estimate bound below.
    // A 1024-frame fade holds the skirt near 0.001, keeping the whole sweep
    // inside the qualified band, where measured error falls to <= 0.006 dB.
    const FRAMES: usize = 4096;
    const FADE: usize = 1024;
    assert!(
        HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE
            >= frequencies[frequencies.len() - 1] + 1.0 / FADE as f64,
        "sweep signals must stay inside the qualified band"
    );

    for frequency in frequencies {
        for phase in phases {
            let signal = cosine_with_flat_center(FRAMES, FADE, frequency, phase);
            let dbtp = finite_dbtp(measure_mono(&signal, TruePeakMode::Headroom64x));
            let under_read_db = -dbtp;
            assert!(
                under_read_db <= HEADROOM64X_MAX_UNDERREAD_DB,
                "frequency={frequency:.6}, phase={phase:.6}, measured={dbtp:.9} dBTP"
            );
            assert!(
                dbtp.abs() <= 0.05,
                "unexpected point-estimate error at frequency={frequency:.6}, phase={phase:.6}: {dbtp:.9} dBTP"
            );
        }
    }
}

#[test]
fn headroom64x_aligned_upper_band_multitones_stay_inside_declared_bound() {
    let cases: &[&[(f64, f64)]] = &[
        &[(0.30, 1.0 / 6.0), (0.35, 1.0 / 6.0), (0.40, 1.0 / 6.0)],
        &[(0.41, 0.20), (0.45, 0.20), (0.49, 0.20)],
        &[
            (0.07, 0.10),
            (0.19, 0.12),
            (0.31, 0.11),
            (0.39, 0.13),
            (0.445, 0.14),
            (0.485, 0.15),
        ],
    ];
    for components in cases {
        let expected_linear = components.iter().map(|(_, amplitude)| amplitude).sum::<f64>();
        let signal = aligned_multitone(2048, 128, 1000.5, components);
        let measured = measure_mono(&signal, TruePeakMode::Headroom64x);
        let expected_dbtp = 20.0 * expected_linear.log10();
        let error_db = finite_dbtp(measured) - expected_dbtp;
        assert!(
            -error_db <= HEADROOM64X_MAX_UNDERREAD_DB,
            "components={components:?}, error={error_db:.9} dB"
        );
        assert!(
            error_db.abs() <= 0.05,
            "components={components:?}, point-estimate error={error_db:.9} dB"
        );
    }
}

#[test]
fn high_resolution_mode_resolves_known_between_grid_peak_more_closely() {
    // A tapered fs/4 sine whose analog maxima sit one eighth of an input
    // sample off-grid. Its interior continuous-time peak is exactly 1.0.
    let frames = 4096usize;
    let ramp = 256usize;
    let omega = PI / 2.0;
    let phase = PI / 2.0 - omega * 0.125;
    let signal = (0..frames)
        .map(|frame| {
            let edge = frame.min(frames - 1 - frame);
            let envelope = if edge < ramp {
                0.5 - 0.5 * (PI * edge as f64 / ramp as f64).cos()
            } else {
                1.0
            };
            envelope * (omega * frame as f64 + phase).sin()
        })
        .collect::<Vec<_>>();
    let sample_peak = signal.iter().copied().map(f64::abs).fold(0.0, f64::max);
    let report = measure_mono(&signal, TruePeakMode::Reporting4x).linear();
    let headroom = measure_mono(&signal, TruePeakMode::Headroom64x).linear();

    assert!(sample_peak < 0.99, "sample peak unexpectedly masks inter-sample test");
    assert!(report < 0.99, "4x grid should deliberately under-read this phase");
    assert!((1.0 - headroom).abs() < 0.004, "64x headroom peak={headroom}");
    assert!(headroom > report + 0.01);
}
