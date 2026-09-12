use std::cmp::Ordering;

use super::{IntegratedLoudness, LoudnessProfile, LoudnessRange};

pub(super) const ABSOLUTE_GATE_ENERGY: f64 = 1.1724653045822981e-7;

#[inline]
pub(super) fn energy_to_lufs(energy: f64, profile: LoudnessProfile) -> f64 {
    match profile {
        LoudnessProfile::Libebur128126 => {
            -0.691 + 10.0 * (energy.ln() / std::f64::consts::LN_10)
        }
        LoudnessProfile::NativeEbu2023 => -0.691 + 10.0 * energy.log10(),
    }
}

#[inline]
pub(super) fn passes_integrated_gate(
    energy: f64,
    threshold: f64,
    profile: LoudnessProfile,
) -> bool {
    match profile {
        LoudnessProfile::Libebur128126 => energy >= threshold,
        LoudnessProfile::NativeEbu2023 => energy > threshold,
    }
}

pub(super) fn integrated_from_absolute_energies(
    energies: &[f64],
    profile: LoudnessProfile,
    real_frames: u64,
    first_window_frames: u64,
) -> IntegratedLoudness {
    if real_frames == 0 {
        return IntegratedLoudness::NoInput;
    }
    if real_frames < first_window_frames {
        return IntegratedLoudness::InsufficientFrames {
            frames: real_frames,
            required_frames: first_window_frames,
        };
    }
    if energies.is_empty() {
        return IntegratedLoudness::BelowAbsoluteGate;
    }

    let mut absolute_sum = 0.0;
    for &energy in energies {
        absolute_sum += energy;
    }
    let absolute_mean = absolute_sum / energies.len() as f64;
    let relative_threshold = absolute_mean * 0.1;

    let mut gated_sum = 0.0;
    let mut gated_count = 0usize;
    for &energy in energies {
        if passes_integrated_gate(energy, relative_threshold, profile) {
            gated_sum += energy;
            gated_count += 1;
        }
    }
    if gated_count == 0 {
        return IntegratedLoudness::BelowRelativeGate;
    }
    let mean = gated_sum / gated_count as f64;
    let lufs = energy_to_lufs(mean, profile);
    if lufs.is_finite() {
        IntegratedLoudness::Finite {
            lufs,
            absolute_observations: energies.len(),
            relative_observations: gated_count,
        }
    } else {
        IntegratedLoudness::NumericalRange
    }
}

pub(super) fn sort_lra_energies(energies: &mut [f64]) {
    energies.sort_unstable_by(|left, right| {
        left.partial_cmp(right).unwrap_or(Ordering::Equal)
    });
}

