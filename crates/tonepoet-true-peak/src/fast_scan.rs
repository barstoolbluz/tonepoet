//! Deterministic bounded-work Fast066V2 HQ1024V1 scanner.
//!
//! Fast066 deliberately has a different execution graph from the shared
//! Reference9 / Standard certified-search engine. It always executes the qualified 2x prefix,
//! surveys every finite interval on the frozen HQ1024 quarter-sample knots,
//! spends bounded optional HQ1024 work on measured proposals and finishers, and
//! covers every target knot with a flat algebraic curvature envelope. No wall
//! clock affects which signal locations are evaluated.

use std::cmp::Ordering;
use std::collections::VecDeque;
use std::sync::OnceLock;

#[cfg(feature = "fast-stage-timing")]
use crate::fast_stage_timing::StageTimer;

use crate::certified_scan::{
    dot_rounding_upper, exact_magnitude, lower_sub, magnitude_bits,
    max_exact_magnitude, max_nonnegative_finite, upper_add, upper_mul,
};
use crate::hq1024_coefficients::{
    HQ1024_NODE_A_UPPER, HQ1024_NODE_B_UPPER, HQ1024_TAIL_COEFFICIENTS,
    HQ1024_TAIL_FACTOR, HQ1024_TAIL_OFFSET_COUNT,
    HQ1024_TAIL_OFFSET_MAX, HQ1024_TAIL_OFFSET_MIN,
};
use crate::qualified_half_delay_fft::{QualifiedHalfDelayBlock, QualifiedHalfDelayFft};
use crate::{
    CertifiedReconstruction, EdgePolicy, PeakCertificate, PeakInterval, PeakLevel, PeakTier,
    ReconstructionId, SearchDiagnostics, SearchStatus, TruePeakError, TruePeakResult,
    HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER,
};

const INPUT_HALO_FRAMES: i128 = 777;
const TILE_FRAMES: i128 = 4096;
const GROUP_FRAMES: i128 = 256;
const MICROGROUPS_PER_TILE: usize = (TILE_FRAMES / GROUP_FRAMES) as usize;
const MAX_NOMINEES_PER_TILE_CHANNEL: usize = 64;
const MAX_FINISHERS_PER_TILE_CHANNEL: usize = 8;
const CANDIDATE_DOMAIN_RADIUS_Q: i128 = 256;
const FINISH_PROBE_STEP_Q: i128 = 32;
const MAX_FINE_EVALUATIONS_PER_TILE_CHANNEL: u64 = 104;
const TAIL_FACTOR_I128: i128 = HQ1024_TAIL_FACTOR as i128;
const TARGET_FACTOR_I128: i128 = 1024;
const MIDPOINT_PHASE: usize = HQ1024_TAIL_FACTOR / 2;
const MIDPOINT_OFFSET_MIN: i32 = -11;
const MIDPOINT_OFFSET_MAX: i32 = 12;
const MIDPOINT_SYMMETRIC_TERMS: usize = 12;
// Conservative operation counts used only by outward numerical envelopes.
// The implemented midpoint pair graph has at most 36 rounded arithmetic
// operations on any scalar dependency path/accounting model used here; the
// ordinary 34-tap tail has at most 68 multiply/add operations, and the second
// difference has at most four arithmetic operations before reduction. These
// deliberately larger counts leave headroom for the scalar/SIMD reductions
// without relying on an optimizer-specific contraction.
const MIDPOINT_ROUNDING_OPS: usize = 48;
const TAIL_ROUNDING_OPS: usize = 96;
const SECOND_DIFFERENCE_ROUNDING_OPS: usize = 8;
// Exact coarse halo required by the V2 possible-work domain. For a nonfirst
// tile, a boundary nominee may evaluate just left of the tile in coarse cell
// 2*a-1; the frozen tail then reaches offset -16, hence 17 knots left of 2*a.
// The last nonfinal owned nominee can approach the right boundary from coarse
// cell 2*b-1; tail offset +17 reaches index 2*b+16. The first finite tile clips
// q at zero and therefore needs only the ordinary group support at -16.
const COARSE_KEEP_LEFT: i128 = 17;
const COARSE_READY_RIGHT: i128 = 16;
const COMPACT_HEAD_KNOTS: usize = 1 << 18;

// Exact powers of two. Scaling is used only for exceptional very-large finite
// inputs where an ordinary short dot product could overflow intermediately.
const HUGE_SCALE: f64 = f64::from_bits(511_u64 << 52); // 2^-512
const HUGE_INVERSE_SCALE: f64 = f64::from_bits(1535_u64 << 52); // 2^512
const ORDINARY_DOT_MAX_INPUT: f64 = f64::MAX / 64.0;

#[derive(Debug, Clone, Copy)]
struct Evaluation {
    value: f64,
    error: f64,
}

impl Evaluation {
    #[inline]
    fn lower(self) -> f64 {
        if self.value.is_finite() && self.error.is_finite() {
            lower_sub(exact_magnitude(self.value), self.error)
        } else {
            0.0
        }
    }

