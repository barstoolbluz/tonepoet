use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fmt;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::qualified_half_delay_fft::QualifiedHalfDelayFft;
use super::hq1024_coefficients::{
    HQ1024_FIRST_HALF_DELAY_TAPS, HQ1024_HALF_DELAY_COEFFICIENTS, HQ1024_NODE_A_UPPER,
    HQ1024_NODE_B_UPPER, HQ1024_TAIL_COEFFICIENTS, HQ1024_TAIL_FACTOR,
    HQ1024_TAIL_NONZERO_COUNTS, HQ1024_TAIL_OFFSET_COUNT, HQ1024_TAIL_OFFSET_MAX,
    HQ1024_TAIL_OFFSET_MIN,
};
use super::{
    build_polyphase_filters, positive_finite_linear_to_dbtp, EdgePolicy, InternalPeakCertificate,
    PeakInterval, PeakLevel, ReconstructionId, SearchDiagnostics, SearchPolicy, SearchStatus, TruePeakError,
    TruePeakResult, Window, HEADROOM64_HALF_DELAY_COEFFICIENTS, HEADROOM64_HALF_DELAY_TAPS,
    HEADROOM64_INTERPOLATION_CALIBRATION_LINEAR, HEADROOM64_STAGE_2_TAPS,
    HEADROOM64_STAGE_3_TAPS, HEADROOM64_STAGE_4_TAPS, HEADROOM64_STAGE_5_TAPS,
    HEADROOM64_STAGE_6_TAPS,
};

const TILE_INPUT_FRAMES: i128 = 4096;
const ROOT_GROUP_CELLS: usize = 64;
const CHILD_GROUP_CELLS: usize = 8;
const RANGE_MAX_BLOCK: usize = 16;

const LEGACY_TAIL_FACTOR: usize = 32;
const LEGACY_TAIL_OFFSET_MIN: i32 = -15;
const LEGACY_TAIL_OFFSET_MAX: i32 = 16;
const LEGACY_TAIL_OFFSET_COUNT: usize =
    (LEGACY_TAIL_OFFSET_MAX - LEGACY_TAIL_OFFSET_MIN + 1) as usize;

const LEGACY_FFT_SIZE: usize = 2048;
const HQ1024_FFT_SIZE: usize = 8192;
const LEGACY_INPUT_HALO_FRAMES: i128 = 201;
const HQ1024_INPUT_HALO_FRAMES: i128 = 777;

// Certified first-stage authority uses `QualifiedHalfDelayFft`: a crate-owned
// fixed radix-2 overlap-save transform with frozen roots and a source-derived
// binary64 enclosure. The retired unqualified RustFFT executor is gone; this
// owned graph is the only FFT authority in the crate. The bounded direct path
// below is retained for strict
// Reference9 winner rescoring and a narrow fail-closed coarse fallback.

// IEEE-754 binary64 unit roundoff.  The direct authority/rescore path uses ordinary
// finite arithmetic only; all product/sum bounds are derived from operation
// counts and are widened with directed helper operations.
const BINARY64_UNIT_ROUNDOFF: f64 = 1.0 / 9_007_199_254_740_992.0; // 2^-53
// Absolute underflow/FTZ/DAZ allowance for one direct arithmetic operation:
// at most two positive/negative subnormal operands can be treated as zero
// (< 2 * MIN_NORMAL total input perturbation), a subnormal result can be
// flushed (< 1 * MIN_NORMAL), and one additional MIN_NORMAL unit covers the
// fused-product-residual step used by the tight evaluator.  All reconstruction
// coefficients are normal binary64 values with |c| <= 1; the offline qualifier
// checks that prerequisite independently.
const DAZ_INPUT_UNITS_PER_OPERATION: f64 = 2.0;
const FTZ_RESULT_UNITS_PER_OPERATION: f64 = 1.0;
const FUSED_RESIDUAL_UNDERFLOW_UNITS: f64 = 1.0;
const DIRECT_DOT_ABSOLUTE_UNDERFLOW_UNITS_PER_OPERATION: f64 =
    DAZ_INPUT_UNITS_PER_OPERATION
        + FTZ_RESULT_UNITS_PER_OPERATION
        + FUSED_RESIDUAL_UNDERFLOW_UNITS;
const RESCORE_FRONTIER_CAPACITY: usize = 64;
const DENSE_CHILD_CELLS: usize = 8;
const FAST90_CREDITS_PER_TILE_CHANNEL: u64 = 32_768;
const FAST90_MAX_CARRY_TILES: u64 = 8;
const NODE_PROCESSING_CREDITS: u64 = 4;

// Retired clock-bounded Fast policy constants. The current public Fast066 path
// never constructs this policy; these remain only for focused regression tests
// of the old fail-closed scanner while Reference/Standard share that module.
const FAST_METER_NANOS_PER_PROGRAMME_MINUTE: u128 = 900_000_000;
const FAST_STARTUP_BURST_NANOS: u128 = 50_000_000;

// Absolute floating-point floors are deliberately expressed in minimum-normal
// units so the enclosure remains safe even on a shipping backend that runs
// SIMD arithmetic with flush-to-zero/denormals-are-zero enabled. They are far
// below any ordinary audio-scale interval, but they matter for a library that
// accepts arbitrary finite binary64 input rather than imposing a music-only
// amplitude floor.

const F64_SIGN_MASK: u64 = 1_u64 << 63;
const F64_MAGNITUDE_MASK: u64 = !F64_SIGN_MASK;
const F64_INFINITY_BITS: u64 = 0x7ff0_0000_0000_0000;
const F64_MIN_NORMAL_BITS: u64 = 0x0010_0000_0000_0000;

#[inline]
pub(crate) fn magnitude_bits(value: f64) -> u64 {
    value.to_bits() & F64_MAGNITUDE_MASK
}

#[inline]
pub(crate) fn exact_magnitude(value: f64) -> f64 {
    f64::from_bits(magnitude_bits(value))
}

/// Exact nonnegative finite classification using only the IEEE encoding.
/// Positive subnormals therefore remain distinguishable from exact zero under
/// an MXCSR mode with DAZ enabled. Negative zero is accepted as zero.
#[inline]
fn nonnegative_finite_bits(value: f64) -> Option<u64> {
    let raw = value.to_bits();
    let magnitude = raw & F64_MAGNITUDE_MASK;
    if magnitude >= F64_INFINITY_BITS || (raw & F64_SIGN_MASK != 0 && magnitude != 0) {
        None
    } else {
        Some(magnitude)
    }
}

#[inline]
fn widened_nonnegative_bound(bits: u64) -> f64 {
    if bits != 0 && bits < F64_MIN_NORMAL_BITS {
        f64::MIN_POSITIVE
    } else {
        f64::from_bits(bits)
    }
}

#[inline]
fn next_up_nonnegative_bits(bits: u64) -> f64 {
    debug_assert!(bits <= F64_INFINITY_BITS);
    if bits == F64_INFINITY_BITS {
        f64::INFINITY
    } else {
        f64::from_bits(bits + 1)
    }
}

#[inline]
pub(crate) fn max_exact_magnitude(current: f64, value: f64) -> f64 {
    f64::from_bits(magnitude_bits(current).max(magnitude_bits(value)))
}

#[inline]
pub(crate) fn max_nonnegative_finite(left: f64, right: f64) -> f64 {
    let Some(left_bits) = nonnegative_finite_bits(left) else {
        return f64::INFINITY;
    };
    let Some(right_bits) = nonnegative_finite_bits(right) else {
        return f64::INFINITY;
    };
    f64::from_bits(left_bits.max(right_bits))
}

#[inline]
fn outward_nonnegative(value: f64) -> f64 {
    let Some(bits) = nonnegative_finite_bits(value) else {
        return f64::INFINITY;
    };
    if bits == 0 {
        0.0
    } else {
        next_up_nonnegative_bits(bits)
    }
}

#[inline]
fn downward_nonnegative(value: f64) -> f64 {
    let Some(bits) = nonnegative_finite_bits(value) else {
        return if magnitude_bits(value) == F64_INFINITY_BITS && value.to_bits() & F64_SIGN_MASK == 0 {
            f64::INFINITY
        } else {
            0.0
        };
    };
    if bits == 0 {
        0.0
    } else {
        f64::from_bits(bits - 1)
    }
}

/// Downward enclosure of `magnitude - error` for nonnegative finite inputs.
///
/// A DAZ host can otherwise treat a positive subnormal `error` as zero and
/// make the lower endpoint too large. Exact zero-error identity knots retain
/// their exact magnitude; any nonzero subnormal uncertainty is promoted to
/// `MIN_NORMAL` before subtraction.
#[inline]
pub(crate) fn lower_sub(magnitude: f64, error: f64) -> f64 {
    let Some(magnitude_bits) = nonnegative_finite_bits(magnitude) else {
        return 0.0;
    };
    let Some(error_bits) = nonnegative_finite_bits(error) else {
        return 0.0;
    };
    if error_bits == 0 {
        return f64::from_bits(magnitude_bits);
    }
    if magnitude_bits < F64_MIN_NORMAL_BITS {
        return 0.0;
    }
    let widened_error_bits = error_bits.max(F64_MIN_NORMAL_BITS);
    if widened_error_bits >= magnitude_bits {
        return 0.0;
    }
    downward_nonnegative(
        f64::from_bits(magnitude_bits) - f64::from_bits(widened_error_bits),
    )
}

#[inline]
pub(crate) fn upper_add(left: f64, right: f64) -> f64 {
    // These are authority helpers, so classification must not depend on the
    // caller's DAZ/FTZ mode. Positive subnormal bounds are widened before any
    // floating arithmetic; exact zero remains exact zero.
    let Some(left_bits) = nonnegative_finite_bits(left) else {
        return f64::INFINITY;
    };
    let Some(right_bits) = nonnegative_finite_bits(right) else {
        return f64::INFINITY;
    };
    let value = widened_nonnegative_bound(left_bits) + widened_nonnegative_bound(right_bits);
    let Some(value_bits) = nonnegative_finite_bits(value) else {
        return f64::INFINITY;
    };
    if value_bits == 0 {
        0.0
    } else if value_bits < F64_MIN_NORMAL_BITS {
        f64::MIN_POSITIVE
    } else {
        next_up_nonnegative_bits(value_bits)
    }
}

#[inline]
pub(crate) fn upper_mul(left: f64, right: f64) -> f64 {
    let Some(left_bits) = nonnegative_finite_bits(left) else {
        return f64::INFINITY;
    };
    let Some(right_bits) = nonnegative_finite_bits(right) else {
        return f64::INFINITY;
    };
    if left_bits == 0 || right_bits == 0 {
        return 0.0;
    }
    let value = widened_nonnegative_bound(left_bits) * widened_nonnegative_bound(right_bits);
    let Some(value_bits) = nonnegative_finite_bits(value) else {
        return f64::INFINITY;
    };
    if value_bits < F64_MIN_NORMAL_BITS {
        // Cover both gradual underflow and a backend running with FTZ/DAZ.
        return f64::MIN_POSITIVE;
    }
    next_up_nonnegative_bits(value_bits)
}

/// Higham-style gamma bound for `operations` correctly-rounded binary64
/// arithmetic operations under round-to-nearest.  The calculation of gamma is
/// itself widened upward; callers add a separate absolute term for gradual
/// underflow or FTZ/DAZ execution.
fn gamma_upper(operations: usize) -> f64 {
    if operations == 0 {
        return 0.0;
    }
    let numerator = upper_mul(operations as f64, BINARY64_UNIT_ROUNDOFF);
    if numerator >= 1.0 {
        return f64::INFINITY;
    }
    // `numerator` is already an upper bound.  The denominator must therefore
    // be rounded in the opposite direction: using a round-to-nearest
    // `1 - numerator` directly could make the quotient microscopically too
    // small if that subtraction rounded upward.
    let denominator = downward_nonnegative(1.0 - numerator);
    if denominator <= 0.0 || !denominator.is_finite() {
        return f64::INFINITY;
    }
    outward_nonnegative(numerator / denominator)
}

fn direct_underflow_upper(operations: usize) -> f64 {
    upper_mul(
        operations as f64 * DIRECT_DOT_ABSOLUTE_UNDERFLOW_UNITS_PER_OPERATION,
        f64::MIN_POSITIVE,
    )
}

pub(crate) fn dot_rounding_upper(operations: usize, absolute_sum_upper: f64) -> f64 {
    upper_add(
        upper_mul(gamma_upper(operations), absolute_sum_upper),
        direct_underflow_upper(operations),
    )
}

#[derive(Debug, Clone, Copy)]
struct Evaluation {
    value: f64,
    error: f64,
}

impl Evaluation {
    fn lower(self) -> f64 {
        if !self.value.is_finite() || !self.error.is_finite() {
            return 0.0;
        }
        lower_sub(exact_magnitude(self.value), self.error)
    }

    fn upper(self) -> f64 {
        if !self.value.is_finite() || !self.error.is_finite() {
            return f64::INFINITY;
        }
        upper_add(exact_magnitude(self.value), self.error)
    }
}

#[derive(Debug, Clone)]
struct TailMetadata {
    factor: usize,
    offset_min: i32,
    offset_max: i32,
    offset_count: usize,
    coefficients: Vec<f64>,
    nonzero_counts: Vec<u8>,
    node_a_upper: Vec<f64>,
    node_b_upper: Vec<f64>,
    /// Maximum outward L1 norm of any frozen tail phase.  Fast90 uses this as
    /// a cheap whole-group authority bound before paying for curvature
    /// summaries.  It is derived from the same coefficient bank at startup.
    phase_l1_upper: f64,
    /// L1 coefficient-space enclosure between this precomposed tail bank and
    /// the exact-real cascade of the same binary64 later-stage coefficients.
    /// Zero means the bank itself defines the frozen reconstruction identity.
    composition_error_per_coarse_peak_upper: f64,
}

impl TailMetadata {
    #[inline]
    fn coefficient(&self, phase: usize, offset: i32) -> f64 {
        debug_assert!(phase < self.factor);
        debug_assert!((self.offset_min..=self.offset_max).contains(&offset));
        self.coefficients[phase * self.offset_count + (offset - self.offset_min) as usize]
    }

    #[inline]
    fn node_bounds(&self, tree_index: usize) -> (f64, f64) {
        debug_assert!(tree_index > 0 && tree_index < self.factor);
        (
            self.node_a_upper[tree_index],
            self.node_b_upper[tree_index],
        )
    }

    #[inline]
    fn phase_product_count(&self, phase: usize) -> u64 {
        u64::from(self.nonzero_counts[phase])
    }
}

fn phase_l1_upper(
    factor: usize,
    offset_count: usize,
    coefficients: &[f64],
) -> f64 {
    debug_assert_eq!(coefficients.len(), factor * offset_count);
    let mut maximum = 0.0_f64;
    for phase in 0..factor {
        let row = &coefficients[phase * offset_count..(phase + 1) * offset_count];
        let mut l1 = 0.0_f64;
        for coefficient in row.iter().copied() {
            l1 = upper_add(l1, exact_magnitude(coefficient));
        }
        maximum = max_nonnegative_finite(maximum, l1);
    }
    maximum
}

fn convolve_with_error(
    left: &[f64],
    left_error: &[f64],
    right: &[f64],
) -> (Vec<f64>, Vec<f64>) {
    debug_assert_eq!(left.len(), left_error.len());
    let output_len = left.len() + right.len() - 1;
    let mut output = vec![0.0; output_len];
    let mut propagated = vec![0.0; output_len];
    let mut absolute_sum = vec![0.0; output_len];
    let mut term_counts = vec![0usize; output_len];

    for (left_index, left_value) in left.iter().copied().enumerate() {
        if left_value == 0.0 && left_error[left_index] == 0.0 {
            continue;
        }
        for (right_index, right_value) in right.iter().copied().enumerate() {
            if right_value == 0.0 {
                continue;
            }
            let index = left_index + right_index;
            if left_value != 0.0 {
                output[index] += left_value * right_value;
                absolute_sum[index] = upper_add(
                    absolute_sum[index],
                    upper_mul(left_value.abs(), right_value.abs()),
                );
                term_counts[index] = term_counts[index].saturating_add(1);
            }
            if left_error[left_index] != 0.0 {
                propagated[index] = upper_add(
                    propagated[index],
                    upper_mul(left_error[left_index], right_value.abs()),
                );
            }
        }
    }

    let mut error = vec![0.0; output_len];
    for index in 0..output_len {
        let rounding = dot_rounding_upper(
            term_counts[index].saturating_mul(2),
            absolute_sum[index],
        );
        error[index] = upper_add(propagated[index], rounding);
    }

    while output.len() > 1
        && output.last() == Some(&0.0)
        && error.last() == Some(&0.0)
    {
        output.pop();
        error.pop();
    }
    (output, error)
}

fn legacy_two_x_impulse_response(taps: usize) -> Vec<f64> {
    let delay_frames = (taps + 1) / 2;
    let filters = build_polyphase_filters(taps, 2, delay_frames, Window::Blackman, true);
    let mut response = vec![0.0; delay_frames * 2];
    for (phase, filter) in filters.iter().enumerate() {
        for (&index, &coefficient) in filter.indices.iter().zip(&filter.coefficients) {
            response[index * 2 + phase] = coefficient;
        }
    }
    while response.len() > 1 && response.last() == Some(&0.0) {
        response.pop();
    }
    response
}

#[derive(Default)]
struct CompensatedSum {
    sum: f64,
    correction: f64,
}

impl CompensatedSum {
    fn add(&mut self, value: f64) {
        let next = self.sum + value;
        if self.sum.abs() >= value.abs() {
            self.correction += (self.sum - next) + value;
        } else {
            self.correction += (value - next) + self.sum;
        }
        self.sum = next;
    }

    fn total(self) -> f64 {
        self.sum + self.correction
    }
}

fn compensated_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut sum = CompensatedSum::default();
    for value in values {
        sum.add(value);
    }
    sum.total()
}

fn widen_node_constant(value: f64, absolute: f64) -> f64 {
    outward_nonnegative(value + absolute + value.abs() * 2.0e-13)
}

fn build_node_metadata(
    factor: usize,
    offset_min: i32,
    offset_max: i32,
    coefficients: &[f64],
) -> (Vec<f64>, Vec<f64>) {
    let offset_count = (offset_max - offset_min + 1) as usize;
    let row = |phase: usize| -> Vec<f64> {
        if phase == factor {
            let mut right = vec![0.0; offset_count];
            right[(1 - offset_min) as usize] = 1.0;
            right
        } else {
            coefficients[phase * offset_count..(phase + 1) * offset_count].to_vec()
        }
    };

    let mut a_upper = vec![0.0; factor];
    let mut b_upper = vec![0.0; factor];
    for tree_index in 1..factor {
        let depth = (usize::BITS - 1 - tree_index.leading_zeros()) as usize;
        let nodes_at_depth = 1usize << depth;
        let width = factor / nodes_at_depth;
        let ordinal = tree_index - nodes_at_depth;
        let start = ordinal * width;
        let end = start + width;
        let left = row(start);
        let right = row(end);
        let mut max_a = 0.0_f64;
        let mut max_b = 0.0_f64;

        for phase in start + 1..end {
            let weight = (phase - start) as f64 / (end - start) as f64;
            let phase_row = &coefficients[phase * offset_count..(phase + 1) * offset_count];
            let mut residual = vec![0.0; offset_count];
            for index in 0..offset_count {
                residual[index] =
                    phase_row[index] - (1.0 - weight) * left[index] - weight * right[index];
            }
            let m0 = compensated_sum(residual.iter().copied());
            let m1 = compensated_sum(
                residual
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(index, value)| f64::from(offset_min + index as i32) * value),
            );
            let a = m0 - m1;
            let b = m1;
            residual[(0 - offset_min) as usize] -= a;
            residual[(1 - offset_min) as usize] -= b;

            let mut q_abs_sum = CompensatedSum::default();
            for j in offset_min..=offset_max - 2 {
                let q = compensated_sum((offset_min..=j).map(|k| {
                    f64::from(j - k + 1) * residual[(k - offset_min) as usize]
                }));
                q_abs_sum.add(q.abs());
            }
            max_a = max_a.max(q_abs_sum.total());
            max_b = max_b.max(a.abs() + b.abs());
        }
        a_upper[tree_index] = widen_node_constant(max_a, 2.0e-15);
        b_upper[tree_index] = widen_node_constant(max_b, 2.0e-14);
    }
    (a_upper, b_upper)
}

fn build_legacy_tail_metadata() -> TailMetadata {
    let mut response = vec![1.0_f64];
    let mut response_error = vec![0.0_f64];
    let mut delay = 0_i32;
    for taps in [
        HEADROOM64_STAGE_2_TAPS,
        HEADROOM64_STAGE_3_TAPS,
        HEADROOM64_STAGE_4_TAPS,
        HEADROOM64_STAGE_5_TAPS,
        HEADROOM64_STAGE_6_TAPS,
    ] {
        let stage = legacy_two_x_impulse_response(taps);
        let mut upsampled = vec![0.0; response.len() * 2 - 1];
        let mut upsampled_error = vec![0.0; response_error.len() * 2 - 1];
        for (index, value) in response.iter().copied().enumerate() {
            upsampled[index * 2] = value;
            upsampled_error[index * 2] = response_error[index];
        }
        (response, response_error) = convolve_with_error(&upsampled, &upsampled_error, &stage);
        delay = (delay + ((taps - 1) / 4) as i32) * 2;
    }
    assert_eq!(delay, 528, "Legacy64 tail delay invariant changed");

    let mut coefficients = vec![0.0; LEGACY_TAIL_FACTOR * LEGACY_TAIL_OFFSET_COUNT];
    let mut nonzero_counts = vec![0_u8; LEGACY_TAIL_FACTOR];
    let mut phase_composition_error = vec![0.0_f64; LEGACY_TAIL_FACTOR];
    let mut represented = vec![false; response.len()];
    for phase in 0..LEGACY_TAIL_FACTOR {
        for offset in LEGACY_TAIL_OFFSET_MIN..=LEGACY_TAIL_OFFSET_MAX {
            let response_index = phase as i32 + delay - LEGACY_TAIL_FACTOR as i32 * offset;
            if response_index >= 0 && (response_index as usize) < response.len() {
                let coefficient = response[response_index as usize];
                represented[response_index as usize] = true;
                coefficients[phase * LEGACY_TAIL_OFFSET_COUNT
                    + (offset - LEGACY_TAIL_OFFSET_MIN) as usize] = coefficient;
                phase_composition_error[phase] = upper_add(
                    phase_composition_error[phase],
                    response_error[response_index as usize],
                );
                if coefficient != 0.0 {
                    nonzero_counts[phase] = nonzero_counts[phase].saturating_add(1);
                }
            }
        }
    }
    assert!(
        response
            .iter()
            .copied()
            .zip(represented.iter().copied())
            .all(|(value, covered)| value == 0.0 || covered),
        "Legacy64 declared tail support omitted a nonzero coefficient",
    );
    assert_eq!(
        coefficients[(0 - LEGACY_TAIL_OFFSET_MIN) as usize],
        1.0,
        "Legacy64 tail identity phase changed",
    );
    assert_eq!(nonzero_counts[0], 1, "Legacy64 identity phase is not sparse");

    let (node_a_upper, node_b_upper) = build_node_metadata(
        LEGACY_TAIL_FACTOR,
        LEGACY_TAIL_OFFSET_MIN,
        LEGACY_TAIL_OFFSET_MAX,
        &coefficients,
    );
    assert!(
        (node_a_upper[1] - 0.385_054_359_363).abs() < 1.0e-9,
        "Legacy64 root curvature invariant changed",
    );

    let phase_l1_upper = phase_l1_upper(
        LEGACY_TAIL_FACTOR,
        LEGACY_TAIL_OFFSET_COUNT,
        &coefficients,
    );
    TailMetadata {
        factor: LEGACY_TAIL_FACTOR,
        offset_min: LEGACY_TAIL_OFFSET_MIN,
        offset_max: LEGACY_TAIL_OFFSET_MAX,
        offset_count: LEGACY_TAIL_OFFSET_COUNT,
        coefficients,
        nonzero_counts,
        phase_l1_upper,
        node_a_upper,
        node_b_upper,
        composition_error_per_coarse_peak_upper: phase_composition_error
            .into_iter()
            .fold(0.0_f64, f64::max),
    }
}