pub(super) fn lra_from_sorted_absolute_energies(
    energies: &[f64],
    profile: LoudnessProfile,
) -> LoudnessRange {
    if energies.is_empty() {
        return LoudnessRange::Unavailable { observations: 0 };
    }

    // The reference sorts first and then accumulates; the sorted order is
    // intentionally retained for the compatibility profile.
    let mut sum = 0.0;
    for &energy in energies {
        sum += energy;
    }
    let threshold = (sum / energies.len() as f64) * 0.01;
    let first = energies.partition_point(|energy| *energy < threshold);
    let gated = &energies[first..];
    if gated.is_empty() {
        return LoudnessRange::Unavailable {
            observations: energies.len(),
        };
    }
    let low = (((gated.len() - 1) as f64 * 0.10) + 0.5).floor() as usize;
    let high = (((gated.len() - 1) as f64 * 0.95) + 0.5).floor() as usize;
    let lo_energy = gated[low];
    let hi_energy = gated[high];
    let lu = match profile {
        LoudnessProfile::Libebur128126 => {
            // Match ebur128_loudness_range_multiple: convert each endpoint to
            // loudness independently, including the -0.691 operation, then
            // subtract. Algebraic cancellation is not assumed in the
            // compatibility operation graph.
            energy_to_lufs(hi_energy, profile) - energy_to_lufs(lo_energy, profile)
        }
        LoudnessProfile::NativeEbu2023 => {
            10.0 * hi_energy.log10() - 10.0 * lo_energy.log10()
        }
    };
    if lu.is_finite() {
        LoudnessRange::Finite {
            lu,
            observations: gated.len(),
        }
    } else {
        LoudnessRange::Unavailable {
            observations: gated.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_gate_constant_is_reference_value() {
        let independently_derived = 10.0_f64.powf((-70.0 + 0.691) / 10.0);
        assert_eq!(ABSOLUTE_GATE_ENERGY.to_bits(), independently_derived.to_bits());
    }

    #[test]
    fn integrated_gate_profiles_expose_boundary_difference() {
        let energy = ABSOLUTE_GATE_ENERGY;
        assert!(passes_integrated_gate(
            energy,
            energy,
            LoudnessProfile::Libebur128126
        ));
        assert!(!passes_integrated_gate(
            energy,
            energy,
            LoudnessProfile::NativeEbu2023
        ));
    }

    #[test]
    fn lra_uses_nearest_rank_not_interpolation() {
        let mut energies = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        sort_lra_energies(&mut energies);
        let result = lra_from_sorted_absolute_energies(
            &energies,
            LoudnessProfile::NativeEbu2023,
        );
        let LoudnessRange::Finite { lu, observations } = result else {
            panic!("expected finite LRA");
        };
        assert_eq!(observations, 5);
        let expected = 10.0 * 5.0_f64.log10() - 10.0 * 1.0_f64.log10();
        assert_eq!(lu, expected);
    }
    fn previous_positive(value: f64) -> f64 {
        assert!(value.is_finite() && value > 0.0);
        f64::from_bits(value.to_bits() - 1)
    }

    fn next_positive(value: f64) -> f64 {
        assert!(value.is_finite() && value > 0.0);
        f64::from_bits(value.to_bits() + 1)
    }

    #[test]
    fn absolute_gate_boundary_matrix_is_profile_exact() {
        let threshold = ABSOLUTE_GATE_ENERGY;
        for profile in [LoudnessProfile::Libebur128126, LoudnessProfile::NativeEbu2023] {
            assert!(!passes_integrated_gate(previous_positive(threshold), threshold, profile));
            assert_eq!(
                passes_integrated_gate(threshold, threshold, profile),
                profile == LoudnessProfile::Libebur128126,
            );
            assert!(passes_integrated_gate(next_positive(threshold), threshold, profile));
        }
    }

    #[test]
    fn relative_gate_boundary_matrix_uses_one_nonrecursive_threshold() {
        let fixtures: &[(&[f64], (usize, usize))] = &[
            (&[9.0, 191.0], (1, 1)),
            (&[10.0, 190.0], (2, 1)),
            (&[11.0, 189.0], (2, 2)),
        ];
        for (energies, expected) in fixtures {
            for (profile, expected_count) in [
                (LoudnessProfile::Libebur128126, expected.0),
                (LoudnessProfile::NativeEbu2023, expected.1),
            ] {
                let result = integrated_from_absolute_energies(energies, profile, 1, 0);
                let IntegratedLoudness::Finite { relative_observations, .. } = result else {
                    panic!("expected finite integrated result for {energies:?} {profile:?}");
                };
                assert_eq!(relative_observations, expected_count, "{energies:?} {profile:?}");
            }
        }

        // The first mean is 100, so the one-shot relative gate is 10.  A
        // recursive re-gate after dropping 9 would raise the threshold and
        // incorrectly drop 11 as well.
        for profile in [LoudnessProfile::Libebur128126, LoudnessProfile::NativeEbu2023] {
            let result = integrated_from_absolute_energies(&[9.0, 11.0, 280.0], profile, 1, 0);
            let IntegratedLoudness::Finite { relative_observations, .. } = result else {
                panic!("expected finite integrated result");
            };
            assert_eq!(relative_observations, 2, "relative gate became recursive for {profile:?}");
        }
    }

    #[test]
    fn silent_programme_sections_do_not_change_a_precomputed_gate_boundary() {
        // Absolute-gate filtering happens before this function. These arrays
        // therefore model the same active programme with arbitrarily long
        // silent sections before or after it: no zero-energy observation may
        // enter the relative-gate population.
        let active = [10.0, 90.0];
        for profile in [LoudnessProfile::Libebur128126, LoudnessProfile::NativeEbu2023] {
            let baseline = integrated_from_absolute_energies(&active, profile, 1, 0);
            let repeated = integrated_from_absolute_energies(&active, profile, 10_000, 0);
            assert_eq!(baseline, repeated);
        }
    }

    #[test]
    fn lra_minus_twenty_lu_gate_has_independent_boundary_cases() {
        // The LRA relative threshold is 1% of the absolute-gated mean.
        // Every fixture has mean 100, so the threshold is exactly 1.
        for (mut energies, expected_observations) in [
            (vec![0.5, 199.5], 1),
            (vec![1.0, 199.0], 2),
            (vec![1.5, 198.5], 2),
        ] {
            sort_lra_energies(&mut energies);
            for profile in [LoudnessProfile::Libebur128126, LoudnessProfile::NativeEbu2023] {
                let result = lra_from_sorted_absolute_energies(&energies, profile);
                assert_eq!(result.observations(), expected_observations, "{energies:?} {profile:?}");
            }
        }
    }

    #[test]
    fn lra_rank_boundary_matrix_covers_empty_singleton_ties_and_half_cases() {
        assert_eq!(
            lra_from_sorted_absolute_energies(&[], LoudnessProfile::NativeEbu2023),
            LoudnessRange::Unavailable { observations: 0 },
        );

        let fixtures: &[(&[f64], usize, usize)] = &[
            (&[100.0], 0, 0),
            (&[100.0, 101.0, 102.0, 103.0, 104.0], 0, 4),
            (&[100.0, 101.0, 102.0, 103.0, 104.0, 105.0], 1, 5),
            (&[100.0, 101.0, 102.0, 103.0, 104.0, 105.0, 106.0, 107.0, 108.0, 109.0, 110.0], 1, 10),
            (&[100.0, 100.0, 100.0, 200.0, 200.0, 200.0], 1, 5),
        ];
        for (energies, low, high) in fixtures {
            for profile in [LoudnessProfile::Libebur128126, LoudnessProfile::NativeEbu2023] {
                let result = lra_from_sorted_absolute_energies(energies, profile);
                let LoudnessRange::Finite { lu, observations } = result else {
                    panic!("expected finite LRA for {energies:?}");
                };
                assert_eq!(observations, energies.len());
                let expected = 10.0 * (energies[*high] / energies[*low]).log10();
                assert!((lu - expected).abs() <= 5.0e-14, "{energies:?} {profile:?}: {lu:?} != {expected:?}");
            }
        }

        let mut unsorted = vec![200.0, 100.0, 150.0, 100.0, 125.0];
        sort_lra_energies(&mut unsorted);
        assert_eq!(unsorted, vec![100.0, 100.0, 125.0, 150.0, 200.0]);
    }

}
