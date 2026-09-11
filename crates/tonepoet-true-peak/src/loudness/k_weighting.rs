use std::f64::consts::PI;

use super::LoudnessError;

#[derive(Debug, Clone, Copy)]
pub(super) struct KWeightingCoefficients {
    pub(super) b: [f64; 5],
    pub(super) a: [f64; 5],
}

impl KWeightingCoefficients {
    pub(super) fn for_rate(sample_rate_hz: u32) -> Result<Self, LoudnessError> {
        if sample_rate_hz == 0 {
            return Err(LoudnessError::InvalidSampleRate);
        }
        let rate = f64::from(sample_rate_hz);

        // ITU-R BS.1770 K-weighting pre-filter.  Keep these operations explicit:
        // the Libebur128126 profile intentionally follows libebur128 1.2.6's
        // coefficient-construction graph rather than a generic biquad helper.
        let shelf_f0 = 1681.974450955533_f64;
        let shelf_g = 3.999843853973347_f64;
        let shelf_q = 0.7071752369554196_f64;
        let shelf_k = (PI * shelf_f0 / rate).tan();
        let vh = 10.0_f64.powf(shelf_g / 20.0);
        let vb = vh.powf(0.4996667741545416_f64);
        let shelf_d = 1.0 + shelf_k / shelf_q + shelf_k * shelf_k;
        let p_b = [
            (vh + vb * shelf_k / shelf_q + shelf_k * shelf_k) / shelf_d,
            2.0 * (shelf_k * shelf_k - vh) / shelf_d,
            (vh - vb * shelf_k / shelf_q + shelf_k * shelf_k) / shelf_d,
        ];
        let p_a = [
            1.0,
            2.0 * (shelf_k * shelf_k - 1.0) / shelf_d,
            (1.0 - shelf_k / shelf_q + shelf_k * shelf_k) / shelf_d,
        ];

        // RLB high-pass.  The numerator is deliberately *not* divided by D;
        // this matches the reference transfer function.
        let rlb_f0 = 38.13547087602444_f64;
        let rlb_q = 0.5003270373238773_f64;
        let rlb_k = (PI * rlb_f0 / rate).tan();
        let rlb_d = 1.0 + rlb_k / rlb_q + rlb_k * rlb_k;
        let r_b = [1.0, -2.0, 1.0];
        let r_a = [
            1.0,
            2.0 * (rlb_k * rlb_k - 1.0) / rlb_d,
            (1.0 - rlb_k / rlb_q + rlb_k * rlb_k) / rlb_d,
        ];

        // Spell out the fourth-order polynomial products in a stable order.
        let b = [
            p_b[0] * r_b[0],
            p_b[0] * r_b[1] + p_b[1] * r_b[0],
            p_b[0] * r_b[2] + p_b[1] * r_b[1] + p_b[2] * r_b[0],
            p_b[1] * r_b[2] + p_b[2] * r_b[1],
            p_b[2] * r_b[2],
        ];
        let a = [
            p_a[0] * r_a[0],
            p_a[0] * r_a[1] + p_a[1] * r_a[0],
            p_a[0] * r_a[2] + p_a[1] * r_a[1] + p_a[2] * r_a[0],
            p_a[1] * r_a[2] + p_a[2] * r_a[1],
            p_a[2] * r_a[2],
        ];

        if !b.iter().chain(a.iter()).all(|value| value.is_finite()) {
            return Err(LoudnessError::UnsupportedSampleRate { sample_rate_hz });
        }
        if a[0] != 1.0 {
            return Err(LoudnessError::NumericalRange);
        }

        Ok(Self { b, a })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct FilterState {
    delay: [f64; 4],
}

impl FilterState {
    #[inline]
    pub(super) fn process(&mut self, input: f64, coefficients: KWeightingCoefficients) -> f64 {
        let z = input
            - coefficients.a[1] * self.delay[0]
            - coefficients.a[2] * self.delay[1]
            - coefficients.a[3] * self.delay[2]
            - coefficients.a[4] * self.delay[3];
        let output = coefficients.b[0] * z
            + coefficients.b[1] * self.delay[0]
            + coefficients.b[2] * self.delay[1]
            + coefficients.b[3] * self.delay[2]
            + coefficients.b[4] * self.delay[3];
        self.delay[3] = self.delay[2];
        self.delay[2] = self.delay[1];
        self.delay[1] = self.delay[0];
        self.delay[0] = z;
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_48k_coefficients_match_independent_fixture() {
        let c = KWeightingCoefficients::for_rate(48_000).expect("48 kHz coefficients");
        let expected_b = [
            1.5351248595869702,
            -5.761945908580321,
            8.116910049252581,
            -5.08848181111208,
            1.19839281085285,
        ];
        let expected_a = [
            1.0,
            -3.68070674801639,
            5.087045247971131,
            -3.1315463514467305,
            0.7252088884778705,
        ];
        for (actual, expected) in c.b.into_iter().zip(expected_b) {
            assert!((actual - expected).abs() <= 2.0e-15, "b: {actual:?} != {expected:?}");
        }
        for (actual, expected) in c.a.into_iter().zip(expected_a) {
            assert!((actual - expected).abs() <= 2.0e-15, "a: {actual:?} != {expected:?}");
        }
    }
    #[test]
    fn recorded_coefficients_match_every_qualified_rate() {
        let fixtures = [
            (8_000, [1.3216235689299776, -3.3695026291756465, 3.0722607975775604, -1.3225079833480926, 0.2981262460162007], [1.0, -2.234394125244139, 1.698215423410948, -0.6390578283780513, 0.17601472922399444]),
            (16_000, [1.4432952234913587, -4.718165828243177, 5.788104743424247, -3.194892896084399, 0.6816587574119699], [1.0, -3.071823297111428, 3.5357633093004717, -1.8471417832251071, 0.383266596714102]),
            (22_050, [1.4798253509777464, -5.130379314812322, 6.682125061417956, -3.892413582309932, 0.8608424847265516], [1.0, -3.316702938656253, 4.134459046823656, -2.3150608859229984, 0.49732462953102896]),
            (24_000, [1.4879002209622763, -5.22200591006569, 6.885220280490988, -4.056023714634013, 0.904909123246438], [1.0, -3.3703787314217526, 4.269946186672436, -2.4257850522811264, 0.5262320656059868]),
            (32_000, [1.5111778995687646, -5.487245212497671, 7.482589999811342, -4.548155960404732, 1.0416332735222955], [1.0, -3.524134765239329, 4.6672546973259585, -2.760768503478049, 0.6176534643851335]),
            (44_100, [1.5308412300503478, -5.712662455255425, 8.001880300281394, -4.989138154997904, 1.1690790799215871], [1.0, -3.6528247868858164, 5.011086762528284, -3.0631592490055355, 0.704898710356287]),
            (44_101, [1.530842422678198, -5.712676167464494, 8.001912228732932, -4.989165645785173, 1.169087161838537], [1.0, -3.6528325574574483, 5.011107856626534, -3.0631781603688335, 0.7049042980677155]),
            (48_000, [1.5351248595869702, -5.761945908580321, 8.116910049252581, -5.08848181111208, 1.19839281085285], [1.0, -3.68070674801639, 5.087045247971131, -3.1315463514467305, 0.7252088884778705]),
            (88_200, [1.5575153755796538, -6.020657831085653, 8.730103512904556, -5.62829503487077, 1.3613339774722124], [1.0, -3.8254975034126764, 5.49063897754419, -3.5047125027774113, 0.8395711259674826]),
            (96_000, [1.5597142289757966, -6.046170036202676, 8.79145858779378, -5.683263982882719, 1.3782612023158187], [1.0, -3.839627014614824, 5.530895337567689, -3.5428526934746705, 0.8515844403325214]),
            (176_400, [1.5711153177418462, -6.178775137888345, 9.113075934540914, -5.974287726384177, 1.4688716119897622], [1.0, -3.9126162392568657, 5.741522705831668, -3.745187198305958, 0.9162807380741807]),
            (192_000, [1.572227215091279, -6.191737481744109, 9.14476465919399, -6.00322573352077, 1.4779713409796094], [1.0, -3.9197094534481884, 5.762239352871299, -3.765342956679401, 0.922813061791401]),
            (352_800, [1.5779729549706938, -6.258808744909173, 9.309467139298002, -6.15439986375126, 1.525768514391737], [1.0, -3.956290169773844, 5.8698070773975095, -3.8707424336450735, 0.9572255264264259]),
            (384_000, [1.5785316857098703, -6.265338949069552, 9.325569649663162, -6.16924919495715, 1.5304868086536694], [1.0, -3.959840704748366, 5.8803139352916824, -3.8811048149455267, 0.9606315846912954]),
        ];
        for (rate, expected_b, expected_a) in fixtures {
            let actual = KWeightingCoefficients::for_rate(rate).expect("qualified rate");
            for (index, (got, expected)) in actual.b.into_iter().zip(expected_b).enumerate() {
                assert!((got - expected).abs() <= 2.0e-14, "rate {rate} b[{index}] {got:?} != {expected:?}");
            }
            for (index, (got, expected)) in actual.a.into_iter().zip(expected_a).enumerate() {
                assert!((got - expected).abs() <= 2.0e-14, "rate {rate} a[{index}] {got:?} != {expected:?}");
            }
        }
    }

    #[test]
    fn rlb_high_pass_numerator_is_not_normalized_by_its_denominator() {
        let rate = 48_000.0;
        let shelf_f0 = 1681.974450955533_f64;
        let shelf_g = 3.999843853973347_f64;
        let shelf_q = 0.7071752369554196_f64;
        let shelf_k = (PI * shelf_f0 / rate).tan();
        let vh = 10.0_f64.powf(shelf_g / 20.0);
        let vb = vh.powf(0.4996667741545416_f64);
        let shelf_d = 1.0 + shelf_k / shelf_q + shelf_k * shelf_k;
        let shelf_b0 = (vh + vb * shelf_k / shelf_q + shelf_k * shelf_k) / shelf_d;
        let rlb_k = (PI * 38.13547087602444_f64 / rate).tan();
        let rlb_d = 1.0 + rlb_k / 0.5003270373238773_f64 + rlb_k * rlb_k;
        let actual = KWeightingCoefficients::for_rate(48_000).unwrap();
        assert_eq!(actual.b[0].to_bits(), shelf_b0.to_bits());
        assert!((actual.b[0] - shelf_b0 / rlb_d).abs() > 1.0e-3);
    }

    #[test]
    fn filter_impulse_matches_recorded_48k_fixture() {
        let expected = [
            1.5351248595869702,
            -0.11160147885084637,
            -0.10311188904658408,
            -0.09297001121706039,
            -0.08203908862043874,
            -0.07098444882543387,
            -0.0602986248580919,
            -0.05032698248265888,
            -0.04129264644636166,
            -0.03331987077535814,
            -0.02645529198694163,
            -0.020686742741190756,
            -0.01595949174082989,
            -0.012189919237357572,
            -0.009276742232401602,
            -0.007109975427653126,
        ];
        let coefficients = KWeightingCoefficients::for_rate(48_000).unwrap();
        let mut state = FilterState::default();
        for (index, expected) in expected.into_iter().enumerate() {
            let input = if index == 0 { 1.0 } else { 0.0 };
            let actual = state.process(input, coefficients);
            assert!((actual - expected).abs() <= 5.0e-13, "impulse[{index}] {actual:?} != {expected:?}");
        }
    }

    #[test]
    fn filter_rejects_dc_after_settling() {
        let coefficients = KWeightingCoefficients::for_rate(48_000).unwrap();
        let mut state = FilterState::default();
        let mut tail_max = 0.0_f64;
        for index in 0..(5 * 48_000) {
            let output = state.process(0.25, coefficients);
            if index >= 4 * 48_000 {
                tail_max = tail_max.max(output.abs());
            }
        }
        assert!(tail_max <= 2.0e-9, "DC tail did not decay: {tail_max:e}");
    }

    #[test]
    fn filter_frequency_response_matches_recorded_48k_points() {
        let fixtures = [
            (20.0, -13.275367788915716),
            (100.0, -1.1334980939400334),
            (1_000.0, 0.6977043960800162),
            (10_000.0, 4.041882222570136),
            (20_000.0, 4.043114183615305),
        ];
        let coefficients = KWeightingCoefficients::for_rate(48_000).unwrap();
        for (frequency, expected_db) in fixtures {
            let mut state = FilterState::default();
            let mut input_sum = 0.0;
            let mut output_sum = 0.0;
            for n in 0..(3 * 48_000) {
                let input = (2.0 * PI * frequency * n as f64 / 48_000.0).sin();
                let output = state.process(input, coefficients);
                if n >= 2 * 48_000 {
                    input_sum += input * input;
                    output_sum += output * output;
                }
            }
            let actual_db = 10.0 * (output_sum / input_sum).log10();
            assert!((actual_db - expected_db).abs() <= 2.0e-6, "{frequency} Hz: {actual_db:?} != {expected_db:?}");
        }
    }

    #[test]
    fn fresh_filter_state_resets_all_history() {
        let coefficients = KWeightingCoefficients::for_rate(48_000).unwrap();
        let mut used = FilterState::default();
        for n in 0..10_000 {
            let _ = used.process((n as f64 * 0.17).sin(), coefficients);
        }
        used = FilterState::default();
        let mut fresh = FilterState::default();
        for input in [1.0, 0.0, 0.0, -0.25, 0.0, 0.5] {
            assert_eq!(used.process(input, coefficients).to_bits(), fresh.process(input, coefficients).to_bits());
        }
    }

    #[test]
    fn filter_output_remains_finite_over_a_long_stream() {
        let rate = 192_000_u32;
        let coefficients = KWeightingCoefficients::for_rate(rate).unwrap();
        let mut state = FilterState::default();
        for n in 0..(12 * rate as usize) {
            let phase = 2.0 * PI * 997.0 * n as f64 / f64::from(rate);
            let input = 1.75 * phase.sin() + 0.125 * (7.0 * phase).cos();
            let output = state.process(input, coefficients);
            assert!(output.is_finite(), "non-finite output at sample {n}");
        }
    }

}