fn legacy_tail_metadata() -> &'static TailMetadata {
    static METADATA: OnceLock<TailMetadata> = OnceLock::new();
    METADATA.get_or_init(build_legacy_tail_metadata)
}

fn hq_tail_metadata() -> &'static TailMetadata {
    static METADATA: OnceLock<TailMetadata> = OnceLock::new();
    METADATA.get_or_init(|| {
        let coefficients = HQ1024_TAIL_COEFFICIENTS
            .iter()
            .flat_map(|row| row.iter().copied())
            .collect::<Vec<_>>();
        TailMetadata {
            factor: HQ1024_TAIL_FACTOR,
            offset_min: HQ1024_TAIL_OFFSET_MIN,
            offset_max: HQ1024_TAIL_OFFSET_MAX,
            offset_count: HQ1024_TAIL_OFFSET_COUNT,
            phase_l1_upper: phase_l1_upper(
                HQ1024_TAIL_FACTOR,
                HQ1024_TAIL_OFFSET_COUNT,
                &coefficients,
            ),
            coefficients,
            nonzero_counts: HQ1024_TAIL_NONZERO_COUNTS.to_vec(),
            node_a_upper: HQ1024_NODE_A_UPPER.to_vec(),
            node_b_upper: HQ1024_NODE_B_UPPER.to_vec(),
            composition_error_per_coarse_peak_upper: 0.0,
        }
    })
}

#[derive(Debug, Clone, Copy)]
struct ReconstructionSpec {
    id: ReconstructionId,
    first_taps: usize,
    fft_size: usize,
    input_halo_frames: i128,
    calibration_linear: f64,
}

impl ReconstructionSpec {
    fn for_id(id: ReconstructionId) -> Self {
        match id {
            ReconstructionId::LegacyHeadroom64 => Self {
                id,
                first_taps: HEADROOM64_HALF_DELAY_TAPS,
                fft_size: LEGACY_FFT_SIZE,
                input_halo_frames: LEGACY_INPUT_HALO_FRAMES,
                calibration_linear: HEADROOM64_INTERPOLATION_CALIBRATION_LINEAR,
            },
            ReconstructionId::Hq1024V1 => Self {
                id,
                first_taps: HQ1024_FIRST_HALF_DELAY_TAPS,
                fft_size: HQ1024_FFT_SIZE,
                input_halo_frames: HQ1024_INPUT_HALO_FRAMES,
                calibration_linear: 1.0,
            },
        }
    }

    fn block_frames(self) -> usize {
        self.fft_size - self.first_taps + 1
    }

    fn group_delay_inputs(self) -> i128 {
        (self.first_taps / 2) as i128
    }

    fn tail(self) -> &'static TailMetadata {
        match self.id {
            ReconstructionId::LegacyHeadroom64 => legacy_tail_metadata(),
            ReconstructionId::Hq1024V1 => hq_tail_metadata(),
        }
    }

    fn first_half_coefficients(self) -> &'static [f64] {
        match self.id {
            ReconstructionId::LegacyHeadroom64 => &HEADROOM64_HALF_DELAY_COEFFICIENTS,
            ReconstructionId::Hq1024V1 => &HQ1024_HALF_DELAY_COEFFICIENTS,
        }
    }

    #[inline]
    fn full_first_coefficient(self, tap: usize) -> f64 {
        let half = self.first_half_coefficients();
        if tap < half.len() {
            half[tap]
        } else {
            half[self.first_taps - 1 - tap]
        }
    }

    /// Conservative original-frame support for an inclusive range of coarse
    /// 2x indices.  It is intentionally allowed to include one harmless extra
    /// endpoint so group bounds never depend on parity-special casing.
    fn raw_support_for_coarse_range(
        self,
        coarse_start: i128,
        coarse_end_inclusive: i128,
    ) -> (i128, i128) {
        debug_assert!(coarse_end_inclusive >= coarse_start);
        let half = (self.first_taps / 2) as i128;
        let first = coarse_start.div_euclid(2) - half + 1;
        let last = coarse_end_inclusive.div_euclid(2) + half;
        (first, last)
    }

}

#[derive(Clone)]
struct CertifiedTwoXEngine {
    inner: QualifiedHalfDelayFft,
}

impl fmt::Debug for CertifiedTwoXEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertifiedTwoXEngine")
            .field("inner", &self.inner)
            .finish()
    }
}

impl CertifiedTwoXEngine {
    fn new(spec: ReconstructionSpec, policy: SearchPolicy, channels: usize) -> Self {
        let inner = match policy {
            SearchPolicy::Reference9 => QualifiedHalfDelayFft::new(spec.id, channels),
            SearchPolicy::Fast90 | SearchPolicy::RetiredClockFast1s => {
                QualifiedHalfDelayFft::new_fast90(spec.id, channels)
            }
        };
        assert_eq!(inner.block_frames(), spec.block_frames());
        Self { inner }
    }

    fn process_frame(&mut self, frame: &[f64], input_index: i128, scanner: &mut CertifiedScanner) {
        // The qualified prefix is normally mandatory. Fast is different: once
        // its cumulative clock allowance is exhausted, an about-to-complete
        // FFT block may be replaced by the source-derived FIR L1 enclosure.
        // This is checked only at block boundaries, not per sample, so the
        // deadline mechanism itself does not become the hot path.
        let execute_fft = scanner.policy != SearchPolicy::RetiredClockFast1s
            || !self.inner.next_frame_completes_block()
            || !scanner.fast_time_budget_exhausted();
        let skips_fft = !execute_fft && self.inner.next_frame_completes_block();
        let emitted = self.inner.process_frame_with_fft_permission(
            frame,
            input_index,
            execute_fft,
            |base, integer, integer_error, half, half_error| {
                scanner.observe_coarse_authority(base, integer, integer_error);
                scanner.observe_coarse_authority(base + 1, half, half_error);
            },
        );
        if skips_fft {
            scanner.diagnostics.time_bounded_prefix_blocks_skipped = scanner
                .diagnostics
                .time_bounded_prefix_blocks_skipped
                .saturating_add(1);
        }
        if emitted {
            scanner.process_ready_full_tiles();
        }
    }

    fn flush(&mut self, scanner: &mut CertifiedScanner) {
        let has_pending = self.inner.has_pending();
        let execute_fft = scanner.policy != SearchPolicy::RetiredClockFast1s
            || !has_pending
            || !scanner.fast_time_budget_exhausted();
        let skips_fft = has_pending && !execute_fft;
        let emitted = self.inner.flush_with_fft_permission(
            execute_fft,
            |base, integer, integer_error, half, half_error| {
                scanner.observe_coarse_authority(base, integer, integer_error);
                scanner.observe_coarse_authority(base + 1, half, half_error);
            },
        );
        if skips_fft {
            scanner.diagnostics.time_bounded_prefix_blocks_skipped = scanner
                .diagnostics
                .time_bounded_prefix_blocks_skipped
                .saturating_add(1);
        }
        if emitted {
            scanner.process_ready_full_tiles();
        }
    }

    fn fast90_same_graph_avx_active(&self) -> bool {
        self.inner.fast90_same_graph_avx_active()
    }
}

#[derive(Debug, Clone)]
struct CoarseBuffer {
    channels: usize,
    start_index: Option<i128>,
    head_frames: usize,
    values: Vec<f64>,
    errors: Vec<f64>,
}

impl CoarseBuffer {
    fn new(channels: usize, capacity_frames: usize) -> Self {
        Self {
            channels,
            start_index: None,
            head_frames: 0,
            values: Vec::with_capacity(capacity_frames * channels),
            errors: Vec::with_capacity(capacity_frames * channels),
        }
    }

    fn active_frames(&self) -> usize {
        self.values.len() / self.channels - self.head_frames
    }

    fn last_index(&self) -> Option<i128> {
        self.start_index
            .map(|start| start + self.active_frames() as i128 - 1)
    }

    fn push(&mut self, index: i128, frame: &[f64], errors: &[f64]) {
        debug_assert_eq!(frame.len(), self.channels);
        debug_assert_eq!(errors.len(), self.channels);
        match self.start_index {
            None => self.start_index = Some(index),
            Some(start) => debug_assert_eq!(index, start + self.active_frames() as i128),
        }
        self.values.extend_from_slice(frame);
        self.errors.extend_from_slice(errors);
    }

    fn contains(&self, start: i128, end_inclusive: i128) -> bool {
        self.start_index.is_some_and(|first| first <= start)
            && self.last_index().is_some_and(|last| last >= end_inclusive)
    }

    fn sample_offset(&self, index: i128, channel: usize) -> usize {
        let start = self.start_index.expect("coarse buffer is non-empty");
        debug_assert!(index >= start && index <= self.last_index().unwrap_or(start));
        (self.head_frames + usize::try_from(index - start).expect("bounded coarse index"))
            * self.channels
            + channel
    }

    fn value(&self, index: i128, channel: usize) -> f64 {
        self.values[self.sample_offset(index, channel)]
    }

    fn evaluation(&self, index: i128, channel: usize) -> Evaluation {
        let offset = self.sample_offset(index, channel);
        Evaluation {
            value: self.values[offset],
            error: self.errors[offset],
        }
    }

    fn discard_before(&mut self, keep_index: i128) {
        let Some(start) = self.start_index else {
            return;
        };
        if keep_index <= start {
            return;
        }
        let available = self.active_frames();
        let discard = usize::try_from(keep_index - start)
            .expect("bounded coarse discard")
            .min(available);
        self.head_frames += discard;
        self.start_index = Some(start + discard as i128);

        // Compact only occasionally. Keeping a logical head avoids memmoving
        // the overlapping FFT/tile halo on every 4096-frame tile.
        if self.head_frames >= 32_768 || self.head_frames * 2 >= self.values.len() / self.channels {
            let head_samples = self.head_frames * self.channels;
            let active_samples = self.values.len() - head_samples;
            self.values.copy_within(head_samples.., 0);
            self.values.truncate(active_samples);
            self.errors.copy_within(head_samples.., 0);
            self.errors.truncate(active_samples);
            self.head_frames = 0;
        }
    }
}

#[derive(Debug, Clone)]
struct RawBuffer {
    channels: usize,
    start_index: Option<i128>,
    head_frames: usize,
    values: Vec<f64>,
}

impl RawBuffer {
    fn new(channels: usize, capacity_frames: usize) -> Self {
        Self {
            channels,
            start_index: None,
            head_frames: 0,
            values: Vec::with_capacity(capacity_frames * channels),
        }
    }

    fn active_frames(&self) -> usize {
        self.values.len() / self.channels - self.head_frames
    }

    fn last_index(&self) -> Option<i128> {
        self.start_index
            .map(|start| start + self.active_frames() as i128 - 1)
    }

    fn push(&mut self, index: i128, frame: &[f64]) {
        debug_assert_eq!(frame.len(), self.channels);
        match self.start_index {
            None => self.start_index = Some(index),
            Some(start) => debug_assert_eq!(index, start + self.active_frames() as i128),
        }
        self.values.extend_from_slice(frame);
    }

    fn contains(&self, start: i128, end_inclusive: i128) -> bool {
        self.start_index.is_some_and(|first| first <= start)
            && self.last_index().is_some_and(|last| last >= end_inclusive)
    }

    fn sample(&self, index: i128, channel: usize) -> f64 {
        let start = self.start_index.expect("raw buffer is non-empty");
        debug_assert!(index >= start && index <= self.last_index().unwrap_or(start));
        let frame = self.head_frames
            + usize::try_from(index - start).expect("bounded raw sample index");
        self.values[frame * self.channels + channel]
    }

    fn discard_before(&mut self, keep_index: i128) {
        let Some(start) = self.start_index else {
            return;
        };
        if keep_index <= start {
            return;
        }
        let available = self.active_frames();
        let discard = usize::try_from(keep_index - start)
            .expect("bounded raw discard")
            .min(available);
        self.head_frames += discard;
        self.start_index = Some(start + discard as i128);

        if self.head_frames >= 16_384 || self.head_frames * 2 >= self.values.len() / self.channels {
            let head_samples = self.head_frames * self.channels;
            let active_samples = self.values.len() - head_samples;
            self.values.copy_within(head_samples.., 0);
            self.values.truncate(active_samples);
            self.head_frames = 0;
        }
    }
}

#[derive(Debug, Clone)]
struct StrictCoarseCache {
    channels: usize,
    start_index: i128,
    end_index: i128,
    values: Vec<f64>,
    errors: Vec<f64>,
    valid: Vec<bool>,
}

impl StrictCoarseCache {
    fn new(channels: usize) -> Self {
        Self {
            channels,
            start_index: 0,
            end_index: -1,
            values: Vec::new(),
            errors: Vec::new(),
            valid: Vec::new(),
        }
    }

    fn reset(&mut self, start_index: i128, end_index: i128) {
        debug_assert!(end_index >= start_index);
        self.start_index = start_index;
        self.end_index = end_index;
        let frames = usize::try_from(end_index - start_index + 1)
            .expect("bounded strict coarse cache");
        let samples = frames * self.channels;
        self.values.resize(samples, 0.0);
        self.errors.resize(samples, 0.0);
        self.valid.resize(samples, false);
        self.valid.fill(false);
    }

    fn contains(&self, index: i128) -> bool {
        index >= self.start_index && index <= self.end_index
    }

    fn offset(&self, index: i128, channel: usize) -> usize {
        debug_assert!(self.contains(index));
        usize::try_from(index - self.start_index).expect("bounded strict cache index")
            * self.channels
            + channel
    }

    fn get(&self, index: i128, channel: usize) -> Option<Evaluation> {
        if !self.contains(index) {
            return None;
        }
        let offset = self.offset(index, channel);
        self.valid[offset].then_some(Evaluation {
            value: self.values[offset],
            error: self.errors[offset],
        })
    }

    fn set(&mut self, index: i128, channel: usize, evaluation: Evaluation) {
        let offset = self.offset(index, channel);
        self.values[offset] = evaluation.value;
        self.errors[offset] = evaluation.error;
        self.valid[offset] = true;
    }


}

#[derive(Debug)]
struct RangeMaximum {
    values: Vec<f64>,
    block_maxima: Vec<f64>,
}

impl RangeMaximum {
    fn new(values: Vec<f64>) -> Self {
        let block_maxima = values
            .chunks(RANGE_MAX_BLOCK)
            .map(|chunk| chunk.iter().copied().fold(f64::NEG_INFINITY, f64::max))
            .collect();
        Self {
            values,
            block_maxima,
        }
    }

    fn query_max(&self, mut start: usize, end: usize) -> f64 {
        debug_assert!(start < end && end <= self.values.len());
        let mut maximum = f64::NEG_INFINITY;
        while start < end && start % RANGE_MAX_BLOCK != 0 {
            maximum = maximum.max(self.values[start]);
            start += 1;
        }
        while start + RANGE_MAX_BLOCK <= end {
            maximum = maximum.max(self.block_maxima[start / RANGE_MAX_BLOCK]);
            start += RANGE_MAX_BLOCK;
        }
        while start < end {
            maximum = maximum.max(self.values[start]);
            start += 1;
        }
        maximum
    }
}

fn second_difference_upper(first: Evaluation, middle: Evaluation, last: Evaluation) -> f64 {
    let approximate = (first.value - middle.value) - middle.value + last.value;
    let propagated = upper_add(
        upper_add(first.error, upper_mul(2.0, middle.error)),
        last.error,
    );
    let operation_scale = upper_add(
        upper_add(first.value.abs(), upper_mul(2.0, middle.value.abs())),
        last.value.abs(),
    );
    let arithmetic = upper_add(
        upper_mul(gamma_upper(3), operation_scale),
        direct_underflow_upper(3),
    );
    upper_add(upper_add(approximate.abs(), propagated), arithmetic)
}

#[derive(Debug)]
struct CoarseMagnitudeSummary {
    start_index: i128,
    magnitude: RangeMaximum,
}

impl CoarseMagnitudeSummary {
    fn from_upper_values(start_index: i128, values: Vec<f64>) -> Self {
        debug_assert!(!values.is_empty());
        let magnitude = RangeMaximum::new(values);
        Self {
            start_index,
            magnitude,
        }
    }

    fn magnitude_max(&self, start: i128, end_exclusive: i128) -> f64 {
        debug_assert!(end_exclusive > start && start >= self.start_index);
        let first = usize::try_from(start - self.start_index)
            .expect("bounded fast coarse magnitude start");
        let last = usize::try_from(end_exclusive - self.start_index)
            .expect("bounded fast coarse magnitude end");
        self.magnitude.query_max(first, last)
    }

    fn group_support_max(
        &self,
        tail: &TailMetadata,
        group_start: i128,
        group_end: i128,
    ) -> f64 {
        debug_assert!(group_end > group_start);
        let support_start = group_start + i128::from(tail.offset_min);
        let support_end = group_end - 1 + i128::from(tail.offset_max);
        self.magnitude_max(support_start, support_end + 1)
    }
}

#[derive(Debug)]
struct CoarseSecondDifferenceSummary {
    d2_start_index: i128,
    d2: RangeMaximum,
}

impl CoarseSecondDifferenceSummary {
    fn build(
        buffer: &CoarseBuffer,
        channel: usize,
        start_index: i128,
        end_index: i128,
    ) -> Self {
        debug_assert!(end_index >= start_index + 2);
        debug_assert!(buffer.contains(start_index, end_index));
        let span = usize::try_from(end_index - start_index)
            .expect("bounded coarse second-difference span");
        let stride = buffer.channels;
        let base = buffer.sample_offset(start_index, channel);
        let evaluation_at = |ordinal: usize| {
            let offset = base + ordinal * stride;
            Evaluation {
                value: buffer.values[offset],
                error: buffer.errors[offset],
            }
        };

        // Each adjacent d2 shares two of its three coarse knots with the next.
        // Carry those evaluations forward instead of repeating three indexed
        // CoarseBuffer lookups per value. The call to second_difference_upper
        // is unchanged, so the numerical enclosure and operation graph are
        // identical; only address arithmetic and loads are removed.
        let mut d2_values = Vec::with_capacity(span - 1);
        let mut first = evaluation_at(0);
        let mut middle = evaluation_at(1);
        for ordinal in 2..=span {
            let last = evaluation_at(ordinal);
            d2_values.push(second_difference_upper(first, middle, last));
            first = middle;
            middle = last;
        }
        let d2 = RangeMaximum::new(d2_values);
        Self {
            d2_start_index: start_index,
            d2,
        }
    }

    fn value_count(start_index: i128, end_index: i128) -> usize {
        debug_assert!(end_index >= start_index + 2);
        usize::try_from(end_index - start_index - 1)
            .expect("bounded coarse second-difference count")
    }

    fn d2_max(&self, start: i128, end_exclusive: i128) -> f64 {
        debug_assert!(end_exclusive > start && start >= self.d2_start_index);
        let first = usize::try_from(start - self.d2_start_index)
            .expect("bounded coarse d2 start");
        let last = usize::try_from(end_exclusive - self.d2_start_index)
            .expect("bounded coarse d2 end");
        self.d2.query_max(first, last)
    }
}

#[derive(Debug)]
struct CoarseChannelSummary {
    start_index: i128,
    magnitude: RangeMaximum,
    d2_start_index: i128,
    d2: RangeMaximum,
}

impl CoarseChannelSummary {
    fn build(
        buffer: &CoarseBuffer,
        channel: usize,
        start_index: i128,
        end_index: i128,
    ) -> Self {
        debug_assert!(end_index >= start_index + 2);
        debug_assert!(buffer.contains(start_index, end_index));
        let magnitude = RangeMaximum::new(
            (start_index..=end_index)
                .map(|index| buffer.evaluation(index, channel).upper())
                .collect(),
        );
        let d2_start_index = start_index;
        let d2 = RangeMaximum::new(
            (start_index..=end_index - 2)
                .map(|index| {
                    second_difference_upper(
                        buffer.evaluation(index, channel),
                        buffer.evaluation(index + 1, channel),
                        buffer.evaluation(index + 2, channel),
                    )
                })
                .collect(),
        );
        Self {
            start_index,
            magnitude,
            d2_start_index,
            d2,
        }
    }

    fn magnitude_max(&self, start: i128, end_exclusive: i128) -> f64 {
        debug_assert!(end_exclusive > start && start >= self.start_index);
        let first = usize::try_from(start - self.start_index)
            .expect("bounded coarse magnitude start");
        let last = usize::try_from(end_exclusive - self.start_index)
            .expect("bounded coarse magnitude end");
        self.magnitude.query_max(first, last)
    }

    fn d2_max(&self, start: i128, end_exclusive: i128) -> f64 {
        debug_assert!(end_exclusive > start && start >= self.d2_start_index);
        let first = usize::try_from(start - self.d2_start_index)
            .expect("bounded coarse d2 start");
        let last = usize::try_from(end_exclusive - self.d2_start_index)
            .expect("bounded coarse d2 end");
        self.d2.query_max(first, last)
    }

    fn root_components(
        &self,
        tail: &TailMetadata,
        group_start: i128,
        group_end: i128,
    ) -> (f64, f64, f64) {
        debug_assert!(group_end > group_start);
        let support_start = group_start + i128::from(tail.offset_min);
        let support_end = group_end - 1 + i128::from(tail.offset_max);
        let endpoint_upper = self.magnitude_max(group_start, group_end + 1);
        let magnitude_upper = self.magnitude_max(support_start, support_end + 1);
        let d2_upper = self.d2_max(support_start, support_end - 1);
        (endpoint_upper, d2_upper, magnitude_upper)
    }
}