    #[inline]
    fn upper(self) -> f64 {
        if self.value.is_finite() && self.error.is_finite() {
            upper_add(exact_magnitude(self.value), self.error)
        } else {
            f64::INFINITY
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    quarter_index: i128,
    score: f64,
    proposal_offset_q: i128,
    fit_valid: bool,
}

#[derive(Debug, Clone, Copy)]
struct Proposal {
    nominee_quarter_index: i128,
    q: i128,
    evaluation: Evaluation,
    neighborhood_upper: f64,
}

#[derive(Debug, Clone, Copy)]
struct FlatSpanMetrics {
    upper: f64,
    p4_hat: f64,
    p4_error: f64,
    input_max: f64,
    coarse_error: f64,
}

#[derive(Debug, Clone, Copy)]
struct FineEvaluationContext {
    common_input_max: f64,
    cached_error: Option<f64>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct RejectedNomineeAudit {
    channel: usize,
    candidate: Candidate,
    start: i128,
    end: i128,
    final_q: i128,
    neighborhood_upper: f64,
    lower_witness: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FastExecutionMode {
    Production,
    SurveyBoundsOnly,
    NominationOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FineStage {
    Proposal,
    Finishing,
}

#[derive(Debug, Clone)]
struct PrefixErrorRun {
    first_coarse_index: i128,
    last_coarse_index: i128,
}

#[derive(Debug, Clone)]
struct CoarseStore {
    first_index: Option<i128>,
    head: usize,
    values: Vec<Vec<f64>>,
    errors: VecDeque<PrefixErrorRun>,
    error_values: VecDeque<f64>,
}

impl CoarseStore {
    fn new(channels: usize) -> Self {
        Self {
            first_index: None,
            head: 0,
            values: (0..channels).map(|_| Vec::new()).collect(),
            errors: VecDeque::new(),
            error_values: VecDeque::new(),
        }
    }

    #[inline]
    fn channels(&self) -> usize {
        self.values.len()
    }

    #[inline]
    fn first_available(&self) -> Option<i128> {
        self.first_index
    }

    #[inline]
    fn last_available(&self) -> Option<i128> {
        self.first_index.map(|first| {
            let live = self.values[0].len() - self.head;
            first + live as i128 - 1
        })
    }

    fn push_block(&mut self, block: QualifiedHalfDelayBlock<'_>) {
        debug_assert_eq!(block.channels, self.channels());
        debug_assert_eq!(block.integer.len(), block.frames * block.channels);
        debug_assert_eq!(block.half.len(), block.frames * block.channels);
        debug_assert_eq!(block.integer_error_by_channel.len(), block.channels);
        debug_assert_eq!(block.half_error_by_channel.len(), block.channels);
        debug_assert_eq!(block.first_coarse_index.rem_euclid(2), 0);
        debug_assert!(block
            .integer_error_by_channel
            .iter()
            .all(|error| magnitude_bits(*error) == 0));
        if block.frames == 0 {
            return;
        }

        if let Some(last) = self.last_available() {
            debug_assert_eq!(block.first_coarse_index, last + 1);
        } else {
            self.first_index = Some(block.first_coarse_index);
        }

        for frame in 0..block.frames {
            let base = frame * block.channels;
            for channel in 0..block.channels {
                self.values[channel].push(block.integer[base + channel]);
                self.values[channel].push(block.half[base + channel]);
            }
        }

        let last = block.first_coarse_index + (block.frames as i128) * 2 - 1;
        if block
            .half_error_by_channel
            .iter()
            .any(|error| magnitude_bits(*error) != 0)
        {
            self.errors.push_back(PrefixErrorRun {
                first_coarse_index: block.first_coarse_index,
                last_coarse_index: last,
            });
            self.error_values
                .extend(block.half_error_by_channel.iter().copied());
        }
    }

    #[inline]
    fn offset(&self, index: i128) -> usize {
        let first = self.first_index.expect("coarse store is not empty");
        debug_assert!(index >= first);
        let offset = usize::try_from(index - first).expect("coarse offset fits usize");
        let position = self.head + offset;
        debug_assert!(position < self.values[0].len());
        position
    }

    #[inline]
    fn get(&self, channel: usize, index: i128) -> f64 {
        self.values[channel][self.offset(index)]
    }

    fn slice(&self, channel: usize, first: i128, last: i128) -> &[f64] {
        debug_assert!(first <= last);
        let begin = self.offset(first);
        let end = self.offset(last) + 1;
        &self.values[channel][begin..end]
    }

    fn max_magnitude(&self, channel: usize, first: i128, last: i128) -> f64 {
        debug_assert!(first <= last);
        self.slice(channel, first, last)
            .iter()
            .copied()
            .fold(0.0, max_exact_magnitude)
    }

    fn max_error(&self, channel: usize, first: i128, last: i128) -> f64 {
        debug_assert!(first <= last);
        let mut maximum = 0.0;
        for (run_index, run) in self.errors.iter().enumerate() {
            if run.last_coarse_index < first {
                continue;
            }
            if run.first_coarse_index > last {
                break;
            }
            // Integer coarse knots are exact. Every run starts at an even
            // integer knot and contains odd half knots, so any nonempty overlap
            // that contains an odd index needs the run's half-phase envelope.
            let overlap_first = first.max(run.first_coarse_index);
            let overlap_last = last.min(run.last_coarse_index);
            let has_odd = overlap_first < overlap_last || overlap_first.rem_euclid(2) == 1;
            if has_odd {
                maximum = max_nonnegative_finite(
                    maximum,
                    self.error_values[run_index * self.channels() + channel],
                );
            }
        }
        maximum
    }

    fn discard_before(&mut self, keep_index: i128) {
        let Some(first) = self.first_index else {
            return;
        };
        if keep_index <= first {
            return;
        }
        let Some(last) = self.last_available() else {
            return;
        };
        let new_first = keep_index.min(last + 1);
        let drop = usize::try_from(new_first - first).expect("coarse discard fits usize");
        self.head += drop;
        self.first_index = (new_first <= last).then_some(new_first);

        while self
            .errors
            .front()
            .is_some_and(|run| run.last_coarse_index < new_first)
        {
            self.errors.pop_front();
            for _ in 0..self.channels() {
                let popped = self.error_values.pop_front();
                debug_assert!(popped.is_some());
            }
        }

        if self.first_index.is_none() {
            for channel in &mut self.values {
                channel.clear();
            }
            self.head = 0;
            self.errors.clear();
            self.error_values.clear();
            return;
        }

        if self.head >= COMPACT_HEAD_KNOTS && self.head * 2 >= self.values[0].len() {
            for channel in &mut self.values {
                channel.drain(..self.head);
            }
            self.head = 0;
        }
    }
}

#[derive(Debug, Clone)]
struct FastMetadata {
    midpoint_coefficients: [f64; MIDPOINT_SYMMETRIC_TERMS],
    midpoint_l1_upper: f64,
    phase_l1_upper: [f64; HQ1024_TAIL_FACTOR],
    tail_l1_upper: f64,
    a4_upper: f64,
    b4_upper: f64,
}

fn fast_metadata() -> &'static FastMetadata {
    static METADATA: OnceLock<FastMetadata> = OnceLock::new();
    METADATA.get_or_init(|| {
        let row = &HQ1024_TAIL_COEFFICIENTS[MIDPOINT_PHASE];
        let mut midpoint_coefficients = [0.0; MIDPOINT_SYMMETRIC_TERMS];
        let mut midpoint_nonzero = 0usize;
        for (index, coefficient) in row.iter().copied().enumerate() {
            if magnitude_bits(coefficient) != 0 {
                midpoint_nonzero += 1;
            }
            let offset = HQ1024_TAIL_OFFSET_MIN + index as i32;
            if (MIDPOINT_OFFSET_MIN..=MIDPOINT_OFFSET_MIN + 11).contains(&offset) {
                midpoint_coefficients[(offset - MIDPOINT_OFFSET_MIN) as usize] = coefficient;
            }
        }
        assert_eq!(midpoint_nonzero, 24, "HQ1024 midpoint row geometry changed");
        for j in 0..MIDPOINT_SYMMETRIC_TERMS {
            let left_offset = MIDPOINT_OFFSET_MIN + j as i32;
            let right_offset = MIDPOINT_OFFSET_MAX - j as i32;
            let left = row[(left_offset - HQ1024_TAIL_OFFSET_MIN) as usize];
            let right = row[(right_offset - HQ1024_TAIL_OFFSET_MIN) as usize];
            assert_eq!(
                left.to_bits(),
                right.to_bits(),
                "HQ1024 midpoint row lost exact symmetry"
            );
        }

        let mut phase_l1_upper = [0.0; HQ1024_TAIL_FACTOR];
        for phase in 0..HQ1024_TAIL_FACTOR {
            let mut l1 = 0.0;
            for coefficient in HQ1024_TAIL_COEFFICIENTS[phase] {
                l1 = upper_add(l1, exact_magnitude(coefficient));
            }
            phase_l1_upper[phase] = l1;
        }

        let tail_l1_upper = phase_l1_upper
            .iter()
            .copied()
            .fold(0.0, max_nonnegative_finite);

        FastMetadata {
            midpoint_coefficients,
            midpoint_l1_upper: phase_l1_upper[MIDPOINT_PHASE],
            phase_l1_upper,
            tail_l1_upper,
            a4_upper: max_nonnegative_finite(HQ1024_NODE_A_UPPER[2], HQ1024_NODE_A_UPPER[3]),
            b4_upper: max_nonnegative_finite(HQ1024_NODE_B_UPPER[2], HQ1024_NODE_B_UPPER[3]),
        }
    })
}

#[inline]
fn candidate_priority(left: Candidate, right: Candidate) -> Ordering {
    // Descending score, then ascending absolute knot index. Scores are always
    // finite nonnegative magnitudes; total_cmp keeps the order total anyway.
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| left.quarter_index.cmp(&right.quarter_index))
}

#[inline]
fn signed_parabolic_delta(left_value: f64, center_value: f64, right_value: f64) -> Option<f64> {
    let center = exact_magnitude(center_value);
    if magnitude_bits(center) == 0 {
        return None;
    }
    let sign = if center_value.is_sign_negative() { -1.0 } else { 1.0 };
    let left = sign * left_value;
    let right = sign * right_value;
    if left < 0.0 || right < 0.0 {
        return None;
    }
    let curvature = left - 2.0 * center + right;
    if !curvature.is_finite() || curvature >= 0.0 {
        return None;
    }
    let delta = 0.5 * (left - right) / curvature;
    (delta.is_finite() && delta.abs() <= 0.5).then_some(delta)
}

#[inline]
fn rounded_local_offset(delta: f64, scale_q: i128) -> i128 {
    debug_assert!(delta.is_finite() && delta.abs() <= 0.5);
    // Keep the absolute HQ coordinate integer-valued. Only this bounded local
    // offset enters binary64, so very long streams cannot lose coordinate bits.
    // Half ties intentionally round toward +infinity.
    (delta * scale_q as f64 + 0.5).floor() as i128
}

#[inline]
fn signed_parabolic_candidate(values: &[f64], index: usize) -> (f64, i128, bool) {
    let center = exact_magnitude(values[index]);
    if index == 0 || index + 1 == values.len() {
        return (center, 0, false);
    }
    let Some(delta) = signed_parabolic_delta(values[index - 1], values[index], values[index + 1]) else {
        return (center, 0, false);
    };
    let sign = if values[index].is_sign_negative() { -1.0 } else { 1.0 };
    let left = sign * values[index - 1];
    let right = sign * values[index + 1];
    let score = center - 0.25 * (left - right) * delta;
    let score = if score.is_finite() && score >= center { score } else { center };
    (score, rounded_local_offset(delta, 256), true)
}

fn scale_back(value: f64, inverse_scale: f64) -> Result<f64, TruePeakError> {
    let value = value * inverse_scale;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(TruePeakError::NumericalOverflow)
    }
}

fn tail_dot_scalar(
    input: &[f64],
    row: &[f64; HQ1024_TAIL_OFFSET_COUNT],
    scale: f64,
    inverse_scale: f64,
) -> Result<f64, TruePeakError> {
    debug_assert_eq!(input.len(), HQ1024_TAIL_OFFSET_COUNT);
    let mut sum = 0.0;
    for index in 0..HQ1024_TAIL_OFFSET_COUNT {
        sum += (input[index] * scale) * row[index];
    }
    scale_back(sum, inverse_scale)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
unsafe fn tail_dot_avx(input: &[f64], row: &[f64; HQ1024_TAIL_OFFSET_COUNT]) -> f64 {
    use core::arch::x86_64::{
        __m256d, _mm256_add_pd, _mm256_loadu_pd, _mm256_mul_pd, _mm256_storeu_pd,
    };
    debug_assert_eq!(input.len(), HQ1024_TAIL_OFFSET_COUNT);
    let mut acc: __m256d = _mm256_mul_pd(_mm256_loadu_pd(input.as_ptr()), _mm256_loadu_pd(row.as_ptr()));
    let mut index = 4;
    while index + 4 <= 32 {
        let products = _mm256_mul_pd(
            _mm256_loadu_pd(input.as_ptr().add(index)),
            _mm256_loadu_pd(row.as_ptr().add(index)),
        );
        acc = _mm256_add_pd(acc, products);
        index += 4;
    }
    let mut lanes = [0.0_f64; 4];
    _mm256_storeu_pd(lanes.as_mut_ptr(), acc);
    let mut sum = ((lanes[0] + lanes[1]) + lanes[2]) + lanes[3];
    sum += input[32] * row[32];
    sum += input[33] * row[33];
    sum
}

#[inline]
fn tail_dot(
    input: &[f64],
    phase: usize,
    input_max: f64,
    use_avx: bool,
) -> Result<f64, TruePeakError> {
    let row = &HQ1024_TAIL_COEFFICIENTS[phase];
    if input_max <= ORDINARY_DOT_MAX_INPUT {
        #[cfg(target_arch = "x86_64")]
        if use_avx {
            // SAFETY: FastState records the qualified prefix's once-per-meter
            // AVX dispatch result. The helper uses unaligned loads only within
            // the supplied 34-element slices.
            let value = unsafe { tail_dot_avx(input, row) };
            return value.is_finite().then_some(value).ok_or(TruePeakError::NumericalOverflow);
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = use_avx;
        tail_dot_scalar(input, row, 1.0, 1.0)
    } else {
        tail_dot_scalar(input, row, HUGE_SCALE, HUGE_INVERSE_SCALE)
    }
}

#[inline]
fn proposal_priority(left: Proposal, right: Proposal) -> Ordering {
    exact_magnitude(right.evaluation.value)
        .total_cmp(&exact_magnitude(left.evaluation.value))
        .then_with(|| left.q.cmp(&right.q))
        .then_with(|| left.nominee_quarter_index.cmp(&right.nominee_quarter_index))
}

#[inline]
fn middle_is_magnitude_winner(left: f64, middle: f64, right: f64) -> bool {
    let left = magnitude_bits(left);
    let middle = magnitude_bits(middle);
    let right = magnitude_bits(right);
    // Equal magnitudes resolve toward the lowest coordinate: the middle must
    // strictly beat the left neighbor but may tie the right neighbor.
    middle > left && middle >= right
}

#[inline]
fn midpoint_scalar(input: &[f64], metadata: &FastMetadata, scale: f64) -> f64 {
    debug_assert!(input.len() >= 24);
    let mut sum = 0.0;
    for j in 0..MIDPOINT_SYMMETRIC_TERMS {
        let pair = input[j] * scale + input[23 - j] * scale;
        sum += pair * metadata.midpoint_coefficients[j];
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
unsafe fn midpoint_batch_avx(
    input: &[f64],
    metadata: &FastMetadata,
    output: &mut [f64],
) {
    use core::arch::x86_64::{
        _mm256_add_pd, _mm256_loadu_pd, _mm256_mul_pd, _mm256_set1_pd, _mm256_setzero_pd,
        _mm256_storeu_pd,
    };
    debug_assert!(input.len() >= output.len() + 23);
    let mut cell = 0usize;
    while cell + 4 <= output.len() {
        let mut acc = _mm256_setzero_pd();
        for j in 0..MIDPOINT_SYMMETRIC_TERMS {
            let left = _mm256_loadu_pd(input.as_ptr().add(cell + j));
            let right = _mm256_loadu_pd(input.as_ptr().add(cell + 23 - j));
            let pair = _mm256_add_pd(left, right);
            acc = _mm256_add_pd(
                acc,
                _mm256_mul_pd(pair, _mm256_set1_pd(metadata.midpoint_coefficients[j])),
            );
        }
        _mm256_storeu_pd(output.as_mut_ptr().add(cell), acc);
        cell += 4;
    }
    while cell < output.len() {
        output[cell] = midpoint_scalar(&input[cell..cell + 24], metadata, 1.0);
        cell += 1;
    }
}

fn midpoint_batch(
    input: &[f64],
    input_max: f64,
    metadata: &FastMetadata,
    use_avx: bool,
    output: &mut [f64],
) -> Result<(), TruePeakError> {
    debug_assert!(input.len() >= output.len() + 23);
    if input_max <= ORDINARY_DOT_MAX_INPUT {
        #[cfg(target_arch = "x86_64")]
        if use_avx {
            // SAFETY: FastState records the qualified prefix's once-per-meter
            // AVX dispatch result and the helper bounds all unaligned loads by
            // the supplied slices.
            unsafe { midpoint_batch_avx(input, metadata, output) };
            if output.iter().all(|value| value.is_finite()) {
                return Ok(());
            }
            return Err(TruePeakError::NumericalOverflow);
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = use_avx;
        for index in 0..output.len() {
            output[index] = midpoint_scalar(&input[index..index + 24], metadata, 1.0);
            if !output[index].is_finite() {
                return Err(TruePeakError::NumericalOverflow);
            }
        }
    } else {
        for index in 0..output.len() {
            let scaled = midpoint_scalar(&input[index..index + 24], metadata, HUGE_SCALE);
            output[index] = scale_back(scaled, HUGE_INVERSE_SCALE)?;
        }
    }
    Ok(())
}

fn second_difference_max(input: &[f64], input_max: f64) -> Result<f64, TruePeakError> {
    if input.len() < 3 {
        return Ok(0.0);
    }
    let (scale, inverse_scale) = if input_max <= ORDINARY_DOT_MAX_INPUT {
        (1.0, 1.0)
    } else {
        (HUGE_SCALE, HUGE_INVERSE_SCALE)
    };
    let mut maximum = 0.0;
    for window in input.windows(3) {
        let value = (window[0] * scale - 2.0 * (window[1] * scale)) + window[2] * scale;
        if !value.is_finite() {
            return Err(TruePeakError::NumericalOverflow);
        }
        maximum = max_exact_magnitude(maximum, value);
    }
    scale_back(maximum, inverse_scale)
}

#[derive(Debug, Clone)]
struct FastState {
    channels: usize,
    metadata: &'static FastMetadata,
    use_avx: bool,
    coarse: CoarseStore,
    nominal_frames_seen: u64,
    next_tile_start: i128,
    channel_sample_peaks: Vec<f64>,
    channel_hq4_peaks: Vec<f64>,
    channel_point_peaks: Vec<f64>,
    channel_lower_peaks: Vec<f64>,
    channel_upper_peaks: Vec<f64>,
    channel_flat_upper_peaks: Vec<f64>,
    max_evaluation_error: f64,
    diagnostics: SearchDiagnostics,
    survey_start_i: i128,
    survey: Vec<f64>,
    midpoint: Vec<f64>,
    candidates: Vec<Candidate>,
    spatial_candidates: [Option<Candidate>; MICROGROUPS_PER_TILE],
    selected: Vec<Candidate>,
    proposals: Vec<Proposal>,
    group_uppers: [f64; MICROGROUPS_PER_TILE],
    group_count: usize,
    left_halo_upper: f64,
    fine_support_input_max: f64,
    fine_support_coarse_error: f64,
    execution_mode: FastExecutionMode,
    #[cfg(test)]
    disable_candidates_for_test: bool,
    #[cfg(test)]
    rejected_nominee_audit: Option<RejectedNomineeAudit>,
}

impl FastState {
    fn new(channels: usize, prefix_avx: bool, execution_mode: FastExecutionMode) -> Self {
        let mut diagnostics = SearchDiagnostics::default();
        diagnostics.accelerated_same_graph_avx_prefix_active = prefix_avx;
        Self {
            channels,
            metadata: fast_metadata(),
            use_avx: prefix_avx,
            coarse: CoarseStore::new(channels),
            nominal_frames_seen: 0,
            next_tile_start: 0,
            channel_sample_peaks: vec![0.0; channels],
            channel_hq4_peaks: vec![0.0; channels],
            channel_point_peaks: vec![0.0; channels],
            channel_lower_peaks: vec![0.0; channels],
            channel_upper_peaks: vec![0.0; channels],
            channel_flat_upper_peaks: vec![0.0; channels],
            max_evaluation_error: 0.0,
            diagnostics,
            survey_start_i: 0,
            survey: Vec::with_capacity((TILE_FRAMES as usize) * 4 + 3),
            midpoint: Vec::with_capacity((TILE_FRAMES as usize) * 2 + 2),
            candidates: Vec::with_capacity((TILE_FRAMES as usize) * 2 + 4),
            spatial_candidates: [None; MICROGROUPS_PER_TILE],
            selected: Vec::with_capacity(MAX_NOMINEES_PER_TILE_CHANNEL),
            proposals: Vec::with_capacity(MAX_NOMINEES_PER_TILE_CHANNEL),
            group_uppers: [0.0; MICROGROUPS_PER_TILE],
            group_count: 0,
            left_halo_upper: 0.0,
            fine_support_input_max: 0.0,
            fine_support_coarse_error: 0.0,
            execution_mode,
            #[cfg(test)]
            disable_candidates_for_test: false,
            #[cfg(test)]
            rejected_nominee_audit: None,
        }
    }

    fn ingest_prefix_block(&mut self, block: QualifiedHalfDelayBlock<'_>) {
        self.coarse.push_block(block);
    }

    fn ready_for_tile(&self, start: i128, end: i128) -> bool {
        let required_left = if start == 0 { -16 } else { 2 * start - COARSE_KEEP_LEFT };
        let required_right = 2 * end + COARSE_READY_RIGHT;
        self.coarse
            .first_available()
            .is_some_and(|first| first <= required_left)
            && self.coarse
                .last_available()
                .is_some_and(|last| last >= required_right)
    }

    fn process_ready_full_tiles(&mut self) -> Result<(), TruePeakError> {
        loop {
            let end = self.next_tile_start + TILE_FRAMES;
            // A tile ending exactly at the current last nominal frame must be
            // deferred. If no later push arrives it is the final tile, whose
            // terminal endpoint is a candidate owner; if more input arrives it
            // becomes an ordinary full tile and is processed then. This is the
            // key to chunk invariance at exact 4096-frame ownership boundaries.
            let final_frame = self.nominal_frames_seen as i128 - 1;
            if end >= final_frame {
                return Ok(());
            }
            if !self.ready_for_tile(self.next_tile_start, end) {
                return Ok(());
            }
            self.process_tile(self.next_tile_start, end, false)?;
            self.next_tile_start = end;
            self.discard_finished_prefix();
        }
    }

    fn process_final_tile(&mut self) -> Result<(), TruePeakError> {
        let final_frame = self.nominal_frames_seen as i128 - 1;
        if self.next_tile_start < final_frame {
            debug_assert!(self.ready_for_tile(self.next_tile_start, final_frame));
            self.process_tile(self.next_tile_start, final_frame, true)?;
            self.next_tile_start = final_frame;
            self.discard_finished_prefix();
        } else if self.next_tile_start == 0 && final_frame == 0 {
            // A one-frame finite target has no interval groups. Its sole HQ
            // target knot is the exact input sample.
            for channel in 0..self.channels {
                self.channel_hq4_peaks[channel] = self.channel_sample_peaks[channel];
                self.channel_point_peaks[channel] = self.channel_sample_peaks[channel];
                self.channel_lower_peaks[channel] = self.channel_sample_peaks[channel];
                self.channel_upper_peaks[channel] = self.channel_sample_peaks[channel];
            }
            self.diagnostics.authoritative_coarse_values = self.channels as u64;
            self.diagnostics.fast_survey_knots = self.channels as u64;
        }
        Ok(())
    }

    fn discard_finished_prefix(&mut self) {
        let keep = 2 * self.next_tile_start - COARSE_KEEP_LEFT;
        self.coarse.discard_before(keep);
    }

    fn survey_index(&self, quarter_index: i128) -> usize {
        usize::try_from(quarter_index - self.survey_start_i).expect("survey index fits usize")
    }

    #[inline]
    fn survey_value(&self, quarter_index: i128) -> f64 {
        self.survey[self.survey_index(quarter_index)]
    }

    fn build_survey(
        &mut self,
        channel: usize,
        start: i128,
        end: i128,
    ) -> Result<(), TruePeakError> {
        let metadata = self.metadata;
        self.survey_start_i = if start == 0 { 0 } else { 4 * start - 1 };
        let survey_end_i = 4 * end;
        let survey_len = usize::try_from(survey_end_i - self.survey_start_i + 1)
            .expect("bounded survey length");
        self.survey.clear();
        self.survey.resize(survey_len, 0.0);

        let first_odd = if self.survey_start_i.rem_euclid(2) == 1 {
            self.survey_start_i
        } else {
            self.survey_start_i + 1
        };
        let last_odd = if survey_end_i.rem_euclid(2) == 1 {
            survey_end_i
        } else {
            survey_end_i - 1
        };
        if first_odd <= last_odd {
            let first_cell = first_odd.div_euclid(2);
            let last_cell = last_odd.div_euclid(2);
            let count = usize::try_from(last_cell - first_cell + 1).expect("midpoint count");
            self.midpoint.clear();
            self.midpoint.resize(count, 0.0);
            let input_first = first_cell + i128::from(MIDPOINT_OFFSET_MIN);
            let input_last = last_cell + i128::from(MIDPOINT_OFFSET_MAX);
            let input = self.coarse.slice(channel, input_first, input_last);
            let input_max = input.iter().copied().fold(0.0, max_exact_magnitude);
            midpoint_batch(input, input_max, metadata, self.use_avx, &mut self.midpoint)?;
        } else {
            self.midpoint.clear();
        }

        for offset in 0..survey_len {
            let quarter_index = self.survey_start_i + offset as i128;
            let value = if quarter_index.rem_euclid(2) == 0 {
                self.coarse.get(channel, quarter_index.div_euclid(2))
            } else {
                let cell = quarter_index.div_euclid(2);
                let first_cell = first_odd.div_euclid(2);
                self.midpoint[usize::try_from(cell - first_cell).expect("midpoint index")]
            };
            self.survey[offset] = value;
        }

        // A non-initial tile retains one left neighbor for local-maximum
        // discovery and recomputes the shared boundary knot. Neither is a new
        // point in the finite HQ 4x survey domain. Counting only unique survey
        // knots makes this invariant independent of tile count:
        // channels * (4 * (frames - 1) + 1).
        let repeated = if start > 0 { 2 } else { 0 };
        let new_survey_knots = (survey_len as u64).saturating_sub(repeated);
        self.diagnostics.authoritative_coarse_values = self
            .diagnostics
            .authoritative_coarse_values
            .saturating_add(new_survey_knots);
        self.diagnostics.fast_survey_knots = self
            .diagnostics
            .fast_survey_knots
            .saturating_add(new_survey_knots);
        Ok(())
    }

    fn midpoint_error_upper_from_bounds(
        &self,
        coarse_error: f64,
        coarse_magnitude_upper: f64,
    ) -> f64 {
        let metadata = self.metadata;
        let propagated = upper_mul(metadata.midpoint_l1_upper, coarse_error);
        let absolute_sum = upper_mul(metadata.midpoint_l1_upper, coarse_magnitude_upper);
        upper_add(
            propagated,
            dot_rounding_upper(MIDPOINT_ROUNDING_OPS, absolute_sum),
        )
    }

    fn midpoint_error_upper(
        &self,
        channel: usize,
        first: i128,
        last: i128,
        coarse_magnitude_upper: f64,
    ) -> f64 {
        let coarse_error = self.coarse.max_error(channel, first, last);
        self.midpoint_error_upper_from_bounds(coarse_error, coarse_magnitude_upper)
    }

    fn update_evaluation(&mut self, channel: usize, evaluation: Evaluation) {
        self.max_evaluation_error =
            max_nonnegative_finite(self.max_evaluation_error, evaluation.error);
        self.channel_point_peaks[channel] =
            max_exact_magnitude(self.channel_point_peaks[channel], evaluation.value);
        self.channel_lower_peaks[channel] =
            max_nonnegative_finite(self.channel_lower_peaks[channel], evaluation.lower());
        self.channel_upper_peaks[channel] =
            max_nonnegative_finite(self.channel_upper_peaks[channel], evaluation.upper());
    }

    fn flat_span_metrics(
        &self,
        channel: usize,
        support_first: i128,
        support_last: i128,
        survey_first_i: i128,
        survey_last_i: i128,
    ) -> Result<FlatSpanMetrics, TruePeakError> {
        debug_assert!(support_first <= support_last);
        debug_assert!(survey_first_i <= survey_last_i);
        let metadata = self.metadata;
        let support = self.coarse.slice(channel, support_first, support_last);
        let input_max = support.iter().copied().fold(0.0, max_exact_magnitude);
        let coarse_error = self.coarse.max_error(channel, support_first, support_last);
        let input_upper = upper_add(input_max, coarse_error);

        let d_hat = second_difference_max(support, input_max)?;
        let diff_abs_sum = upper_mul(4.0, input_max);
        let diff_rounding = dot_rounding_upper(SECOND_DIFFERENCE_ROUNDING_OPS, diff_abs_sum);
        let d_upper = upper_add(
            upper_add(d_hat, upper_mul(4.0, coarse_error)),
            diff_rounding,
        );

        let mut p4_hat = 0.0;
        for quarter_index in survey_first_i..=survey_last_i {
            p4_hat = max_exact_magnitude(p4_hat, self.survey_value(quarter_index));
        }
        let midpoint_error = self.midpoint_error_upper_from_bounds(coarse_error, input_max);
        let p4_error = max_nonnegative_finite(coarse_error, midpoint_error);
        let p4_upper = upper_add(p4_hat, p4_error);
        let upper = upper_add(
            p4_upper,
            upper_add(
                upper_mul(metadata.a4_upper, d_upper),
                upper_mul(metadata.b4_upper, input_upper),
            ),
        );
        if !upper.is_finite() {
            return Err(TruePeakError::NumericalOverflow);
        }
        Ok(FlatSpanMetrics {
            upper,
            p4_hat,
            p4_error,
            input_max,
            coarse_error,
        })
    }

    fn cover_groups(
        &mut self,
        channel: usize,
        start: i128,
        end: i128,
    ) -> Result<(), TruePeakError> {
        self.group_count = 0;
        self.group_uppers.fill(0.0);
        self.left_halo_upper = 0.0;
        self.fine_support_input_max = 0.0;
        self.fine_support_coarse_error = 0.0;

        let mut group_start = start;
        while group_start < end {
            let group_end = (group_start + GROUP_FRAMES).min(end);
            let metrics = self.flat_span_metrics(
                channel,
                2 * group_start - 16,
                2 * group_end + 16,
                4 * group_start,
                4 * group_end,
            )?;

            debug_assert!(self.group_count < MICROGROUPS_PER_TILE);
            self.group_uppers[self.group_count] = metrics.upper;
            self.group_count += 1;
            self.fine_support_input_max = max_nonnegative_finite(
                self.fine_support_input_max,
                metrics.input_max,
            );
            self.fine_support_coarse_error = max_nonnegative_finite(
                self.fine_support_coarse_error,
                metrics.coarse_error,
            );

            let p4_lower = lower_sub(metrics.p4_hat, metrics.p4_error);
            self.channel_point_peaks[channel] =
                max_nonnegative_finite(self.channel_point_peaks[channel], metrics.p4_hat);
            self.channel_hq4_peaks[channel] =
                max_nonnegative_finite(self.channel_hq4_peaks[channel], metrics.p4_hat);
            self.channel_lower_peaks[channel] =
                max_nonnegative_finite(self.channel_lower_peaks[channel], p4_lower);
            self.channel_upper_peaks[channel] =
                max_nonnegative_finite(self.channel_upper_peaks[channel], metrics.upper);
            self.channel_flat_upper_peaks[channel] = max_nonnegative_finite(
                self.channel_flat_upper_peaks[channel],
                metrics.upper,
            );
            self.max_evaluation_error =
                max_nonnegative_finite(self.max_evaluation_error, metrics.p4_error);
            self.diagnostics.groups_expanded = self.diagnostics.groups_expanded.saturating_add(1);
            self.diagnostics.authoritative_coarse_groups = self
                .diagnostics
                .authoritative_coarse_groups
                .saturating_add(1);
            self.diagnostics.fast_flat_groups =
                self.diagnostics.fast_flat_groups.saturating_add(1);
            group_start = group_end;
        }

        // A candidate owned by a noninitial tile may refine left of the tile
        // boundary. The ordinary group envelopes begin at `start`, so retain
        // one additional quarter-cell envelope solely for optional-work
        // dominance and for the common fine-tail numerical envelope. It is not
        // part of the returned finite-programme certificate: the same finite
        // interval was already covered by the preceding tile.
        if start > 0 {
            let halo = self.flat_span_metrics(
                channel,
                2 * start - 17,
                2 * start + 16,
                4 * start - 1,
                4 * start,
            )?;
            self.left_halo_upper = halo.upper;
            self.fine_support_input_max = max_nonnegative_finite(
                self.fine_support_input_max,
                halo.input_max,
            );
            self.fine_support_coarse_error = max_nonnegative_finite(
                self.fine_support_coarse_error,
                halo.coarse_error,
            );
        }
        Ok(())
    }

    fn discover_candidates(&mut self, start: i128, end: i128, final_tile: bool) {
        self.candidates.clear();
        self.spatial_candidates.fill(None);
        let first_owned = 4 * start;
        let last_owned_exclusive = 4 * end;
        for quarter_index in first_owned..last_owned_exclusive {
            let index = self.survey_index(quarter_index);
            let magnitude = exact_magnitude(self.survey[index]);
            let local = if quarter_index == 0 {
                magnitude_bits(magnitude) != 0
            } else {
                magnitude_bits(magnitude) > magnitude_bits(self.survey[index - 1])
                    && magnitude_bits(magnitude) >= magnitude_bits(self.survey[index + 1])
            };
            if local {
                let (score, proposal_offset_q, fit_valid) =
                    signed_parabolic_candidate(&self.survey, index);
                let candidate = Candidate {
                    quarter_index,
                    score,
                    proposal_offset_q,
                    fit_valid,
                };
                self.candidates.push(candidate);
                let relative = quarter_index - first_owned;
                let microgroup = usize::try_from(relative / (4 * GROUP_FRAMES))
                    .expect("candidate microgroup fits usize");
                debug_assert!(microgroup < MICROGROUPS_PER_TILE);
                let slot = &mut self.spatial_candidates[microgroup];
                if slot.is_none_or(|current| {
                    candidate_priority(candidate, current) == Ordering::Less
                }) {
                    *slot = Some(candidate);
                }
            }
        }

        if final_tile {
            let quarter_index = 4 * end;
            let index = self.survey_index(quarter_index);
            let magnitude = exact_magnitude(self.survey[index]);
            if magnitude_bits(magnitude) != 0 {
                self.candidates.push(Candidate {
                    quarter_index,
                    score: magnitude,
                    proposal_offset_q: 0,
                    fit_valid: false,
                });
            }
        }

        self.diagnostics.fast_candidates_observed = self
            .diagnostics
            .fast_candidates_observed
            .saturating_add(self.candidates.len() as u64);
    }

    fn select_candidates(&mut self) {
        self.selected.clear();
        if self.candidates.is_empty() {
            return;
        }
        let discovered = self.candidates.len();

        #[cfg(test)]
        if self.disable_candidates_for_test {
            // Test-only proof that the flat certificate is independent of the
            // point-refinement heuristic. Production Fast066 never takes this path.
            return;
        }

        // Discovery already retained the best local maximum from each
        // canonical 256-frame microgroup. Do not rescan the entire candidate
        // population sixteen times here: dense hostile content must remain a
        // linear-time selection workload.
        self.selected
            .extend(self.spatial_candidates.iter().copied().flatten());

        let keep = MAX_NOMINEES_PER_TILE_CHANNEL.min(self.candidates.len());
        if keep < self.candidates.len() {
            let nth = keep - 1;
            self.candidates.select_nth_unstable_by(nth, |left, right| {
                candidate_priority(*left, *right)
            });
            self.candidates.truncate(keep);
        }
        self.candidates
            .sort_unstable_by(|left, right| candidate_priority(*left, *right));

        for candidate in self.candidates.iter().copied() {
            if self.selected.len() >= MAX_NOMINEES_PER_TILE_CHANNEL {
                break;
            }
            if !self
                .selected
                .iter()
                .any(|selected| selected.quarter_index == candidate.quarter_index)
            {
                self.selected.push(candidate);
            }
        }
        self.selected
            .sort_unstable_by(|left, right| candidate_priority(*left, *right));

        self.diagnostics.candidate_cells = self
            .diagnostics
            .candidate_cells
            .saturating_add(self.selected.len() as u64);
        self.diagnostics.fast_candidates_selected = self
            .diagnostics
            .fast_candidates_selected
            .saturating_add(self.selected.len() as u64);
        if discovered > MAX_NOMINEES_PER_TILE_CHANNEL {
            self.diagnostics.dense_regions = self.diagnostics.dense_regions.saturating_add(1);
            self.diagnostics.fast_candidate_saturated_tiles = self
                .diagnostics
                .fast_candidate_saturated_tiles
                .saturating_add(1);
        }
    }

    fn fine_evaluation_context(&self) -> FineEvaluationContext {
        let metadata = self.metadata;
        let propagated = upper_mul(metadata.tail_l1_upper, self.fine_support_coarse_error);
        let absolute_sum = upper_mul(metadata.tail_l1_upper, self.fine_support_input_max);
        let error = upper_add(
            propagated,
            dot_rounding_upper(TAIL_ROUNDING_OPS, absolute_sum),
        );
        FineEvaluationContext {
            common_input_max: self.fine_support_input_max,
            // Exceptional finite amplitudes can make a tile-wide envelope
            // overflow even when an individual short support remains usable.
            // Retain the old bounded per-knot calculation in that case rather
            // than rejecting an input solely because of this optimization.
            cached_error: error.is_finite().then_some(error),
        }
    }

    fn fine_evaluation(
        &self,
        channel: usize,
        q: i128,
        context: FineEvaluationContext,
    ) -> Result<Evaluation, TruePeakError> {
        debug_assert!(q >= 0);
        let cell = q.div_euclid(TAIL_FACTOR_I128);
        let phase = usize::try_from(q.rem_euclid(TAIL_FACTOR_I128)).expect("tail phase");
        if phase == 0 {
            return Ok(Evaluation {
                value: self.coarse.get(channel, cell),
                error: self.coarse.max_error(channel, cell, cell),
            });
        }
        let support_first = cell + i128::from(HQ1024_TAIL_OFFSET_MIN);
        let support_last = cell + i128::from(HQ1024_TAIL_OFFSET_MAX);
        let support = self.coarse.slice(channel, support_first, support_last);

        if let Some(error) = context.cached_error {
            let value = tail_dot(
                support,
                phase,
                context.common_input_max,
                self.use_avx,
            )?;
            return Ok(Evaluation { value, error });
        }

        let input_max = support.iter().copied().fold(0.0, max_exact_magnitude);
        let value = tail_dot(support, phase, input_max, self.use_avx)?;
        let coarse_error = self.coarse.max_error(channel, support_first, support_last);
        let metadata = self.metadata;
        let propagated = upper_mul(metadata.phase_l1_upper[phase], coarse_error);
        let abs_sum = upper_mul(metadata.phase_l1_upper[phase], input_max);
        let error = upper_add(
            propagated,
            dot_rounding_upper(TAIL_ROUNDING_OPS, abs_sum),
        );
        if !error.is_finite() {
            return Err(TruePeakError::NumericalOverflow);
        }
        Ok(Evaluation { value, error })
    }

    fn survey_evaluation(&self, channel: usize, quarter_index: i128) -> Evaluation {
        let value = self.survey_value(quarter_index);
        let q = quarter_index * 256;
        let cell = q.div_euclid(TAIL_FACTOR_I128);
        if q.rem_euclid(TAIL_FACTOR_I128) == 0 {
            Evaluation {
                value,
                error: self.coarse.max_error(channel, cell, cell),
            }
        } else {
            let first = cell + i128::from(MIDPOINT_OFFSET_MIN);
            let last = cell + i128::from(MIDPOINT_OFFSET_MAX);
            let m_hat = self.coarse.max_magnitude(channel, first, last);
            Evaluation {
                value,
                error: self.midpoint_error_upper(channel, first, last, m_hat),
            }
        }
    }

    #[inline]
    fn tile_optional_upper(&self) -> f64 {
        let mut upper = self.left_halo_upper;
        for group_upper in self.group_uppers[..self.group_count].iter().copied() {
            upper = max_nonnegative_finite(upper, group_upper);
        }
        upper
    }

    fn candidate_neighborhood_upper(
        &self,
        candidate: Candidate,
        start: i128,
        end: i128,
        final_q: i128,
    ) -> f64 {
        let center_q = candidate.quarter_index * 256;
        let first_q = (center_q - CANDIDATE_DOMAIN_RADIUS_Q).max(0);
        let last_q = (center_q + CANDIDATE_DOMAIN_RADIUS_Q).min(final_q);
        let tile_start_q = start * TARGET_FACTOR_I128;
        let mut upper = 0.0;

        if start > 0 && first_q < tile_start_q {
            upper = self.left_halo_upper;
        }

        for group_index in 0..self.group_count {
            let group_start = start + group_index as i128 * GROUP_FRAMES;
            let group_end = (group_start + GROUP_FRAMES).min(end);
            let group_first_q = group_start * TARGET_FACTOR_I128;
            let group_last_q = group_end * TARGET_FACTOR_I128;
            if first_q <= group_last_q && last_q >= group_first_q {
                upper = max_nonnegative_finite(upper, self.group_uppers[group_index]);
            }
        }
        debug_assert!(upper.is_finite());
        debug_assert!(magnitude_bits(upper) != 0 || magnitude_bits(self.tile_optional_upper()) == 0);
        upper
    }

    fn evaluate_optional_q(
        &mut self,
        channel: usize,
        q: i128,
        final_q: i128,
        context: FineEvaluationContext,
        stage: FineStage,
        direct_rescore: bool,
    ) -> Result<Evaluation, TruePeakError> {
        debug_assert!(q >= 0 && q <= final_q);
        let evaluation = if q.rem_euclid(256) == 0 {
            self.survey_evaluation(channel, q / 256)
        } else {
            self.diagnostics.phase_evaluations =
                self.diagnostics.phase_evaluations.saturating_add(1);
            self.diagnostics.fast_fine_knots_evaluated = self
                .diagnostics
                .fast_fine_knots_evaluated
                .saturating_add(1);
            match stage {
                FineStage::Proposal => {
                    self.diagnostics.fast_proposal_fine_knots_evaluated = self
                        .diagnostics
                        .fast_proposal_fine_knots_evaluated
                        .saturating_add(1);
                }
                FineStage::Finishing => {
                    self.diagnostics.fast_finishing_fine_knots_evaluated = self
                        .diagnostics
                        .fast_finishing_fine_knots_evaluated
                        .saturating_add(1);
                    if direct_rescore {
                        self.diagnostics.direct_rescore_evaluations = self
                            .diagnostics
                            .direct_rescore_evaluations
                            .saturating_add(1);
                    } else {
                        self.diagnostics.dense_phase_evaluations = self
                            .diagnostics
                            .dense_phase_evaluations
                            .saturating_add(1);
                    }
                }
            }
            self.fine_evaluation(channel, q, context)?
        };
        self.update_evaluation(channel, evaluation);
        Ok(evaluation)
    }

    fn probe_nominees(
        &mut self,
        channel: usize,
        start: i128,
        end: i128,
        final_q: i128,
        context: FineEvaluationContext,
    ) -> Result<(), TruePeakError> {
        self.proposals.clear();
        let selected_len = self.selected.len();
        for index in 0..selected_len {
            let candidate = self.selected[index];
            let neighborhood_upper =
                self.candidate_neighborhood_upper(candidate, start, end, final_q);
            if neighborhood_upper <= self.channel_lower_peaks[channel] {
                #[cfg(test)]
                if self.rejected_nominee_audit.is_none() {
                    self.rejected_nominee_audit = Some(RejectedNomineeAudit {
                        channel,
                        candidate,
                        start,
                        end,
                        final_q,
                        neighborhood_upper,
                        lower_witness: self.channel_lower_peaks[channel],
                    });
                }
                self.diagnostics.fast_bound_pruned_nominees = self
                    .diagnostics
                    .fast_bound_pruned_nominees
                    .saturating_add(1);
                continue;
            }
            if !candidate.fit_valid {
                self.diagnostics.fast_invalid_proposal_fits = self
                    .diagnostics
                    .fast_invalid_proposal_fits
                    .saturating_add(1);
            }
            let center_q = candidate.quarter_index * 256;
            let q = (center_q + candidate.proposal_offset_q).clamp(0, final_q);
            let evaluation = self.evaluate_optional_q(
                channel,
                q,
                final_q,
                context,
                FineStage::Proposal,
                false,
            )?;
            self.diagnostics.fast_proposals_evaluated = self
                .diagnostics
                .fast_proposals_evaluated
                .saturating_add(1);
            self.proposals.push(Proposal {
                nominee_quarter_index: candidate.quarter_index,
                q,
                evaluation,
                neighborhood_upper,
            });
        }
        debug_assert!(self.proposals.len() <= MAX_NOMINEES_PER_TILE_CHANNEL);
        Ok(())
    }

    fn finish_proposals(
        &mut self,
        channel: usize,
        final_q: i128,
        context: FineEvaluationContext,
    ) -> Result<(), TruePeakError> {
        self.proposals
            .sort_unstable_by(|left, right| proposal_priority(*left, *right));
        let finish_count = self.proposals.len().min(MAX_FINISHERS_PER_TILE_CHANNEL);
        for index in 0..finish_count {
            let proposal = self.proposals[index];
            if proposal.neighborhood_upper <= self.channel_lower_peaks[channel] {
                self.diagnostics.fast_bound_pruned_finishers = self
                    .diagnostics
                    .fast_bound_pruned_finishers
                    .saturating_add(1);
                continue;
            }
            self.diagnostics.fast_finishing_candidates = self
                .diagnostics
                .fast_finishing_candidates
                .saturating_add(1);
            self.diagnostics.refined_cells = self.diagnostics.refined_cells.saturating_add(1);

            let left_q = proposal.q - FINISH_PROBE_STEP_Q;
            let right_q = proposal.q + FINISH_PROBE_STEP_Q;
            let left = if left_q >= 0 {
                Some(self.evaluate_optional_q(
                    channel,
                    left_q,
                    final_q,
                    context,
                    FineStage::Finishing,
                    false,
                )?)
            } else {
                None
            };
            let right = if right_q <= final_q {
                Some(self.evaluate_optional_q(
                    channel,
                    right_q,
                    final_q,
                    context,
                    FineStage::Finishing,
                    false,
                )?)
            } else {
                None
            };

            let (Some(left), Some(right)) = (left, right) else {
                self.diagnostics.fast_unbracketed_finishers = self
                    .diagnostics
                    .fast_unbracketed_finishers
                    .saturating_add(1);
                continue;
            };

            if !middle_is_magnitude_winner(
                left.value,
                proposal.evaluation.value,
                right.value,
            ) {
                self.diagnostics.fast_unbracketed_finishers = self
                    .diagnostics
                    .fast_unbracketed_finishers
                    .saturating_add(1);
                continue;
            }

            let Some(delta) = signed_parabolic_delta(
                left.value,
                proposal.evaluation.value,
                right.value,
            ) else {
                self.diagnostics.fast_invalid_finishing_fits = self
                    .diagnostics
                    .fast_invalid_finishing_fits
                    .saturating_add(1);
                continue;
            };
            let q1 = proposal.q + rounded_local_offset(delta, FINISH_PROBE_STEP_Q);
            debug_assert!(q1 >= 0 && q1 <= final_q);

            for trial in q1 - 1..=q1 + 1 {
                if trial < 0 || trial > final_q {
                    continue;
                }
                if trial == proposal.q || trial == left_q || trial == right_q {
                    continue;
                }
                let _ = self.evaluate_optional_q(
                    channel,
                    trial,
                    final_q,
                    context,
                    FineStage::Finishing,
                    true,
                )?;
            }
        }
        Ok(())
    }

    fn process_tile(
        &mut self,
        start: i128,
        end: i128,
        final_tile: bool,
    ) -> Result<(), TruePeakError> {
        debug_assert!(start < end);
        debug_assert!(end - start <= TILE_FRAMES);
        debug_assert!(self.ready_for_tile(start, end));
        let final_q = (self.nominal_frames_seen as i128 - 1) * TARGET_FACTOR_I128;

        for channel in 0..self.channels {
            #[cfg(feature = "fast-stage-timing")]
            let survey_timer = StageTimer::start();
            self.build_survey(channel, start, end)?;
            #[cfg(feature = "fast-stage-timing")]
            survey_timer.finish(&mut self.diagnostics.fast_stage_midpoint_survey_nanos);

            #[cfg(feature = "fast-stage-timing")]
            let envelope_timer = StageTimer::start();
            self.cover_groups(channel, start, end)?;
            let dominated_tile = self.tile_optional_upper() <= self.channel_lower_peaks[channel];
            #[cfg(feature = "fast-stage-timing")]
            envelope_timer.finish(&mut self.diagnostics.fast_stage_flat_envelope_nanos);

            if dominated_tile {
                self.diagnostics.fast_bound_pruned_channel_tiles = self
                    .diagnostics
                    .fast_bound_pruned_channel_tiles
                    .saturating_add(1);
                continue;
            }
            if self.execution_mode == FastExecutionMode::SurveyBoundsOnly {
                continue;
            }

            #[cfg(feature = "fast-stage-timing")]
            let candidate_timer = StageTimer::start();
            #[cfg(feature = "fast-stage-timing")]
            let nomination_timer = StageTimer::start();
            self.discover_candidates(start, end, final_tile);
            self.select_candidates();
            #[cfg(feature = "fast-stage-timing")]
            nomination_timer.finish(&mut self.diagnostics.fast_stage_nomination_nanos);

            if self.execution_mode == FastExecutionMode::NominationOnly {
                #[cfg(feature = "fast-stage-timing")]
                candidate_timer.finish(&mut self.diagnostics.fast_stage_candidate_refinement_nanos);
                continue;
            }

            let context = self.fine_evaluation_context();
            let fine_before = self.diagnostics.fast_fine_knots_evaluated;

            #[cfg(feature = "fast-stage-timing")]
            let proposal_timer = StageTimer::start();
            self.probe_nominees(channel, start, end, final_q, context)?;
            #[cfg(feature = "fast-stage-timing")]
            proposal_timer.finish(&mut self.diagnostics.fast_stage_proposal_nanos);

            #[cfg(feature = "fast-stage-timing")]
            let finishing_timer = StageTimer::start();
            self.finish_proposals(channel, final_q, context)?;
            #[cfg(feature = "fast-stage-timing")]
            finishing_timer.finish(&mut self.diagnostics.fast_stage_finishing_nanos);

            let fine_delta = self
                .diagnostics
                .fast_fine_knots_evaluated
                .saturating_sub(fine_before);
            debug_assert!(fine_delta <= MAX_FINE_EVALUATIONS_PER_TILE_CHANNEL);
            #[cfg(feature = "fast-stage-timing")]
            candidate_timer.finish(&mut self.diagnostics.fast_stage_candidate_refinement_nanos);
        }
        self.diagnostics.tiles_processed = self.diagnostics.tiles_processed.saturating_add(1);
        Ok(())
    }

    fn finalize_certificate(mut self) -> Result<PeakCertificate, TruePeakError> {
        self.process_final_tile()?;
        #[cfg(feature = "fast-stage-timing")]
        let finalize_timer = StageTimer::start();

        let mut channel_intervals = Vec::with_capacity(self.channels);
        let mut channel_upper_linear_peaks = Vec::with_capacity(self.channels);
        let mut point_channels = Vec::with_capacity(self.channels);
        let mut overall_lower = 0.0;
        let mut overall_upper = 0.0;
        let mut overall_point = 0.0;
        let mut unresolved_upper = 0.0;

        for channel in 0..self.channels {
            let sample_peak = self.channel_sample_peaks[channel];
            self.channel_hq4_peaks[channel] =
                max_nonnegative_finite(self.channel_hq4_peaks[channel], sample_peak);
            self.channel_point_peaks[channel] =
                max_nonnegative_finite(self.channel_point_peaks[channel], sample_peak);
            self.channel_lower_peaks[channel] =
                max_nonnegative_finite(self.channel_lower_peaks[channel], sample_peak);
            self.channel_upper_peaks[channel] =
                max_nonnegative_finite(self.channel_upper_peaks[channel], sample_peak);

            // The public reconstruction norm is an independent global upper.
            // Taking the minimum of two valid upper bounds can only tighten the
            // certificate and, importantly, preserves exact digital silence.
            let norm_upper = upper_mul(HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER, sample_peak);
            let upper = self.channel_upper_peaks[channel].min(norm_upper);
            let lower = self.channel_lower_peaks[channel];
            let point = self.channel_point_peaks[channel].min(upper);
            // Do not manufacture a plausible interval if two independently
            // valid enclosures ever contradict one another. That can only mean
            // the numerical contract has failed, so fail closed instead of
            // clamping a lower witness to an arbitrary upper value.
            if !upper.is_finite()
                || !lower.is_finite()
                || !point.is_finite()
                || lower > upper
            {
                return Err(TruePeakError::NumericalOverflow);
            }
            channel_intervals.push(PeakInterval::new(lower, upper));
            channel_upper_linear_peaks.push(upper);
            point_channels.push(point);
            overall_lower = max_nonnegative_finite(overall_lower, lower);
            overall_upper = max_nonnegative_finite(overall_upper, upper);
            overall_point = max_nonnegative_finite(overall_point, point);
            unresolved_upper = max_nonnegative_finite(
                unresolved_upper,
                self.channel_flat_upper_peaks[channel].min(norm_upper),
            );
        }

        let finite_interval = PeakInterval::new(overall_lower, overall_upper);
        self.diagnostics.max_evaluation_error_linear = self.max_evaluation_error;
        self.diagnostics.unresolved_upper_linear = unresolved_upper;
        self.diagnostics.fast_input_sample_peak_linear = self
            .channel_sample_peaks
            .iter()
            .copied()
            .fold(0.0, max_nonnegative_finite);
        self.diagnostics.fast_hq4_peak_linear = self
            .channel_hq4_peaks
            .iter()
            .copied()
            .fold(0.0, max_nonnegative_finite);
        let status = if unresolved_upper <= overall_lower {
            SearchStatus::Complete
        } else {
            SearchStatus::WorkLimited
        };
        let overall = if magnitude_bits(overall_point) == 0 {
            PeakLevel::Silence
        } else {
            PeakLevel::Finite {
                linear: overall_point,
                dbtp: crate::positive_finite_linear_to_dbtp(overall_point),
            }
        };
        #[cfg(feature = "fast-stage-timing")]
        finalize_timer.finish(&mut self.diagnostics.fast_stage_finalize_nanos);

        Ok(PeakCertificate {
            reconstruction: CertifiedReconstruction::Hq1024V1,
            tier: PeakTier::Fast,
            reconstruction_linf_gain_upper: HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER,
            numerical_envelope_linear: self.max_evaluation_error,
            reported_point_estimate: TruePeakResult {
                overall,
                channel_linear_peaks: point_channels,
                frames: self.nominal_frames_seen,
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
pub(super) struct FastPeakMeterImpl {
    edge_policy: EdgePolicy,
    prefix: QualifiedHalfDelayFft,
    state: FastState,
    first_frame: Vec<f64>,
    last_frame: Vec<f64>,
    edge_scratch: Vec<f64>,
    validation_peaks: Vec<f64>,
    started: bool,
    next_input_index: i128,
}

impl FastPeakMeterImpl {
    pub(super) fn new(
        sample_rate_hz: u32,
        channels: usize,
        edge_policy: EdgePolicy,
    ) -> Result<Self, TruePeakError> {
        Self::new_with_execution_mode(
            sample_rate_hz,
            channels,
            edge_policy,
            FastExecutionMode::Production,
        )
    }

    pub(super) fn new_with_execution_mode(
        sample_rate_hz: u32,
        channels: usize,
        edge_policy: EdgePolicy,
        execution_mode: FastExecutionMode,
    ) -> Result<Self, TruePeakError> {
        if sample_rate_hz == 0 {
            return Err(TruePeakError::InvalidSampleRate);
        }
        if channels == 0 {
            return Err(TruePeakError::InvalidChannelCount);
        }
        let prefix = QualifiedHalfDelayFft::new_fast90(ReconstructionId::Hq1024V1, channels);
        let prefix_avx = prefix.fast90_same_graph_avx_active();
        Ok(Self {
            edge_policy,
            prefix,
            state: FastState::new(channels, prefix_avx, execution_mode),
            first_frame: vec![0.0; channels],
            last_frame: vec![0.0; channels],
            edge_scratch: Vec::with_capacity(INPUT_HALO_FRAMES as usize * channels),
            validation_peaks: vec![0.0; channels],
            started: false,
            next_input_index: 0,
        })
    }

    fn feed_prefix(&mut self, samples: &[f64], start_input_index: i128) -> Result<(), TruePeakError> {
        let channels = self.state.channels;
        debug_assert_eq!(samples.len() % channels, 0);
        let total_frames = samples.len() / channels;
        let mut consumed_frames = 0usize;
        while consumed_frames < total_frames {
            let take = self
                .prefix
                .frames_until_block()
                .min(total_frames - consumed_frames);
            let first = consumed_frames * channels;
            let last = (consumed_frames + take) * channels;
            #[cfg(feature = "fast-stage-timing")]
            let prefix_timer = StageTimer::start();
            let consumed = {
                let state = &mut self.state;
                self.prefix.process_interleaved_blocks(
                    &samples[first..last],
                    start_input_index + consumed_frames as i128,
                    |block| state.ingest_prefix_block(block),
                )
            };
            #[cfg(feature = "fast-stage-timing")]
            prefix_timer.finish(&mut self.state.diagnostics.fast_stage_prefix_block_ingest_nanos);
            debug_assert_eq!(consumed, take);
            consumed_frames += take;
            self.state.process_ready_full_tiles()?;
        }
        Ok(())
    }

    fn build_edge_extension(
        edge_policy: EdgePolicy,
        channels: usize,
        frame: &[f64],
        scratch: &mut Vec<f64>,
    ) {
        debug_assert_eq!(frame.len(), channels);
        scratch.clear();
        scratch.resize(INPUT_HALO_FRAMES as usize * channels, 0.0);
        if edge_policy == EdgePolicy::RepeatEndpoints {
            for destination in scratch.chunks_exact_mut(channels) {
                destination.copy_from_slice(frame);
            }
        }
    }

    pub(super) fn push_interleaved(&mut self, samples: &[f64]) -> Result<(), TruePeakError> {
        let channels = self.state.channels;
        if samples.len() % channels != 0 {
            return Err(TruePeakError::IncompleteFrame {
                samples: samples.len(),
                channels,
            });
        }
        if samples.is_empty() {
            return Ok(());
        }
        let chunk_frames = samples.len() / channels;
        let chunk_frames_u64 = u64::try_from(chunk_frames).map_err(|_| TruePeakError::InputTooLong)?;
        let new_frames = self
            .state
            .nominal_frames_seen
            .checked_add(chunk_frames_u64)
            .ok_or(TruePeakError::InputTooLong)?;
        if i128::from(new_frames)
            .checked_mul(TARGET_FACTOR_I128)
            .is_none()
        {
            return Err(TruePeakError::InputTooLong);
        }

        // Complete validation and tentative peak reduction in one contiguous
        // pass before mutating streaming state. `validation_peaks` is scratch,
        // so a rejected push cannot change the measurement result.
        self.validation_peaks.fill(0.0);
        for (frame_index, frame) in samples.chunks_exact(channels).enumerate() {
            for (channel, sample) in frame.iter().copied().enumerate() {
                if !sample.is_finite() {
                    return Err(TruePeakError::NonFiniteSample {
                        sample_index: frame_index * channels + channel,
                    });
                }
                self.validation_peaks[channel] =
                    max_exact_magnitude(self.validation_peaks[channel], sample);
            }
        }

        if !self.started {
            self.first_frame.copy_from_slice(&samples[..channels]);
            Self::build_edge_extension(
                self.edge_policy,
                channels,
                &self.first_frame,
                &mut self.edge_scratch,
            );
            let extension = std::mem::take(&mut self.edge_scratch);
            let result = self.feed_prefix(&extension, -INPUT_HALO_FRAMES);
            self.edge_scratch = extension;
            result?;
            self.started = true;
        }

        self.last_frame
            .copy_from_slice(&samples[samples.len() - channels..]);
        for channel in 0..channels {
            self.state.channel_sample_peaks[channel] = max_nonnegative_finite(
                self.state.channel_sample_peaks[channel],
                self.validation_peaks[channel],
            );
        }
        self.state.nominal_frames_seen = new_frames;
        let start = self.next_input_index;
        self.next_input_index += chunk_frames as i128;
        self.feed_prefix(samples, start)
    }

    pub(super) fn finalize(mut self) -> Result<PeakCertificate, TruePeakError> {
        if !self.started {
            return Err(TruePeakError::EmptyInput);
        }
        Self::build_edge_extension(
            self.edge_policy,
            self.state.channels,
            &self.last_frame,
            &mut self.edge_scratch,
        );
        let extension = std::mem::take(&mut self.edge_scratch);
        let trailing_start = self.next_input_index;
        let result = self.feed_prefix(&extension, trailing_start);
        self.edge_scratch = extension;
        result?;
        #[cfg(feature = "fast-stage-timing")]
        let prefix_timer = StageTimer::start();
        {
            let state = &mut self.state;
            self.prefix.flush_block(|block| state.ingest_prefix_block(block));
        }
        #[cfg(feature = "fast-stage-timing")]
        prefix_timer.finish(&mut self.state.diagnostics.fast_stage_prefix_block_ingest_nanos);
        self.state.process_ready_full_tiles()?;
        self.state.finalize_certificate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hq1024_coefficients::{
        HQ1024_FIRST_HALF_DELAY_TAPS, HQ1024_HALF_DELAY_COEFFICIENTS,
    };

    #[derive(Debug)]
    struct DenseOracle {
        channel_peaks: Vec<f64>,
        winner_q: Vec<usize>,
    }

    // Test-oracle scaling is intentionally defined independently of Fast066's
    // exceptional-lane constants. Power-of-two scaling preserves the exact
    // frozen reconstruction while keeping large finite fixtures representable.
    const ORACLE_SCALE_THRESHOLD: f64 = 1.0e200;
    const ORACLE_HUGE_SCALE: f64 = f64::from_bits(511_u64 << 52); // 2^-512
    const ORACLE_HUGE_INVERSE_SCALE: f64 = f64::from_bits(1535_u64 << 52); // 2^512

    fn deterministic_values(count: usize) -> Vec<f64> {
        let mut state = 0xd1b5_4a32_d192_ed03_u64;
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let bits = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
            let unit = ((bits >> 11) as f64) * (1.0 / ((1_u64 << 53) as f64));
            values.push(unit.mul_add(1.9, -0.95));
        }
        values
    }

    fn capacity_signal(frames: usize, scale: f64) -> Vec<f64> {
        // This carrier was chosen from the frozen V2 numerical model because a
        // full canonical tile fills all 64 nomination slots and all eight
        // finishing slots without relying on pruning. Keep the generator here
        // instead of checking in another long binary fixture.
        let mut state = 2_u64;
        let mut values = Vec::with_capacity(frames);
        for frame in 0..frames {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let bits = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
            let unit = ((bits >> 11) as f64) * (1.0 / ((1_u64 << 53) as f64));
            let phase = std::f64::consts::TAU * 0.4375 * frame as f64 + 0.47;
            values.push(scale * (0.9 + 0.05 * unit) * phase.sin());
        }
        values
    }

    fn full_tile_capacity_signal(frames: usize) -> Vec<f64> {
        assert!(frames >= 4097);
        let mut values = capacity_signal(4097, 1.0);
        let endpoint = *values.last().expect("capacity carrier endpoint");
        values.resize(frames, endpoint);
        values
    }

    fn pruning_signal(frames: usize) -> Vec<f64> {
        let mut values = capacity_signal(frames, 0.04);
        assert!(frames > 128);
        values[128] = 1.0;
        values
    }

    fn oracle_hq1024_domain_magnitudes(
        interleaved: &[f64],
        channels: usize,
        channel: usize,
        edge: EdgePolicy,
        first_q: i128,
        last_q: i128,
    ) -> Vec<(i128, f64)> {
        assert!(first_q >= 0);
        assert!(first_q <= last_q);
        let first_cell = first_q.div_euclid(TAIL_FACTOR_I128);
        let last_cell = last_q.div_euclid(TAIL_FACTOR_I128);
        let coarse_first = first_cell + i128::from(HQ1024_TAIL_OFFSET_MIN);
        let coarse_last = last_cell + i128::from(HQ1024_TAIL_OFFSET_MAX);
        let mut coarse = Vec::with_capacity(
            usize::try_from(coarse_last - coarse_first + 1).expect("oracle coarse span"),
        );
        for coarse_index in coarse_first..=coarse_last {
            coarse.push(oracle_direct_coarse(
                interleaved,
                channels,
                coarse_index,
                channel,
                edge,
            ));
        }

        let mut values = Vec::with_capacity(
            usize::try_from(last_q - first_q + 1).expect("oracle HQ domain span"),
        );
        for q in first_q..=last_q {
            let cell = q.div_euclid(TAIL_FACTOR_I128);
            let phase = usize::try_from(q.rem_euclid(TAIL_FACTOR_I128)).expect("HQ phase");
            let value = if phase == 0 {
                coarse[usize::try_from(cell - coarse_first).expect("oracle coarse index")]
            } else {
                let support_first = cell + i128::from(HQ1024_TAIL_OFFSET_MIN);
                let begin = usize::try_from(support_first - coarse_first)
                    .expect("oracle tail support index");
                oracle_tail_dot(
                    &coarse[begin..begin + HQ1024_TAIL_OFFSET_COUNT],
                    &HQ1024_TAIL_COEFFICIENTS[phase],
                )
            };
            values.push((q, exact_magnitude(value)));
        }
        values
    }

    fn install_test_coarse_values(state: &mut FastState, first: i128, values: Vec<f64>) {
        assert_eq!(state.channels, 1);
        assert!(!values.is_empty());
        state.coarse.first_index = Some(first);
        state.coarse.head = 0;
        state.coarse.values[0] = values;
        state.coarse.errors.clear();
        state.coarse.error_values.clear();
    }

    fn finisher_state_for_neighbors(
        left_target: f64,
        right_target: f64,
    ) -> (FastState, i128, i128, FineEvaluationContext) {
        let mut state = FastState::new(1, false, FastExecutionMode::Production);
        let q = 100 * TARGET_FACTOR_I128 + TARGET_FACTOR_I128 / 4;
        let left_phase = usize::try_from((q - FINISH_PROBE_STEP_Q).rem_euclid(TAIL_FACTOR_I128))
            .unwrap();
        let right_phase = usize::try_from((q + FINISH_PROBE_STEP_Q).rem_euclid(TAIL_FACTOR_I128))
            .unwrap();
        let left_row = &HQ1024_TAIL_COEFFICIENTS[left_phase];
        let right_row = &HQ1024_TAIL_COEFFICIENTS[right_phase];

        // Two independently weighted taps are enough to prescribe the two
        // finishing-probe values. Choose the best-conditioned 2x2 subsystem so
        // the fixture is stable if harmless coefficient-rounding details move.
        let mut best = (0usize, 1usize, 0.0_f64);
        for first in 0..HQ1024_TAIL_OFFSET_COUNT {
            for second in first + 1..HQ1024_TAIL_OFFSET_COUNT {
                let determinant = left_row[first] * right_row[second]
                    - left_row[second] * right_row[first];
                if determinant.abs() > best.2.abs() {
                    best = (first, second, determinant);
                }
            }
        }
        let (first_tap, second_tap, determinant) = best;
        assert!(determinant.abs() > 1.0e-6, "ill-conditioned finishing fixture");
        let first_value =
            (left_target * right_row[second_tap] - left_row[second_tap] * right_target)
                / determinant;
        let second_value =
            (left_row[first_tap] * right_target - left_target * right_row[first_tap])
                / determinant;
        assert!(first_value.is_finite() && second_value.is_finite());

        let cell = q.div_euclid(TAIL_FACTOR_I128);
        let support_first = cell + i128::from(HQ1024_TAIL_OFFSET_MIN);
        let mut coarse = vec![0.0; HQ1024_TAIL_OFFSET_COUNT];
        coarse[first_tap] = first_value;
        coarse[second_tap] = second_value;
        install_test_coarse_values(&mut state, support_first, coarse);
        state.fine_support_input_max = max_exact_magnitude(first_value, second_value);
        state.fine_support_coarse_error = 0.0;
        state.channel_lower_peaks[0] = 0.9;
        state.proposals.push(Proposal {
            nominee_quarter_index: q / 256,
            q,
            evaluation: Evaluation {
                value: 1.0,
                error: 0.0,
            },
            neighborhood_upper: 10.0,
        });
        let context = state.fine_evaluation_context();
        let final_q = q + TARGET_FACTOR_I128;

        let actual_left = state
            .fine_evaluation(0, q - FINISH_PROBE_STEP_Q, context)
            .unwrap()
            .value;
        let actual_right = state
            .fine_evaluation(0, q + FINISH_PROBE_STEP_Q, context)
            .unwrap()
            .value;
        let tolerance = 2.0e-12;
        assert!((actual_left - left_target).abs() <= tolerance);
        assert!((actual_right - right_target).abs() <= tolerance);
        (state, q, final_q, context)
    }

    fn oracle_extended_sample(
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

    #[inline]
    fn oracle_first_coefficient(tap: usize) -> f64 {
        if tap < HQ1024_HALF_DELAY_COEFFICIENTS.len() {
            HQ1024_HALF_DELAY_COEFFICIENTS[tap]
        } else {
            HQ1024_HALF_DELAY_COEFFICIENTS[HQ1024_FIRST_HALF_DELAY_TAPS - 1 - tap]
        }
    }

    fn oracle_direct_coarse(
        interleaved: &[f64],
        channels: usize,
        coarse_index: i128,
        channel: usize,
        edge: EdgePolicy,
    ) -> f64 {
        if coarse_index.rem_euclid(2) == 0 {
            return oracle_extended_sample(
                interleaved,
                channels,
                coarse_index.div_euclid(2),
                channel,
                edge,
            );
        }

        let source_center = (coarse_index - 1).div_euclid(2)
            + (HQ1024_FIRST_HALF_DELAY_TAPS / 2) as i128;
        let mut input_max = 0.0;
        for tap in 0..HQ1024_FIRST_HALF_DELAY_TAPS {
            input_max = max_exact_magnitude(
                input_max,
                oracle_extended_sample(
                    interleaved,
                    channels,
                    source_center - tap as i128,
                    channel,
                    edge,
                ),
            );
        }
        let (scale, inverse_scale) = if input_max <= ORACLE_SCALE_THRESHOLD {
            (1.0, 1.0)
        } else {
            (ORACLE_HUGE_SCALE, ORACLE_HUGE_INVERSE_SCALE)
        };
        let mut sum = 0.0;
        for tap in 0..HQ1024_FIRST_HALF_DELAY_TAPS {
            let sample = oracle_extended_sample(
                interleaved,
                channels,
                source_center - tap as i128,
                channel,
                edge,
            );
            sum += (sample * scale) * oracle_first_coefficient(tap);
        }
        let value = sum * inverse_scale;
        assert!(value.is_finite(), "dense oracle first-stage overflowed on finite test fixture");
        value
    }

    fn oracle_tail_dot(input: &[f64], row: &[f64; HQ1024_TAIL_OFFSET_COUNT]) -> f64 {
        let input_max = input.iter().copied().fold(0.0, max_exact_magnitude);
        let (scale, inverse_scale) = if input_max <= ORACLE_SCALE_THRESHOLD {
            (1.0, 1.0)
        } else {
            (ORACLE_HUGE_SCALE, ORACLE_HUGE_INVERSE_SCALE)
        };
        let mut sum = 0.0;
        for index in 0..HQ1024_TAIL_OFFSET_COUNT {
            sum += (input[index] * scale) * row[index];
        }
        let value = sum * inverse_scale;
        assert!(value.is_finite(), "dense oracle tail overflowed on finite test fixture");
        value
    }

    /// Test-only dense authority: reconstruct the frozen first-stage FIR directly,
    /// then enumerate every HQ1024 tail knot. It never calls Fast survey,
    /// candidate, refinement, or flat-bound code.
    fn dense_hq1024_oracle(
        interleaved: &[f64],
        channels: usize,
        edge: EdgePolicy,
    ) -> DenseOracle {
        assert!(channels > 0);
        assert!(!interleaved.is_empty());
        assert_eq!(interleaved.len() % channels, 0);
        let frames = interleaved.len() / channels;
        let final_coarse = (frames as i128 - 1) * 2;
        let coarse_start = i128::from(HQ1024_TAIL_OFFSET_MIN);
        let coarse_end = final_coarse + i128::from(HQ1024_TAIL_OFFSET_MAX);
        let coarse_len = usize::try_from(coarse_end - coarse_start + 1).unwrap();
        let mut coarse = vec![vec![0.0_f64; coarse_len]; channels];
        for channel in 0..channels {
            for index in coarse_start..=coarse_end {
                coarse[channel][usize::try_from(index - coarse_start).unwrap()] =
                    oracle_direct_coarse(interleaved, channels, index, channel, edge);
            }
        }

        let final_q = (frames - 1) * 1024;
        let mut channel_peaks = vec![0.0; channels];
        let mut winner_q = vec![0; channels];
        for channel in 0..channels {
            for q in 0..=final_q {
                let cell = (q / HQ1024_TAIL_FACTOR) as i128;
                let phase = q % HQ1024_TAIL_FACTOR;
                let value = if phase == 0 {
                    coarse[channel][usize::try_from(cell - coarse_start).unwrap()]
                } else {
                    let first = cell + i128::from(HQ1024_TAIL_OFFSET_MIN);
                    let begin = usize::try_from(first - coarse_start).unwrap();
                    let support = &coarse[channel][begin..begin + HQ1024_TAIL_OFFSET_COUNT];
                    oracle_tail_dot(support, &HQ1024_TAIL_COEFFICIENTS[phase])
                };
                let magnitude = exact_magnitude(value);
                if magnitude_bits(magnitude) > magnitude_bits(channel_peaks[channel]) {
                    channel_peaks[channel] = magnitude;
                    winner_q[channel] = q;
                }
            }
        }
        DenseOracle { channel_peaks, winner_q }
    }

    fn run_fast_private(
        interleaved: &[f64],
        channels: usize,
        edge: EdgePolicy,
        disable_candidates: bool,
    ) -> PeakCertificate {
        let mut meter = FastPeakMeterImpl::new(48_000, channels, edge).unwrap();
        meter.state.disable_candidates_for_test = disable_candidates;
        meter.push_interleaved(interleaved).unwrap();
        meter.finalize().unwrap()
    }

    fn assert_dense_containment(
        name: &str,
        interleaved: &[f64],
        channels: usize,
        edge: EdgePolicy,
        disable_candidates: bool,
    ) -> (PeakCertificate, DenseOracle) {
        let dense = dense_hq1024_oracle(interleaved, channels, edge);
        let certificate = run_fast_private(interleaved, channels, edge, disable_candidates);
        assert_eq!(certificate.channel_intervals.len(), channels, "{name} {edge:?}");
        for (channel, target) in dense.channel_peaks.iter().copied().enumerate() {
            let interval = certificate.channel_intervals[channel];
            assert!(
                interval.lower_linear <= target,
                "{name} {edge:?} channel {channel}: dense target {target:.17e} below lower {:.17e}",
                interval.lower_linear,
            );
            assert!(
                interval.upper_linear >= target,
                "{name} {edge:?} channel {channel}: dense target {target:.17e} above upper {:.17e}",
                interval.upper_linear,
            );
        }
        let overall = dense
            .channel_peaks
            .iter()
            .copied()
            .fold(0.0, max_nonnegative_finite);
        assert!(certificate.finite_interval.lower_linear <= overall, "{name} {edge:?}");
        assert!(certificate.finite_interval.upper_linear >= overall, "{name} {edge:?}");
        (certificate, dense)
    }

    fn two_sum(left: f64, right: f64) -> (f64, f64) {
        let sum = left + right;
        let right_virtual = sum - left;
        let error = (left - (sum - right_virtual)) + (right - right_virtual);
        (sum, error)
    }

    /// Error-reduced dot used only as an independent test reference for the
    /// finite ordinary-magnitude kernel fixtures below.
    fn compensated_dot(left: &[f64], right: &[f64]) -> f64 {
        assert_eq!(left.len(), right.len());
        let mut high = 0.0;
        let mut low = 0.0;
        for (&a, &b) in left.iter().zip(right) {
            let product = a * b;
            let product_error = a.mul_add(b, -product);
            let (sum, add_error) = two_sum(high, product);
            let correction = low + product_error + add_error;
            let (next_high, next_low) = two_sum(sum, correction);
            high = next_high;
            low = next_low;
        }
        high + low
    }

    fn midpoint_reference(input: &[f64], metadata: &FastMetadata) -> f64 {
        assert_eq!(input.len(), 24);
        let mut samples = [0.0_f64; 24];
        let mut coefficients = [0.0_f64; 24];
        for j in 0..MIDPOINT_SYMMETRIC_TERMS {
            samples[2 * j] = input[j];
            coefficients[2 * j] = metadata.midpoint_coefficients[j];
            samples[2 * j + 1] = input[23 - j];
            coefficients[2 * j + 1] = metadata.midpoint_coefficients[j];
        }
        compensated_dot(&samples, &coefficients)
    }

    fn midpoint_kernel_error(metadata: &FastMetadata, input_max: f64) -> f64 {
        dot_rounding_upper(
            MIDPOINT_ROUNDING_OPS,
            upper_mul(metadata.midpoint_l1_upper, input_max),
        )
    }

    fn tail_kernel_error(metadata: &FastMetadata, phase: usize, input_max: f64) -> f64 {
        dot_rounding_upper(
            TAIL_ROUNDING_OPS,
            upper_mul(metadata.phase_l1_upper[phase], input_max),
        )
    }

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
            // SAFETY: this guard restores the exact thread-local MXCSR value
            // captured by the test before it changed DAZ/FTZ mode.
            unsafe { write_mxcsr(self.0) };
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    fn force_gradual_underflow() -> MxcsrRestore {
        const DAZ_FTZ: u32 = (1 << 6) | (1 << 15);
        // SAFETY: MXCSR is thread-local. The returned guard restores the exact
        // incoming mode on every exit, including unwinding.
        let original = unsafe { read_mxcsr() };
        unsafe { write_mxcsr(original & !DAZ_FTZ) };
        MxcsrRestore(original)
    }

    #[test]
    fn frozen_midpoint_metadata_matches_design_constants() {
        let metadata = fast_metadata();
        assert!((metadata.midpoint_l1_upper - 1.963_847_537_721_252_8).abs() < 2.0e-15);
        assert!((metadata.a4_upper - 0.076_668_792_502_239_3).abs() < 2.0e-16);
        assert!((metadata.b4_upper - 9.273_373_783_036_083e-16).abs() < 2.0e-30);
        assert_eq!(
            crate::hq1024_coefficients::HQ1024_TAIL_NONZERO_COUNTS[MIDPOINT_PHASE],
            24
        );
    }

    #[test]
    fn candidate_priority_is_total_and_deterministic() {
        let mut values = [
            Candidate { quarter_index: 9, score: 2.0, proposal_offset_q: 0, fit_valid: false },
            Candidate { quarter_index: 4, score: 2.0, proposal_offset_q: 0, fit_valid: false },
            Candidate { quarter_index: 1, score: 3.0, proposal_offset_q: 0, fit_valid: false },
        ];
        values.sort_unstable_by(|left, right| candidate_priority(*left, *right));
        assert_eq!(values.map(|value| value.quarter_index), [1, 4, 9]);
    }

    #[test]
    fn v2_proposal_priority_uses_measured_magnitude_then_coordinate_then_nominee() {
        let evaluation = |value| Evaluation { value, error: 0.0 };
        let mut proposals = [
            Proposal { nominee_quarter_index: 12, q: 900, evaluation: evaluation(-2.0), neighborhood_upper: 3.0 },
            Proposal { nominee_quarter_index: 11, q: 800, evaluation: evaluation(2.0), neighborhood_upper: 3.0 },
            Proposal { nominee_quarter_index: 10, q: 800, evaluation: evaluation(-2.0), neighborhood_upper: 3.0 },
            Proposal { nominee_quarter_index: 1, q: 100, evaluation: evaluation(3.0), neighborhood_upper: 4.0 },
        ];
        proposals.sort_unstable_by(|left, right| proposal_priority(*left, *right));
        assert_eq!(
            proposals.map(|proposal| (proposal.q, proposal.nominee_quarter_index)),
            [(100, 1), (800, 10), (800, 11), (900, 12)],
        );
    }

    #[test]
    fn v2_signed_fits_round_only_bounded_local_offsets_and_respect_ties() {
        let delta = signed_parabolic_delta(0.8, 1.0, 0.6).unwrap();
        assert!(delta > 0.0 && delta <= 0.5);
        let offset = rounded_local_offset(delta, 256);
        assert!((-128..=128).contains(&offset));
        assert_eq!(rounded_local_offset(0.5, 256), 128);
        assert_eq!(rounded_local_offset(-0.5, 256), -128);
        assert_eq!(rounded_local_offset(0.5, 32), 16);
        assert_eq!(rounded_local_offset(-0.5, 32), -16);

        assert!(middle_is_magnitude_winner(0.9, 1.0, 1.0));
        assert!(!middle_is_magnitude_winner(1.0, 1.0, 0.9));
        assert!(!middle_is_magnitude_winner(0.9, 1.0, 1.1));
        assert!(signed_parabolic_delta(1.0, 1.0, 1.0).is_none());
        assert!(signed_parabolic_delta(-0.1, 1.0, 0.9).is_none());
    }

    #[test]
    fn v2_neighborhood_bound_covers_cross_group_and_left_halo_domains() {
        let mut state = FastState::new(1, false, FastExecutionMode::Production);
        state.group_count = 2;
        state.group_uppers[0] = 1.25;
        state.group_uppers[1] = 2.5;
        state.left_halo_upper = 3.75;
        let candidate = |quarter_index| Candidate {
            quarter_index, score: 1.0, proposal_offset_q: 0, fit_valid: false,
        };
        let start = 4096_i128;
        let end = start + 512;
        let final_q = end * TARGET_FACTOR_I128;

        // Center one quarter-sample before the group boundary: D_i touches both groups.
        let cross = candidate(4 * (start + GROUP_FRAMES) - 1);
        assert_eq!(state.candidate_neighborhood_upper(cross, start, end, final_q), 2.5);

        // A center exactly on the tile boundary reaches into [a-1/4,a], which
        // must use the retained left-halo envelope rather than group 0 alone.
        let boundary = candidate(4 * start);
        assert_eq!(state.candidate_neighborhood_upper(boundary, start, end, final_q), 3.75);
    }

    #[test]
    fn v2_full_tile_capacity_and_work_ceiling_are_per_channel_tile() {
        // 6000 nominal frames are enough to emit the first prefix block and
        // retire exactly one nonfinal 4096-interval tile. The second tile is
        // deliberately not finalised, so cumulative diagnostics are identical
        // to per-channel/tile diagnostics here.
        let samples = full_tile_capacity_signal(6000);
        let mut meter = FastPeakMeterImpl::new(192_000, 1, EdgePolicy::RepeatEndpoints).unwrap();
        meter.push_interleaved(&samples).unwrap();
        let diagnostics = &meter.state.diagnostics;

        assert_eq!(meter.state.next_tile_start, TILE_FRAMES);
        assert_eq!(diagnostics.tiles_processed, 1);
        assert_eq!(diagnostics.fast_candidates_selected, 64);
        assert_eq!(diagnostics.fast_proposals_evaluated, 64);
        assert_eq!(diagnostics.fast_finishing_candidates, 8);
        assert!(diagnostics.fast_proposal_fine_knots_evaluated <= 64);
        assert!(diagnostics.fast_finishing_fine_knots_evaluated <= 8 * 5);
        assert_eq!(
            diagnostics.fast_fine_knots_evaluated,
            diagnostics.fast_proposal_fine_knots_evaluated
                + diagnostics.fast_finishing_fine_knots_evaluated,
        );
        assert_eq!(diagnostics.fast_fine_knots_evaluated, 104);
        assert_eq!(MAX_NOMINEES_PER_TILE_CHANNEL, 64);
        assert_eq!(MAX_FINISHERS_PER_TILE_CHANNEL, 8);
        assert_eq!(MAX_FINE_EVALUATIONS_PER_TILE_CHANNEL, 104);
    }

    #[test]
    fn v2_rejected_nominee_bound_encloses_every_hq_knot_and_records_same_channel_witness() {
        let samples = pruning_signal(6000);
        let mut meter = FastPeakMeterImpl::new(192_000, 1, EdgePolicy::RepeatEndpoints).unwrap();
        meter.push_interleaved(&samples).unwrap();
        assert_eq!(meter.state.diagnostics.tiles_processed, 1);
        let audit = meter
            .state
            .rejected_nominee_audit
            .expect("fixture must reject at least one nominee");

        assert_eq!(audit.channel, 0);
        assert_eq!(audit.start, 0);
        assert_eq!(audit.end, TILE_FRAMES);
        assert!(audit.neighborhood_upper <= audit.lower_witness);
        assert_eq!(
            audit.final_q,
            (meter.state.nominal_frames_seen as i128 - 1) * TARGET_FACTOR_I128,
        );

        let center_q = audit.candidate.quarter_index * 256;
        let first_q = (center_q - CANDIDATE_DOMAIN_RADIUS_Q).max(0);
        let last_q = (center_q + CANDIDATE_DOMAIN_RADIUS_Q).min(audit.final_q);
        let oracle = oracle_hq1024_domain_magnitudes(
            &samples,
            1,
            audit.channel,
            EdgePolicy::RepeatEndpoints,
            first_q,
            last_q,
        );
        assert_eq!(oracle.len(), usize::try_from(last_q - first_q + 1).unwrap());
        for (q, magnitude) in oracle {
            assert!(
                magnitude <= audit.neighborhood_upper,
                "rejected D_i knot q={q} magnitude={magnitude:.17e} exceeds U_i={:.17e}; witness={:.17e}",
                audit.neighborhood_upper,
                audit.lower_witness,
            );
        }
    }

    #[test]
    fn v2_cross_group_neighbor_prevents_center_group_only_rejection() {
        let mut state = FastState::new(1, false, FastExecutionMode::Production);
        let start = 4096_i128;
        let end = start + 2 * GROUP_FRAMES;
        let boundary = start + GROUP_FRAMES;
        let candidate = Candidate {
            quarter_index: 4 * boundary - 1,
            score: 0.75,
            proposal_offset_q: 0,
            fit_valid: false,
        };

        let coarse_first = 2 * start - 64;
        let coarse_last = 2 * end + 64;
        install_test_coarse_values(
            &mut state,
            coarse_first,
            vec![0.0; usize::try_from(coarse_last - coarse_first + 1).unwrap()],
        );
        state.survey_start_i = candidate.quarter_index;
        state.survey = vec![0.75];
        state.group_count = 2;
        state.group_uppers[0] = 0.75;
        state.group_uppers[1] = 1.25;
        state.left_halo_upper = 0.0;
        state.channel_lower_peaks[0] = 1.0;
        state.selected.push(candidate);

        let final_q = end * TARGET_FACTOR_I128;
        let neighborhood_upper =
            state.candidate_neighborhood_upper(candidate, start, end, final_q);
        assert_eq!(neighborhood_upper, 1.25);
        assert!(state.group_uppers[0] <= state.channel_lower_peaks[0]);
        assert!(neighborhood_upper > state.channel_lower_peaks[0]);

        let context = state.fine_evaluation_context();
        state
            .probe_nominees(0, start, end, final_q, context)
            .unwrap();
        assert_eq!(state.diagnostics.fast_bound_pruned_nominees, 0);
        assert_eq!(state.diagnostics.fast_proposals_evaluated, 1);
        assert_eq!(state.proposals.len(), 1);
    }

    #[test]
    fn v2_common_tail_envelope_dominates_every_phase_local_envelope() {
        let metadata = fast_metadata();
        let common_input_max = 1.75_f64;
        let common_prefix_error = 3.0e-14_f64;
        let common = upper_add(
            upper_mul(metadata.tail_l1_upper, common_prefix_error),
            dot_rounding_upper(
                TAIL_ROUNDING_OPS,
                upper_mul(metadata.tail_l1_upper, common_input_max),
            ),
        );
        assert!(common.is_finite());
        for phase in 1..HQ1024_TAIL_FACTOR {
            let local_input_max = common_input_max * (phase as f64 / HQ1024_TAIL_FACTOR as f64);
            let local_prefix_error = common_prefix_error * 0.75;
            let local = upper_add(
                upper_mul(metadata.phase_l1_upper[phase], local_prefix_error),
                dot_rounding_upper(
                    TAIL_ROUNDING_OPS,
                    upper_mul(metadata.phase_l1_upper[phase], local_input_max),
                ),
            );
            assert!(common >= local, "phase {phase}: common={common:.17e} local={local:.17e}");
        }
    }

    #[test]
    fn v2_common_tail_envelope_covers_multiple_prefix_error_runs_and_boundary_supports() {
        let mut state = FastState::new(1, false, FastExecutionMode::Production);
        let frames = 64usize;
        let integer_error = [0.0_f64];
        let first_error = [1.0e-12_f64];
        let second_error = [4.0e-12_f64];

        let make_values = |first_coarse_index: i128| {
            let integer = (0..frames)
                .map(|frame| {
                    let index = first_coarse_index + 2 * frame as i128;
                    0.125 + index as f64 * 1.0e-4
                })
                .collect::<Vec<_>>();
            let half = (0..frames)
                .map(|frame| {
                    let index = first_coarse_index + 2 * frame as i128 + 1;
                    -0.25 + index as f64 * 7.5e-5
                })
                .collect::<Vec<_>>();
            (integer, half)
        };

        let (integer_a, half_a) = make_values(0);
        state.coarse.push_block(QualifiedHalfDelayBlock {
            first_coarse_index: 0,
            frames,
            channels: 1,
            integer: &integer_a,
            integer_error_by_channel: &integer_error,
            half: &half_a,
            half_error_by_channel: &first_error,
        });
        let (integer_b, half_b) = make_values(128);
        state.coarse.push_block(QualifiedHalfDelayBlock {
            first_coarse_index: 128,
            frames,
            channels: 1,
            integer: &integer_b,
            integer_error_by_channel: &integer_error,
            half: &half_b,
            half_error_by_channel: &second_error,
        });

        let support_cases = [
            (110_i128, first_error[0]),
            (127_i128, second_error[0]),
            (144_i128, second_error[0]),
        ];
        for (cell, expected_error) in support_cases {
            let first = cell + i128::from(HQ1024_TAIL_OFFSET_MIN);
            let last = cell + i128::from(HQ1024_TAIL_OFFSET_MAX);
            assert_eq!(state.coarse.max_error(0, first, last).to_bits(), expected_error.to_bits());
        }

        let common_first = 110 + i128::from(HQ1024_TAIL_OFFSET_MIN);
        let common_last = 144 + i128::from(HQ1024_TAIL_OFFSET_MAX);
        state.fine_support_input_max = state.coarse.max_magnitude(0, common_first, common_last);
        state.fine_support_coarse_error = state.coarse.max_error(0, common_first, common_last);
        assert_eq!(
            state.fine_support_coarse_error.to_bits(),
            second_error[0].to_bits(),
        );
        let context = state.fine_evaluation_context();
        let cached = context.cached_error.expect("ordinary synthetic envelope must be finite");

        for (cell, _) in support_cases {
            let support_first = cell + i128::from(HQ1024_TAIL_OFFSET_MIN);
            let support_last = cell + i128::from(HQ1024_TAIL_OFFSET_MAX);
            let local_input_max = state.coarse.max_magnitude(0, support_first, support_last);
            let local_prefix_error = state.coarse.max_error(0, support_first, support_last);
            for phase in 1..HQ1024_TAIL_FACTOR {
                let local = upper_add(
                    upper_mul(state.metadata.phase_l1_upper[phase], local_prefix_error),
                    tail_kernel_error(state.metadata, phase, local_input_max),
                );
                let evaluation = state
                    .fine_evaluation(
                        0,
                        cell * TAIL_FACTOR_I128 + phase as i128,
                        context,
                    )
                    .unwrap();
                assert_eq!(evaluation.error.to_bits(), cached.to_bits());
                assert!(
                    evaluation.error >= local,
                    "cell {cell} phase {phase}: cached={:.17e} local={local:.17e}",
                    evaluation.error,
                );
            }
        }
    }

    #[test]
    fn v2_finishing_missing_endpoint_stops_after_available_probe() {
        let mut state = FastState::new(1, false, FastExecutionMode::Production);
        install_test_coarse_values(&mut state, -16, vec![0.0; HQ1024_TAIL_OFFSET_COUNT]);
        state.fine_support_input_max = 0.0;
        state.fine_support_coarse_error = 0.0;
        state.proposals.push(Proposal {
            nominee_quarter_index: 0,
            q: 16,
            evaluation: Evaluation { value: 1.0, error: 0.0 },
            neighborhood_upper: 2.0,
        });
        let context = state.fine_evaluation_context();
        let before = state.diagnostics.fast_fine_knots_evaluated;
        state.finish_proposals(0, TARGET_FACTOR_I128, context).unwrap();
        let delta = state.diagnostics.fast_fine_knots_evaluated - before;
        assert_eq!(state.diagnostics.fast_finishing_candidates, 1);
        assert_eq!(state.diagnostics.fast_unbracketed_finishers, 1);
        assert_eq!(delta, 1);
        assert_eq!(state.diagnostics.direct_rescore_evaluations, 0);
    }

    #[test]
    fn v2_finishing_invalid_curvature_stops_after_two_probes() {
        let (mut state, _q, final_q, context) = finisher_state_for_neighbors(-0.6, 0.6);
        let before = state.diagnostics.fast_fine_knots_evaluated;
        state.finish_proposals(0, final_q, context).unwrap();
        let delta = state.diagnostics.fast_fine_knots_evaluated - before;
        assert_eq!(state.diagnostics.fast_finishing_candidates, 1);
        assert_eq!(state.diagnostics.fast_invalid_finishing_fits, 1);
        assert_eq!(state.diagnostics.fast_unbracketed_finishers, 0);
        assert_eq!(delta, 2);
        assert_eq!(state.diagnostics.direct_rescore_evaluations, 0);
    }

    #[test]
    fn v2_finishing_left_tie_stops_without_opening_a_second_search() {
        let mut state = FastState::new(1, false, FastExecutionMode::Production);
        let first = -16_i128;
        let mut coarse = vec![0.0; HQ1024_TAIL_OFFSET_COUNT];
        coarse[usize::try_from(-first).unwrap()] = 1.0;
        install_test_coarse_values(&mut state, first, coarse);
        state.survey_start_i = 0;
        state.survey = vec![1.0];
        state.fine_support_input_max = 1.0;
        state.fine_support_coarse_error = 0.0;
        state.proposals.push(Proposal {
            nominee_quarter_index: 0,
            q: 32,
            evaluation: Evaluation { value: 1.0, error: 0.0 },
            neighborhood_upper: 2.0,
        });
        let context = state.fine_evaluation_context();
        let before = state.diagnostics.fast_fine_knots_evaluated;
        state.finish_proposals(0, TARGET_FACTOR_I128, context).unwrap();
        let delta = state.diagnostics.fast_fine_knots_evaluated - before;
        assert_eq!(state.diagnostics.fast_finishing_candidates, 1);
        assert_eq!(state.diagnostics.fast_unbracketed_finishers, 1);
        assert_eq!(delta, 1, "left HQ4 tie is reused; only the right probe is new fine work");
        assert_eq!(state.diagnostics.direct_rescore_evaluations, 0);
    }

    #[test]
    fn v2_finishing_larger_probe_stops_after_two_probes() {
        let (mut state, _q, final_q, context) = finisher_state_for_neighbors(0.8, 1.1);
        let before = state.diagnostics.fast_fine_knots_evaluated;
        state.finish_proposals(0, final_q, context).unwrap();
        let delta = state.diagnostics.fast_fine_knots_evaluated - before;
        assert_eq!(state.diagnostics.fast_finishing_candidates, 1);
        assert_eq!(state.diagnostics.fast_unbracketed_finishers, 1);
        assert_eq!(state.diagnostics.fast_invalid_finishing_fits, 0);
        assert_eq!(delta, 2);
        assert_eq!(state.diagnostics.direct_rescore_evaluations, 0);
    }

    #[test]
    fn v2_loud_then_quiet_gates_a_dominated_tile_and_quiet_first_gets_optional_work() {
        let mut loud_then_quiet = capacity_signal(13_000, 1.0);
        for sample in &mut loud_then_quiet[4097..] {
            *sample *= 0.002;
        }
        let loud_certificate = run_fast_private(
            &loud_then_quiet,
            1,
            EdgePolicy::RepeatEndpoints,
            false,
        );
        assert!(loud_certificate.diagnostics.fast_bound_pruned_channel_tiles >= 1);

        let quiet_prefix = capacity_signal(6000, 0.04);
        let mut quiet_then_loud = FastPeakMeterImpl::new(
            192_000,
            1,
            EdgePolicy::RepeatEndpoints,
        )
        .unwrap();
        quiet_then_loud.push_interleaved(&quiet_prefix).unwrap();
        assert_eq!(quiet_then_loud.state.diagnostics.tiles_processed, 1);
        assert_eq!(quiet_then_loud.state.diagnostics.fast_bound_pruned_channel_tiles, 0);
        assert_eq!(quiet_then_loud.state.diagnostics.fast_candidates_selected, 64);
        assert_eq!(quiet_then_loud.state.diagnostics.fast_finishing_candidates, 8);
        assert!(quiet_then_loud.state.diagnostics.fast_fine_knots_evaluated > 0);
        assert!(quiet_then_loud.state.diagnostics.fast_fine_knots_evaluated <= 104);

        let loud_suffix = capacity_signal(7000, 1.0);
        quiet_then_loud.push_interleaved(&loud_suffix).unwrap();
        let quiet_certificate = quiet_then_loud.finalize().unwrap();
        assert!(quiet_certificate.diagnostics.tiles_processed >= 2);
    }

    #[test]
    fn silence_is_exact_and_work_is_deterministic() {
        let mut meter = FastPeakMeterImpl::new(192_000, 2, EdgePolicy::RepeatEndpoints).unwrap();
        meter.push_interleaved(&vec![0.0; 5000 * 2]).unwrap();
        let certificate = meter.finalize().unwrap();
        assert!(certificate.finite_interval.is_silence());
        assert_eq!(certificate.status, SearchStatus::Complete);
        assert_eq!(certificate.reported_point_estimate.frames, 5000);
    }

    #[test]
    fn dense_all_hq1024_knots_are_contained_per_channel_for_finite_edge_cases() {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        let _mxcsr = force_gradual_underflow();

        let mut cases: Vec<(&str, Vec<f64>, usize)> = Vec::new();
        cases.push(("one_frame", vec![0.625], 1));
        cases.push(("two_frames", vec![0.75, -0.25], 1));
        cases.push(("three_frames", vec![-0.2, 0.85, 0.1], 1));

        let mut impulse_start = vec![0.0; 11];
        impulse_start[0] = 0.9;
        cases.push(("impulse_start", impulse_start, 1));
        let mut impulse_interior = vec![0.0; 11];
        impulse_interior[5] = -0.9;
        cases.push(("impulse_interior", impulse_interior, 1));
        let mut impulse_end = vec![0.0; 11];
        impulse_end[10] = 0.9;
        cases.push(("impulse_end", impulse_end, 1));

        cases.push((
            "alternating",
            (0..17).map(|index| if index & 1 == 0 { 0.95 } else { -0.95 }).collect(),
            1,
        ));
        cases.push(("constant", vec![0.8125; 13], 1));
        cases.push(("deterministic_random", deterministic_values(23), 1));
        cases.push((
            "subnormal_mixed",
            vec![
                f64::from_bits(1),
                -f64::from_bits(7),
                f64::MIN_POSITIVE,
                f64::from_bits(11),
                -f64::from_bits(3),
            ],
            1,
        ));

        // Above ORDINARY_DOT_MAX_INPUT, but far below the certified 4.68x
        // whole-reconstruction overflow ceiling. This forces the bounded scaled
        // arithmetic path without making the finite target itself unrepresentable.
        let huge = f64::MAX / 32.0;
        cases.push(("large_finite", vec![huge, -0.75 * huge, 0.5 * huge, -0.25 * huge], 1));

        for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
            for (name, samples, channels) in &cases {
                assert_dense_containment(name, samples, *channels, edge, false);
            }
        }
    }

    #[test]
    fn asymmetric_stereo_dense_targets_are_contained_per_channel() {
        let frames = 31usize;
        let mut samples = vec![0.0; frames * 2];
        samples[3 * 2] = 0.9;
        samples[23 * 2 + 1] = -0.4;
        for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
            let (_certificate, dense) =
                assert_dense_containment("asymmetric_stereo", &samples, 2, edge, false);
            assert!(dense.channel_peaks[1] < dense.channel_peaks[0]);
            assert_ne!(dense.winner_q[0], dense.winner_q[1]);
        }
    }

    #[test]
    fn flat_envelope_contains_dense_target_when_candidate_refinement_is_starved() {
        let samples = deterministic_values(97);
        for edge in [EdgePolicy::RepeatEndpoints, EdgePolicy::ZeroExtend] {
            let (certificate, _dense) =
                assert_dense_containment("candidate_starved", &samples, 1, edge, true);
            assert!(certificate.diagnostics.fast_candidates_observed > 0);
            assert_eq!(certificate.diagnostics.fast_candidates_selected, 0);
            assert_eq!(certificate.diagnostics.fast_fine_knots_evaluated, 0);
        }
    }

    #[test]
    fn scalar_and_avx_midpoint_graphs_fit_declared_enclosure() {
        let metadata = fast_metadata();
        let ordinary = deterministic_values(71);
        let huge = f64::MAX / 32.0;
        let exceptional = (0..47)
            .map(|index| if index & 1 == 0 { huge } else { -0.5 * huge })
            .collect::<Vec<_>>();

        for input in [&ordinary, &exceptional] {
            let input_max = input.iter().copied().fold(0.0, max_exact_magnitude);
            let error = midpoint_kernel_error(metadata, input_max);
            let output_len = input.len() - 23;
            let mut scalar = vec![0.0; output_len];
            midpoint_batch(input, input_max, metadata, false, &mut scalar).unwrap();
            for index in 0..output_len {
                let reference = midpoint_reference(&input[index..index + 24], metadata);
                assert!(
                    (scalar[index] - reference).abs() <= error,
                    "scalar midpoint index {index}: value={} reference={} error={error}",
                    scalar[index],
                    reference,
                );
            }

            #[cfg(target_arch = "x86_64")]
            if std::arch::is_x86_feature_detected!("avx")
                && input_max <= ORDINARY_DOT_MAX_INPUT
            {
                let mut avx = vec![0.0; output_len];
                midpoint_batch(input, input_max, metadata, true, &mut avx).unwrap();
                for index in 0..output_len {
                    let reference = midpoint_reference(&input[index..index + 24], metadata);
                    assert!(
                        (avx[index] - reference).abs() <= error,
                        "AVX midpoint index {index}",
                    );
                    assert!(
                        (avx[index] - scalar[index]).abs() <= upper_add(error, error),
                        "scalar/AVX midpoint envelopes do not overlap at index {index}",
                    );
                }
            }
        }
    }

    #[test]
    fn scalar_and_avx_tail_graphs_fit_declared_enclosure() {
        let metadata = fast_metadata();
        let mut patterns = Vec::new();
        patterns.push(deterministic_values(HQ1024_TAIL_OFFSET_COUNT));
        patterns.push(
            (0..HQ1024_TAIL_OFFSET_COUNT)
                .map(|index| {
                    let amplitude = 0.45 + index as f64 / 100.0;
                    if index & 1 == 0 { amplitude } else { -amplitude }
                })
                .collect::<Vec<_>>(),
        );
        let huge = f64::MAX / 32.0;
        patterns.push(
            (0..HQ1024_TAIL_OFFSET_COUNT)
                .map(|index| if index & 1 == 0 { huge } else { -0.5 * huge })
                .collect::<Vec<_>>(),
        );

        for input in patterns {
            let input_max = input.iter().copied().fold(0.0, max_exact_magnitude);
            for phase in [1usize, 17, 127, 255, 256, 383, 511] {
                let reference = compensated_dot(&input, &HQ1024_TAIL_COEFFICIENTS[phase]);
                let error = tail_kernel_error(metadata, phase, input_max);
                let scalar = tail_dot(&input, phase, input_max, false).unwrap();
                assert!(
                    (scalar - reference).abs() <= error,
                    "scalar tail phase {phase}: value={scalar} reference={reference} error={error}",
                );

                #[cfg(target_arch = "x86_64")]
                if std::arch::is_x86_feature_detected!("avx")
                    && input_max <= ORDINARY_DOT_MAX_INPUT
                {
                    let avx = tail_dot(&input, phase, input_max, true).unwrap();
                    assert!((avx - reference).abs() <= error, "AVX tail phase {phase}");
                    assert!(
                        (avx - scalar).abs() <= upper_add(error, error),
                        "scalar/AVX tail envelopes do not overlap at phase {phase}",
                    );
                }
            }
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn daz_ftz_cannot_invalidate_fast_midpoint_or_tail_enclosures() {
        const DAZ: u32 = 1 << 6;
        const FTZ: u32 = 1 << 15;
        const DAZ_FTZ: u32 = DAZ | FTZ;

        // SAFETY: MXCSR is thread-local. The guard restores the incoming mode
        // on every exit, including unwinding.
        let original = unsafe { read_mxcsr() };
        let _restore = MxcsrRestore(original);
        unsafe { write_mxcsr(original & !DAZ_FTZ) };

        let metadata = fast_metadata();
        let midpoint_input = (0..47)
            .map(|index| {
                let value = f64::from_bits(1 + (index % 13) as u64);
                if index & 1 == 0 { value } else { -value }
            })
            .collect::<Vec<_>>();
        let midpoint_max = midpoint_input.iter().copied().fold(0.0, max_exact_magnitude);
        let midpoint_error = midpoint_kernel_error(metadata, midpoint_max);
        let mut ordinary_midpoint = vec![0.0; midpoint_input.len() - 23];
        midpoint_batch(
            &midpoint_input,
            midpoint_max,
            metadata,
            false,
            &mut ordinary_midpoint,
        )
        .unwrap();

        let tail_input = (0..HQ1024_TAIL_OFFSET_COUNT)
            .map(|index| {
                let value = f64::from_bits(1 + (index % 11) as u64);
                if index % 3 == 0 { -value } else { value }
            })
            .collect::<Vec<_>>();
        let tail_max = tail_input.iter().copied().fold(0.0, max_exact_magnitude);
        let phase = 383usize;
        let tail_error = tail_kernel_error(metadata, phase, tail_max);
        let ordinary_tail = tail_dot(&tail_input, phase, tail_max, false).unwrap();

        unsafe { write_mxcsr(original | DAZ_FTZ) };
        assert_eq!(unsafe { read_mxcsr() } & DAZ_FTZ, DAZ_FTZ);
        let mut daz_midpoint = vec![0.0; ordinary_midpoint.len()];
        midpoint_batch(
            &midpoint_input,
            midpoint_max,
            metadata,
            false,
            &mut daz_midpoint,
        )
        .unwrap();
        let daz_tail = tail_dot(&tail_input, phase, tail_max, false).unwrap();

        #[cfg(target_arch = "x86_64")]
        let (daz_midpoint_avx, daz_tail_avx) = if std::arch::is_x86_feature_detected!("avx") {
            let mut midpoint = vec![0.0; ordinary_midpoint.len()];
            midpoint_batch(&midpoint_input, midpoint_max, metadata, true, &mut midpoint).unwrap();
            let tail = tail_dot(&tail_input, phase, tail_max, true).unwrap();
            (Some(midpoint), Some(tail))
        } else {
            (None, None)
        };

        // Compare after restoring gradual underflow so the comparison itself
        // cannot flush the ordinary subnormal reference to zero.
        unsafe { write_mxcsr(original & !DAZ_FTZ) };
        for (index, (&ordinary, &daz)) in ordinary_midpoint.iter().zip(&daz_midpoint).enumerate() {
            assert!(
                (ordinary - daz).abs() <= midpoint_error,
                "DAZ/FTZ scalar midpoint escaped enclosure at index {index}",
            );
        }
        assert!((ordinary_tail - daz_tail).abs() <= tail_error);

        #[cfg(target_arch = "x86_64")]
        if let Some(avx) = daz_midpoint_avx {
            for (index, (&ordinary, &daz)) in ordinary_midpoint.iter().zip(&avx).enumerate() {
                assert!(
                    (ordinary - daz).abs() <= midpoint_error,
                    "DAZ/FTZ AVX midpoint escaped enclosure at index {index}",
                );
            }
        }
        #[cfg(target_arch = "x86_64")]
        if let Some(avx) = daz_tail_avx {
            assert!((ordinary_tail - avx).abs() <= tail_error);
        }
    }

    #[test]
    fn flat_group_inequality_contains_dense_hq1024_knots_on_arbitrary_coarse_arrays() {
        let metadata = fast_metadata();
        let group_frames = 8_i128;
        let first = -16_i128;
        let last = 2 * group_frames + 16;
        let len = usize::try_from(last - first + 1).unwrap();
        let mut patterns = Vec::new();
        patterns.push(vec![0.75; len]);
        patterns.push(
            (0..len)
                .map(|index| if index & 1 == 0 { 0.9 } else { -0.9 })
                .collect::<Vec<_>>(),
        );
        patterns.push(deterministic_values(len));

        for coarse in patterns {
            let get = |index: i128| coarse[usize::try_from(index - first).unwrap()];
            let m_hat = coarse.iter().copied().fold(0.0, max_exact_magnitude);
            let d_hat = second_difference_max(&coarse, m_hat).unwrap();
            let d_rounding = dot_rounding_upper(
                SECOND_DIFFERENCE_ROUNDING_OPS,
                upper_mul(4.0, m_hat),
            );
            let d_upper = upper_add(d_hat, d_rounding);

            let midpoint_error = midpoint_kernel_error(metadata, m_hat);
            let mut p4_hat = 0.0;
            for quarter in 0..=4 * group_frames {
                let q = quarter * 256;
                let cell = q.div_euclid(TAIL_FACTOR_I128);
                let phase = usize::try_from(q.rem_euclid(TAIL_FACTOR_I128)).unwrap();
                let value = if phase == 0 {
                    get(cell)
                } else {
                    debug_assert_eq!(phase, MIDPOINT_PHASE);
                    let start = cell + i128::from(MIDPOINT_OFFSET_MIN);
                    let begin = usize::try_from(start - first).unwrap();
                    midpoint_scalar(&coarse[begin..begin + 24], metadata, 1.0)
                };
                p4_hat = max_exact_magnitude(p4_hat, value);
            }
            let p4_upper = upper_add(p4_hat, midpoint_error);
            let group_upper = upper_add(
                p4_upper,
                upper_add(
                    upper_mul(metadata.a4_upper, d_upper),
                    upper_mul(metadata.b4_upper, m_hat),
                ),
            );

            let mut dense = 0.0;
            for q in 0..=group_frames * TARGET_FACTOR_I128 {
                let cell = q.div_euclid(TAIL_FACTOR_I128);
                let phase = usize::try_from(q.rem_euclid(TAIL_FACTOR_I128)).unwrap();
                let value = if phase == 0 {
                    get(cell)
                } else {
                    let start = cell + i128::from(HQ1024_TAIL_OFFSET_MIN);
                    let begin = usize::try_from(start - first).unwrap();
                    compensated_dot(
                        &coarse[begin..begin + HQ1024_TAIL_OFFSET_COUNT],
                        &HQ1024_TAIL_COEFFICIENTS[phase],
                    )
                };
                dense = max_exact_magnitude(dense, value);
            }
            assert!(
                dense <= group_upper,
                "flat group coefficient inequality failed: dense={dense:.17e} upper={group_upper:.17e}",
            );
        }
    }

    #[cfg(feature = "fast-stage-timing")]
    #[test]
    fn commissioning_stage_timing_accumulates_for_nontrivial_fast_scan() {
        let samples = deterministic_values(8193);
        let certificate = run_fast_private(
            &samples,
            1,
            EdgePolicy::RepeatEndpoints,
            false,
        );
        assert!(certificate.diagnostics.fast_stage_prefix_block_ingest_nanos > 0);
        assert!(certificate.diagnostics.fast_stage_midpoint_survey_nanos > 0);
        assert!(certificate.diagnostics.fast_stage_flat_envelope_nanos > 0);
        assert!(certificate.diagnostics.fast_stage_candidate_refinement_nanos > 0);
        assert!(certificate.diagnostics.fast_stage_nomination_nanos > 0);
        assert!(certificate.diagnostics.fast_stage_proposal_nanos > 0);
        assert!(certificate.diagnostics.fast_stage_finishing_nanos > 0);
        assert!(certificate.diagnostics.fast_stage_finalize_nanos > 0);
    }

}