#[derive(Debug, Clone, Copy)]
struct StrictCellComponents {
    left: Evaluation,
    right: Evaluation,
    d2_upper: f64,
    magnitude_upper: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KnotLocation {
    cell: i128,
    phase: usize,
}

#[derive(Debug, Clone, Copy)]
struct TrackedEvaluation {
    location: KnotLocation,
    evaluation: Evaluation,
}

impl TrackedEvaluation {
    /// Total priority used by the bounded rescore frontier. Larger means the
    /// candidate is more valuable to retain. Equal numerical uppers are
    /// resolved by absolute knot location rather than arrival order so a
    /// caller's `push_interleaved()` partition cannot affect which live
    /// competitors survive for deferred rescoring.
    fn retention_cmp(&self, other: &Self) -> Ordering {
        self.evaluation
            .upper()
            .total_cmp(&other.evaluation.upper())
            // For equal authority, prefer the earlier absolute knot. Reversing
            // the location comparison makes that knot the greater priority.
            .then_with(|| other.location.cell.cmp(&self.location.cell))
            .then_with(|| other.location.phase.cmp(&self.location.phase))
    }
}

#[derive(Debug, Clone, Default)]
struct TileEvaluationFrontier {
    retained: Vec<TrackedEvaluation>,
}

impl TileEvaluationFrontier {
    /// Keep the strongest numerically ambiguous competitors bounded in memory.
    /// If the frontier is full, return one still-competitive evaluation for
    /// immediate tight rescoring rather than discarding authority-relevant
    /// state.  Entries already below the channel lower can be forgotten
    /// permanently because that lower is monotone.
    fn observe(
        &mut self,
        location: KnotLocation,
        evaluation: Evaluation,
        channel_lower: f64,
    ) -> Option<TrackedEvaluation> {
        if evaluation.upper() <= channel_lower {
            return None;
        }
        if let Some(existing) = self
            .retained
            .iter_mut()
            .find(|tracked| tracked.location == location)
        {
            if evaluation.error < existing.evaluation.error {
                existing.evaluation = evaluation;
            }
            return None;
        }
        if self.retained.len() < RESCORE_FRONTIER_CAPACITY {
            self.retained.push(TrackedEvaluation { location, evaluation });
            return None;
        }

        let incoming = TrackedEvaluation { location, evaluation };
        let (minimum_index, minimum) = self
            .retained
            .iter()
            .enumerate()
            .min_by(|left, right| left.1.retention_cmp(right.1))
            .expect("non-empty rescore frontier");
        if minimum.evaluation.upper() <= channel_lower {
            self.retained[minimum_index] = incoming;
            return None;
        }
        if incoming.retention_cmp(minimum) == Ordering::Greater {
            let evicted = self.retained[minimum_index];
            self.retained[minimum_index] = incoming;
            Some(evicted)
        } else {
            Some(incoming)
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum CandidateWork {
    DenseRegion { end_cell: i128 },
    Dyadic {
        phase_start: usize,
        phase_end: usize,
        tree_index: usize,
        left: Evaluation,
        right: Evaluation,
        d2_upper: f64,
        magnitude_upper: f64,
    },
}

#[derive(Debug, Clone, Copy)]
struct CandidateNode {
    cell: i128,
    channel: usize,
    upper: f64,
    screening_priority: f64,
    work: CandidateWork,
}

impl CandidateNode {
    fn cmp_evaluation(left: Evaluation, right: Evaluation) -> Ordering {
        left.value
            .total_cmp(&right.value)
            .then_with(|| left.error.total_cmp(&right.error))
    }

    fn cmp_work(&self, other: &Self) -> Ordering {
        match (self.work, other.work) {
            (
                CandidateWork::DenseRegion { end_cell },
                CandidateWork::DenseRegion {
                    end_cell: other_end_cell,
                },
            ) => other_end_cell.cmp(&end_cell),
            // Preserve the previous phase-key preference: a dyadic interval
            // sorts ahead of a dense region when every numerical/source key
            // above it ties. The variant discriminator now makes the order
            // genuinely total instead of mapping all dense regions to the
            // same synthetic phase pair.
            (CandidateWork::DenseRegion { .. }, CandidateWork::Dyadic { .. }) => Ordering::Less,
            (CandidateWork::Dyadic { .. }, CandidateWork::DenseRegion { .. }) => Ordering::Greater,
            (
                CandidateWork::Dyadic {
                    phase_start,
                    phase_end,
                    tree_index,
                    left,
                    right,
                    d2_upper,
                    magnitude_upper,
                },
                CandidateWork::Dyadic {
                    phase_start: other_phase_start,
                    phase_end: other_phase_end,
                    tree_index: other_tree_index,
                    left: other_left,
                    right: other_right,
                    d2_upper: other_d2_upper,
                    magnitude_upper: other_magnitude_upper,
                },
            ) => other_phase_start
                .cmp(&phase_start)
                .then_with(|| other_phase_end.cmp(&phase_end))
                .then_with(|| other_tree_index.cmp(&tree_index))
                .then_with(|| Self::cmp_evaluation(left, other_left))
                .then_with(|| Self::cmp_evaluation(right, other_right))
                .then_with(|| d2_upper.total_cmp(&other_d2_upper))
                .then_with(|| magnitude_upper.total_cmp(&other_magnitude_upper)),
        }
    }
}

impl PartialEq for CandidateNode {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for CandidateNode {}

impl PartialOrd for CandidateNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CandidateNode {
    fn cmp(&self, other: &Self) -> Ordering {
        self.upper
            .total_cmp(&other.upper)
            .then_with(|| self.screening_priority.total_cmp(&other.screening_priority))
            // Equal-upper ties are deterministic: lower source position and
            // channel are processed first. `cmp_work` then provides a complete
            // ordering for every distinct work item, including dense spans and
            // dyadic evaluation state. BinaryHeap must never fall back to
            // insertion order for semantically different candidates.
            .then_with(|| other.cell.cmp(&self.cell))
            .then_with(|| other.channel.cmp(&self.channel))
            .then_with(|| self.cmp_work(other))
    }
}

fn accurate_dot(pairs: impl IntoIterator<Item = (f64, f64)>) -> Evaluation {
    let mut terms = Vec::new();
    let mut absolute_sum = 0.0_f64;
    let mut products = 0usize;
    for (left, right) in pairs {
        let product = left * right;
        if !product.is_finite() {
            return Evaluation {
                value: product,
                error: f64::INFINITY,
            };
        }
        // `mul_add` has fused semantics even when the target lacks hardware
        // FMA.  Away from underflow, product + residual is the exact real
        // product of the two binary64 inputs.  The absolute underflow term
        // below covers the remaining tiny cases conservatively.
        let residual = left.mul_add(right, -product);
        if !residual.is_finite() {
            return Evaluation {
                value: product,
                error: f64::INFINITY,
            };
        }
        terms.push(product);
        if residual != 0.0 {
            terms.push(residual);
        }
        absolute_sum = upper_add(absolute_sum, product.abs());
        absolute_sum = upper_add(absolute_sum, residual.abs());
        products += 1;
    }

    if terms.is_empty() {
        return Evaluation { value: 0.0, error: 0.0 };
    }

    let summand_count = terms.len();
    let mut depth = 0usize;
    while terms.len() > 1 {
        let mut write = 0usize;
        let mut read = 0usize;
        while read + 1 < terms.len() {
            terms[write] = terms[read] + terms[read + 1];
            write += 1;
            read += 2;
        }
        if read < terms.len() {
            terms[write] = terms[read];
            write += 1;
        }
        terms.truncate(write);
        depth += 1;
    }

    // Pairwise summation gives every summand at most `depth` ordinary
    // roundings, so gamma depends on depth. Absolute FTZ/DAZ loss is different:
    // it can occur at every internal addition, hence `summand_count - 1`.
    let relative_rounding = upper_mul(gamma_upper(depth), absolute_sum);
    let addition_underflow = direct_underflow_upper(summand_count.saturating_sub(1));
    let rounding = upper_add(relative_rounding, addition_underflow);
    let product_underflow = direct_underflow_upper(products.saturating_mul(2));
    Evaluation {
        value: terms[0],
        error: upper_add(rounding, product_underflow),
    }
}


#[derive(Debug, Clone)]
struct LegacyDenseStage {
    group_delay_inputs: i128,
    half_coefficients: Vec<f64>,
    pair_means: Vec<f64>,
    pair_delta_abs: Vec<f64>,
}

fn legacy_dense_stages() -> &'static [LegacyDenseStage] {
    static STAGES: OnceLock<Vec<LegacyDenseStage>> = OnceLock::new();
    STAGES.get_or_init(|| {
        [
            HEADROOM64_STAGE_2_TAPS,
            HEADROOM64_STAGE_3_TAPS,
            HEADROOM64_STAGE_4_TAPS,
            HEADROOM64_STAGE_5_TAPS,
            HEADROOM64_STAGE_6_TAPS,
        ]
        .into_iter()
        .map(|taps| {
            let response = legacy_two_x_impulse_response(taps);
            let half_coefficients = response
                .iter()
                .copied()
                .skip(1)
                .step_by(2)
                .collect::<Vec<_>>();
            assert_eq!(half_coefficients.len() % 2, 0, "Legacy dense half phase lost symmetry geometry");
            let pair_count = half_coefficients.len() / 2;
            let mut pair_means = Vec::with_capacity(pair_count);
            let mut pair_delta_abs = Vec::with_capacity(pair_count);
            for index in 0..pair_count {
                let mirror = half_coefficients.len() - 1 - index;
                let left = half_coefficients[index];
                let right = half_coefficients[mirror];
                let mean = 0.5 * left + 0.5 * right;
                pair_means.push(mean);
                // Exact target uses the two original binary64 coefficients.
                // Pair-product execution substitutes their stored mean, so
                // charge the coefficient-space discrepancy explicitly.
                pair_delta_abs.push(outward_nonnegative(
                    (left - mean).abs().max((right - mean).abs()),
                ));
            }
            LegacyDenseStage {
                group_delay_inputs: ((taps - 1) / 4) as i128,
                half_coefficients,
                pair_means,
                pair_delta_abs,
            }
        })
        .collect()
    })
}

#[derive(Debug, Clone)]
struct DenseEvaluationBuffer {
    start_index: i128,
    values: Vec<Evaluation>,
}

impl DenseEvaluationBuffer {
    fn get(&self, index: i128) -> Evaluation {
        let offset = usize::try_from(index - self.start_index).expect("dense block index");
        self.values[offset]
    }
}

fn dense_stage_required_source_range(
    target_start: i128,
    target_end: i128,
    stage: &LegacyDenseStage,
) -> (i128, i128) {
    debug_assert!(target_end >= target_start);
    let half_len = stage.half_coefficients.len() as i128;
    let mut source_start = i128::MAX;
    let mut source_end = i128::MIN;
    for target in target_start..=target_end {
        if target.rem_euclid(2) == 0 {
            let source = target.div_euclid(2);
            source_start = source_start.min(source);
            source_end = source_end.max(source);
        } else {
            let cell = (target - 1).div_euclid(2);
            source_start = source_start.min(
                cell + stage.group_delay_inputs - (half_len - 1),
            );
            source_end = source_end.max(cell + stage.group_delay_inputs);
        }
    }
    (source_start, source_end)
}

fn apply_legacy_dense_stage(
    source: &DenseEvaluationBuffer,
    target_start: i128,
    target_end: i128,
    stage: &LegacyDenseStage,
) -> DenseEvaluationBuffer {
    let mut values = Vec::with_capacity(
        usize::try_from(target_end - target_start + 1).expect("bounded dense stage"),
    );
    let half_len = stage.half_coefficients.len();
    let pair_count = half_len / 2;
    for target in target_start..=target_end {
        if target.rem_euclid(2) == 0 {
            values.push(source.get(target.div_euclid(2)));
            continue;
        }

        let cell = (target - 1).div_euclid(2);
        let source_center = cell + stage.group_delay_inputs;
        let mut sum = 0.0_f64;
        let mut arithmetic_scale = 0.0_f64;
        let mut propagated_error = 0.0_f64;
        let mut pairing_error = 0.0_f64;
        for pair in 0..pair_count {
            let mirror = half_len - 1 - pair;
            let recent = source.get(source_center - pair as i128);
            let old = source.get(source_center - mirror as i128);
            let mean = stage.pair_means[pair];
            let pair_sum = recent.value + old.value;
            sum += mean * pair_sum;

            arithmetic_scale = upper_add(
                arithmetic_scale,
                upper_mul(mean.abs(), upper_add(recent.value.abs(), old.value.abs())),
            );
            propagated_error = upper_add(
                propagated_error,
                upper_add(
                    upper_mul(stage.half_coefficients[pair].abs(), recent.error),
                    upper_mul(stage.half_coefficients[mirror].abs(), old.error),
                ),
            );
            pairing_error = upper_add(
                pairing_error,
                upper_mul(
                    stage.pair_delta_abs[pair],
                    upper_add(recent.upper(), old.upper()),
                ),
            );
        }
        // Each pair executes one addition, one multiplication and one running
        // accumulation.  The gamma term bounds that straight-line arithmetic;
        // the absolute term covers gradual underflow and FTZ/DAZ execution.
        let rounding = dot_rounding_upper(pair_count.saturating_mul(3), arithmetic_scale);
        values.push(Evaluation {
            value: sum,
            error: upper_add(upper_add(propagated_error, pairing_error), rounding),
        });
    }
    DenseEvaluationBuffer {
        start_index: target_start,
        values,
    }
}

fn legacy_dense_ranges(final_start: i128, final_end: i128) -> Vec<(i128, i128)> {
    let stages = legacy_dense_stages();
    let mut ranges = vec![(0_i128, 0_i128); stages.len() + 1];
    ranges[stages.len()] = (final_start, final_end);
    for stage_index in (0..stages.len()).rev() {
        let (target_start, target_end) = ranges[stage_index + 1];
        ranges[stage_index] = dense_stage_required_source_range(
            target_start,
            target_end,
            &stages[stage_index],
        );
    }
    ranges
}

fn legacy_dense_product_estimate(start_cell: i128, end_cell: i128) -> u64 {
    let final_start = start_cell.saturating_mul(LEGACY_TAIL_FACTOR as i128);
    let final_end = end_cell.saturating_mul(LEGACY_TAIL_FACTOR as i128);
    let ranges = legacy_dense_ranges(final_start, final_end);
    let mut products = 0_u64;
    for (stage_index, stage) in legacy_dense_stages().iter().enumerate() {
        let (target_start, target_end) = ranges[stage_index + 1];
        let odd_count = (target_start..=target_end)
            .filter(|index| index.rem_euclid(2) != 0)
            .count();
        products = products.saturating_add(
            u64::try_from(odd_count)
                .unwrap_or(u64::MAX)
                .saturating_mul(stage.pair_means.len() as u64),
        );
    }
    products
}

fn legacy_dense_eight_cell_product_estimate() -> u64 {
    static PRODUCTS: OnceLock<u64> = OnceLock::new();
    *PRODUCTS.get_or_init(|| legacy_dense_product_estimate(0, DENSE_CHILD_CELLS as i128))
}

#[derive(Debug, Clone)]
struct CertifiedScanner {
    spec: ReconstructionSpec,
    policy: SearchPolicy,
    channels: usize,
    coarse: CoarseBuffer,
    raw: RawBuffer,
    strict_cache: StrictCoarseCache,
    nominal_frames_seen: i128,
    next_tile_start_frame: i128,
    point_channel_peaks: Vec<f64>,
    lower_channel_peaks: Vec<f64>,
    overall_lower_peak: f64,
    settled_evaluated_upper_channel_peaks: Vec<f64>,
    resolved_pruned_upper_channel_peaks: Vec<f64>,
    unresolved_upper_channel_peaks: Vec<f64>,
    carried_credits: Vec<u64>,
    fast_credits_per_tile_channel: u64,
    sample_rate_hz: u32,
    processing_time_spent: Duration,
    processing_started: Option<Instant>,
    numerical_invalid: bool,
    tile_frontiers: Vec<TileEvaluationFrontier>,
    diagnostics: SearchDiagnostics,
}

impl CertifiedScanner {
    #[cfg(test)]
    fn new(spec: ReconstructionSpec, policy: SearchPolicy, channels: usize) -> Self {
        Self::new_at_rate(spec, policy, channels, 192_000)
    }

    fn new_at_rate(
        spec: ReconstructionSpec,
        policy: SearchPolicy,
        channels: usize,
        sample_rate_hz: u32,
    ) -> Self {
        Self::new_with_fast_credits_at_rate(
            spec,
            policy,
            channels,
            sample_rate_hz,
            FAST90_CREDITS_PER_TILE_CHANNEL,
        )
    }

    fn new_with_fast_credits_at_rate(
        spec: ReconstructionSpec,
        policy: SearchPolicy,
        channels: usize,
        sample_rate_hz: u32,
        fast_credits_per_tile_channel: u64,
    ) -> Self {
        let coarse_capacity = TILE_INPUT_FRAMES as usize * 2
            + spec.block_frames() * 2
            + 2 * (spec.tail().offset_max - spec.tail().offset_min).unsigned_abs() as usize
            + 64;
        let raw_capacity = TILE_INPUT_FRAMES as usize
            + spec.block_frames()
            + usize::try_from(spec.input_halo_frames * 2).expect("bounded raw halo")
            + 64;
        Self {
            spec,
            policy,
            channels,
            coarse: CoarseBuffer::new(channels, coarse_capacity),
            raw: RawBuffer::new(channels, raw_capacity),
            strict_cache: StrictCoarseCache::new(channels),
            nominal_frames_seen: 0,
            next_tile_start_frame: 0,
            point_channel_peaks: vec![0.0; channels],
            lower_channel_peaks: vec![0.0; channels],
            overall_lower_peak: 0.0,
            settled_evaluated_upper_channel_peaks: vec![0.0; channels],
            resolved_pruned_upper_channel_peaks: vec![0.0; channels],
            unresolved_upper_channel_peaks: vec![0.0; channels],
            carried_credits: vec![0; channels],
            fast_credits_per_tile_channel,
            sample_rate_hz,
            processing_time_spent: Duration::ZERO,
            processing_started: None,
            numerical_invalid: false,
            tile_frontiers: vec![TileEvaluationFrontier::default(); channels],
            diagnostics: SearchDiagnostics::default(),
        }
    }

    fn begin_processing_at(&mut self, started: Instant) {
        if self.policy == SearchPolicy::RetiredClockFast1s && self.processing_started.is_none() {
            self.processing_started = Some(started);
        }
    }

    fn begin_processing(&mut self) {
        self.begin_processing_at(Instant::now());
    }

    fn end_processing(&mut self) {
        if let Some(started) = self.processing_started.take() {
            self.processing_time_spent = self.processing_time_spent.saturating_add(started.elapsed());
        }
    }

    fn processing_elapsed_nanos(&self) -> u128 {
        self.processing_time_spent.as_nanos().saturating_add(
            self.processing_started
                .map(|started| started.elapsed().as_nanos())
                .unwrap_or(0),
        )
    }

    fn fast_allowed_processing_nanos(&self) -> u128 {
        let frames = self.nominal_frames_seen.max(0) as u128;
        let denominator = u128::from(self.sample_rate_hz).saturating_mul(60);
        if denominator == 0 {
            return 0;
        }
        FAST_STARTUP_BURST_NANOS.saturating_add(
            frames
                .saturating_mul(FAST_METER_NANOS_PER_PROGRAMME_MINUTE)
                .checked_div(denominator)
                .unwrap_or(0),
        )
    }

    fn fast_time_budget_exhausted(&self) -> bool {
        self.policy == SearchPolicy::RetiredClockFast1s
            && self.processing_elapsed_nanos() >= self.fast_allowed_processing_nanos()
    }

    fn fast_tile_raw_support(&self, coarse_start: i128, coarse_end: i128) -> (i128, i128) {
        debug_assert!(coarse_end > coarse_start);
        let tail = self.spec.tail();
        let required_coarse_start = coarse_start + i128::from(tail.offset_min);
        let required_coarse_end = coarse_end - 1 + i128::from(tail.offset_max);
        self.spec
            .raw_support_for_coarse_range(required_coarse_start, required_coarse_end)
    }

    /// Retain a conservative upper for one channel of a skipped target tile.
    /// `HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER` is an induced L-infinity
    /// bound for the complete finite reconstruction; every target knot in this
    /// tile depends only on the raw support below. Taking the exact raw
    /// magnitude maximum over that support therefore encloses every skipped
    /// knot even when DAZ/FTZ is active.
    fn retain_fast_tile_fallback_channel(
        &mut self,
        coarse_start: i128,
        coarse_end: i128,
        channel: usize,
    ) {
        debug_assert_eq!(self.policy, SearchPolicy::RetiredClockFast1s);
        debug_assert!(channel < self.channels);
        let (raw_start, raw_end) = self.fast_tile_raw_support(coarse_start, coarse_end);
        debug_assert!(self.raw.contains(raw_start, raw_end));

        let mut raw_peak = 0.0_f64;
        for input_index in raw_start..=raw_end {
            raw_peak = max_exact_magnitude(raw_peak, self.raw.sample(input_index, channel));
        }
        let reconstruction_gain = self.spec.id.reconstruction_linf_gain_upper();
        let tile_upper = upper_mul(raw_peak, reconstruction_gain);
        self.unresolved_upper_channel_peaks[channel] = max_nonnegative_finite(
            self.unresolved_upper_channel_peaks[channel],
            tile_upper,
        );
    }

    fn retain_fast_tile_fallback(&mut self, coarse_start: i128, coarse_end: i128) {
        for channel in 0..self.channels {
            self.retain_fast_tile_fallback_channel(coarse_start, coarse_end, channel);
        }
    }

    fn mark_fast_tile_time_limited(&mut self, coarse_start: i128, coarse_end: i128) {
        self.retain_fast_tile_fallback(coarse_start, coarse_end);
        self.diagnostics.time_limited_tiles =
            self.diagnostics.time_limited_tiles.saturating_add(1);
    }

    /// Candidate construction has already paid for exact root uppers for the
    /// completed channels. Preserve those tighter bounds and use the raw-domain
    /// fallback only for channels whose candidate roots were never constructed.
    fn mark_fast_partial_candidate_tile_time_limited(
        &mut self,
        coarse_start: i128,
        coarse_end: i128,
        root_upper_by_channel: &[f64],
        completed_channels: usize,
    ) {
        debug_assert!(completed_channels <= self.channels);
        debug_assert_eq!(root_upper_by_channel.len(), self.channels);
        for channel in 0..completed_channels {
            self.unresolved_upper_channel_peaks[channel] = max_nonnegative_finite(
                self.unresolved_upper_channel_peaks[channel],
                root_upper_by_channel[channel],
            );
        }
        for channel in completed_channels..self.channels {
            self.retain_fast_tile_fallback_channel(coarse_start, coarse_end, channel);
        }
        self.diagnostics.time_limited_tiles =
            self.diagnostics.time_limited_tiles.saturating_add(1);
    }

    fn observe_raw_frame(&mut self, input_index: i128, frame: &[f64]) {
        if frame.iter().any(|sample| !sample.is_finite()) {
            self.numerical_invalid = true;
        }
        self.raw.push(input_index, frame);
    }

    fn observe_nominal_frame(&mut self, input_index: i128, frame: &[f64]) {
        debug_assert_eq!(input_index, self.nominal_frames_seen);
        self.nominal_frames_seen += 1;
        for (channel, sample) in frame.iter().copied().enumerate() {
            self.observe_target_evaluation(
                channel,
                KnotLocation {
                    cell: input_index * 2,
                    phase: 0,
                },
                Evaluation {
                    value: sample,
                    error: 0.0,
                },
            );
        }
    }

    /// Observe the qualified 2x authority stream. Integer phases carry zero
    /// error; half phases carry the fixed-prefix block enclosure derived by
    /// `qualified_half_delay_fft`.
    fn observe_coarse_authority(
        &mut self,
        output_index: i128,
        frame: &[f64],
        errors: &[f64],
    ) {
        if frame.iter().any(|value| !value.is_finite())
            || errors.iter().any(|error| !error.is_finite() || *error < 0.0)
        {
            self.numerical_invalid = true;
        }
        self.coarse.push(output_index, frame, errors);
    }

    fn ranges_available(&self, coarse_start: i128, coarse_end: i128) -> bool {
        let tail = self.spec.tail();
        let required_coarse_start = coarse_start + i128::from(tail.offset_min);
        let required_coarse_end = if coarse_end <= coarse_start {
            coarse_start
        } else {
            coarse_end - 1 + i128::from(tail.offset_max)
        };
        if !self
            .coarse
            .contains(required_coarse_start, required_coarse_end)
        {
            return false;
        }
        let (raw_start, raw_end) = self
            .spec
            .raw_support_for_coarse_range(required_coarse_start, required_coarse_end);
        self.raw.contains(raw_start, raw_end)
    }

    fn process_ready_full_tiles(&mut self) {
        loop {
            let start_frame = self.next_tile_start_frame;
            let end_frame = start_frame + TILE_INPUT_FRAMES;
            if end_frame >= self.nominal_frames_seen {
                break;
            }
            let coarse_start = start_frame * 2;
            let coarse_end = end_frame * 2;
            if !self.ranges_available(coarse_start, coarse_end) {
                break;
            }
            self.process_tile(start_frame, end_frame, coarse_start, coarse_end);
            self.next_tile_start_frame = end_frame;
            let keep_coarse = coarse_end + i128::from(self.spec.tail().offset_min) - 2;
            self.coarse.discard_before(keep_coarse);
            let keep_raw = end_frame - self.spec.input_halo_frames - 4;
            self.raw.discard_before(keep_raw);
        }
    }

    fn observe_evaluation_base(&mut self, channel: usize, evaluation: Evaluation) {
        if !evaluation.value.is_finite() || !evaluation.error.is_finite() {
            self.numerical_invalid = true;
            self.settled_evaluated_upper_channel_peaks[channel] = f64::INFINITY;
            return;
        }
        self.point_channel_peaks[channel] =
            max_exact_magnitude(self.point_channel_peaks[channel], evaluation.value);
        self.lower_channel_peaks[channel] =
            max_nonnegative_finite(self.lower_channel_peaks[channel], evaluation.lower());
        self.overall_lower_peak =
            max_nonnegative_finite(self.overall_lower_peak, self.lower_channel_peaks[channel]);
        self.diagnostics.max_evaluation_error_linear = self
            .diagnostics
            .max_evaluation_error_linear
            .max(evaluation.error);
    }

    fn observe_target_evaluation(
        &mut self,
        channel: usize,
        location: KnotLocation,
        evaluation: Evaluation,
    ) {
        self.observe_evaluation_base(channel, evaluation);
        if magnitude_bits(evaluation.error) == 0 {
            self.settled_evaluated_upper_channel_peaks[channel] = self
                .settled_evaluated_upper_channel_peaks[channel]
                .max(evaluation.upper());
            return;
        }

        if self.policy.uses_accelerated_prefix() {
            // Standard/Fast do not spend unbounded work on strict rescoring.  The
            // ordinary direct enclosure is already authoritative.
            self.settled_evaluated_upper_channel_peaks[channel] = self
                .settled_evaluated_upper_channel_peaks[channel]
                .max(evaluation.upper());
            return;
        }

        let channel_lower = self.lower_channel_peaks[channel];
        let rescore = self.tile_frontiers[channel].observe(location, evaluation, channel_lower);
        if let Some(tracked) = rescore {
            let accurate = self.accurate_target_evaluation(tracked.location, channel);
            self.diagnostics.direct_rescore_evaluations = self
                .diagnostics
                .direct_rescore_evaluations
                .saturating_add(1);
            self.observe_evaluation_base(channel, accurate);
            self.settled_evaluated_upper_channel_peaks[channel] = self
                .settled_evaluated_upper_channel_peaks[channel]
                .max(accurate.upper());
        }
    }

    fn bound_from_components(
        &self,
        endpoint_upper: f64,
        d2_upper: f64,
        magnitude_upper: f64,
        tree_index: usize,
    ) -> f64 {
        let tail = self.spec.tail();
        let (a, b) = tail.node_bounds(tree_index);
        let curvature = upper_mul(a, d2_upper);
        let affine = upper_mul(b, magnitude_upper);
        let model = upper_mul(
            self.spec.tail().composition_error_per_coarse_peak_upper,
            magnitude_upper,
        );
        upper_add(upper_add(upper_add(endpoint_upper, curvature), affine), model)
    }

    fn coarse_group_upper(
        &self,
        summary: &CoarseChannelSummary,
        group_start: i128,
        group_end: i128,
    ) -> f64 {
        let (endpoint_upper, d2_upper, magnitude_upper) =
            summary.root_components(self.spec.tail(), group_start, group_end);
        self.bound_from_components(endpoint_upper, d2_upper, magnitude_upper, 1)
    }

    fn fast90_l1_group_upper(
        &self,
        summary: &CoarseMagnitudeSummary,
        group_start: i128,
        group_end: i128,
    ) -> f64 {
        let tail = self.spec.tail();
        let magnitude_upper = summary.group_support_max(tail, group_start, group_end);
        let coefficient_upper = upper_add(
            tail.phase_l1_upper,
            tail.composition_error_per_coarse_peak_upper,
        );
        upper_mul(coefficient_upper, magnitude_upper)
    }

    fn fast90_group_components(
        &self,
        magnitude_summary: &CoarseMagnitudeSummary,
        d2_summary: &CoarseSecondDifferenceSummary,
        group_start: i128,
        group_end: i128,
    ) -> (f64, f64, f64) {
        let tail = self.spec.tail();
        let support_start = group_start + i128::from(tail.offset_min);
        let support_end = group_end - 1 + i128::from(tail.offset_max);
        let endpoint_upper = magnitude_summary.magnitude_max(group_start, group_end + 1);
        let magnitude_upper =
            magnitude_summary.magnitude_max(support_start, support_end + 1);
        let d2_upper = d2_summary.d2_max(support_start, support_end - 1);
        (endpoint_upper, d2_upper, magnitude_upper)
    }

    fn fast90_coarse_group_upper(
        &self,
        magnitude_summary: &CoarseMagnitudeSummary,
        d2_summary: &CoarseSecondDifferenceSummary,
        group_start: i128,
        group_end: i128,
    ) -> f64 {
        let (endpoint_upper, d2_upper, magnitude_upper) = self.fast90_group_components(
            magnitude_summary,
            d2_summary,
            group_start,
            group_end,
        );
        self.bound_from_components(endpoint_upper, d2_upper, magnitude_upper, 1)
    }

    fn build_fast90_magnitude_summary(
        &mut self,
        channel: usize,
        coarse_start: i128,
        coarse_end: i128,
    ) -> CoarseMagnitudeSummary {
        debug_assert!(self.policy.uses_accelerated_prefix());
        let tail = self.spec.tail();
        let support_start = coarse_start + i128::from(tail.offset_min);
        let support_end = coarse_end - 1 + i128::from(tail.offset_max);
        debug_assert!(self.coarse.contains(support_start, support_end));

        let mut upper_values = Vec::with_capacity(
            usize::try_from(support_end - support_start + 1)
                .expect("bounded Fast90 coarse support"),
        );
        let mut point_peak = self.point_channel_peaks[channel];
        let mut lower_peak = self.lower_channel_peaks[channel];
        let mut maximum_error = self.diagnostics.max_evaluation_error_linear;
        let mut authoritative_values = 0_u64;

        for index in support_start..=support_end {
            let evaluation = self.coarse.evaluation(index, channel);
            upper_values.push(evaluation.upper());
            if index >= coarse_start && index <= coarse_end {
                point_peak = max_exact_magnitude(point_peak, evaluation.value);
                lower_peak = max_nonnegative_finite(lower_peak, evaluation.lower());
                maximum_error = maximum_error.max(evaluation.error);
                authoritative_values = authoritative_values.saturating_add(1);
            }
        }

        // These are maxima, so aggregating once after the scan is exactly the
        // same authority update as calling observe_evaluation_base for every
        // coarse knot, without repeating vector indexing and cross-channel
        // maximum updates in the hottest mandatory loop.
        self.point_channel_peaks[channel] = point_peak;
        self.lower_channel_peaks[channel] = lower_peak;
        self.overall_lower_peak =
            max_nonnegative_finite(self.overall_lower_peak, lower_peak);
        self.diagnostics.max_evaluation_error_linear = maximum_error;
        self.diagnostics.authoritative_coarse_values = self
            .diagnostics
            .authoritative_coarse_values
            .saturating_add(authoritative_values);

        CoarseMagnitudeSummary::from_upper_values(support_start, upper_values)
    }

    fn screening_priority(&self, cell: i128, channel: usize) -> f64 {
        let left = self.coarse.value(cell, channel).abs();
        let right = self.coarse.value(cell + 1, channel).abs();
        let priority = left.max(right);
        if priority.is_finite() { priority } else { 0.0 }
    }

    fn generate_fast90_channel_candidates(
        &mut self,
        channel: usize,
        coarse_start: i128,
        coarse_end: i128,
        magnitude_summary: &CoarseMagnitudeSummary,
    ) -> BinaryHeap<CandidateNode> {
        debug_assert!(self.policy.uses_accelerated_prefix());
        let tail = self.spec.tail();
        let mut heap = BinaryHeap::new();
        let search_lower = self.overall_lower_peak;
        let interval_count = usize::try_from(coarse_end - coarse_start).expect("bounded tile cells");

        // First reject entire 64-cell roots with the cheap L1 authority bound.
        // No second differences are needed for those roots.  Keep the
        // surviving roots in source order so the subsequent cost decision is
        // deterministic and independent of caller chunking.
        let mut surviving_roots = Vec::new();
        let mut sparse_d2_values = 0usize;
        let mut root_offset = 0usize;
        while root_offset < interval_count {
            let root_len = ROOT_GROUP_CELLS.min(interval_count - root_offset);
            let root_start = coarse_start + root_offset as i128;
            let root_end = root_start + root_len as i128;
            let l1_upper =
                self.fast90_l1_group_upper(magnitude_summary, root_start, root_end);
            self.diagnostics.authoritative_coarse_groups = self
                .diagnostics
                .authoritative_coarse_groups
                .saturating_add(1);
            self.diagnostics.accelerated_l1_groups_tested = self
                .diagnostics
                .accelerated_l1_groups_tested
                .saturating_add(1);
            if l1_upper <= search_lower {
                self.resolved_pruned_upper_channel_peaks[channel] = self
                    .resolved_pruned_upper_channel_peaks[channel]
                    .max(l1_upper);
                self.diagnostics.groups_rejected =
                    self.diagnostics.groups_rejected.saturating_add(1);
                self.diagnostics.accelerated_l1_groups_rejected = self
                    .diagnostics
                    .accelerated_l1_groups_rejected
                    .saturating_add(1);
            } else {
                let support_start = root_start + i128::from(tail.offset_min);
                let support_end = root_end - 1 + i128::from(tail.offset_max);
                sparse_d2_values = sparse_d2_values.saturating_add(
                    CoarseSecondDifferenceSummary::value_count(support_start, support_end),
                );
                surviving_roots.push((root_start, root_end, root_len));
            }
            root_offset += root_len;
        }

        if surviving_roots.is_empty() {
            return heap;
        }

        // Sparse per-root summaries avoid all curvature arithmetic for roots
        // rejected above, but their tail halos overlap.  Once those overlaps
        // would cost at least as many second differences as one full summary,
        // build the full summary exactly once instead.  This arithmetic-count
        // switch prevents the Fast90 shortcut from becoming a regression on
        // dense material without content classification or runtime tuning.
        let full_support_start = coarse_start + i128::from(tail.offset_min);
        let full_support_end = coarse_end - 1 + i128::from(tail.offset_max);
        let full_d2_values = CoarseSecondDifferenceSummary::value_count(
            full_support_start,
            full_support_end,
        );
        let full_d2_summary = (sparse_d2_values >= full_d2_values).then(|| {
            CoarseSecondDifferenceSummary::build(
                &self.coarse,
                channel,
                full_support_start,
                full_support_end,
            )
        });

        for (root_start, root_end, root_len) in surviving_roots {
            let root_support_start = root_start + i128::from(tail.offset_min);
            let root_support_end = root_end - 1 + i128::from(tail.offset_max);
            let local_d2_summary;
            let d2_summary = if let Some(summary) = full_d2_summary.as_ref() {
                summary
            } else {
                local_d2_summary = CoarseSecondDifferenceSummary::build(
                    &self.coarse,
                    channel,
                    root_support_start,
                    root_support_end,
                );
                &local_d2_summary
            };
            self.diagnostics.accelerated_curvature_roots = self
                .diagnostics
                .accelerated_curvature_roots
                .saturating_add(1);

            let root_upper = self.fast90_coarse_group_upper(
                magnitude_summary,
                d2_summary,
                root_start,
                root_end,
            );
            self.diagnostics.authoritative_coarse_groups = self
                .diagnostics
                .authoritative_coarse_groups
                .saturating_add(1);
            if root_upper <= search_lower {
                self.resolved_pruned_upper_channel_peaks[channel] = self
                    .resolved_pruned_upper_channel_peaks[channel]
                    .max(root_upper);
                self.diagnostics.groups_rejected =
                    self.diagnostics.groups_rejected.saturating_add(1);
                continue;
            }
            self.diagnostics.groups_expanded =
                self.diagnostics.groups_expanded.saturating_add(1);

            let mut child_offset = 0usize;
            while child_offset < root_len {
                let child_len = CHILD_GROUP_CELLS.min(root_len - child_offset);
                let child_start = root_start + child_offset as i128;
                let child_end = child_start + child_len as i128;
                let child_upper = self.fast90_coarse_group_upper(
                    magnitude_summary,
                    d2_summary,
                    child_start,
                    child_end,
                );
                self.diagnostics.authoritative_coarse_groups = self
                    .diagnostics
                    .authoritative_coarse_groups
                    .saturating_add(1);
                if child_upper <= search_lower {
                    self.resolved_pruned_upper_channel_peaks[channel] = self
                        .resolved_pruned_upper_channel_peaks[channel]
                        .max(child_upper);
                    self.diagnostics.groups_rejected =
                        self.diagnostics.groups_rejected.saturating_add(1);
                    child_offset += child_len;
                    continue;
                }
                self.diagnostics.groups_expanded =
                    self.diagnostics.groups_expanded.saturating_add(1);

                for local in 0..child_len {
                    let cell = child_start + local as i128;
                    let (endpoint_upper, d2_upper, magnitude_upper) =
                        self.fast90_group_components(
                            magnitude_summary,
                            d2_summary,
                            cell,
                            cell + 1,
                        );
                    let upper = self.bound_from_components(
                        endpoint_upper,
                        d2_upper,
                        magnitude_upper,
                        1,
                    );
                    self.diagnostics.authoritative_coarse_groups = self
                        .diagnostics
                        .authoritative_coarse_groups
                        .saturating_add(1);
                    self.diagnostics.candidate_cells =
                        self.diagnostics.candidate_cells.saturating_add(1);
                    if upper <= search_lower {
                        self.resolved_pruned_upper_channel_peaks[channel] = self
                            .resolved_pruned_upper_channel_peaks[channel]
                            .max(upper);
                        continue;
                    }

                    let left = self.coarse.evaluation(cell, channel);
                    let right = self.coarse.evaluation(cell + 1, channel);
                    self.observe_target_evaluation(
                        channel,
                        KnotLocation { cell, phase: 0 },
                        left,
                    );
                    self.observe_target_evaluation(
                        channel,
                        KnotLocation {
                            cell: cell + 1,
                            phase: 0,
                        },
                        right,
                    );
                    heap.push(CandidateNode {
                        cell,
                        channel,
                        upper,
                        screening_priority: self.screening_priority(cell, channel),
                        work: CandidateWork::Dyadic {
                            phase_start: 0,
                            phase_end: tail.factor,
                            tree_index: 1,
                            left,
                            right,
                            d2_upper,
                            magnitude_upper,
                        },
                    });
                }
                child_offset += child_len;
            }
        }
        heap
    }

    fn generate_channel_candidates(
        &mut self,
        channel: usize,
        coarse_start: i128,
        coarse_end: i128,
    ) -> BinaryHeap<CandidateNode> {
        let tail = self.spec.tail();
        let coarse_support_start = coarse_start + i128::from(tail.offset_min);
        let coarse_support_end = coarse_end - 1 + i128::from(tail.offset_max);
        let summary = CoarseChannelSummary::build(
            &self.coarse,
            channel,
            coarse_support_start,
            coarse_support_end,
        );
        let mut heap = BinaryHeap::new();
        let search_lower = self.overall_lower_peak;
        let interval_count = usize::try_from(coarse_end - coarse_start).expect("bounded tile cells");

        let mut root_offset = 0usize;
        while root_offset < interval_count {
            let root_len = ROOT_GROUP_CELLS.min(interval_count - root_offset);
            let root_start = coarse_start + root_offset as i128;
            let root_end = root_start + root_len as i128;
            let root_upper = self.coarse_group_upper(&summary, root_start, root_end);
            self.diagnostics.authoritative_coarse_groups =
                self.diagnostics.authoritative_coarse_groups.saturating_add(1);
            if root_upper <= search_lower {
                self.resolved_pruned_upper_channel_peaks[channel] = self
                    .resolved_pruned_upper_channel_peaks[channel]
                    .max(root_upper);
                self.diagnostics.groups_rejected =
                    self.diagnostics.groups_rejected.saturating_add(1);
                root_offset += root_len;
                continue;
            }
            self.diagnostics.groups_expanded = self.diagnostics.groups_expanded.saturating_add(1);

            let mut child_offset = 0usize;
            while child_offset < root_len {
                let child_len = CHILD_GROUP_CELLS.min(root_len - child_offset);
                let child_start = root_start + child_offset as i128;
                let child_end = child_start + child_len as i128;
                let child_upper = self.coarse_group_upper(&summary, child_start, child_end);
                self.diagnostics.authoritative_coarse_groups =
                    self.diagnostics.authoritative_coarse_groups.saturating_add(1);
                if child_upper <= search_lower {
                    self.resolved_pruned_upper_channel_peaks[channel] = self
                        .resolved_pruned_upper_channel_peaks[channel]
                        .max(child_upper);
                    self.diagnostics.groups_rejected =
                        self.diagnostics.groups_rejected.saturating_add(1);
                    child_offset += child_len;
                    continue;
                }
                self.diagnostics.groups_expanded =
                    self.diagnostics.groups_expanded.saturating_add(1);

                let mut cells = Vec::with_capacity(child_len);
                for local in 0..child_len {
                    let cell = child_start + local as i128;
                    let (endpoint_upper, d2_upper, magnitude_upper) =
                        summary.root_components(tail, cell, cell + 1);
                    let upper = self.bound_from_components(
                        endpoint_upper,
                        d2_upper,
                        magnitude_upper,
                        1,
                    );
                    self.diagnostics.authoritative_coarse_groups =
                        self.diagnostics.authoritative_coarse_groups.saturating_add(1);
                    self.diagnostics.candidate_cells =
                        self.diagnostics.candidate_cells.saturating_add(1);
                    if upper <= search_lower {
                        self.resolved_pruned_upper_channel_peaks[channel] = self
                            .resolved_pruned_upper_channel_peaks[channel]
                            .max(upper);
                    } else {
                        let left = self.coarse.evaluation(cell, channel);
                        let right = self.coarse.evaluation(cell + 1, channel);
                        cells.push(CandidateNode {
                            cell,
                            channel,
                            upper,
                            screening_priority: self.screening_priority(cell, channel),
                            work: CandidateWork::Dyadic {
                                phase_start: 0,
                                phase_end: tail.factor,
                                tree_index: 1,
                                left,
                                right,
                                d2_upper,
                                magnitude_upper,
                            },
                        });
                    }
                }

                if self.policy == SearchPolicy::Reference9
                    && child_len == DENSE_CHILD_CELLS
                    && cells.len() == DENSE_CHILD_CELLS
                    && self.dense_region_is_cheaper(child_start, child_end)
                {
                    let upper = cells.iter().map(|node| node.upper).fold(0.0, f64::max);
                    let priority = cells
                        .iter()
                        .map(|node| node.screening_priority)
                        .fold(0.0, f64::max);
                    heap.push(CandidateNode {
                        cell: child_start,
                        channel,
                        upper,
                        screening_priority: priority,
                        work: CandidateWork::DenseRegion { end_cell: child_end },
                    });
                } else {
                    for node in cells {
                        if let CandidateWork::Dyadic { left, right, .. } = node.work {
                            self.observe_target_evaluation(
                                channel,
                                KnotLocation { cell: node.cell, phase: 0 },
                                left,
                            );
                            self.observe_target_evaluation(
                                channel,
                                KnotLocation { cell: node.cell + 1, phase: 0 },
                                right,
                            );
                        }
                        heap.push(node);
                    }
                }
                child_offset += child_len;
            }
            root_offset += root_len;
        }
        heap
    }

    fn direct_first_half(&self, coarse_index: i128, channel: usize) -> Evaluation {
        debug_assert!(coarse_index.rem_euclid(2) != 0);
        let source_center = (coarse_index - 1).div_euclid(2) + self.spec.group_delay_inputs();
        let mut sums = [0.0_f64; 4];
        let mut absolute_sum = 0.0_f64;
        for tap in 0..self.spec.first_taps {
            let coefficient = self.spec.full_first_coefficient(tap);
            let sample = self.raw.sample(source_center - tap as i128, channel);
            let product = coefficient * sample;
            sums[tap & 3] += product;
            absolute_sum = upper_add(
                absolute_sum,
                upper_mul(coefficient.abs(), sample.abs()),
            );
        }
        let value = (sums[0] + sums[1]) + (sums[2] + sums[3]);
        let operations = self.spec.first_taps.saturating_mul(2).saturating_add(8);
        Evaluation {
            value,
            error: dot_rounding_upper(operations, absolute_sum),
        }
    }

    fn accurate_direct_coarse(&self, coarse_index: i128, channel: usize) -> Evaluation {
        if coarse_index.rem_euclid(2) == 0 {
            return Evaluation {
                value: self.raw.sample(coarse_index.div_euclid(2), channel),
                error: 0.0,
            };
        }
        let source_center = (coarse_index - 1).div_euclid(2) + self.spec.group_delay_inputs();
        accurate_dot((0..self.spec.first_taps).map(|tap| {
            (
                self.spec.full_first_coefficient(tap),
                self.raw.sample(source_center - tap as i128, channel),
            )
        }))
    }

    fn strict_coarse_evaluation(&mut self, coarse_index: i128, channel: usize) -> Evaluation {
        if let Some(evaluation) = self.strict_cache.get(coarse_index, channel) {
            return evaluation;
        }
        let evaluation = if self.coarse.contains(coarse_index, coarse_index) {
            self.coarse.evaluation(coarse_index, channel)
        } else if coarse_index.rem_euclid(2) == 0 {
            Evaluation {
                value: self.raw.sample(coarse_index.div_euclid(2), channel),
                error: 0.0,
            }
        } else {
            // Narrow fail-closed fallback only. The normal scanner receives
            // every first-stage value from the qualified 2x prefix.
            self.diagnostics.strict_coarse_evaluations =
                self.diagnostics.strict_coarse_evaluations.saturating_add(1);
            self.direct_first_half(coarse_index, channel)
        };
        if self.strict_cache.contains(coarse_index) {
            self.strict_cache.set(coarse_index, channel, evaluation);
        }
        evaluation
    }

    fn strict_cell_components(&mut self, cell: i128, channel: usize) -> StrictCellComponents {
        let tail = self.spec.tail();
        let support_start = cell + i128::from(tail.offset_min);
        let support_end = cell + i128::from(tail.offset_max);
        let left = self.strict_coarse_evaluation(cell, channel);
        let right = self.strict_coarse_evaluation(cell + 1, channel);
        let mut magnitude_upper = 0.0_f64;
        for index in support_start..=support_end {
            magnitude_upper = magnitude_upper.max(self.strict_coarse_evaluation(index, channel).upper());
        }

        let mut d2_upper = 0.0_f64;
        for index in support_start..=support_end - 2 {
            d2_upper = d2_upper.max(second_difference_upper(
                self.strict_coarse_evaluation(index, channel),
                self.strict_coarse_evaluation(index + 1, channel),
                self.strict_coarse_evaluation(index + 2, channel),
            ));
        }
        StrictCellComponents {
            left,
            right,
            d2_upper,
            magnitude_upper,
        }
    }

    fn strict_tail_phase(&mut self, cell: i128, channel: usize, phase: usize) -> Evaluation {
        let tail = self.spec.tail();
        debug_assert!(phase > 0 && phase < tail.factor);
        let mut sums = [0.0_f64; 4];
        let mut absolute_sum = 0.0_f64;
        let mut propagated_error = 0.0_f64;
        let mut local_magnitude = 0.0_f64;
        let mut term_index = 0usize;
        for offset in tail.offset_min..=tail.offset_max {
            let coefficient = tail.coefficient(phase, offset);
            if coefficient == 0.0 {
                continue;
            }
            let evaluation = self.strict_coarse_evaluation(cell + i128::from(offset), channel);
            let product = coefficient * evaluation.value;
            sums[term_index & 3] += product;
            absolute_sum = upper_add(
                absolute_sum,
                upper_mul(coefficient.abs(), evaluation.value.abs()),
            );
            propagated_error = upper_add(
                propagated_error,
                upper_mul(coefficient.abs(), evaluation.error),
            );
            local_magnitude = local_magnitude.max(evaluation.upper());
            term_index += 1;
        }
        let value = (sums[0] + sums[1]) + (sums[2] + sums[3]);
        let rounding = dot_rounding_upper(term_index.saturating_mul(2).saturating_add(8), absolute_sum);
        let model = upper_mul(
            self.spec.tail().composition_error_per_coarse_peak_upper,
            local_magnitude,
        );
        Evaluation {
            value,
            error: upper_add(upper_add(propagated_error, rounding), model),
        }
    }

    fn accurate_target_evaluation(
        &self,
        location: KnotLocation,
        channel: usize,
    ) -> Evaluation {
        if location.phase == 0 {
            return self.accurate_direct_coarse(location.cell, channel);
        }
        let tail = self.spec.tail();
        debug_assert!(location.phase < tail.factor);
        let mut coarse = Vec::with_capacity(tail.nonzero_counts[location.phase] as usize);
        let mut propagated = 0.0_f64;
        let mut local_magnitude = 0.0_f64;
        for offset in tail.offset_min..=tail.offset_max {
            let coefficient = tail.coefficient(location.phase, offset);
            if coefficient == 0.0 {
                continue;
            }
            let evaluation = self.accurate_direct_coarse(
                location.cell + i128::from(offset),
                channel,
            );
            propagated = upper_add(
                propagated,
                upper_mul(coefficient.abs(), evaluation.error),
            );
            local_magnitude = local_magnitude.max(evaluation.upper());
            coarse.push((coefficient, evaluation.value));
        }
        let dot = accurate_dot(coarse);
        let model = upper_mul(
            self.spec.tail().composition_error_per_coarse_peak_upper,
            local_magnitude,
        );
        Evaluation {
            value: dot.value,
            error: upper_add(upper_add(dot.error, propagated), model),
        }
    }


    fn child_node(
        &self,
        parent: CandidateNode,
        left_child: bool,
        midpoint_phase: usize,
        midpoint: Evaluation,
    ) -> Option<CandidateNode> {
        let CandidateWork::Dyadic {
            phase_start: parent_start,
            phase_end: parent_end,
            tree_index: parent_tree,
            left: parent_left,
            right: parent_right,
            d2_upper,
            magnitude_upper,
        } = parent.work
        else {
            return None;
        };
        let (phase_start, phase_end, left, right, tree_index) = if left_child {
            (
                parent_start,
                midpoint_phase,
                parent_left,
                midpoint,
                parent_tree * 2,
            )
        } else {
            (
                midpoint_phase,
                parent_end,
                midpoint,
                parent_right,
                parent_tree * 2 + 1,
            )
        };
        if phase_end - phase_start <= 1 {
            return None;
        }
        let endpoint_upper = left.upper().max(right.upper());
        let upper = self.bound_from_components(endpoint_upper, d2_upper, magnitude_upper, tree_index);
        Some(CandidateNode {
            cell: parent.cell,
            channel: parent.channel,
            upper,
            screening_priority: parent.screening_priority,
            work: CandidateWork::Dyadic {
                phase_start,
                phase_end,
                tree_index,
                left,
                right,
                d2_upper,
                magnitude_upper,
            },
        })
    }

    fn interval_node(
        &self,
        cell: i128,
        channel: usize,
        screening_priority: f64,
        phase_start: usize,
        phase_end: usize,
        tree_index: usize,
        left: Evaluation,
        right: Evaluation,
        components: StrictCellComponents,
    ) -> CandidateNode {
        let upper = self.bound_from_components(
            left.upper().max(right.upper()),
            components.d2_upper,
            components.magnitude_upper,
            tree_index,
        );
        CandidateNode {
            cell,
            channel,
            upper,
            screening_priority,
            work: CandidateWork::Dyadic {
                phase_start,
                phase_end,
                tree_index,
                left,
                right,
                d2_upper: components.d2_upper,
                magnitude_upper: components.magnitude_upper,
            },
        }
    }

    fn sparse_full_phase_product_estimate(&self, cell_count: usize) -> u64 {
        let per_cell = (1..self.spec.tail().factor)
            .map(|phase| {
                self.spec
                    .tail()
                    .phase_product_count(phase)
                    .saturating_add(NODE_PROCESSING_CREDITS)
            })
            .fold(0_u64, |total, value| total.saturating_add(value));
        per_cell.saturating_mul(cell_count as u64)
    }

    fn dense_region_is_cheaper(&self, start_cell: i128, end_cell: i128) -> bool {
        let cell_count = usize::try_from(end_cell - start_cell).expect("bounded dense child");
        if cell_count != DENSE_CHILD_CELLS {
            return false;
        }
        match self.spec.id {
            ReconstructionId::LegacyHeadroom64 => {
                // Every dense child starts on the same 32x-aligned Legacy
                // phase geometry, so the 8-cell halo/product count is
                // translation invariant. Cache it rather than recomputing the
                // stage ranges for every group/channel.
                legacy_dense_eight_cell_product_estimate()
                    < self.sparse_full_phase_product_estimate(cell_count)
            }
            ReconstructionId::Hq1024V1 => {
                // Reaching the same 8x intermediate level sparsely needs the
                // midpoint plus both quarter midpoints in every live cell and
                // the associated heap-node work.  Batching those three knots
                // removes that repeated node/control cost while leaving the
                // subsequent certified HQ tail unchanged.
                let quarter = self.spec.tail().factor / 4;
                let phase_cost = [quarter, quarter * 2, quarter * 3]
                    .into_iter()
                    .map(|phase| self.spec.tail().phase_product_count(phase))
                    .fold(0_u64, |total, value| total.saturating_add(value))
                    .saturating_mul(cell_count as u64);
                let sparse_control = NODE_PROCESSING_CREDITS
                    .saturating_mul(7)
                    .saturating_mul(cell_count as u64);
                let dense_control = NODE_PROCESSING_CREDITS
                    .saturating_mul(cell_count as u64);
                phase_cost.saturating_add(dense_control)
                    < phase_cost.saturating_add(sparse_control)
            }
        }
    }

    fn legacy_dense_block(
        &mut self,
        start_cell: i128,
        end_cell: i128,
        channel: usize,
    ) -> Vec<(KnotLocation, Evaluation)> {
        debug_assert_eq!(self.spec.id, ReconstructionId::LegacyHeadroom64);
        let final_start = start_cell * LEGACY_TAIL_FACTOR as i128;
        let final_end = end_cell * LEGACY_TAIL_FACTOR as i128;
        let ranges = legacy_dense_ranges(final_start, final_end);
        let (coarse_start, coarse_end) = ranges[0];
        let mut initial = Vec::with_capacity(
            usize::try_from(coarse_end - coarse_start + 1).expect("bounded legacy dense halo"),
        );
        for index in coarse_start..=coarse_end {
            initial.push(self.strict_coarse_evaluation(index, channel));
        }
        let mut buffer = DenseEvaluationBuffer {
            start_index: coarse_start,
            values: initial,
        };
        for (stage_index, stage) in legacy_dense_stages().iter().enumerate() {
            let (target_start, target_end) = ranges[stage_index + 1];
            buffer = apply_legacy_dense_stage(&buffer, target_start, target_end, stage);
        }

        let mut result = Vec::with_capacity(
            usize::try_from((end_cell - start_cell) * LEGACY_TAIL_FACTOR as i128 + 1)
                .expect("bounded legacy dense result"),
        );
        for target in final_start..=final_end {
            let relative = target - final_start;
            let cell = start_cell + relative.div_euclid(LEGACY_TAIL_FACTOR as i128);
            let phase = usize::try_from(relative.rem_euclid(LEGACY_TAIL_FACTOR as i128))
                .expect("legacy dense phase");
            result.push((KnotLocation { cell, phase }, buffer.get(target)));
        }
        result
    }

    fn process_dense_region(
        &mut self,
        start_cell: i128,
        end_cell: i128,
        channel: usize,
        screening_priority: f64,
        heap: &mut BinaryHeap<CandidateNode>,
    ) {
        self.diagnostics.dense_regions = self.diagnostics.dense_regions.saturating_add(1);
        let factor = self.spec.tail().factor;
        let quarter = factor / 4;
        debug_assert!(quarter > 0);
        let mut all_quarters_live = self.spec.id == ReconstructionId::LegacyHeadroom64;
        let mut legacy_live_cells = Vec::new();

        for cell in start_cell..end_cell {
            self.diagnostics.work_credits_consumed = self
                .diagnostics
                .work_credits_consumed
                .saturating_add(NODE_PROCESSING_CREDITS);
            let components = self.strict_cell_components(cell, channel);
            self.observe_target_evaluation(
                channel,
                KnotLocation { cell, phase: 0 },
                components.left,
            );
            self.observe_target_evaluation(
                channel,
                KnotLocation {
                    cell: cell + 1,
                    phase: 0,
                },
                components.right,
            );
            let root_upper = self.bound_from_components(
                components.left.upper().max(components.right.upper()),
                components.d2_upper,
                components.magnitude_upper,
                1,
            );
            if root_upper <= self.overall_lower_peak {
                self.resolved_pruned_upper_channel_peaks[channel] = self
                    .resolved_pruned_upper_channel_peaks[channel]
                    .max(root_upper);
                all_quarters_live = false;
                continue;
            }

            let mut knots = [components.left; 5];
            knots[4] = components.right;
            for ordinal in 1..4 {
                let phase = ordinal * quarter;
                let evaluation = self.strict_tail_phase(cell, channel, phase);
                self.diagnostics.phase_evaluations =
                    self.diagnostics.phase_evaluations.saturating_add(1);
                self.diagnostics.dense_phase_evaluations =
                    self.diagnostics.dense_phase_evaluations.saturating_add(1);
                let charge = self
                    .spec
                    .tail()
                    .phase_product_count(phase)
                    .saturating_add(NODE_PROCESSING_CREDITS);
                self.diagnostics.work_credits_consumed =
                    self.diagnostics.work_credits_consumed.saturating_add(charge);
                self.observe_target_evaluation(
                    channel,
                    KnotLocation { cell, phase },
                    evaluation,
                );
                knots[ordinal] = evaluation;
            }
            self.diagnostics.dense_intermediate_cells =
                self.diagnostics.dense_intermediate_cells.saturating_add(1);

            let mut live_quarters = Vec::new();
            for ordinal in 0..4 {
                let phase_start = ordinal * quarter;
                let phase_end = (ordinal + 1) * quarter;
                let node = self.interval_node(
                    cell,
                    channel,
                    screening_priority,
                    phase_start,
                    phase_end,
                    4 + ordinal,
                    knots[ordinal],
                    knots[ordinal + 1],
                    components,
                );
                if node.upper > self.overall_lower_peak {
                    live_quarters.push(node);
                } else {
                    self.resolved_pruned_upper_channel_peaks[channel] = self
                        .resolved_pruned_upper_channel_peaks[channel]
                        .max(node.upper);
                }
            }
            if live_quarters.len() != 4 {
                all_quarters_live = false;
            }
            legacy_live_cells.push((cell, live_quarters));
        }

        if self.spec.id == ReconstructionId::LegacyHeadroom64
            && all_quarters_live
            && !legacy_live_cells.is_empty()
        {
            // The complete Legacy block becomes cheaper in control/indexing
            // terms only after the certificate has demonstrated that every 8x
            // quarter in the dense child remains live.  Resolve the exact same
            // 64x finite reconstruction; this is an execution change only.
            self.diagnostics.dense_complete_regions =
                self.diagnostics.dense_complete_regions.saturating_add(1);
            let block = self.legacy_dense_block(start_cell, end_cell, channel);
            let block_charge = legacy_dense_eight_cell_product_estimate()
                .saturating_add(NODE_PROCESSING_CREDITS);
            self.diagnostics.work_credits_consumed = self
                .diagnostics
                .work_credits_consumed
                .saturating_add(block_charge);
            for (location, evaluation) in block {
                if location.phase == 0
                    || location.phase == quarter
                    || location.phase == quarter * 2
                    || location.phase == quarter * 3
                {
                    continue;
                }
                // The final endpoint belongs to the next cell and was already
                // observed as that cell's phase-0 knot.
                if location.cell >= end_cell {
                    continue;
                }
                self.diagnostics.phase_evaluations =
                    self.diagnostics.phase_evaluations.saturating_add(1);
                self.diagnostics.dense_phase_evaluations =
                    self.diagnostics.dense_phase_evaluations.saturating_add(1);
                self.observe_target_evaluation(channel, location, evaluation);
            }
        } else {
            for (_, live_quarters) in legacy_live_cells {
                for node in live_quarters {
                    heap.push(node);
                }
            }
        }
    }

    fn process_global_heap(
        &mut self,
        mut heap: BinaryHeap<CandidateNode>,
        root_upper_by_channel: &[f64],
    ) -> (bool, bool) {
        debug_assert_eq!(root_upper_by_channel.len(), self.channels);
        let base_allowance = self.fast_credits_per_tile_channel;
        let mut available = (0..self.channels)
            .map(|channel| match self.policy {
                SearchPolicy::Reference9 | SearchPolicy::RetiredClockFast1s => u64::MAX,
                SearchPolicy::Fast90 => base_allowance.saturating_add(self.carried_credits[channel]),
            })
            .collect::<Vec<_>>();
        let mut work_exhausted = vec![false; self.channels];
        let mut refined_roots = 0_u64;
        let mut work_limited = false;
        let mut time_limited = false;

        while let Some(node) = heap.pop() {
            let channel = node.channel;
            if node.upper <= self.overall_lower_peak {
                self.resolved_pruned_upper_channel_peaks[channel] = self
                    .resolved_pruned_upper_channel_peaks[channel]
                    .max(node.upper);
                continue;
            }

            // Fast is a cumulative wall-clock policy. Once its meter-processing
            // allowance is spent, the precomputed root uppers retain every
            // still-live candidate without draining the heap after the deadline.
            // Those roots remain authoritative even when their coarse inputs
            // came from the time-bounded FIR-L1 prefix enclosure.
            if self.policy == SearchPolicy::RetiredClockFast1s && self.fast_time_budget_exhausted() {
                time_limited = true;
                for (channel, root_upper) in root_upper_by_channel.iter().copied().enumerate() {
                    self.unresolved_upper_channel_peaks[channel] = self
                        .unresolved_upper_channel_peaks[channel]
                        .max(root_upper);
                }
                break;
            }

            if work_exhausted[channel] {
                self.unresolved_upper_channel_peaks[channel] = self
                    .unresolved_upper_channel_peaks[channel]
                    .max(node.upper);
                continue;
            }

            match node.work {
                CandidateWork::DenseRegion { end_cell } => {
                    debug_assert_eq!(self.policy, SearchPolicy::Reference9);
                    self.process_dense_region(
                        node.cell,
                        end_cell,
                        channel,
                        node.screening_priority,
                        &mut heap,
                    );
                }
                CandidateWork::Dyadic {
                    phase_start,
                    phase_end,
                    tree_index,
                    ..
                } => {
                    let midpoint_phase = (phase_start + phase_end) / 2;
                    debug_assert!(midpoint_phase > phase_start && midpoint_phase < phase_end);
                    let charge = self
                        .spec
                        .tail()
                        .phase_product_count(midpoint_phase)
                        .saturating_add(NODE_PROCESSING_CREDITS);
                    if self.policy == SearchPolicy::Fast90 && charge > available[channel] {
                        work_exhausted[channel] = true;
                        work_limited = true;
                        self.unresolved_upper_channel_peaks[channel] = self
                            .unresolved_upper_channel_peaks[channel]
                            .max(node.upper);
                        continue;
                    }
                    if self.policy == SearchPolicy::Fast90 {
                        available[channel] -= charge;
                    }

                    // The loop-head deadline check is intentionally the sole
                    // clock read for this node. No candidate-specific expensive
                    // work occurs between that check and this reconstruction, so
                    // a second Instant::elapsed() here would only add hot-path
                    // overhead. One midpoint evaluation is the maximum deadline
                    // overshoot quantum.

                    self.diagnostics.work_credits_consumed = self
                        .diagnostics
                        .work_credits_consumed
                        .saturating_add(charge);
                    self.diagnostics.phase_evaluations = self
                        .diagnostics
                        .phase_evaluations
                        .saturating_add(1);
                    if tree_index == 1 {
                        refined_roots = refined_roots.saturating_add(1);
                    }

                    let midpoint = self.strict_tail_phase(node.cell, channel, midpoint_phase);
                    self.observe_target_evaluation(
                        channel,
                        KnotLocation {
                            cell: node.cell,
                            phase: midpoint_phase,
                        },
                        midpoint,
                    );

                    if let Some(left) = self.child_node(node, true, midpoint_phase, midpoint) {
                        if left.upper > self.overall_lower_peak {
                            heap.push(left);
                        } else {
                            self.resolved_pruned_upper_channel_peaks[channel] = self
                                .resolved_pruned_upper_channel_peaks[channel]
                                .max(left.upper);
                        }
                    }
                    if let Some(right) = self.child_node(node, false, midpoint_phase, midpoint) {
                        if right.upper > self.overall_lower_peak {
                            heap.push(right);
                        } else {
                            self.resolved_pruned_upper_channel_peaks[channel] = self
                                .resolved_pruned_upper_channel_peaks[channel]
                                .max(right.upper);
                        }
                    }
                }
            }
        }

        self.diagnostics.refined_cells = self.diagnostics.refined_cells.saturating_add(refined_roots);
        if self.policy == SearchPolicy::Fast90 {
            let maximum_carry = base_allowance.saturating_mul(FAST90_MAX_CARRY_TILES);
            for channel in 0..self.channels {
                self.carried_credits[channel] = available[channel].min(maximum_carry);
            }
        }
        (work_limited, time_limited)
    }

    fn settle_tile_frontiers(&mut self) {
        if self.policy.uses_accelerated_prefix() {
            return;
        }
        for channel in 0..self.channels {
            let mut frontier = std::mem::take(&mut self.tile_frontiers[channel]);
            frontier.retained.sort_unstable_by(|left, right| {
                right
                    .evaluation
                    .upper()
                    .total_cmp(&left.evaluation.upper())
                    .then_with(|| left.location.cell.cmp(&right.location.cell))
                    .then_with(|| left.location.phase.cmp(&right.location.phase))
            });

            for tracked in frontier.retained {
                // The channel lower is monotone.  Once an ordinary enclosure
                // is below it, this candidate cannot affect the final upper.
                if tracked.evaluation.upper() <= self.lower_channel_peaks[channel] {
                    continue;
                }
                let accurate = self.accurate_target_evaluation(tracked.location, channel);
                self.diagnostics.direct_rescore_evaluations = self
                    .diagnostics
                    .direct_rescore_evaluations
                    .saturating_add(1);
                self.observe_evaluation_base(channel, accurate);
                self.settled_evaluated_upper_channel_peaks[channel] = self
                    .settled_evaluated_upper_channel_peaks[channel]
                    .max(accurate.upper());
            }
        }
    }

    fn process_tile(
        &mut self,
        _start_frame: i128,
        _end_frame: i128,
        coarse_start: i128,
        coarse_end: i128,
    ) {
        debug_assert!(coarse_end >= coarse_start);
        self.diagnostics.tiles_processed = self.diagnostics.tiles_processed.saturating_add(1);
        let tail = self.spec.tail();
        let cache_start = coarse_start + i128::from(tail.offset_min);
        let cache_end = if coarse_end == coarse_start {
            coarse_start
        } else {
            coarse_end - 1 + i128::from(tail.offset_max)
        };
        self.strict_cache.reset(cache_start, cache_end);

        if coarse_end == coarse_start {
            for channel in 0..self.channels {
                let evaluation = self.strict_coarse_evaluation(coarse_start, channel);
                self.observe_target_evaluation(
                    channel,
                    KnotLocation {
                        cell: coarse_start,
                        phase: 0,
                    },
                    evaluation,
                );
            }
            self.settle_tile_frontiers();
            return;
        }

        // Fast may reach its cumulative allowance while the prefix is being
        // produced. Do not spend additional time constructing a selective
        // candidate tree after that point. Retain the complete-reconstruction
        // L-infinity upper over this tile's exact raw support before returning.
        if self.policy == SearchPolicy::RetiredClockFast1s && self.fast_time_budget_exhausted() {
            self.mark_fast_tile_time_limited(coarse_start, coarse_end);
            return;
        }

        // The qualified 2x prefix is mandatory work, outside Fast90's
        // discretionary refinement allowance. Establish the strongest coarse
        // lower first so the authority screens can reject against it. Fast90
        // also constructs its cheap magnitude summary in this same pass;
        // Reference9 deliberately retains the existing execution path.
        let fast90_magnitude_summaries = match self.policy {
            SearchPolicy::Reference9 => {
                for channel in 0..self.channels {
                    for index in coarse_start..=coarse_end {
                        let evaluation = self.coarse.evaluation(index, channel);
                        self.observe_evaluation_base(channel, evaluation);
                        self.diagnostics.authoritative_coarse_values = self
                            .diagnostics
                            .authoritative_coarse_values
                            .saturating_add(1);
                    }
                }
                None
            }
            SearchPolicy::Fast90 | SearchPolicy::RetiredClockFast1s => {
                let mut summaries = Vec::with_capacity(self.channels);
                for channel in 0..self.channels {
                    summaries.push(self.build_fast90_magnitude_summary(
                        channel,
                        coarse_start,
                        coarse_end,
                    ));
                    if self.policy == SearchPolicy::RetiredClockFast1s
                        && self.fast_time_budget_exhausted()
                    {
                        self.mark_fast_tile_time_limited(coarse_start, coarse_end);
                        return;
                    }
                }
                Some(summaries)
            }
        };

        let mut heap = BinaryHeap::new();
        let mut root_upper_by_channel = vec![0.0_f64; self.channels];
        for channel in 0..self.channels {
            let candidates = match self.policy {
                SearchPolicy::Reference9 => {
                    self.generate_channel_candidates(channel, coarse_start, coarse_end)
                }
                SearchPolicy::Fast90 | SearchPolicy::RetiredClockFast1s => {
                    self.generate_fast90_channel_candidates(
                        channel,
                        coarse_start,
                        coarse_end,
                        &fast90_magnitude_summaries
                            .as_ref()
                            .expect("Fast90 magnitude summaries")
                            [channel],
                    )
                }
            };
            for candidate in &candidates {
                root_upper_by_channel[channel] =
                    root_upper_by_channel[channel].max(candidate.upper);
            }
            heap.extend(candidates);
            if self.policy == SearchPolicy::RetiredClockFast1s
                && self.fast_time_budget_exhausted()
            {
                self.mark_fast_partial_candidate_tile_time_limited(
                    coarse_start,
                    coarse_end,
                    &root_upper_by_channel,
                    channel + 1,
                );
                return;
            }
        }
        let (tile_work_limited, tile_time_limited) =
            self.process_global_heap(heap, &root_upper_by_channel);
        if tile_work_limited {
            self.diagnostics.work_limited_tiles =
                self.diagnostics.work_limited_tiles.saturating_add(1);
        }
        if tile_time_limited {
            self.diagnostics.time_limited_tiles =
                self.diagnostics.time_limited_tiles.saturating_add(1);
        }
        self.settle_tile_frontiers();
    }

    fn finalize(
        mut self,
        frames: u64,
        input_channel_peaks: &[f64],
    ) -> Result<InternalPeakCertificate, TruePeakError> {
        self.process_ready_full_tiles();
        if self.numerical_invalid {
            return Err(TruePeakError::NumericalOverflow);
        }
        let last_frame = i128::from(frames - 1);
        let coarse_start = self.next_tile_start_frame * 2;
        let coarse_end = last_frame * 2;
        if coarse_start < coarse_end {
            if !self.ranges_available(coarse_start, coarse_end) {
                return Err(TruePeakError::NumericalOverflow);
            }
            self.process_tile(self.next_tile_start_frame, last_frame, coarse_start, coarse_end);
        } else if coarse_start == coarse_end {
            if !self.ranges_available(coarse_start, coarse_end) {
                return Err(TruePeakError::NumericalOverflow);
            }
            self.process_tile(self.next_tile_start_frame, last_frame, coarse_start, coarse_end);
        }
        if self.numerical_invalid {
            return Err(TruePeakError::NumericalOverflow);
        }

        // The exact input sample maxima are identity knots of both
        // reconstructions.  Reassert them at finalization so the public report
        // remains robust if the streaming lower bookkeeping is changed later.
        for (channel, sample_peak) in input_channel_peaks.iter().copied().enumerate() {
            self.point_channel_peaks[channel] =
                max_nonnegative_finite(self.point_channel_peaks[channel], sample_peak);
            self.lower_channel_peaks[channel] =
                max_nonnegative_finite(self.lower_channel_peaks[channel], sample_peak);
            self.overall_lower_peak =
                max_nonnegative_finite(self.overall_lower_peak, self.lower_channel_peaks[channel]);
            self.settled_evaluated_upper_channel_peaks[channel] = max_nonnegative_finite(
                self.settled_evaluated_upper_channel_peaks[channel],
                sample_peak,
            );
        }

        let mut channel_intervals = Vec::with_capacity(self.channels);
        let mut channel_upper_linear_peaks = Vec::with_capacity(self.channels);
        let mut reported_channels = Vec::with_capacity(self.channels);
        let mut unresolved_max = 0.0_f64;
        for channel in 0..self.channels {
            let lower = self.lower_channel_peaks[channel];
            let upper = lower
                .max(self.settled_evaluated_upper_channel_peaks[channel])
                .max(self.resolved_pruned_upper_channel_peaks[channel])
                .max(self.unresolved_upper_channel_peaks[channel]);
            if !lower.is_finite() || !upper.is_finite() {
                return Err(TruePeakError::NumericalOverflow);
            }
            channel_intervals.push(PeakInterval::new(lower, upper));
            channel_upper_linear_peaks.push(upper);
            unresolved_max = unresolved_max.max(self.unresolved_upper_channel_peaks[channel]);
            reported_channels.push(max_nonnegative_finite(
                self.point_channel_peaks[channel] * self.spec.calibration_linear,
                input_channel_peaks[channel],
            ));
        }

        let lower = self
            .lower_channel_peaks
            .iter()
            .copied()
            .fold(0.0, max_nonnegative_finite);
        let upper = channel_upper_linear_peaks
            .iter()
            .copied()
            .fold(0.0, max_nonnegative_finite);
        let finite_interval = PeakInterval::new(lower, upper);
        let reported_linear = reported_channels
            .iter()
            .copied()
            .fold(0.0, max_nonnegative_finite);
        let reported_overall = if magnitude_bits(reported_linear) == 0 {
            PeakLevel::Silence
        } else {
            PeakLevel::Finite {
                linear: reported_linear,
                dbtp: positive_finite_linear_to_dbtp(reported_linear),
            }
        };
        let all_unresolved_dominated = self
            .unresolved_upper_channel_peaks
            .iter()
            .copied()
            .all(|unresolved| unresolved <= self.overall_lower_peak);
        let status = match self.policy {
            SearchPolicy::RetiredClockFast1s
                if self.diagnostics.time_limited_tiles != 0
                    || self.diagnostics.time_bounded_prefix_blocks_skipped != 0 =>
            {
                // Fast's contract includes reporting that the clock stopped
                // either search or prefix work, even when a later/higher lower
                // bound makes every retained skipped upper noncompetitive.
                SearchStatus::TimeLimited
            }
            _ if all_unresolved_dominated => SearchStatus::Complete,
            SearchPolicy::Reference9 => return Err(TruePeakError::ReferenceSearchIncomplete),
            SearchPolicy::Fast90 => SearchStatus::WorkLimited,
            SearchPolicy::RetiredClockFast1s => SearchStatus::TimeLimited,
        };
        self.diagnostics.unresolved_upper_linear = unresolved_max;
        self.end_processing();

        Ok(InternalPeakCertificate {
            reconstruction: self.spec.id,
            search_policy: self.policy,
            reconstruction_linf_gain_upper: self.spec.id.reconstruction_linf_gain_upper(),
            numerical_envelope_linear: self.diagnostics.max_evaluation_error_linear,
            reported_point_estimate: TruePeakResult {
                overall: reported_overall,
                channel_linear_peaks: reported_channels,
                frames,
            },
            finite_interval,
            channel_intervals,
            channel_upper_linear_peaks,
            status,
            diagnostics: self.diagnostics,
        })
    }
}


#[derive(Debug, Clone)]
pub(super) struct CertifiedPeakMeterImpl {
    spec: ReconstructionSpec,
    edge_policy: EdgePolicy,
    engine: CertifiedTwoXEngine,
    scanner: CertifiedScanner,
    first_frame: Vec<f64>,
    last_frame: Vec<f64>,
    started: bool,
    next_input_index: i128,
    frames: u64,
    input_channel_peaks: Vec<f64>,
}

impl CertifiedPeakMeterImpl {
    pub(super) fn new(
        sample_rate_hz: u32,
        channels: usize,
        edge_policy: EdgePolicy,
        reconstruction: ReconstructionId,
        policy: SearchPolicy,
    ) -> Result<Self, TruePeakError> {
        if sample_rate_hz == 0 {
            return Err(TruePeakError::InvalidSampleRate);
        }
        if channels == 0 {
            return Err(TruePeakError::InvalidChannelCount);
        }
        let construction_started =
            (policy == SearchPolicy::RetiredClockFast1s).then(Instant::now);
        let spec = ReconstructionSpec::for_id(reconstruction);
        let engine = CertifiedTwoXEngine::new(spec, policy, channels);
        let mut scanner = CertifiedScanner::new_at_rate(spec, policy, channels, sample_rate_hz);
        if let Some(started) = construction_started {
            scanner.processing_time_spent = started.elapsed();
        }
        scanner.diagnostics.accelerated_same_graph_avx_prefix_active =
            engine.fast90_same_graph_avx_active();
        Ok(Self {
            spec,
            edge_policy,
            engine,
            scanner,
            first_frame: vec![0.0; channels],
            last_frame: vec![0.0; channels],
            started: false,
            next_input_index: 0,
            frames: 0,
            input_channel_peaks: vec![0.0; channels],
        })
    }

    pub(super) fn push_interleaved(&mut self, samples: &[f64]) -> Result<(), TruePeakError> {
        let validation_started =
            (self.scanner.policy == SearchPolicy::RetiredClockFast1s).then(Instant::now);
        let channels = self.last_frame.len();
        if samples.len() % channels != 0 {
            return Err(TruePeakError::IncompleteFrame {
                samples: samples.len(),
                channels,
            });
        }
        if let Some((sample_index, _)) = samples
            .iter()
            .enumerate()
            .find(|(_, sample)| !sample.is_finite())
        {
            return Err(TruePeakError::NonFiniteSample { sample_index });
        }
        if samples.is_empty() {
            return Ok(());
        }

        if let Some(started) = validation_started {
            self.scanner.begin_processing_at(started);
        } else {
            self.scanner.begin_processing();
        }

        if !self.started {
            self.first_frame.copy_from_slice(&samples[..channels]);
            let extension = match self.edge_policy {
                EdgePolicy::RepeatEndpoints => self.first_frame.clone(),
                EdgePolicy::ZeroExtend => vec![0.0; channels],
            };
            for input_index in -self.spec.input_halo_frames..0 {
                self.scanner.observe_raw_frame(input_index, &extension);
                self.engine
                    .process_frame(&extension, input_index, &mut self.scanner);
            }
            self.started = true;
        }

        for frame in samples.chunks_exact(channels) {
            let input_index = self.next_input_index;
            for (peak, sample) in self
                .input_channel_peaks
                .iter_mut()
                .zip(frame.iter().copied())
            {
                *peak = max_exact_magnitude(*peak, sample);
            }
            self.scanner.observe_raw_frame(input_index, frame);
            self.scanner.observe_nominal_frame(input_index, frame);
            self.engine
                .process_frame(frame, input_index, &mut self.scanner);
            self.last_frame.copy_from_slice(frame);
            self.next_input_index += 1;
            match self.frames.checked_add(1) {
                Some(frames) => self.frames = frames,
                None => {
                    self.scanner.end_processing();
                    return Err(TruePeakError::InputTooLong);
                }
            }
        }
        self.scanner.end_processing();
        Ok(())
    }

    pub(super) fn finalize(mut self) -> Result<InternalPeakCertificate, TruePeakError> {
        if self.frames == 0 {
            return Err(TruePeakError::EmptyInput);
        }
        self.scanner.begin_processing();
        let channels = self.last_frame.len();
        let extension = match self.edge_policy {
            EdgePolicy::RepeatEndpoints => self.last_frame.clone(),
            EdgePolicy::ZeroExtend => vec![0.0; channels],
        };
        let stop = self.next_input_index + self.spec.input_halo_frames;
        for input_index in self.next_input_index..stop {
            self.scanner.observe_raw_frame(input_index, &extension);
            self.engine
                .process_frame(&extension, input_index, &mut self.scanner);
        }
        self.engine.flush(&mut self.scanner);
        self.scanner.finalize(self.frames, &self.input_channel_peaks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hq1024_coefficients::{
        HQ1024_DELAY_SUBFRAMES, HQ1024_MEASURED_OPERATOR_LINF,
        HQ1024_TAIL_DELAY_SUBFRAMES,
    };
    use crate::HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER;

    #[cfg(target_arch = "x86")]
    #[allow(deprecated)]
    unsafe fn read_mxcsr() -> u32 {
        core::arch::x86::_mm_getcsr()
    }

    #[cfg(target_arch = "x86_64")]
    #[allow(deprecated)]
    unsafe fn read_mxcsr() -> u32 {
        core::arch::x86_64::_mm_getcsr()
    }

    #[cfg(target_arch = "x86")]
    #[allow(deprecated)]
    unsafe fn write_mxcsr(value: u32) {
        core::arch::x86::_mm_setcsr(value);
    }

    #[cfg(target_arch = "x86_64")]
    #[allow(deprecated)]
    unsafe fn write_mxcsr(value: u32) {
        core::arch::x86_64::_mm_setcsr(value);
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    struct MxcsrRestore(u32);

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    impl Drop for MxcsrRestore {
        fn drop(&mut self) {
            // SAFETY: restoring the MXCSR value captured on this same test
            // thread; the guard never crosses threads.
            unsafe { write_mxcsr(self.0) };
        }
    }

    fn first_half_coefficients(spec: ReconstructionSpec) -> &'static [f64] {
        match spec.id {
            ReconstructionId::LegacyHeadroom64 => &HEADROOM64_HALF_DELAY_COEFFICIENTS,
            ReconstructionId::Hq1024V1 => &HQ1024_HALF_DELAY_COEFFICIENTS,
        }
    }

    fn full_first_coefficient(spec: ReconstructionSpec, tap: usize) -> f64 {
        let half = first_half_coefficients(spec);
        if tap < half.len() {
            half[tap]
        } else {
            half[spec.first_taps - 1 - tap]
        }
    }

    fn extended_sample(
        interleaved: &[f64],
        channels: usize,
        frame: i128,
        channel: usize,
        edge: EdgePolicy,
    ) -> f64 {
        let frames = interleaved.len() / channels;
        if frame >= 0 && frame < frames as i128 {
            return interleaved[frame as usize * channels + channel];
        }
        match edge {
            EdgePolicy::ZeroExtend => 0.0,
            EdgePolicy::RepeatEndpoints if frame < 0 => interleaved[channel],
            EdgePolicy::RepeatEndpoints => interleaved[(frames - 1) * channels + channel],
        }
    }

    fn direct_coarse(
        spec: ReconstructionSpec,
        interleaved: &[f64],
        channels: usize,
        coarse_index: i128,
        channel: usize,
        edge: EdgePolicy,
    ) -> f64 {
        if coarse_index.rem_euclid(2) == 0 {
            return extended_sample(
                interleaved,
                channels,
                coarse_index.div_euclid(2),
                channel,
                edge,
            );
        }
        let source_center = (coarse_index - 1).div_euclid(2) + spec.group_delay_inputs();
        let mut sum = 0.0_f64;
        for tap in 0..spec.first_taps {
            sum += extended_sample(
                interleaved,
                channels,
                source_center - tap as i128,
                channel,
                edge,
            ) * full_first_coefficient(spec, tap);
        }
        sum
    }

    fn exhaustive_peak(
        reconstruction: ReconstructionId,
        interleaved: &[f64],
        channels: usize,
        edge: EdgePolicy,
    ) -> f64 {
        let spec = ReconstructionSpec::for_id(reconstruction);
        let tail = spec.tail();
        let frames = interleaved.len() / channels;
        let final_coarse = (frames as i128 - 1) * 2;
        let coarse_start = i128::from(tail.offset_min);
        let coarse_end = final_coarse + i128::from(tail.offset_max);
        let coarse_len = usize::try_from(coarse_end - coarse_start + 1).unwrap();
        let mut coarse = vec![vec![0.0_f64; coarse_len]; channels];
        for channel in 0..channels {
            for index in coarse_start..=coarse_end {
                coarse[channel][usize::try_from(index - coarse_start).unwrap()] =
                    direct_coarse(spec, interleaved, channels, index, channel, edge);
            }
        }
        let lookup = |channel: usize, index: i128| {
            coarse[channel][usize::try_from(index - coarse_start).unwrap()]
        };

        let target_factor = tail.factor * 2;
        let final_target = (frames - 1) * target_factor;
        let mut peak = 0.0_f64;
        for channel in 0..channels {
            for target in 0..=final_target {
                let cell = (target / tail.factor) as i128;
                let phase = target % tail.factor;
                let value = if phase == 0 {
                    lookup(channel, cell)
                } else {
                    let mut sum = 0.0_f64;
                    for offset in tail.offset_min..=tail.offset_max {
                        sum += tail.coefficient(phase, offset)
                            * lookup(channel, cell + i128::from(offset));
                    }
                    sum
                };
                peak = peak.max(value.abs());
            }
        }
        peak
    }

    fn direct_target_value(
        reconstruction: ReconstructionId,
        interleaved: &[f64],
        channels: usize,
        edge: EdgePolicy,
        location: KnotLocation,
        channel: usize,
    ) -> f64 {
        let spec = ReconstructionSpec::for_id(reconstruction);
        if location.phase == 0 {
            return direct_coarse(
                spec,
                interleaved,
                channels,
                location.cell,
                channel,
                edge,
            );
        }
        let tail = spec.tail();
        assert!(location.phase < tail.factor);
        let mut sum = 0.0_f64;
        for offset in tail.offset_min..=tail.offset_max {
            sum += tail.coefficient(location.phase, offset)
                * direct_coarse(
                    spec,
                    interleaved,
                    channels,
                    location.cell + i128::from(offset),
                    channel,
                    edge,
                );
        }
        sum
    }

    #[test]
    fn frozen_geometry_and_curvature_metadata_match_the_design() {
        let legacy = legacy_tail_metadata();
        let hq = hq_tail_metadata();
        assert_eq!((legacy.factor, legacy.offset_min, legacy.offset_max), (32, -15, 16));
        assert_eq!((hq.factor, hq.offset_min, hq.offset_max), (512, -16, 17));
        assert_eq!(HQ1024_DELAY_SUBFRAMES, 795_354);
        assert_eq!(HQ1024_TAIL_DELAY_SUBFRAMES, 8_922);
        assert!((HQ1024_MEASURED_OPERATOR_LINF - 4.676_026_435_151_282).abs() < 5.0e-15);
        assert!((legacy.node_a_upper[1] - 0.385_054_359_363).abs() < 2.0e-12);
        assert!((hq.node_a_upper[1] - 0.329_422_342_667).abs() < 2.0e-12);
        assert_eq!(legacy.coefficient(0, 0), 1.0);
        assert_eq!(hq.coefficient(0, 0), 1.0);
        assert_eq!(legacy.nonzero_counts[0], 1);
        assert_eq!(hq.nonzero_counts[0], 1);
    }

    fn scan_with_chunks(
        reconstruction: ReconstructionId,
        policy: SearchPolicy,
        interleaved: &[f64],
        channels: usize,
        edge: EdgePolicy,
        chunk_frames: usize,
    ) -> InternalPeakCertificate {
        let mut meter = CertifiedPeakMeterImpl::new(
            192_000,
            channels,
            edge,
            reconstruction,
            policy,
        )
        .unwrap();
        let width = chunk_frames * channels;
        for chunk in interleaved.chunks(width) {
            meter.push_interleaved(chunk).unwrap();
        }
        meter.finalize().unwrap()
    }

    fn scan_with_chunk_pattern(
        reconstruction: ReconstructionId,
        policy: SearchPolicy,
        interleaved: &[f64],
        channels: usize,
        edge: EdgePolicy,
        chunk_pattern_frames: &[usize],
    ) -> InternalPeakCertificate {
        assert!(channels > 0);
        assert!(!chunk_pattern_frames.is_empty());
        assert!(chunk_pattern_frames.iter().all(|frames| *frames > 0));
        assert_eq!(interleaved.len() % channels, 0);

        let mut meter = CertifiedPeakMeterImpl::new(
            192_000,
            channels,
            edge,
            reconstruction,
            policy,
        )
        .unwrap();
        let total_frames = interleaved.len() / channels;
        let mut frame = 0usize;
        let mut pattern_index = 0usize;
        while frame < total_frames {
            let take = chunk_pattern_frames[pattern_index % chunk_pattern_frames.len()]
                .min(total_frames - frame);
            let start = frame * channels;
            let stop = (frame + take) * channels;
            meter.push_interleaved(&interleaved[start..stop]).unwrap();
            frame += take;
            pattern_index += 1;
        }
        meter.finalize().unwrap()
    }

    fn legacy_direct_oracle_peak(
        interleaved: &[f64],
        channels: usize,
        edge: EdgePolicy,
    ) -> f64 {
        crate::legacy_headroom64_direct_oracle_peak(interleaved, channels, edge).unwrap()
    }

    #[test]
    fn legacy_dense_block_encloses_the_same_later_stage_reconstruction() {
        let spec = ReconstructionSpec::for_id(ReconstructionId::LegacyHeadroom64);
        let mut scanner = CertifiedScanner::new(spec, SearchPolicy::Reference9, 1);
        for frame in -spec.input_halo_frames..=spec.input_halo_frames + 32 {
            let x = frame as f64;
            scanner.observe_raw_frame(
                frame,
                &[0.71 * (0.37 * x).sin() + 0.22 * (1.11 * x).cos()],
            );
        }
        let ranges = legacy_dense_ranges(0, 8 * LEGACY_TAIL_FACTOR as i128);
        scanner.strict_cache.reset(ranges[0].0, ranges[0].1);
        let block = scanner.legacy_dense_block(0, 8, 0);
        let mut exercised_rounding_difference = false;
        for (location, dense) in block {
            if location.cell >= 8 || location.phase == 0 {
                continue;
            }
            let sparse = scanner.strict_tail_phase(location.cell, 0, location.phase);
            let difference = (dense.value - sparse.value).abs();
            exercised_rounding_difference |= difference > 0.0;
            assert!(
                difference <= upper_add(dense.error, sparse.error),
                "cell={} phase={} difference={difference:.17e} dense_error={:.17e} sparse_error={:.17e}",
                location.cell,
                location.phase,
                dense.error,
                sparse.error,
            );
        }
        assert!(
            exercised_rounding_difference,
            "fixture must exercise the independent dense/sparse arithmetic paths",
        );
    }

    #[test]
    fn legacy_complete_dense_fallback_has_a_stable_forcing_fixture() {
        let spec = ReconstructionSpec::for_id(ReconstructionId::LegacyHeadroom64);
        let mut scanner = CertifiedScanner::new(spec, SearchPolicy::Reference9, 1);
        let start_cell = 0_i128;
        let end_cell = DENSE_CHILD_CELLS as i128;
        let ranges = legacy_dense_ranges(
            start_cell * LEGACY_TAIL_FACTOR as i128,
            end_cell * LEGACY_TAIL_FACTOR as i128,
        );
        let (coarse_start, coarse_end) = ranges[0];

        // Deliberately broad, finite coarse enclosures keep every 8x quarter
        // live without depending on the spectral accident of a programme-like
        // waveform. This test is about observability of the complete dense
        // execution branch, not about manufacturing a difficult audio signal.
        for coarse_index in coarse_start..=coarse_end {
            scanner.observe_coarse_authority(coarse_index, &[0.0], &[1.0]);
        }
        for frame in -spec.input_halo_frames..=spec.input_halo_frames + 32 {
            scanner.observe_raw_frame(frame, &[0.0]);
        }
        scanner.strict_cache.reset(coarse_start, coarse_end);

        let mut heap = BinaryHeap::new();
        scanner.process_dense_region(start_cell, end_cell, 0, 1.0, &mut heap);

        assert_eq!(
            scanner.diagnostics.dense_intermediate_cells,
            DENSE_CHILD_CELLS as u64,
            "forcing fixture must keep every dense child alive through the 8x level",
        );
        assert_eq!(
            scanner.diagnostics.dense_complete_regions,
            1,
            "forcing fixture must enter the complete Legacy64 dense fallback",
        );
        assert!(
            heap.is_empty(),
            "complete Legacy64 dense fallback must consume rather than requeue live quarters",
        );
    }

    #[test]
    fn hq_reference_dense_intermediate_path_has_a_stable_forcing_fixture() {
        let spec = ReconstructionSpec::for_id(ReconstructionId::Hq1024V1);
        let mut scanner = CertifiedScanner::new(spec, SearchPolicy::Reference9, 1);
        let start_cell = 0_i128;
        let end_cell = DENSE_CHILD_CELLS as i128;
        let tail = spec.tail();
        let coarse_start = start_cell + i128::from(tail.offset_min);
        let coarse_end = end_cell - 1 + i128::from(tail.offset_max);

        // Broad finite enclosures make all four quarters of every child live.
        // This drives HQ's intermediate dense path by state construction, not
        // by relying on a programme waveform to defeat the sparse screen.
        for coarse_index in coarse_start..=coarse_end {
            scanner.observe_coarse_authority(coarse_index, &[0.0], &[1.0]);
        }
        scanner.strict_cache.reset(coarse_start, coarse_end);

        let mut heap = BinaryHeap::new();
        scanner.process_dense_region(start_cell, end_cell, 0, 1.0, &mut heap);

        assert_eq!(scanner.diagnostics.dense_regions, 1);
        assert!(
            scanner.diagnostics.dense_intermediate_cells > 0,
            "forcing fixture must enter HQ intermediate dense execution",
        );
        assert_eq!(
            scanner.diagnostics.dense_complete_regions,
            0,
            "complete dense reconstruction is a Legacy64-only execution path",
        );
        assert!(
            !heap.is_empty(),
            "forced-live HQ dense work must return unresolved quarters to hierarchical refinement",
        );
    }

    #[test]
    fn qualified_prefix_encloses_direct_half_delay_for_packed_and_partial_blocks() {
        use crate::qualified_half_delay_fft::QualifiedHalfDelayFft;

        for reconstruction in [
            ReconstructionId::LegacyHeadroom64,
            ReconstructionId::Hq1024V1,
        ] {
            let spec = ReconstructionSpec::for_id(reconstruction);
            let channels = 3usize;
            let frames = spec.block_frames() + 23;
            let mut samples = vec![0.0_f64; frames * channels];
            for frame in 0..frames {
                let x = frame as f64;
                samples[frame * channels] = 0.97 * (0.37 * x).sin() + 0.11 * (1.07 * x).cos();
                samples[frame * channels + 1] = 1.0e-12 * (0.71 * x).sin();
                samples[frame * channels + 2] = 0.61 * (1.31 * x).cos();
            }
            assert!(
                samples.chunks_exact(channels).map(|frame| frame[0].abs()).fold(0.0, f64::max)
                    > 0.9,
                "packed-prefix fixture lost its hot channel",
            );
            assert!(
                samples.chunks_exact(channels).map(|frame| frame[1].abs()).fold(0.0, f64::max)
                    < 1.0e-10,
                "packed-prefix fixture lost its very quiet partner",
            );

            let mut engine = QualifiedHalfDelayFft::new(reconstruction, channels);
            let mut observed = Vec::<(i128, Vec<f64>, Vec<f64>)>::new();
            for (frame_index, frame) in samples.chunks_exact(channels).enumerate() {
                engine.process_frame(frame, frame_index as i128, |base, _, _, half, half_error| {
                    observed.push((base + 1, half.to_vec(), half_error.to_vec()));
                });
            }
            assert!(engine.flush(|base, _, _, half, half_error| {
                observed.push((base + 1, half.to_vec(), half_error.to_vec()));
            }), "fixture must exercise the final partial qualified-prefix block");
            assert_eq!(observed.len(), frames);

            let mut sampled = 0usize;
            for (ordinal, (coarse_index, actual, error)) in observed.iter().enumerate() {
                // Check every point near both block boundaries, and a regular
                // interior sample elsewhere. This independently exercises the
                // overlap history and final partial block without turning the
                // ordinary unit suite into an exhaustive 1536-tap benchmark.
                let near_boundary = ordinal < 32
                    || ordinal + 32 >= observed.len()
                    || ordinal.abs_diff(spec.block_frames()) < 32;
                if !near_boundary && ordinal % 97 != 0 {
                    continue;
                }
                sampled += 1;
                assert_eq!(error[0].to_bits(), error[1].to_bits(),
                    "a packed hot/quiet pair must share the same block enclosure");
                for channel in 0..channels {
                    let source_center = (coarse_index - 1).div_euclid(2) + spec.group_delay_inputs();
                    let mut direct = 0.0_f64;
                    for tap in 0..spec.first_taps {
                        let source = source_center - tap as i128;
                        let sample = if source >= 0 && source < frames as i128 {
                            samples[source as usize * channels + channel]
                        } else {
                            0.0
                        };
                        direct += sample * full_first_coefficient(spec, tap);
                    }
                    let difference = (actual[channel] - direct).abs();
                    assert!(
                        difference <= error[channel],
                        "{reconstruction:?} coarse={coarse_index} channel={channel} difference={difference:.17e} error={:.17e}",
                        error[channel],
                    );
                }
            }
            assert!(sampled > 100, "qualified-prefix fixture sampled too few points");
        }
    }

    #[test]
    fn enclosed_prefix_perturbation_remains_safe_certificate_authority() {
        let reconstruction = ReconstructionId::Hq1024V1;
        let spec = ReconstructionSpec::for_id(reconstruction);
        let edge = EdgePolicy::RepeatEndpoints;
        let channels = 2usize;
        let frames = 67usize;
        let mut samples = vec![0.0_f64; frames * channels];
        for frame in 0..frames {
            let x = frame as f64;
            samples[frame * channels] = 0.83 * (1.19 * x).sin() + 0.09 * (0.31 * x).cos();
            samples[frame * channels + 1] = 1.0e-11 * (0.73 * x).cos();
        }

        let packed_peak = samples.iter().copied().map(f64::abs).fold(0.0_f64, f64::max);
        let injected_error = crate::qualified_prefix_coefficients::
            HQ1024_QUALIFIED_PREFIX_ERROR_PER_PACKED_COMPONENT_PEAK_UPPER
            * packed_peak
            + crate::qualified_prefix_coefficients::HQ1024_QUALIFIED_PREFIX_ABSOLUTE_ERROR_UPPER;
        assert!(injected_error.is_finite() && injected_error > 0.0);

        let tail = spec.tail();
        let coarse_start = 0_i128;
        let coarse_end = (frames as i128 - 1) * 2;
        let support_start = coarse_start + i128::from(tail.offset_min);
        let support_end = coarse_end - 1 + i128::from(tail.offset_max);
        let (raw_start, raw_end) = spec.raw_support_for_coarse_range(support_start, support_end);
        let mut scanner = CertifiedScanner::new(spec, SearchPolicy::Fast90, channels);

        for source in raw_start..=raw_end {
            let frame = (0..channels)
                .map(|channel| extended_sample(&samples, channels, source, channel, edge))
                .collect::<Vec<_>>();
            scanner.observe_raw_frame(source, &frame);
        }
        for frame_index in 0..frames {
            let frame = &samples[frame_index * channels..(frame_index + 1) * channels];
            scanner.observe_nominal_frame(frame_index as i128, frame);
        }

        let mut perturbed_half_phases = 0usize;
        for coarse_index in support_start..=support_end {
            let mut values = vec![0.0_f64; channels];
            let mut errors = vec![0.0_f64; channels];
            for channel in 0..channels {
                if coarse_index.rem_euclid(2) == 0 {
                    values[channel] = extended_sample(
                        &samples,
                        channels,
                        coarse_index.div_euclid(2),
                        channel,
                        edge,
                    );
                } else {
                    let source_center =
                        (coarse_index - 1).div_euclid(2) + spec.group_delay_inputs();
                    let mut direct = 0.0_f64;
                    for tap in 0..spec.first_taps {
                        direct += extended_sample(
                            &samples,
                            channels,
                            source_center - tap as i128,
                            channel,
                            edge,
                        ) * full_first_coefficient(spec, tap);
                    }
                    let error = injected_error;
                    let direction = if (coarse_index + channel as i128).rem_euclid(4) < 2 {
                        1.0
                    } else {
                        -1.0
                    };
                    values[channel] = direct + direction * 0.25 * error;
                    errors[channel] = error;
                    perturbed_half_phases += 1;
                }
            }
            scanner.observe_coarse_authority(coarse_index, &values, &errors);
        }
        assert!(
            perturbed_half_phases > frames * channels,
            "fixture must inject enclosed perturbations across the coarse support",
        );

        let input_peaks = (0..channels)
            .map(|channel| {
                samples
                    .chunks_exact(channels)
                    .map(|frame| frame[channel].abs())
                    .fold(0.0_f64, f64::max)
            })
            .collect::<Vec<_>>();
        let certificate = scanner.finalize(frames as u64, &input_peaks).unwrap();
        let exact = exhaustive_peak(reconstruction, &samples, channels, edge);
        assert!(certificate.finite_interval.lower_linear <= exact);
        assert!(certificate.finite_interval.upper_linear >= exact);
        assert!(certificate.diagnostics.max_evaluation_error_linear > 0.0);
        assert_eq!(certificate.diagnostics.strict_coarse_evaluations, 0);
    }

    #[test]
    fn fast90_acceptance_class_material_uses_mandatory_coarse_authority() {
        let reconstruction = ReconstructionId::Hq1024V1;
        let frames = TILE_INPUT_FRAMES as usize + 911;
        let channels = 2usize;

        let make_fixture = |shape: u8| {
            let mut samples = vec![0.0_f64; frames * channels];
            let mut state = 0x9e37_79b9_u64;
            for frame in 0..frames {
                let x = frame as f64;
                let hot = match shape {
                    0 => 0.93 * (2.0 * std::f64::consts::PI * 0.49 * x).sin(),
                    1 => {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        let signed = ((state >> 11) as f64 / ((1_u64 << 53) as f64)) * 2.0 - 1.0;
                        let carrier = if frame & 1 == 0 { 1.0 } else { -1.0 };
                        0.72 * signed + 0.18 * carrier
                    }
                    _ => {
                        let background = 0.47 * (2.0 * std::f64::consts::PI * 0.41 * x).sin();
                        let local = if (1900..2240).contains(&frame) {
                            0.45 * (2.0 * std::f64::consts::PI * 0.487 * x).sin()
                        } else {
                            0.0
                        };
                        background + local
                    }
                };
                samples[frame * channels] = hot;
                samples[frame * channels + 1] = 1.0e-12 * hot;
            }
            samples
        };

        for shape in 0..3u8 {
            let samples = make_fixture(shape);
            let hot_min = samples
                .chunks_exact(channels)
                .take(HQ1024_FIRST_HALF_DELAY_TAPS)
                .map(|frame| frame[0])
                .fold(f64::INFINITY, f64::min);
            let hot_max = samples
                .chunks_exact(channels)
                .take(HQ1024_FIRST_HALF_DELAY_TAPS)
                .map(|frame| frame[0])
                .fold(f64::NEG_INFINITY, f64::max);
            assert!(
                hot_min < 0.0 && hot_max > 0.0 && hot_max - hot_min > 0.90,
                "acceptance fixture must retain a wide zero-centered first-stage support range",
            );
            let hot_peak = samples.chunks_exact(channels).map(|frame| frame[0].abs()).fold(0.0, f64::max);
            let quiet_peak = samples.chunks_exact(channels).map(|frame| frame[1].abs()).fold(0.0, f64::max);
            assert!(hot_peak > 0.7 && quiet_peak > 0.0 && quiet_peak < hot_peak * 1.0e-10);

            for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
                let certificate = scan_with_chunks(
                    reconstruction,
                    SearchPolicy::Fast90,
                    &samples,
                    channels,
                    edge,
                    37,
                );
                assert!(certificate.diagnostics.authoritative_coarse_values >= 8_193 * channels as u64,
                    "fixture must process at least one complete canonical tile through the qualified 2x authority layer");
                assert!(certificate.diagnostics.authoritative_coarse_groups > 0);
                assert!(certificate.diagnostics.accelerated_l1_groups_tested > 0,
                    "Fast90 fixture must exercise the cheap L1 root authority screen");
                assert!(certificate.diagnostics.accelerated_l1_groups_rejected > 0,
                    "the deliberately quiet packed partner must be rejected by the L1 root screen");
                assert!(certificate.diagnostics.accelerated_curvature_roots
                    < certificate.diagnostics.accelerated_l1_groups_tested,
                    "Fast90 must avoid constructing curvature summaries for at least the quiet-channel roots");
                assert_eq!(certificate.diagnostics.strict_coarse_evaluations, 0,
                    "normal Fast90 must not reconstruct the long first-stage FIR candidate by candidate");
                assert!(certificate.diagnostics.work_credits_consumed
                    < certificate.diagnostics.authoritative_coarse_values
                        .saturating_mul(HQ1024_FIRST_HALF_DELAY_TAPS as u64),
                    "discretionary work must remain smaller than reconstructing the whole prefix directly");
                if shape == 2 {
                    assert!(
                        certificate.diagnostics.groups_rejected > 0,
                        "localized-peak fixture must demonstrate certified 64/8/1 rejection",
                    );
                }

                if shape == 0 && edge == EdgePolicy::RepeatEndpoints {
                    let alternate = scan_with_chunks(
                        reconstruction,
                        SearchPolicy::Fast90,
                        &samples,
                        channels,
                        edge,
                        1024,
                    );
                    assert_eq!(certificate, alternate,
                        "caller push size must not alter the certified result or deterministic diagnostics");
                }
            }
        }

        // A separate mono carrier exercises the unpaired complex lane over a
        // complete tile; the stereo fixtures above cover hot+quiet packing.
        let stereo = make_fixture(0);
        let mono = stereo
            .chunks_exact(channels)
            .map(|frame| frame[0])
            .collect::<Vec<_>>();
        let mono_certificate = scan_with_chunks(
            reconstruction,
            SearchPolicy::Fast90,
            &mono,
            1,
            EdgePolicy::ZeroExtend,
            113,
        );
        assert!(
            mono_certificate.diagnostics.authoritative_coarse_values >= 8_193,
            "mono fixture must process a complete tile through qualified 2x authority",
        );
        assert_eq!(
            mono_certificate.diagnostics.strict_coarse_evaluations,
            0,
            "normal mono Fast90 must not fall back to direct first-stage reconstruction",
        );
    }

    #[test]
    fn shipping_fft_stress_shapes_remain_certified_by_the_qualified_prefix() {
        let cases = [
            (2usize, 41usize, 0u8), // hot + silence, final partial FFT block
            (2usize, 43usize, 1u8), // hot + very quiet
            (2usize, 47usize, 2u8), // opposite-sign / cancellation-heavy
            (3usize, 53usize, 3u8), // odd channel count
            (2usize, 59usize, 4u8), // subnormal-scale secondary channel
            (2usize, 61usize, 5u8), // large but well below overflow boundary
        ];
        for (channels, frames, shape) in cases {
            let mut samples = vec![0.0_f64; channels * frames];
            for frame in 0..frames {
                let x = frame as f64;
                for channel in 0..channels {
                    samples[frame * channels + channel] = match shape {
                        0 => {
                            if channel == 0 {
                                0.97 * (1.41 * x).sin()
                            } else {
                                0.0
                            }
                        }
                        1 => {
                            if channel == 0 {
                                0.93 * (1.17 * x).sin()
                            } else {
                                1.0e-13 * (0.71 * x).cos()
                            }
                        }
                        2 => {
                            if channel == 0 {
                                0.8 * (2.79 * x).sin()
                            } else {
                                -0.8 * (2.79 * x).sin() + 1.0e-12 * (0.2 * x).cos()
                            }
                        }
                        3 => {
                            (0.81 - channel as f64 * 0.11)
                                * ((0.61 + channel as f64 * 0.37) * x).sin()
                        }
                        4 => {
                            if channel == 0 {
                                0.7 * (0.9 * x).sin()
                            } else {
                                f64::from_bits(1 + (frame as u64 & 7))
                            }
                        }
                        _ => (f64::MAX / 4.0)
                            * (0.70 + 0.03 * channel as f64)
                            * ((0.43 + channel as f64 * 0.19) * x).sin(),
                    };
                }
            }
            let channel_peak = |channel: usize| {
                samples
                    .chunks_exact(channels)
                    .map(|frame| frame[channel].abs())
                    .fold(0.0_f64, f64::max)
            };
            match shape {
                0 => {
                    assert!(channel_peak(0) > 0.9, "hot/silence fixture lost its hot channel");
                    assert_eq!(
                        channel_peak(1),
                        0.0,
                        "hot/silence fixture lost its exactly silent packed partner",
                    );
                }
                1 => {
                    let hot = channel_peak(0);
                    let quiet = channel_peak(1);
                    assert!(hot > 0.8 && quiet > 0.0 && quiet < hot * 1.0e-10);
                }
                2 => {
                    let cancellation_peak = samples
                        .chunks_exact(channels)
                        .map(|frame| (frame[0] + frame[1]).abs())
                        .fold(0.0_f64, f64::max);
                    assert!(channel_peak(0) > 0.7);
                    assert!(
                        cancellation_peak < 2.0e-12,
                        "cancellation fixture no longer reaches the intended near-cancellation state",
                    );
                }
                3 => assert_eq!(channels, 3, "odd-channel fixture must remain odd"),
                4 => {
                    let quiet = channel_peak(1);
                    assert!(
                        quiet > 0.0 && quiet < f64::MIN_POSITIVE,
                        "subnormal fixture must contain a positive subnormal packed partner",
                    );
                }
                _ => {
                    let peak = channel_peak(0).max(channel_peak(1));
                    assert!(peak.is_finite() && peak > f64::MAX / 8.0);
                }
            }

            for reconstruction in [
                ReconstructionId::LegacyHeadroom64,
                ReconstructionId::Hq1024V1,
            ] {
                let spec = ReconstructionSpec::for_id(reconstruction);
                assert!(
                    frames < spec.block_frames(),
                    "fixture must force the final partial FFT block through flush",
                );
                for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
                    let exact = exhaustive_peak(reconstruction, &samples, channels, edge);
                    let certificate = scan_with_chunks(
                        reconstruction,
                        SearchPolicy::Reference9,
                        &samples,
                        channels,
                        edge,
                        17,
                    );
                    assert!(certificate.finite_interval.lower_linear <= exact);
                    assert!(certificate.finite_interval.upper_linear >= exact);
                }
            }
        }
    }

    #[test]
    fn directed_bound_primitives_cover_subnormal_uncertainty_in_release_semantics() {
        let max_subnormal = f64::from_bits(f64::MIN_POSITIVE.to_bits() - 1);
        let magnitude = f64::MIN_POSITIVE * 2.0;
        let lower = lower_sub(magnitude, max_subnormal);
        // Promoting the uncertainty to MIN_NORMAL makes this no larger than
        // the exact-real subtraction; the extra next-down keeps the direction
        // correct even when the hardware subtraction rounds upward.
        assert!(lower <= f64::MIN_POSITIVE);
        assert_eq!(lower_sub(f64::from_bits(7), 0.0), f64::from_bits(7));
        assert!(upper_add(-1.0, 1.0).is_infinite());
        assert!(upper_mul(f64::NAN, 1.0).is_infinite());
    }

    #[test]
    fn subnormal_only_input_keeps_a_finite_authoritative_interval() {
        let samples = [
            f64::from_bits(1),
            -f64::from_bits(7),
            f64::from_bits(3),
            -f64::from_bits(11),
            f64::from_bits(5),
        ];
        for reconstruction in [ReconstructionId::LegacyHeadroom64, ReconstructionId::Hq1024V1] {
            for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
                let exact = exhaustive_peak(reconstruction, &samples, 1, edge);
                let certificate = scan_with_chunks(
                    reconstruction,
                    SearchPolicy::Reference9,
                    &samples,
                    1,
                    edge,
                    2,
                );
                assert!(certificate.finite_interval.lower_linear <= exact);
                assert!(certificate.finite_interval.upper_linear >= exact);
                assert!(certificate.finite_interval.upper_linear.is_finite());
                assert!(certificate.finite_interval.upper_linear > 0.0);
            }
        }
    }

    #[test]
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[allow(deprecated)]
    fn daz_ftz_subnormal_input_cannot_become_exact_silence() {
        std::thread::spawn(|| {
            const MXCSR_DAZ: u32 = 1 << 6;
            const MXCSR_FTZ: u32 = 1 << 15;
            const MXCSR_DAZ_FTZ: u32 = MXCSR_DAZ | MXCSR_FTZ;

            // SAFETY: MXCSR is thread-local architectural state. This test owns
            // the spawned thread and restores the exact incoming value through
            // the guard before the thread exits, including during unwinding.
            let original = unsafe { read_mxcsr() };
            let _restore = MxcsrRestore(original);
            unsafe { write_mxcsr(original | MXCSR_DAZ_FTZ) };
            assert_eq!(unsafe { read_mxcsr() } & MXCSR_DAZ_FTZ, MXCSR_DAZ_FTZ);

            let samples = [
                f64::from_bits(1),
                -f64::from_bits(7),
                f64::from_bits(3),
                -f64::from_bits(11),
            ];
            let fixture_peak_bits = samples
                .iter()
                .copied()
                .map(magnitude_bits)
                .max()
                .unwrap();
            assert_eq!(fixture_peak_bits, 11);
            assert!(samples.iter().copied().all(|value| {
                let bits = magnitude_bits(value);
                bits != 0 && bits < F64_MIN_NORMAL_BITS
            }));

            // Bound classification itself must not ask DAZ-sensitive floating
            // comparisons whether a subnormal is exact zero.
            assert_eq!(magnitude_bits(upper_add(0.0, 0.0)), 0);
            assert!(magnitude_bits(upper_add(f64::from_bits(1), 0.0)) >= F64_MIN_NORMAL_BITS);
            assert!(magnitude_bits(upper_mul(f64::from_bits(7), 1.0)) >= F64_MIN_NORMAL_BITS);
            assert_eq!(magnitude_bits(lower_sub(f64::from_bits(11), 0.0)), 11);
            assert_ne!(magnitude_bits(f64::from_bits(1)), 0);
            assert!(upper_add(-1.0, 1.0).is_infinite());
            assert!(upper_mul(f64::NAN, 1.0).is_infinite());

            // The public logarithmic views must preserve mathematical
            // non-silence too; fixing only the scanner's point field would
            // leave `PeakInterval::upper_level` vulnerable to the same DAZ
            // collapse through libm.
            let subnormal_interval = PeakInterval::new(f64::from_bits(7), f64::from_bits(11));
            assert!(!subnormal_interval.is_silence());
            assert!(subnormal_interval.width_db().unwrap().is_finite());
            match subnormal_interval.upper_level() {
                PeakLevel::Finite { dbtp, .. } => assert!(dbtp.is_finite()),
                PeakLevel::Silence => panic!("positive subnormal interval upper became silence"),
            }
            let silence_interval = PeakInterval::new(0.0, 0.0);
            assert!(silence_interval.is_silence());
            assert!(silence_interval.width_db().is_none());
            assert_eq!(silence_interval.upper_level(), PeakLevel::Silence);

            let mut certificates = Vec::new();
            for reconstruction in [
                ReconstructionId::LegacyHeadroom64,
                ReconstructionId::Hq1024V1,
            ] {
                // Exercise the qualified prefix directly. The all-subnormal
                // block must not take its exact-silence shortcut: its half-phase
                // error enclosure is required to remain nonzero.
                let mut prefix = QualifiedHalfDelayFft::new(reconstruction, 1);
                let mut saw_nonzero_half_error = false;
                for (index, sample) in samples.iter().copied().enumerate() {
                    prefix.process_frame(&[sample], index as i128, |_, _, _, _, half_error| {
                        saw_nonzero_half_error |= magnitude_bits(half_error[0]) != 0;
                    });
                }
                prefix.flush(|_, _, _, _, half_error| {
                    saw_nonzero_half_error |= magnitude_bits(half_error[0]) != 0;
                });
                assert!(
                    saw_nonzero_half_error,
                    "{reconstruction:?} qualified prefix misclassified a nonzero subnormal block as silence",
                );

                for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
                    let certificate = scan_with_chunks(
                        reconstruction,
                        SearchPolicy::Reference9,
                        &samples,
                        1,
                        edge,
                        2,
                    );
                    assert!(
                        magnitude_bits(certificate.finite_interval.upper_linear) != 0,
                        "{reconstruction:?} {edge:?} lost its authoritative upper",
                    );
                    assert!(
                        magnitude_bits(certificate.finite_interval.lower_linear) >= fixture_peak_bits,
                        "{reconstruction:?} {edge:?} lost the exact identity-sample lower",
                    );
                    assert!(
                        magnitude_bits(certificate.numerical_envelope_linear) != 0,
                        "{reconstruction:?} {edge:?} lost the required arithmetic enclosure",
                    );
                    assert!(
                        magnitude_bits(certificate.reported_point_estimate.channel_linear_peaks[0])
                            >= fixture_peak_bits,
                        "{reconstruction:?} {edge:?} lost exact sample-peak bookkeeping",
                    );
                    match certificate.reported_point_estimate.overall {
                        PeakLevel::Finite { dbtp, .. } => assert!(
                            dbtp.is_finite(),
                            "{reconstruction:?} {edge:?} produced non-finite dBTP for finite subnormal input: {dbtp:?}",
                        ),
                        PeakLevel::Silence => panic!(
                            "{reconstruction:?} {edge:?} reported mathematical non-silence as exact silence",
                        ),
                    }
                    certificates.push((reconstruction, edge, certificate));
                }
            }

            // Compute the existing exhaustive finite-reconstruction oracle with
            // gradual underflow restored, then compare it with the certificates
            // produced while DAZ+FTZ were active.
            unsafe { write_mxcsr(original & !MXCSR_DAZ_FTZ) };
            for (reconstruction, edge, certificate) in certificates {
                let exact = exhaustive_peak(reconstruction, &samples, 1, edge);
                assert!(certificate.finite_interval.lower_linear <= exact);
                assert!(certificate.finite_interval.upper_linear >= exact);
            }
        })
        .join()
        .expect("DAZ/FTZ subnormal authority test thread panicked");
    }

    #[test]
    fn hq_reference_is_cross_checked_against_legacy_direct_oracle_on_analytical_intersample_carrier() {
        // Exact reviewer_three_tone construction from the retained Legacy64
        // qualification. For every real time t, each cosine is <= 1, so the
        // continuous waveform is <= 3/6 = 0.5 and reaches that bound exactly
        // at aligned_time = 2000.5. The stored samples never land on that
        // half-sample maximum. All three frequencies are inside the qualified
        // <=0.495 Fs domain.
        let frames = 4096usize;
        let aligned_time = 2000.5_f64;
        let frequencies = [0.30_f64, 0.35_f64, 0.40_f64];
        let analytical_peak = 0.5_f64;
        let samples = (0..frames)
            .map(|frame| {
                let relative = frame as f64 - aligned_time;
                frequencies
                    .iter()
                    .copied()
                    .map(|frequency| {
                        (2.0 * std::f64::consts::PI * frequency * relative).cos()
                    })
                    .sum::<f64>()
                    / 6.0
            })
            .collect::<Vec<_>>();
        let sample_peak = samples.iter().copied().map(f64::abs).fold(0.0, f64::max);
        assert!(
            sample_peak < analytical_peak * 0.92,
            "fixture lost its nontrivial half-sample maximum: sample={sample_peak:.17e} analytical={analytical_peak:.17e}",
        );

        // RepeatEndpoints matches the qualification meter's finite-input
        // convention for this exact analytical carrier.
        let legacy_raw = legacy_direct_oracle_peak(&samples, 1, EdgePolicy::RepeatEndpoints);
        let legacy_point = legacy_raw * HEADROOM64_INTERPOLATION_CALIBRATION_LINEAR;
        let hq = scan_with_chunks(
            ReconstructionId::Hq1024V1,
            SearchPolicy::Reference9,
            &samples,
            1,
            EdgePolicy::RepeatEndpoints,
            137,
        );
        assert_eq!(hq.status, SearchStatus::Complete);
        let hq_point = match hq.reported_point_estimate.overall {
            PeakLevel::Finite { linear, .. } => linear,
            PeakLevel::Silence => panic!("analytical carrier cannot report silence"),
        };

        // design_headroom64_filter.py declares 0.050 dB absolute point error
        // for qualified <=0.495 Fs analytical material, and this exact
        // reviewer_three_tone vector is one of its required cases. Reuse that
        // established envelope; do not invent a tighter cross-oracle epsilon.
        // HQ's separate response qualification is orders tighter, so requiring
        // HQ merely to remain inside the Legacy analytical envelope is a
        // conservative detector for gross/self-consistent coefficient errors.
        const LEGACY_QUALIFIED_ABS_ERROR_DB: f64 = 0.050;
        let legacy_error_db =
            positive_finite_linear_to_dbtp(legacy_point / analytical_peak).abs();
        let hq_error_db = positive_finite_linear_to_dbtp(hq_point / analytical_peak).abs();
        let cross_error_db =
            (positive_finite_linear_to_dbtp(hq_point) - positive_finite_linear_to_dbtp(legacy_point))
                .abs();
        assert!(
            legacy_error_db <= LEGACY_QUALIFIED_ABS_ERROR_DB,
            "Legacy64 direct oracle left its qualified analytical envelope: {legacy_error_db:.9} dB",
        );
        assert!(
            hq_error_db <= LEGACY_QUALIFIED_ABS_ERROR_DB,
            "HQ1024V1 disagrees with independently known analytical peak by {hq_error_db:.9} dB",
        );
        assert!(
            cross_error_db <= 2.0 * LEGACY_QUALIFIED_ABS_ERROR_DB,
            "independent Legacy64 and HQ1024V1 points differ by {cross_error_db:.9} dB",
        );
    }

    #[test]
    fn reference9_contains_independent_exhaustive_short_reconstructions() {
        let frames = 65usize;
        let channels = 2usize;
        let mut samples = Vec::with_capacity(frames * channels);
        for frame in 0..frames {
            let x = frame as f64;
            samples.push(1.07 * (0.37 * x).sin() + 0.19 * (1.13 * x).cos());
            samples.push(if frame == 0 {
                -0.92
            } else if frame + 1 == frames {
                0.83
            } else {
                0.23 * (0.91 * x).sin()
            });
        }

        for reconstruction in [ReconstructionId::LegacyHeadroom64, ReconstructionId::Hq1024V1] {
            for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
                let exact = exhaustive_peak(reconstruction, &samples, channels, edge);
                let mut meter = CertifiedPeakMeterImpl::new(
                    192_000,
                    channels,
                    edge,
                    reconstruction,
                    SearchPolicy::Reference9,
                )
                .unwrap();
                for chunk in samples.chunks(channels * 7) {
                    meter.push_interleaved(chunk).unwrap();
                }
                let certificate = meter.finalize().unwrap();
                assert_eq!(certificate.status, SearchStatus::Complete);
                assert!(
                    certificate.finite_interval.lower_linear <= exact,
                    "reconstruction={reconstruction:?} edge={edge:?} lower={} exact={exact}",
                    certificate.finite_interval.lower_linear,
                );
                assert!(
                    certificate.finite_interval.upper_linear >= exact,
                    "reconstruction={reconstruction:?} edge={edge:?} upper={} exact={exact}",
                    certificate.finite_interval.upper_linear,
                );
            }
        }
    }

    #[test]
    fn reference9_certificate_is_chunk_invariant_on_non_degenerate_audio() {
        // Dense execution and strict rescoring have focused state-forcing tests
        // below/above. Do not couple chunk invariance to an accidental audio
        // fixture that can turn a pruning regression into a multi-minute test.
        let frames = 41usize;
        let samples = (0..frames)
            .map(|frame| {
                let x = frame as f64;
                0.73 * (0.413 * x).sin() + 0.21 * (1.177 * x).cos()
            })
            .collect::<Vec<_>>();
        let one = scan_with_chunks(
            ReconstructionId::Hq1024V1,
            SearchPolicy::Reference9,
            &samples,
            1,
            EdgePolicy::RepeatEndpoints,
            1,
        );
        let uneven = scan_with_chunks(
            ReconstructionId::Hq1024V1,
            SearchPolicy::Reference9,
            &samples,
            1,
            EdgePolicy::RepeatEndpoints,
            13,
        );
        assert_eq!(one, uneven, "caller push size must not affect Reference9");
        assert_eq!(one.status, SearchStatus::Complete);
    }

    #[test]
    fn reference9_constant_carrier_preserves_certificate_across_former_1_vs_37_boundary() {
        // Minimized structurally from the known 193-frame constant-0.75
        // regression: 38 frames is the smallest carrier that still exercises a
        // 37-frame caller push followed by a remainder while preserving the
        // tie-heavy signal that exposed non-canonical candidate/frontier state.
        // Keep full certificate equality, including deterministic diagnostics.
        let samples = vec![0.75_f64; 38];
        for reconstruction in [ReconstructionId::Hq1024V1, ReconstructionId::LegacyHeadroom64] {
            for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
                let whole = scan_with_chunks(
                    reconstruction,
                    SearchPolicy::Reference9,
                    &samples,
                    1,
                    edge,
                    samples.len(),
                );
                let one = scan_with_chunks(
                    reconstruction,
                    SearchPolicy::Reference9,
                    &samples,
                    1,
                    edge,
                    1,
                );
                let thirty_seven = scan_with_chunks(
                    reconstruction,
                    SearchPolicy::Reference9,
                    &samples,
                    1,
                    edge,
                    37,
                );
                let irregular = scan_with_chunk_pattern(
                    reconstruction,
                    SearchPolicy::Reference9,
                    &samples,
                    1,
                    edge,
                    &[5, 1, 17, 3, 12],
                );
                assert_eq!(whole, one, "{reconstruction:?} {edge:?}: whole vs 1-frame");
                assert_eq!(whole, thirty_seven, "{reconstruction:?} {edge:?}: whole vs 37-frame");
                assert_eq!(whole, irregular, "{reconstruction:?} {edge:?}: whole vs irregular");
                assert_eq!(whole.status, SearchStatus::Complete);
            }
        }
    }

    #[test]
    fn endpoint_winners_use_real_track_edges_during_rescore() {
        let samples = [1.0_f64, -0.93, 0.21, -0.17, 0.91, -0.99];
        for reconstruction in [ReconstructionId::LegacyHeadroom64, ReconstructionId::Hq1024V1] {
            for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
                let spec = ReconstructionSpec::for_id(reconstruction);
                let mut scanner = CertifiedScanner::new(spec, SearchPolicy::Reference9, 1);
                let frames = samples.len() as i128;
                for frame in -spec.input_halo_frames..frames + spec.input_halo_frames {
                    scanner.observe_raw_frame(
                        frame,
                        &[extended_sample(&samples, 1, frame, 0, edge)],
                    );
                }

                let tail = spec.tail();
                let locations = [
                    KnotLocation {
                        cell: 0,
                        phase: tail.factor / 2,
                    },
                    KnotLocation {
                        cell: (frames - 1) * 2 - 1,
                        phase: tail.factor / 2,
                    },
                ];
                let mut expected_upper = 0.0_f64;
                for location in locations {
                    let oracle = direct_target_value(
                        reconstruction,
                        &samples,
                        1,
                        edge,
                        location,
                        0,
                    );
                    let accurate = scanner.accurate_target_evaluation(location, 0);
                    assert!(
                        accurate.lower() <= oracle.abs() && accurate.upper() >= oracle.abs(),
                        "{reconstruction:?} {edge:?} endpoint rescore enclosure missed its independent direct value",
                    );
                    expected_upper = expected_upper.max(accurate.upper());

                    // Force only the frontier state. The deliberately loose
                    // ordinary enclosure keeps both edge candidates live even
                    // after the first accurate rescore raises the lower bound.
                    scanner.tile_frontiers[0].retained.push(TrackedEvaluation {
                        location,
                        evaluation: Evaluation {
                            value: 0.0,
                            error: 4.0,
                        },
                    });
                }

                scanner.settle_tile_frontiers();
                assert_eq!(scanner.diagnostics.direct_rescore_evaluations, 2);
                assert!(scanner.settled_evaluated_upper_channel_peaks[0] >= expected_upper);
            }
        }
    }

    #[test]
    fn rescore_frontier_equal_upper_retention_is_canonical() {
        fn run(order: impl IntoIterator<Item = usize>) -> (Vec<(i128, usize)>, Vec<(i128, usize)>) {
            let mut frontier = TileEvaluationFrontier::default();
            let mut immediate = Vec::new();
            for index in order {
                if let Some(tracked) = frontier.observe(
                    KnotLocation {
                        cell: index as i128,
                        phase: 1,
                    },
                    Evaluation {
                        value: 1.0,
                        error: 1.0e-9,
                    },
                    0.5,
                ) {
                    immediate.push((tracked.location.cell, tracked.location.phase));
                }
            }
            let mut retained = frontier
                .retained
                .iter()
                .map(|tracked| (tracked.location.cell, tracked.location.phase))
                .collect::<Vec<_>>();
            retained.sort_unstable();
            immediate.sort_unstable();
            (retained, immediate)
        }

        let total = RESCORE_FRONTIER_CAPACITY + 17;
        let ascending = run(0..total);
        let descending = run((0..total).rev());
        assert_eq!(
            ascending, descending,
            "equal-upper frontier state must depend on absolute knot keys, not arrival order",
        );
        assert_eq!(
            ascending.0,
            (0..RESCORE_FRONTIER_CAPACITY)
                .map(|index| (index as i128, 1usize))
                .collect::<Vec<_>>(),
            "canonical frontier retains the earliest absolute knots on an exact tie",
        );
    }

    #[test]
    fn candidate_node_order_is_total_for_equal_upper_distinct_work() {
        let dense_a = CandidateNode {
            cell: 10,
            channel: 0,
            upper: 1.0,
            screening_priority: 0.75,
            work: CandidateWork::DenseRegion { end_cell: 18 },
        };
        let dense_b = CandidateNode {
            work: CandidateWork::DenseRegion { end_cell: 19 },
            ..dense_a
        };
        assert_ne!(dense_a, dense_b);
        assert_ne!(dense_a.cmp(&dense_b), Ordering::Equal);

        let evaluation = Evaluation {
            value: 0.75,
            error: 1.0e-12,
        };
        let dyadic_a = CandidateNode {
            cell: 10,
            channel: 0,
            upper: 1.0,
            screening_priority: 0.75,
            work: CandidateWork::Dyadic {
                phase_start: 0,
                phase_end: 32,
                tree_index: 1,
                left: evaluation,
                right: evaluation,
                d2_upper: 0.1,
                magnitude_upper: 0.75,
            },
        };
        let dyadic_b = CandidateNode {
            cell: 10,
            channel: 0,
            upper: 1.0,
            screening_priority: 0.75,
            work: CandidateWork::Dyadic {
                phase_start: 0,
                phase_end: 32,
                tree_index: 2,
                left: evaluation,
                right: evaluation,
                d2_upper: 0.1,
                magnitude_upper: 0.75,
            },
        };
        assert_ne!(dyadic_a, dyadic_b);
        assert_ne!(dyadic_a.cmp(&dyadic_b), Ordering::Equal);
    }

    #[test]
    fn rescore_frontier_returns_every_live_competitor_it_cannot_retain() {
        let mut frontier = TileEvaluationFrontier::default();
        let total = RESCORE_FRONTIER_CAPACITY + 17;
        let mut immediate = Vec::new();
        for index in 0..total {
            let tracked = frontier.observe(
                KnotLocation {
                    cell: index as i128,
                    phase: 1,
                },
                Evaluation {
                    value: 1.0 + index as f64 * 1.0e-12,
                    error: 1.0e-9,
                },
                0.5,
            );
            if let Some(tracked) = tracked {
                immediate.push(tracked);
            }
        }
        assert_eq!(frontier.retained.len(), RESCORE_FRONTIER_CAPACITY);
        assert_eq!(
            frontier.retained.len() + immediate.len(),
            total,
            "a full frontier must return, not discard, every still-live competitor",
        );
        assert!(
            frontier
                .retained
                .iter()
                .chain(immediate.iter())
                .all(|tracked| tracked.evaluation.upper() > 0.5),
        );
    }

    #[test]
    fn reference9_live_unresolved_upper_fails_closed_in_release_semantics() {
        let mut meter = CertifiedPeakMeterImpl::new(
            192_000,
            1,
            EdgePolicy::RepeatEndpoints,
            ReconstructionId::Hq1024V1,
            SearchPolicy::Reference9,
        )
        .unwrap();
        meter.push_interleaved(&[0.25, -0.5, 0.125]).unwrap();
        meter.scanner.unresolved_upper_channel_peaks[0] = 2.0;
        assert_eq!(meter.finalize(), Err(TruePeakError::ReferenceSearchIncomplete));
    }

    #[test]
    fn zero_fast90_discretionary_credit_retains_the_unresolved_upper() {
        let frames = 257usize;
        let mut samples = Vec::with_capacity(frames);
        for frame in 0..frames {
            let x = frame as f64;
            samples.push(0.76 * (2.73 * x).sin() + 0.17 * (1.91 * x).cos());
        }
        let mut meter = CertifiedPeakMeterImpl::new(
            192_000,
            1,
            EdgePolicy::RepeatEndpoints,
            ReconstructionId::Hq1024V1,
            SearchPolicy::Fast90,
        )
        .unwrap();
        meter.scanner.fast_credits_per_tile_channel = 0;
        meter.push_interleaved(&samples).unwrap();
        let certificate = meter.finalize().unwrap();
        assert_eq!(certificate.status, SearchStatus::WorkLimited);
        assert!(certificate.diagnostics.work_limited_tiles > 0);
        assert!(
            certificate.diagnostics.unresolved_upper_linear
                > certificate.finite_interval.lower_linear,
        );
        assert_eq!(certificate.diagnostics.phase_evaluations, 0);
        assert_eq!(
            certificate.diagnostics.direct_rescore_evaluations,
            0,
            "Fast90 must not spend unbudgeted strict-rescore work",
        );
    }

    #[test]
    fn fast_tile_fallback_is_local_to_the_skipped_tile_support() {
        let spec = ReconstructionSpec::for_id(ReconstructionId::Hq1024V1);
        let mut scanner = CertifiedScanner::new_at_rate(
            spec,
            SearchPolicy::RetiredClockFast1s,
            1,
            192_000,
        );
        let coarse_start = 512_i128;
        let coarse_end = 640_i128;
        let tail = spec.tail();
        let required_coarse_start = coarse_start + i128::from(tail.offset_min);
        let required_coarse_end = coarse_end - 1 + i128::from(tail.offset_max);
        let (raw_start, raw_end) =
            spec.raw_support_for_coarse_range(required_coarse_start, required_coarse_end);

        for input_index in (raw_start - 3)..=(raw_end + 3) {
            let sample = if input_index < raw_start || input_index > raw_end {
                0.9
            } else {
                0.2
            };
            scanner.raw.push(input_index, &[sample]);
        }
        scanner.retain_fast_tile_fallback(coarse_start, coarse_end);

        let local = upper_mul(0.2, HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER);
        let whole_buffer = upper_mul(0.9, HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER);
        assert_eq!(scanner.unresolved_upper_channel_peaks[0].to_bits(), local.to_bits());
        assert!(scanner.unresolved_upper_channel_peaks[0] < whole_buffer);
    }

    #[test]
    fn fast_partial_candidate_deadline_preserves_completed_channel_root() {
        let spec = ReconstructionSpec::for_id(ReconstructionId::Hq1024V1);
        let mut scanner = CertifiedScanner::new_at_rate(
            spec,
            SearchPolicy::RetiredClockFast1s,
            2,
            192_000,
        );
        let coarse_start = 512_i128;
        let coarse_end = 640_i128;
        let (raw_start, raw_end) = scanner.fast_tile_raw_support(coarse_start, coarse_end);
        for input_index in raw_start..=raw_end {
            scanner.raw.push(input_index, &[0.9, 0.2]);
        }

        let completed_root = 1.2_f64;
        scanner.mark_fast_partial_candidate_tile_time_limited(
            coarse_start,
            coarse_end,
            &[completed_root, 0.0],
            1,
        );

        assert_eq!(scanner.unresolved_upper_channel_peaks[0].to_bits(), completed_root.to_bits());
        assert!(
            scanner.unresolved_upper_channel_peaks[0]
                < upper_mul(0.9, HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER),
            "completed candidate work must not be widened back to the raw fallback",
        );
        assert_eq!(
            scanner.unresolved_upper_channel_peaks[1].to_bits(),
            upper_mul(0.2, HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER).to_bits(),
        );
        assert_eq!(scanner.diagnostics.time_limited_tiles, 1);
    }

    #[test]
    fn fast_internal_allowance_scales_with_programme_frames_plus_bounded_startup_burst() {
        let spec = ReconstructionSpec::for_id(ReconstructionId::Hq1024V1);
        let mut scanner = CertifiedScanner::new_at_rate(
            spec,
            SearchPolicy::RetiredClockFast1s,
            1,
            192_000,
        );
        scanner.nominal_frames_seen = 192_000 * 60;
        assert_eq!(scanner.fast_allowed_processing_nanos(), 950_000_000);
        scanner.nominal_frames_seen = 192_000 * 120;
        assert_eq!(scanner.fast_allowed_processing_nanos(), 1_850_000_000);
    }

    #[test]
    fn exhausted_fast_clock_retains_unresolved_upper_and_reports_time_limited() {
        let frames = 257usize;
        let mut samples = Vec::with_capacity(frames);
        for frame in 0..frames {
            let x = frame as f64;
            samples.push(0.76 * (2.73 * x).sin() + 0.17 * (1.91 * x).cos());
        }
        let mut meter = CertifiedPeakMeterImpl::new(
            192_000,
            1,
            EdgePolicy::RepeatEndpoints,
            ReconstructionId::Hq1024V1,
            SearchPolicy::RetiredClockFast1s,
        )
        .unwrap();
        meter.push_interleaved(&samples).unwrap();

        // Deterministically model a caller whose mandatory meter processing
        // has already consumed more than the source-duration allowance. This
        // avoids a scheduler-sensitive sleep in the regression itself.
        meter.scanner.processing_time_spent = Duration::from_secs(1);
        let certificate = meter.finalize().unwrap();
        assert_eq!(certificate.status, SearchStatus::TimeLimited);
        assert!(certificate.diagnostics.time_bounded_prefix_blocks_skipped > 0);
        assert!(certificate.diagnostics.time_limited_tiles > 0);
        assert!(
            certificate.diagnostics.unresolved_upper_linear
                > certificate.finite_interval.lower_linear,
        );
        assert_eq!(certificate.diagnostics.direct_rescore_evaluations, 0);
        let sample_peak = samples.iter().copied().map(f64::abs).fold(0.0_f64, f64::max);
        assert!(
            certificate.finite_interval.upper_linear
                >= upper_mul(sample_peak, HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER),
            "skipping Fast tile work must attach the tile-local HQ L-infinity fallback",
        );
    }

}
