//! Certified selective Fast066V2 HQ1024V1 scanner.
//!
//! The cheap path works in the native PCM domain. It proves most regions
//! incapable of beating an already established same-channel HQ4 witness before
//! paying for the 1536-tap first stage. Surviving 32-interval spans receive
//! selective first-stage reconstruction, a local HQ4 survey, the frozen flat
//! curvature bound, and (only when required) deterministic dyadic resolution.
//! No wall clock and no heuristic work quota can discard unresolved coverage.

use std::cmp::Ordering;
use std::sync::OnceLock;

#[cfg(feature = "fast-stage-timing")]
use crate::fast_stage_timing::StageTimer;

use crate::certified_scan::{
    dot_rounding_upper, exact_magnitude, lower_sub, magnitude_bits,
    max_exact_magnitude, max_nonnegative_finite, upper_add, upper_mul,
};
use crate::hq1024_coefficients::{
    HQ1024_HALF_DELAY_COEFFICIENTS,
    HQ1024_NODE_A_UPPER, HQ1024_NODE_B_UPPER, HQ1024_TAIL_COEFFICIENTS,
    HQ1024_TAIL_FACTOR, HQ1024_TAIL_OFFSET_COUNT, HQ1024_TAIL_OFFSET_MAX,
    HQ1024_TAIL_OFFSET_MIN,
};
use crate::qualified_half_delay_fft::QualifiedHalfDelayFft;
use crate::raw_screen_metadata::{FAST_ACCEPT_RATIO_DOWN, RAW_A_UPPER, RAW_B_UPPER};
use crate::{
    CertifiedReconstruction, EdgePolicy, PeakCertificate, PeakInterval, PeakLevel, PeakTier,
    ReconstructionId, SearchDiagnostics, SearchStatus, TruePeakError, TruePeakResult,
    HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER,
};

const RAW_HALO_FRAMES: i128 = 777;
const TILE_INTERVALS: i128 = 4096;
const ROOT_INTERVALS: i128 = 256;
const CHILD_INTERVALS: i128 = 32;
const D_SUMMARY_STARTS: usize = 32;
const DENSE_HALF_KNOT_THRESHOLD: usize = 128;
const DIRECT_HALF_ROUNDING_OPS: usize = 3072;
const DIRECT_HALF_EXTREME_ROUNDING_OPS: usize = 6144;
const MIDPOINT_ROUNDING_OPS: usize = 48;
// The scale-aware exceptional graph performs two input scales per symmetric
// pair plus pair/multiply/accumulate work and one final rescale: 61 binary64
// operations. Keep a simple conservative power-of-two account.
const MIDPOINT_EXTREME_ROUNDING_OPS: usize = 64;
const TAIL_ROUNDING_OPS: usize = 96;
// The scale-aware scalar tail graph performs 34 scale/multiply/accumulate
// triples plus one final rescale: 103 binary64 operations.
const TAIL_EXTREME_ROUNDING_OPS: usize = 128;
const SECOND_DIFFERENCE_ROUNDING_OPS: usize = 8;
const MIDPOINT_PHASE: usize = HQ1024_TAIL_FACTOR / 2;
const MIDPOINT_OFFSET_MIN: i32 = -11;
const MIDPOINT_OFFSET_MAX: i32 = 12;
const MIDPOINT_SYMMETRIC_TERMS: usize = 12;
const F64_MIN_NORMAL_BITS: u64 = 0x0010_0000_0000_0000;
const F64_INFINITY_BITS: u64 = 0x7ff0_0000_0000_0000;
const ORDINARY_DOT_MAX_INPUT: f64 = f64::MAX / 64.0;
const HUGE_SCALE: f64 = f64::from_bits(511_u64 << 52); // 2^-512
const HUGE_INVERSE_SCALE: f64 = f64::from_bits(1535_u64 << 52); // 2^512
const RAW_COMPACT_HEAD_FRAMES: usize = 1 << 17;

#[derive(Debug, Clone, Copy)]
struct Evaluation {
    value: f64,
    error: f64,
}

impl Evaluation {
    const ZERO: Self = Self { value: 0.0, error: 0.0 };

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FastExecutionMode {
    Production,
    SurveyBoundsOnly,
    NominationOnly,
}

impl FastExecutionMode {
    #[inline]
    fn stops_after_flat_bounds(self) -> bool {
        matches!(self, Self::SurveyBoundsOnly | Self::NominationOnly)
    }
}

#[derive(Debug, Clone)]
struct FastMetadata {
    midpoint_coefficients: [f64; MIDPOINT_SYMMETRIC_TERMS],
    midpoint_l1_upper: f64,
    phase_l1_upper: [f64; HQ1024_TAIL_FACTOR],
    first_l1_upper: f64,
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
            assert_eq!(left.to_bits(), right.to_bits(), "HQ1024 midpoint symmetry changed");
        }

        let mut phase_l1_upper = [0.0; HQ1024_TAIL_FACTOR];
        for phase in 0..HQ1024_TAIL_FACTOR {
            let mut l1 = 0.0;
            for coefficient in HQ1024_TAIL_COEFFICIENTS[phase] {
                l1 = upper_add(l1, exact_magnitude(coefficient));
            }
            phase_l1_upper[phase] = l1;
        }

        let mut first_l1_upper = 0.0;
        for coefficient in HQ1024_HALF_DELAY_COEFFICIENTS {
            first_l1_upper = upper_add(
                first_l1_upper,
                upper_mul(2.0, exact_magnitude(coefficient)),
            );
        }

        FastMetadata {
            midpoint_coefficients,
            midpoint_l1_upper: phase_l1_upper[MIDPOINT_PHASE],
            phase_l1_upper,
            first_l1_upper,
            a4_upper: max_nonnegative_finite(HQ1024_NODE_A_UPPER[2], HQ1024_NODE_A_UPPER[3]),
            b4_upper: max_nonnegative_finite(HQ1024_NODE_B_UPPER[2], HQ1024_NODE_B_UPPER[3]),
        }
    })
}

#[derive(Debug, Clone)]
struct RawStore {
    channels: usize,
    first_index: Option<i128>,
    head_samples: usize,
    data: Vec<f64>,
}

impl RawStore {
    fn new(channels: usize) -> Self {
        Self { channels, first_index: None, head_samples: 0, data: Vec::new() }
    }

    #[inline]
    fn live_frames(&self) -> usize {
        (self.data.len() - self.head_samples) / self.channels
    }

    #[inline]
    fn last_index(&self) -> Option<i128> {
        self.first_index.map(|first| first + self.live_frames() as i128 - 1)
    }

    fn append(&mut self, start_index: i128, samples: &[f64]) {
        debug_assert_eq!(samples.len() % self.channels, 0);
        if samples.is_empty() {
            return;
        }
        if let Some(last) = self.last_index() {
            debug_assert_eq!(start_index, last + 1);
        } else {
            self.first_index = Some(start_index);
        }
        self.data.extend_from_slice(samples);
    }

    #[inline]
    fn frame_offset(&self, index: i128) -> usize {
        let first = self.first_index.expect("raw store is nonempty");
        debug_assert!(index >= first);
        let frame = usize::try_from(index - first).expect("bounded raw frame offset");
        debug_assert!(frame < self.live_frames());
        self.head_samples + frame * self.channels
    }

    #[inline]
    fn sample(&self, index: i128, channel: usize) -> f64 {
        self.data[self.frame_offset(index) + channel]
    }

    #[inline]
    fn channel_range(&self, channel: usize, first: i128, last: i128) -> (usize, usize) {
        debug_assert!(channel < self.channels);
        debug_assert!(first <= last);
        let first_sample = self.frame_offset(first) + channel;
        let frames = usize::try_from(last - first + 1).expect("bounded raw range");
        debug_assert!(last <= self.last_index().expect("raw store is nonempty"));
        (first_sample, frames)
    }

    fn max_magnitude(&self, channel: usize, first: i128, last: i128) -> f64 {
        let (mut sample_index, frames) = self.channel_range(channel, first, last);
        let mut maximum = 0.0;
        for _ in 0..frames {
            maximum = max_exact_magnitude(maximum, self.data[sample_index]);
            sample_index += self.channels;
        }
        maximum
    }


    fn channel_window(&self, channel: usize, first: i128, last: i128) -> Vec<f64> {
        let (mut sample_index, frames) = self.channel_range(channel, first, last);
        let mut output = Vec::with_capacity(frames);
        for _ in 0..frames {
            output.push(self.data[sample_index]);
            sample_index += self.channels;
        }
        output
    }

    fn discard_before(&mut self, keep_index: i128) {
        let Some(first) = self.first_index else { return; };
        if keep_index <= first {
            return;
        }
        let Some(last) = self.last_index() else { return; };
        let new_first = keep_index.min(last + 1);
        let drop_frames = usize::try_from(new_first - first).expect("raw discard fits usize");
        self.head_samples += drop_frames * self.channels;
        if new_first > last {
            self.data.clear();
            self.head_samples = 0;
            self.first_index = None;
            return;
        }
        self.first_index = Some(new_first);
        if self.head_samples / self.channels >= RAW_COMPACT_HEAD_FRAMES
            && self.head_samples * 2 >= self.data.len()
        {
            self.data.drain(..self.head_samples);
            self.head_samples = 0;
        }
    }
}

#[derive(Debug, Clone)]
struct ChannelRawSummary {
    d_bins: Vec<f64>,
    e_d: f64,
    qualified: bool,
}

impl ChannelRawSummary {
    fn build(raw: &RawStore, channel: usize, start: i128, end: i128) -> Self {
        let raw_first = start - RAW_HALO_FRAMES;
        let raw_last = end + RAW_HALO_FRAMES;
        // Convert absolute coordinates once, then walk the interleaved channel
        // by a fixed stride. The raw pass is intentionally free of per-sample
        // i128 conversion/division.
        let (first_sample, raw_frames) = raw.channel_range(channel, raw_first, raw_last);
        let mut x_max = 0.0;
        let mut qualified = true;
        let mut sample_index = first_sample;
        for _ in 0..raw_frames {
            let sample = raw.data[sample_index];
            let bits = magnitude_bits(sample);
            x_max = max_exact_magnitude(x_max, sample);
            if bits != 0 && bits < F64_MIN_NORMAL_BITS {
                // A subnormal operand can be suppressed before ordinary
                // subtraction on a DAZ host. Fail the cheap screen open.
                qualified = false;
            }
            sample_index += raw.channels;
        }

        let d_count = raw_frames - 2;
        let mut d_bins = Vec::with_capacity((d_count + D_SUMMARY_STARTS - 1) / D_SUMMARY_STARTS);
        let mut bin_max = 0.0;
        let use_scale = x_max > ORDINARY_DOT_MAX_INPUT;
        let mut first_index = first_sample;
        for offset in 0..d_count {
            let middle_index = first_index + raw.channels;
            let last_index = middle_index + raw.channels;
            let mut value = if use_scale {
                let first = raw.data[first_index] * HUGE_SCALE;
                let middle = raw.data[middle_index] * HUGE_SCALE;
                let last = raw.data[last_index] * HUGE_SCALE;
                ((first - middle) - middle) + last
            } else {
                let first = raw.data[first_index];
                let middle = raw.data[middle_index];
                let last = raw.data[last_index];
                ((first - middle) - middle) + last
            };
            if use_scale {
                value *= HUGE_INVERSE_SCALE;
            }
            if !value.is_finite() {
                qualified = false;
                value = 0.0;
            }
            bin_max = max_exact_magnitude(bin_max, value);
            if (offset + 1) % D_SUMMARY_STARTS == 0 || offset + 1 == d_count {
                d_bins.push(bin_max);
                bin_max = 0.0;
            }
            first_index += raw.channels;
        }
        let e_d = dot_rounding_upper(
            SECOND_DIFFERENCE_ROUNDING_OPS,
            upper_mul(4.0, x_max),
        );
        if !e_d.is_finite() {
            qualified = false;
        }
        Self { d_bins, e_d, qualified }
    }

    fn root_d_upper(&self, tile_start: i128, group_start: i128, group_end: i128) -> f64 {
        if !self.qualified {
            return f64::INFINITY;
        }
        // D starts are [group_start-775, group_end+773]. The summary's first
        // D start is tile_start-777, hence offsets +2 and +1550.
        let first = usize::try_from(group_start - tile_start + 2).expect("bounded D offset");
        let last = usize::try_from(group_end - tile_start + 1550).expect("bounded D offset");
        let first_bin = first / D_SUMMARY_STARTS;
        let last_bin = last / D_SUMMARY_STARTS;
        let mut observed = 0.0;
        for value in &self.d_bins[first_bin..=last_bin] {
            observed = max_nonnegative_finite(observed, *value);
        }
        upper_add(observed, self.e_d)
    }
}

#[derive(Debug, Clone, Copy)]
struct ActiveChild {
    start: i128,
    end: i128,
    raw_upper: f64,
}

#[inline]
fn active_priority(left: &ActiveChild, right: &ActiveChild) -> Ordering {
    right
        .raw_upper
        .total_cmp(&left.raw_upper)
        .then_with(|| left.start.cmp(&right.start))
}

fn raw_group_upper(sample_max: f64, d_upper: f64) -> f64 {
    upper_add(
        sample_max,
        upper_add(
            upper_mul(RAW_B_UPPER, sample_max),
            upper_mul(RAW_A_UPPER, d_upper),
        ),
    )
}

#[inline]
fn tolerance_accepts(upper: f64, lower: f64) -> bool {
    let lower_bits = magnitude_bits(lower);
    if !upper.is_finite()
        || lower_bits < F64_MIN_NORMAL_BITS
        || lower_bits >= F64_INFINITY_BITS
    {
        return false;
    }
    let product = lower * FAST_ACCEPT_RATIO_DOWN;
    let bits = magnitude_bits(product);
    if !product.is_finite() || bits == 0 || bits >= F64_INFINITY_BITS {
        return false;
    }
    let threshold = f64::from_bits(bits - 1);
    upper <= threshold
}

#[derive(Debug, Clone)]
struct TileHalfCache {
    first_m: i128,
    values: Vec<Option<Evaluation>>,
    wanted: Vec<bool>,
    dense_source: bool,
}

impl TileHalfCache {
    fn new(tile_start: i128, tile_end: i128, ranges: &[(i128, i128)]) -> Self {
        let first_m = tile_start - 8;
        let last_m = tile_end + 7;
        let len = usize::try_from(last_m - first_m + 1).expect("bounded half cache");
        let mut wanted = vec![false; len];
        for &(first, last) in ranges {
            for m in first..=last {
                wanted[usize::try_from(m - first_m).unwrap()] = true;
            }
        }
        Self { first_m, values: vec![None; len], wanted, dense_source: false }
    }

    #[inline]
    fn offset(&self, m: i128) -> usize {
        usize::try_from(m - self.first_m).expect("bounded half-cache index")
    }

    fn set(&mut self, m: i128, evaluation: Evaluation) {
        let offset = self.offset(m);
        self.values[offset] = Some(evaluation);
    }

    fn get(&self, m: i128) -> Evaluation {
        self.values[self.offset(m)].expect("requested half knot was reconstructed")
    }
}

fn child_ranges(children: &[ActiveChild]) -> Vec<(i128, i128)> {
    let mut ranges = children
        .iter()
        .map(|child| (child.start - 8, child.end + 7))
        .collect::<Vec<_>>();
    merge_ranges(&mut ranges)
}

fn merge_ranges(ranges: &mut Vec<(i128, i128)>) -> Vec<(i128, i128)> {
    if ranges.is_empty() {
        return Vec::new();
    }
    ranges.sort_unstable_by_key(|range| range.0);
    let mut merged = Vec::with_capacity(ranges.len());
    let mut current = ranges[0];
    for &(first, last) in &ranges[1..] {
        if first <= current.1 + 1 {
            current.1 = current.1.max(last);
        } else {
            merged.push(current);
            current = (first, last);
        }
    }
    merged.push(current);
    merged
}

fn union_count(first: &[(i128, i128)], second: &[(i128, i128)]) -> usize {
    let mut ranges = Vec::with_capacity(first.len() + second.len());
    ranges.extend_from_slice(first);
    ranges.extend_from_slice(second);
    merge_ranges(&mut ranges)
        .into_iter()
        .map(|(a, b)| usize::try_from(b - a + 1).unwrap())
        .sum()
}

fn scale_back(value: f64, inverse_scale: f64) -> Result<f64, TruePeakError> {
    let value = value * inverse_scale;
    value.is_finite().then_some(value).ok_or(TruePeakError::NumericalOverflow)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
unsafe fn direct_half_batch4_avx(
    input: &[f64],
    left_anchor: usize,
    right_anchor: usize,
    output: &mut [f64; 4],
) {
    use core::arch::x86_64::{
        _mm256_add_pd, _mm256_loadu_pd, _mm256_mul_pd, _mm256_set1_pd,
        _mm256_setzero_pd, _mm256_storeu_pd,
    };
    let mut acc = _mm256_setzero_pd();
    for j in 0..HQ1024_HALF_DELAY_COEFFICIENTS.len() {
        let pair = _mm256_add_pd(
            _mm256_loadu_pd(input.as_ptr().add(left_anchor - j)),
            _mm256_loadu_pd(input.as_ptr().add(right_anchor + j)),
        );
        acc = _mm256_add_pd(
            acc,
            _mm256_mul_pd(
                pair,
                _mm256_set1_pd(HQ1024_HALF_DELAY_COEFFICIENTS[j]),
            ),
        );
    }
    _mm256_storeu_pd(output.as_mut_ptr(), acc);
}

fn direct_half_scalar(
    input: &[f64],
    left_anchor: usize,
    right_anchor: usize,
    scale: f64,
    inverse_scale: f64,
) -> Result<f64, TruePeakError> {
    let mut sum = 0.0;
    if scale.to_bits() == 1.0_f64.to_bits() {
        // Ordinary authority graph: one pair add, one coefficient multiply,
        // and one accumulator add per symmetric term. Keep the no-op scaling
        // out of this graph so the 3072-operation enclosure is source-true.
        for j in 0..HQ1024_HALF_DELAY_COEFFICIENTS.len() {
            let pair = input[left_anchor - j] + input[right_anchor + j];
            sum += pair * HQ1024_HALF_DELAY_COEFFICIENTS[j];
        }
        return sum
            .is_finite()
            .then_some(sum)
            .ok_or(TruePeakError::NumericalOverflow);
    }
    for j in 0..HQ1024_HALF_DELAY_COEFFICIENTS.len() {
        let left = input[left_anchor - j] * scale;
        let right = input[right_anchor + j] * scale;
        sum += (left + right) * HQ1024_HALF_DELAY_COEFFICIENTS[j];
    }
    scale_back(sum, inverse_scale)
}

fn execute_direct_ranges(
    raw: &RawStore,
    channel: usize,
    ranges: &[(i128, i128)],
    cache: &mut TileHalfCache,
    metadata: &FastMetadata,
    use_avx: bool,
) -> Result<usize, TruePeakError> {
    let mut count = 0usize;
    for &(first, last) in ranges {
        // Scope the copied mono slice to this merged request's exact FIR
        // support. Sparse work therefore never copies the full tile halo.
        let input_first = first - 767;
        let input_last = last + 768;
        let input = raw.channel_window(channel, input_first, input_last);
        // Reduce the support maximum in IEEE magnitude encodings. This keeps
        // nonzero classification explicit under DAZ/FTZ and avoids round-
        // tripping the accumulator through `f64` for every input sample.
        let input_max_bits = input
            .iter()
            .copied()
            .map(magnitude_bits)
            .max()
            .unwrap_or(0);
        if input_max_bits == 0 {
            for m in first..=last {
                cache.set(m, Evaluation::ZERO);
                count += 1;
            }
            continue;
        }
        let input_max = f64::from_bits(input_max_bits);

        let extreme = input_max > ORDINARY_DOT_MAX_INPUT;
        let operations = if extreme {
            DIRECT_HALF_EXTREME_ROUNDING_OPS
        } else {
            DIRECT_HALF_ROUNDING_OPS
        };
        let error = dot_rounding_upper(
            operations,
            upper_mul(metadata.first_l1_upper, input_max),
        );
        if !error.is_finite() {
            return Err(TruePeakError::NumericalOverflow);
        }
        // A nonzero source support can legitimately produce an exact-zero
        // numerical value under DAZ/FTZ, but its authoritative enclosure must
        // never collapse to exact zero with it. The ordinary derivation
        // already contains a larger absolute underflow allowance; this final
        // boundary guard makes the fail-closed invariant explicit.
        let error = if magnitude_bits(error) < F64_MIN_NORMAL_BITS {
            f64::MIN_POSITIVE
        } else {
            error
        };

        let mut m = first;
        #[cfg(target_arch = "x86_64")]
        if use_avx && !extreme {
            while m + 3 <= last {
                let left_anchor = usize::try_from(m + 768 - input_first)
                    .expect("bounded direct left anchor");
                let right_anchor = usize::try_from(m - 767 - input_first)
                    .expect("bounded direct right anchor");
                let mut values = [0.0; 4];
                // SAFETY: dispatch is checked once per meter. Each lane is an
                // independent consecutive output and the range-local input
                // slice contains the complete support through `last + 768`.
                unsafe {
                    direct_half_batch4_avx(
                        &input,
                        left_anchor,
                        right_anchor,
                        &mut values,
                    )
                };
                if values.iter().any(|value| !value.is_finite()) {
                    return Err(TruePeakError::NumericalOverflow);
                }
                for (lane, value) in values.into_iter().enumerate() {
                    cache.set(m + lane as i128, Evaluation { value, error });
                }
                m += 4;
                count += 4;
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = use_avx;

        let (scale, inverse_scale) = if extreme {
            (HUGE_SCALE, HUGE_INVERSE_SCALE)
        } else {
            (1.0, 1.0)
        };
        while m <= last {
            let left_anchor = usize::try_from(m + 768 - input_first)
                .expect("bounded direct left anchor");
            let right_anchor = usize::try_from(m - 767 - input_first)
                .expect("bounded direct right anchor");
            let value = direct_half_scalar(
                &input,
                left_anchor,
                right_anchor,
                scale,
                inverse_scale,
            )?;
            cache.set(m, Evaluation { value, error });
            m += 1;
            count += 1;
        }
    }
    Ok(count)
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
unsafe fn midpoint_batch_avx(input: &[f64], metadata: &FastMetadata, output: &mut [f64]) {
    use core::arch::x86_64::{
        _mm256_add_pd, _mm256_loadu_pd, _mm256_mul_pd, _mm256_set1_pd,
        _mm256_setzero_pd, _mm256_storeu_pd,
    };
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
            // SAFETY: caller checked AVX support and the helper bounds all loads.
            unsafe { midpoint_batch_avx(input, metadata, output) };
            return output
                .iter()
                .all(|value| value.is_finite())
                .then_some(())
                .ok_or(TruePeakError::NumericalOverflow);
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
            let value = midpoint_scalar(&input[index..index + 24], metadata, HUGE_SCALE);
            output[index] = scale_back(value, HUGE_INVERSE_SCALE)?;
        }
    }
    Ok(())
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
unsafe fn tail_dot_avx(input: &[f64], row: &[f64; HQ1024_TAIL_OFFSET_COUNT]) -> f64 {
    use core::arch::x86_64::{
        __m256d, _mm256_add_pd, _mm256_loadu_pd, _mm256_mul_pd, _mm256_storeu_pd,
    };
    let mut acc: __m256d = _mm256_mul_pd(
        _mm256_loadu_pd(input.as_ptr()),
        _mm256_loadu_pd(row.as_ptr()),
    );
    let mut index = 4;
    while index + 4 <= 32 {
        acc = _mm256_add_pd(
            acc,
            _mm256_mul_pd(
                _mm256_loadu_pd(input.as_ptr().add(index)),
                _mm256_loadu_pd(row.as_ptr().add(index)),
            ),
        );
        index += 4;
    }
    let mut lanes = [0.0; 4];
    _mm256_storeu_pd(lanes.as_mut_ptr(), acc);
    let mut sum = ((lanes[0] + lanes[1]) + lanes[2]) + lanes[3];
    sum += input[32] * row[32];
    sum += input[33] * row[33];
    sum
}

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
            // SAFETY: AVX was detected once per meter; input is exactly 34 values.
            let value = unsafe { tail_dot_avx(input, row) };
            return value.is_finite().then_some(value).ok_or(TruePeakError::NumericalOverflow);
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = use_avx;
        let mut sum = 0.0;
        for index in 0..HQ1024_TAIL_OFFSET_COUNT {
            sum += input[index] * row[index];
        }
        sum.is_finite().then_some(sum).ok_or(TruePeakError::NumericalOverflow)
    } else {
        let mut sum = 0.0;
        for index in 0..HQ1024_TAIL_OFFSET_COUNT {
            sum += (input[index] * HUGE_SCALE) * row[index];
        }
        scale_back(sum, HUGE_INVERSE_SCALE)
    }
}

#[derive(Debug, Clone)]
struct LocalCoarse {
    first: i128,
    values: Vec<Evaluation>,
}

impl LocalCoarse {
    fn build(raw: &RawStore, channel: usize, cache: &TileHalfCache, start: i128, end: i128) -> Self {
        let first = 2 * start - 16;
        let native_first = start - 8;
        let native_last = end + 8;
        let originals = raw.channel_window(channel, native_first, native_last);
        let original_count = originals.len();
        let cache_first = cache.offset(native_first);
        let mut values = Vec::with_capacity(original_count * 2 - 1);
        for (offset, sample) in originals.into_iter().enumerate() {
            values.push(Evaluation { value: sample, error: 0.0 });
            if offset + 1 < original_count {
                values.push(
                    cache.values[cache_first + offset]
                        .expect("requested half knot was reconstructed"),
                );
            }
        }
        debug_assert_eq!(values.len(), usize::try_from(2 * (end - start) + 33).unwrap());
        Self { first, values }
    }

    #[inline]
    fn offset(&self, index: i128) -> usize {
        usize::try_from(index - self.first).expect("bounded local coarse index")
    }

    #[inline]
    fn get(&self, index: i128) -> Evaluation {
        self.values[self.offset(index)]
    }

    fn slice_values(&self, first: i128, last: i128) -> Vec<f64> {
        self.values[self.offset(first)..=self.offset(last)]
            .iter()
            .map(|evaluation| evaluation.value)
            .collect()
    }

    fn support_bounds(&self, first: i128, last: i128) -> (f64, f64) {
        let mut value_max = 0.0;
        let mut error_max = 0.0;
        for evaluation in &self.values[self.offset(first)..=self.offset(last)] {
            value_max = max_exact_magnitude(value_max, evaluation.value);
            error_max = max_nonnegative_finite(error_max, evaluation.error);
        }
        (value_max, error_max)
    }
}

#[derive(Debug, Clone)]
struct LocalSurvey {
    first_q4: i128,
    values: Vec<Evaluation>,
}

impl LocalSurvey {
    fn get(&self, q4: i128) -> Evaluation {
        self.values[usize::try_from(q4 - self.first_q4).expect("bounded survey index")]
    }
}

fn second_difference_upper(first: Evaluation, middle: Evaluation, last: Evaluation) -> f64 {
    let approximate = ((first.value - middle.value) - middle.value) + last.value;
    if !approximate.is_finite() {
        return f64::INFINITY;
    }
    let propagated = upper_add(
        upper_add(first.error, upper_mul(2.0, middle.error)),
        last.error,
    );
    let operation_scale = upper_add(
        upper_add(exact_magnitude(first.value), upper_mul(2.0, exact_magnitude(middle.value))),
        exact_magnitude(last.value),
    );
    upper_add(
        upper_add(exact_magnitude(approximate), propagated),
        dot_rounding_upper(SECOND_DIFFERENCE_ROUNDING_OPS, operation_scale),
    )
}

fn local_flat_upper(
    coarse: &LocalCoarse,
    survey: &LocalSurvey,
    start: i128,
    end: i128,
    metadata: &FastMetadata,
) -> f64 {
    let support_first = 2 * start - 16;
    let support_last = 2 * end + 16;
    let first_offset = coarse.offset(support_first);
    let last_offset = coarse.offset(support_last);
    let support = &coarse.values[first_offset..=last_offset];
    let mut magnitude_upper = 0.0;
    let mut d2_upper = 0.0;
    for &evaluation in support {
        magnitude_upper = max_nonnegative_finite(magnitude_upper, evaluation.upper());
    }
    for window in support.windows(3) {
        d2_upper = max_nonnegative_finite(
            d2_upper,
            second_difference_upper(window[0], window[1], window[2]),
        );
    }
    let mut p4_upper = 0.0;
    for evaluation in &survey.values {
        p4_upper = max_nonnegative_finite(p4_upper, evaluation.upper());
    }
    upper_add(
        p4_upper,
        upper_add(
            upper_mul(metadata.a4_upper, d2_upper),
            upper_mul(metadata.b4_upper, magnitude_upper),
        ),
    )
}

#[derive(Debug, Clone, Copy)]
struct CellComponents {
    d2_upper: f64,
    magnitude_upper: f64,
}

fn cell_components(coarse: &LocalCoarse, cell: i128) -> CellComponents {
    let first = cell + i128::from(HQ1024_TAIL_OFFSET_MIN);
    let last = cell + i128::from(HQ1024_TAIL_OFFSET_MAX);
    let first_offset = coarse.offset(first);
    let last_offset = coarse.offset(last);
    let support = &coarse.values[first_offset..=last_offset];
    let mut magnitude_upper = 0.0;
    let mut d2_upper = 0.0;
    for &evaluation in support {
        magnitude_upper = max_nonnegative_finite(magnitude_upper, evaluation.upper());
    }
    for window in support.windows(3) {
        d2_upper = max_nonnegative_finite(
            d2_upper,
            second_difference_upper(window[0], window[1], window[2]),
        );
    }
    CellComponents { d2_upper, magnitude_upper }
}

fn node_upper(
    tree_index: usize,
    width: usize,
    left: Evaluation,
    right: Evaluation,
    components: CellComponents,
) -> f64 {
    let endpoint = max_nonnegative_finite(left.upper(), right.upper());
    if width == 1 {
        return endpoint;
    }
    debug_assert!(tree_index > 0 && tree_index < HQ1024_TAIL_FACTOR);
    upper_add(
        endpoint,
        upper_add(
            upper_mul(HQ1024_NODE_A_UPPER[tree_index], components.d2_upper),
            upper_mul(HQ1024_NODE_B_UPPER[tree_index], components.magnitude_upper),
        ),
    )
}

#[derive(Debug, Clone, Copy)]
struct ResolveNode {
    tree_index: usize,
    low_phase: usize,
    high_phase: usize,
    left: Evaluation,
    right: Evaluation,
    upper: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct ResolveResult {
    terminal_upper: f64,
    needs_direct_tightening: bool,
}

#[derive(Debug, Clone)]
struct DensePairResult {
    first_m: i128,
    first: Vec<Option<Evaluation>>,
    second: Vec<Option<Evaluation>>,
    emitted_frames: usize,
}

#[derive(Debug, Clone)]
struct FastState {
    channels: usize,
    metadata: &'static FastMetadata,
    use_avx: bool,
    raw: RawStore,
    nominal_frames_seen: u64,
    next_tile_start: i128,
    channel_sample_peaks: Vec<f64>,
    channel_hq4_peaks: Vec<f64>,
    channel_point_peaks: Vec<f64>,
    channel_l4_lower: Vec<f64>,
    channel_lower_peaks: Vec<f64>,
    channel_terminal_upper: Vec<f64>,
    max_evaluation_error: f64,
    diagnostics: SearchDiagnostics,
    execution_mode: FastExecutionMode,
    dense_prefix: Option<QualifiedHalfDelayFft>,
}

impl FastState {
    fn new(channels: usize, execution_mode: FastExecutionMode) -> Self {
        #[cfg(target_arch = "x86_64")]
        let use_avx = std::is_x86_feature_detected!("avx");
        #[cfg(not(target_arch = "x86_64"))]
        let use_avx = false;
        let mut diagnostics = SearchDiagnostics::default();
        diagnostics.accelerated_same_graph_avx_prefix_active = use_avx;
        Self {
            channels,
            metadata: fast_metadata(),
            use_avx,
            raw: RawStore::new(channels),
            nominal_frames_seen: 0,
            next_tile_start: 0,
            channel_sample_peaks: vec![0.0; channels],
            channel_hq4_peaks: vec![0.0; channels],
            channel_point_peaks: vec![0.0; channels],
            channel_l4_lower: vec![0.0; channels],
            channel_lower_peaks: vec![0.0; channels],
            channel_terminal_upper: vec![0.0; channels],
            max_evaluation_error: 0.0,
            diagnostics,
            execution_mode,
            dense_prefix: None,
        }
    }

    fn append_nominal(&mut self, start: i128, samples: &[f64]) -> Result<(), TruePeakError> {
        debug_assert_eq!(samples.len() % self.channels, 0);
        let frames = samples.len() / self.channels;
        self.raw.append(start, samples);
        let frames_u64 = u64::try_from(frames).map_err(|_| TruePeakError::InputTooLong)?;
        self.nominal_frames_seen = self
            .nominal_frames_seen
            .checked_add(frames_u64)
            .ok_or(TruePeakError::InputTooLong)?;
        self.process_ready_full_tiles()
    }

    fn ready_for_tile(&self, start: i128, end: i128) -> bool {
        self.raw.first_index.is_some_and(|first| first <= start - RAW_HALO_FRAMES)
            && self.raw.last_index().is_some_and(|last| last >= end + RAW_HALO_FRAMES)
    }

    fn process_ready_full_tiles(&mut self) -> Result<(), TruePeakError> {
        loop {
            let end = self.next_tile_start + TILE_INTERVALS;
            let final_frame = self.nominal_frames_seen as i128 - 1;
            if end >= final_frame || !self.ready_for_tile(self.next_tile_start, end) {
                return Ok(());
            }
            self.process_tile(self.next_tile_start, end)?;
            self.next_tile_start = end;
            self.raw.discard_before(self.next_tile_start - RAW_HALO_FRAMES);
        }
    }

    fn process_eof_tiles(&mut self) -> Result<(), TruePeakError> {
        let final_frame = self.nominal_frames_seen as i128 - 1;
        while self.next_tile_start + TILE_INTERVALS < final_frame {
            let end = self.next_tile_start + TILE_INTERVALS;
            debug_assert!(self.ready_for_tile(self.next_tile_start, end));
            self.process_tile(self.next_tile_start, end)?;
            self.next_tile_start = end;
            self.raw.discard_before(self.next_tile_start - RAW_HALO_FRAMES);
        }
        if self.next_tile_start < final_frame {
            debug_assert!(self.ready_for_tile(self.next_tile_start, final_frame));
            self.process_tile(self.next_tile_start, final_frame)?;
            self.next_tile_start = final_frame;
            self.raw.discard_before(self.next_tile_start - RAW_HALO_FRAMES);
        } else if final_frame == 0 && self.next_tile_start == 0 {
            for channel in 0..self.channels {
                let sample = self.raw.sample(0, channel);
                let magnitude = exact_magnitude(sample);
                self.channel_sample_peaks[channel] = magnitude;
                self.channel_hq4_peaks[channel] = magnitude;
                self.channel_point_peaks[channel] = magnitude;
                self.channel_l4_lower[channel] = magnitude;
                self.channel_lower_peaks[channel] = magnitude;
            }
            self.diagnostics.authoritative_coarse_values = self.channels as u64;
            self.diagnostics.fast_survey_knots = self.channels as u64;
        }
        Ok(())
    }

    fn observe_sample_tile(&mut self, channel: usize, start: i128, end: i128) {
        let peak = self.raw.max_magnitude(channel, start, end);
        self.channel_hq4_peaks[channel] = max_nonnegative_finite(self.channel_hq4_peaks[channel], peak);
        self.channel_point_peaks[channel] = max_nonnegative_finite(self.channel_point_peaks[channel], peak);
        self.channel_l4_lower[channel] = max_nonnegative_finite(self.channel_l4_lower[channel], peak);
        self.channel_lower_peaks[channel] = max_nonnegative_finite(self.channel_lower_peaks[channel], peak);
        let unique = u64::try_from(end - start + 1 - (if start > 0 { 1 } else { 0 })).unwrap();
        self.diagnostics.fast_survey_knots = self.diagnostics.fast_survey_knots.saturating_add(unique);
        self.diagnostics.authoritative_coarse_values = self
            .diagnostics
            .authoritative_coarse_values
            .saturating_add(unique);
    }

    fn screen_channel(
        &mut self,
        channel: usize,
        start: i128,
        end: i128,
        summary: &ChannelRawSummary,
    ) -> Vec<ActiveChild> {
        let mut active = Vec::new();
        let mut root_start = start;
        while root_start < end {
            let root_end = (root_start + ROOT_INTERVALS).min(end);
            let d_upper = summary.root_d_upper(start, root_start, root_end);
            let root_s = self.raw.max_magnitude(channel, root_start, root_end);
            let root_upper = raw_group_upper(root_s, d_upper);
            if summary.qualified
                && root_upper.is_finite()
                && root_upper <= self.channel_l4_lower[channel]
            {
                self.diagnostics.groups_rejected = self.diagnostics.groups_rejected.saturating_add(1);
                root_start = root_end;
                continue;
            }

            let mut child_start = root_start;
            while child_start < root_end {
                let child_end = (child_start + CHILD_INTERVALS).min(root_end);
                let sample_max = self.raw.max_magnitude(channel, child_start, child_end);
                let child_upper = raw_group_upper(sample_max, d_upper);
                if summary.qualified
                    && child_upper.is_finite()
                    && child_upper <= self.channel_l4_lower[channel]
                {
                    self.diagnostics.groups_rejected = self.diagnostics.groups_rejected.saturating_add(1);
                } else {
                    active.push(ActiveChild { start: child_start, end: child_end, raw_upper: child_upper });
                    self.diagnostics.groups_expanded = self.diagnostics.groups_expanded.saturating_add(1);
                }
                child_start = child_end;
            }
            root_start = root_end;
        }
        active.sort_by(active_priority);
        active
    }

    fn execute_dense_pair(
        &mut self,
        tile_start: i128,
        tile_end: i128,
        first_channel: usize,
        second_channel: Option<usize>,
        first_cache: &TileHalfCache,
        second_cache: Option<&TileHalfCache>,
    ) -> Result<DensePairResult, TruePeakError> {
        let window_start = tile_start - RAW_HALO_FRAMES;
        let window_end = tile_end + RAW_HALO_FRAMES;
        let frames = usize::try_from(window_end - window_start + 1).expect("bounded dense window");
        let need_first = first_cache.wanted.iter().any(|wanted| *wanted);
        let need_second = second_cache.is_some_and(|cache| cache.wanted.iter().any(|wanted| *wanted));
        let mut packed = Vec::with_capacity(frames * 2);
        let (mut first_sample, _) = self.raw.channel_range(first_channel, window_start, window_end);
        let mut second_sample = second_channel.map(|channel| {
            self.raw.channel_range(channel, window_start, window_end).0
        });
        for _ in 0..frames {
            packed.push(if need_first { self.raw.data[first_sample] } else { 0.0 });
            packed.push(if need_second {
                self.raw.data[second_sample.expect("second requested channel exists")]
            } else {
                0.0
            });
            first_sample += self.raw.channels;
            if let Some(sample) = &mut second_sample {
                *sample += self.raw.channels;
            }
        }

        let first_m = tile_start - 8;
        let len = usize::try_from(tile_end - tile_start + 16).expect("bounded dense result");
        let mut first = vec![None; len];
        let mut second = vec![None; len];
        let wanted_first = first_cache.wanted.clone();
        let wanted_second = second_cache.map(|cache| cache.wanted.clone()).unwrap_or_else(|| vec![false; len]);
        let prefix = self.dense_prefix.get_or_insert_with(|| {
            QualifiedHalfDelayFft::new_fast90(ReconstructionId::Hq1024V1, 2)
        });
        self.diagnostics.accelerated_same_graph_avx_prefix_active = prefix.fast90_same_graph_avx_active();
        let mut emitted_frames = 0usize;
        let consumed = prefix.process_finite_window_blocks(&packed, window_start, |block| {
            emitted_frames += block.frames;
            let block_first_m = block.first_coarse_index.div_euclid(2);
            for frame in 0..block.frames {
                let m = block_first_m + frame as i128;
                let offset = m - first_m;
                if offset < 0 {
                    continue;
                }
                let Ok(index) = usize::try_from(offset) else { continue; };
                if index >= len {
                    continue;
                }
                if wanted_first[index] {
                    first[index] = Some(Evaluation {
                        value: block.half[frame * 2],
                        error: block.half_error_by_channel[0],
                    });
                }
                if wanted_second[index] {
                    second[index] = Some(Evaluation {
                        value: block.half[frame * 2 + 1],
                        error: block.half_error_by_channel[1],
                    });
                }
            }
        });
        debug_assert_eq!(consumed, frames);
        Ok(DensePairResult { first_m, first, second, emitted_frames })
    }

    fn apply_dense_result(cache: &mut TileHalfCache, values: &[Option<Evaluation>], first_m: i128) -> usize {
        let mut retained = 0usize;
        for (index, evaluation) in values.iter().copied().enumerate() {
            if let Some(evaluation) = evaluation {
                cache.set(first_m + index as i128, evaluation);
                retained += 1;
            }
        }
        cache.dense_source = true;
        retained
    }

    fn observe_hq4(&mut self, channel: usize, evaluation: Evaluation) {
        self.max_evaluation_error = max_nonnegative_finite(self.max_evaluation_error, evaluation.error);
        self.channel_point_peaks[channel] = max_exact_magnitude(self.channel_point_peaks[channel], evaluation.value);
        self.channel_hq4_peaks[channel] = max_exact_magnitude(self.channel_hq4_peaks[channel], evaluation.value);
        self.channel_l4_lower[channel] = max_nonnegative_finite(self.channel_l4_lower[channel], evaluation.lower());
        self.channel_lower_peaks[channel] = max_nonnegative_finite(self.channel_lower_peaks[channel], evaluation.lower());
    }

    fn observe_target(&mut self, channel: usize, evaluation: Evaluation) {
        self.max_evaluation_error = max_nonnegative_finite(self.max_evaluation_error, evaluation.error);
        self.channel_point_peaks[channel] = max_exact_magnitude(self.channel_point_peaks[channel], evaluation.value);
        self.channel_lower_peaks[channel] = max_nonnegative_finite(self.channel_lower_peaks[channel], evaluation.lower());
    }

    fn build_local_survey(
        &mut self,
        channel: usize,
        coarse: &LocalCoarse,
        start: i128,
        end: i128,
        count_unique: bool,
    ) -> Result<LocalSurvey, TruePeakError> {
        let first_q4 = 4 * start;
        let last_q4 = 4 * end;
        let len = usize::try_from(last_q4 - first_q4 + 1).expect("bounded local survey");
        let mut values = vec![Evaluation::ZERO; len];

        let first_mid_cell = 2 * start;
        let last_mid_cell = 2 * end - 1;
        let midpoint_count = usize::try_from(last_mid_cell - first_mid_cell + 1).unwrap();
        let midpoint_input_first = first_mid_cell + i128::from(MIDPOINT_OFFSET_MIN);
        let midpoint_input_last = last_mid_cell + i128::from(MIDPOINT_OFFSET_MAX);
        let midpoint_input = coarse.slice_values(midpoint_input_first, midpoint_input_last);
        let (midpoint_input_max, midpoint_coarse_error) =
            coarse.support_bounds(midpoint_input_first, midpoint_input_last);
        let midpoint_error = upper_add(
            upper_mul(self.metadata.midpoint_l1_upper, midpoint_coarse_error),
            dot_rounding_upper(
                if midpoint_input_max > ORDINARY_DOT_MAX_INPUT {
                    MIDPOINT_EXTREME_ROUNDING_OPS
                } else {
                    MIDPOINT_ROUNDING_OPS
                },
                upper_mul(self.metadata.midpoint_l1_upper, midpoint_input_max),
            ),
        );
        if !midpoint_error.is_finite() {
            return Err(TruePeakError::NumericalOverflow);
        }
        let mut midpoint_values = vec![0.0; midpoint_count];
        midpoint_batch(
            &midpoint_input,
            midpoint_input_max,
            self.metadata,
            self.use_avx,
            &mut midpoint_values,
        )?;

        let coarse_count = usize::try_from(2 * (end - start) + 1).unwrap();
        let coarse_first_offset = coarse.offset(first_mid_cell);
        for offset in 0..coarse_count {
            let coarse_evaluation = coarse.values[coarse_first_offset + offset];
            let q4_offset = 2 * offset;
            values[q4_offset] = coarse_evaluation;
            self.observe_hq4(channel, coarse_evaluation);
            if offset < midpoint_values.len() {
                let midpoint_evaluation = Evaluation {
                    value: midpoint_values[offset],
                    error: midpoint_error,
                };
                values[q4_offset + 1] = midpoint_evaluation;
                self.observe_hq4(channel, midpoint_evaluation);
            }
        }
        // Native samples were already reduced exactly once by the tile screen.
        // Each active interval contributes one first-stage half knot and two
        // midpoint knots to the unique physical HQ4 work count.
        if count_unique {
            let additional = u64::try_from(3 * (end - start)).unwrap();
            self.diagnostics.fast_survey_knots = self.diagnostics.fast_survey_knots.saturating_add(additional);
        }
        self.diagnostics.phase_evaluations = self
            .diagnostics
            .phase_evaluations
            .saturating_add(u64::try_from(midpoint_count).unwrap());
        Ok(LocalSurvey { first_q4, values })
    }

    fn tail_evaluation(
        &mut self,
        channel: usize,
        coarse: &LocalCoarse,
        cell: i128,
        phase: usize,
    ) -> Result<Evaluation, TruePeakError> {
        debug_assert!(phase > 0 && phase < HQ1024_TAIL_FACTOR);
        let first = cell + i128::from(HQ1024_TAIL_OFFSET_MIN);
        let last = cell + i128::from(HQ1024_TAIL_OFFSET_MAX);
        let input = coarse.slice_values(first, last);
        let (input_max, coarse_error) = coarse.support_bounds(first, last);
        let l1 = self.metadata.phase_l1_upper[phase];
        let error = upper_add(
            upper_mul(l1, coarse_error),
            dot_rounding_upper(
                if input_max > ORDINARY_DOT_MAX_INPUT {
                    TAIL_EXTREME_ROUNDING_OPS
                } else {
                    TAIL_ROUNDING_OPS
                },
                upper_mul(l1, input_max),
            ),
        );
        if !error.is_finite() {
            return Err(TruePeakError::NumericalOverflow);
        }
        let value = tail_dot(&input, phase, input_max, self.use_avx)?;
        let evaluation = Evaluation { value, error };
        self.observe_target(channel, evaluation);
        self.diagnostics.phase_evaluations = self.diagnostics.phase_evaluations.saturating_add(1);
        self.diagnostics.refined_cells = self.diagnostics.refined_cells.saturating_add(1);
        Ok(evaluation)
    }

    fn resolve_span(
        &mut self,
        channel: usize,
        coarse: &LocalCoarse,
        survey: &LocalSurvey,
        start: i128,
        end: i128,
    ) -> Result<ResolveResult, TruePeakError> {
        let mut result = ResolveResult::default();
        for cell in 2 * start..2 * end {
            self.diagnostics.candidate_cells = self.diagnostics.candidate_cells.saturating_add(1);
            let components = cell_components(coarse, cell);
            let phase0 = coarse.get(cell);
            let phase256 = survey.get(2 * cell + 1);
            let phase512 = coarse.get(cell + 1);
            let left = ResolveNode {
                tree_index: 2,
                low_phase: 0,
                high_phase: MIDPOINT_PHASE,
                left: phase0,
                right: phase256,
                upper: node_upper(2, MIDPOINT_PHASE, phase0, phase256, components),
            };
            let right = ResolveNode {
                tree_index: 3,
                low_phase: MIDPOINT_PHASE,
                high_phase: HQ1024_TAIL_FACTOR,
                left: phase256,
                right: phase512,
                upper: node_upper(3, MIDPOINT_PHASE, phase256, phase512, components),
            };
            let mut stack = Vec::with_capacity(18);
            // Higher upper first; ties resolve toward the lower HQ coordinate.
            if left.upper >= right.upper {
                stack.push(right);
                stack.push(left);
            } else {
                stack.push(left);
                stack.push(right);
            }

            while let Some(node) = stack.pop() {
                let lower = self.channel_lower_peaks[channel];
                if node.upper <= lower {
                    continue;
                }
                if tolerance_accepts(node.upper, lower) {
                    result.terminal_upper = max_nonnegative_finite(result.terminal_upper, node.upper);
                    continue;
                }
                let width = node.high_phase - node.low_phase;
                if width == 1 {
                    result.terminal_upper = max_nonnegative_finite(result.terminal_upper, node.upper);
                    result.needs_direct_tightening = true;
                    continue;
                }

                let middle_phase = (node.low_phase + node.high_phase) / 2;
                let middle = if middle_phase == MIDPOINT_PHASE {
                    phase256
                } else {
                    self.tail_evaluation(channel, coarse, cell, middle_phase)?
                };
                let child_width = width / 2;
                let left_index = node.tree_index * 2;
                let right_index = left_index + 1;
                let left_child = ResolveNode {
                    tree_index: left_index,
                    low_phase: node.low_phase,
                    high_phase: middle_phase,
                    left: node.left,
                    right: middle,
                    upper: node_upper(left_index, child_width, node.left, middle, components),
                };
                let right_child = ResolveNode {
                    tree_index: right_index,
                    low_phase: middle_phase,
                    high_phase: node.high_phase,
                    left: middle,
                    right: node.right,
                    upper: node_upper(right_index, child_width, middle, node.right, components),
                };
                if left_child.upper >= right_child.upper {
                    stack.push(right_child);
                    stack.push(left_child);
                } else {
                    stack.push(left_child);
                    stack.push(right_child);
                }
            }
        }
        Ok(result)
    }

    fn evaluate_child_once(
        &mut self,
        channel: usize,
        coarse: &LocalCoarse,
        child: ActiveChild,
        count_unique_survey: bool,
    ) -> Result<ResolveResult, TruePeakError> {
        #[cfg(feature = "fast-stage-timing")]
        let survey_timer = StageTimer::start();
        let survey = self.build_local_survey(
            channel,
            coarse,
            child.start,
            child.end,
            count_unique_survey,
        )?;
        #[cfg(feature = "fast-stage-timing")]
        survey_timer.finish(&mut self.diagnostics.fast_stage_midpoint_survey_nanos);

        #[cfg(feature = "fast-stage-timing")]
        let flat_timer = StageTimer::start();
        self.diagnostics.fast_flat_groups = self.diagnostics.fast_flat_groups.saturating_add(1);
        self.diagnostics.authoritative_coarse_groups = self
            .diagnostics
            .authoritative_coarse_groups
            .saturating_add(1);
        let flat = local_flat_upper(coarse, &survey, child.start, child.end, self.metadata);
        #[cfg(feature = "fast-stage-timing")]
        flat_timer.finish(&mut self.diagnostics.fast_stage_flat_envelope_nanos);

        if flat <= self.channel_lower_peaks[channel] {
            return Ok(ResolveResult::default());
        }
        if self.execution_mode.stops_after_flat_bounds()
            || tolerance_accepts(flat, self.channel_lower_peaks[channel])
        {
            return Ok(ResolveResult {
                terminal_upper: flat,
                needs_direct_tightening: false,
            });
        }

        #[cfg(feature = "fast-stage-timing")]
        let resolver_timer = StageTimer::start();
        let result = self.resolve_span(channel, coarse, &survey, child.start, child.end);
        #[cfg(feature = "fast-stage-timing")]
        resolver_timer.finish(&mut self.diagnostics.fast_stage_candidate_refinement_nanos);
        result
    }

    fn process_active_child(
        &mut self,
        channel: usize,
        child: ActiveChild,
        cache: &mut TileHalfCache,
    ) -> Result<(), TruePeakError> {
        // A previous high-priority span may have improved L4 enough to retire
        // this span after its first-stage values were already collected.
        if child.raw_upper.is_finite() && child.raw_upper <= self.channel_l4_lower[channel] {
            self.diagnostics.groups_rejected = self.diagnostics.groups_rejected.saturating_add(1);
            return Ok(());
        }

        let coarse = LocalCoarse::build(&self.raw, channel, cache, child.start, child.end);
        let first_attempt = self.evaluate_child_once(channel, &coarse, child, true);
        let needs_dense_retry = match &first_attempt {
            Ok(result) => result.needs_direct_tightening && cache.dense_source,
            Err(TruePeakError::NumericalOverflow) => cache.dense_source,
            Err(_) => false,
        };

        let resolved = if needs_dense_retry {
            // Numerical ambiguity from a packed FFT is not permanent. Replace
            // every first-stage half knot needed by this still-competitive span
            // with the channel-local direct authority and rebuild its bounds.
            let ranges = [(child.start - 8, child.end + 7)];
            #[cfg(feature = "fast-stage-timing")]
            let direct_timer = StageTimer::start();
            let direct = execute_direct_ranges(
                &self.raw,
                channel,
                &ranges,
                cache,
                self.metadata,
                self.use_avx,
            )?;
            #[cfg(feature = "fast-stage-timing")]
            direct_timer.finish(&mut self.diagnostics.fast_stage_prefix_block_ingest_nanos);
            self.diagnostics.strict_coarse_evaluations = self
                .diagnostics
                .strict_coarse_evaluations
                .saturating_add(direct as u64);
            self.diagnostics.direct_rescore_evaluations = self
                .diagnostics
                .direct_rescore_evaluations
                .saturating_add(direct as u64);

            let tightened = LocalCoarse::build(&self.raw, channel, cache, child.start, child.end);
            // If the first survey completed, these are repeated physical HQ4
            // values and must not increment the unique survey count. If it
            // failed numerically, the direct retry is the first completed survey.
            let count_unique = first_attempt.is_err();
            self.evaluate_child_once(channel, &tightened, child, count_unique)?
        } else {
            first_attempt?
        };

        self.channel_terminal_upper[channel] = max_nonnegative_finite(
            self.channel_terminal_upper[channel],
            resolved.terminal_upper,
        );
        Ok(())
    }

    fn process_tile(&mut self, start: i128, end: i128) -> Result<(), TruePeakError> {
        debug_assert!(start < end && end - start <= TILE_INTERVALS);
        debug_assert!(self.ready_for_tile(start, end));

        #[cfg(feature = "fast-stage-timing")]
        let envelope_timer = StageTimer::start();
        let mut active = Vec::with_capacity(self.channels);
        for channel in 0..self.channels {
            self.observe_sample_tile(channel, start, end);
            let summary = ChannelRawSummary::build(&self.raw, channel, start, end);
            let children = self.screen_channel(channel, start, end, &summary);
            active.push(children);
        }
        #[cfg(feature = "fast-stage-timing")]
        envelope_timer.finish(&mut self.diagnostics.fast_stage_flat_envelope_nanos);

        let ranges = active.iter().map(|children| child_ranges(children)).collect::<Vec<_>>();
        let mut caches = ranges
            .iter()
            .map(|channel_ranges| TileHalfCache::new(start, end, channel_ranges))
            .collect::<Vec<_>>();

        #[cfg(feature = "fast-stage-timing")]
        let prefix_timer = StageTimer::start();
        for first_channel in (0..self.channels).step_by(2) {
            let second_channel = (first_channel + 1 < self.channels).then_some(first_channel + 1);
            let second_ranges = second_channel.map(|channel| ranges[channel].as_slice()).unwrap_or(&[]);
            let requested_union = union_count(&ranges[first_channel], second_ranges);
            if requested_union == 0 {
                continue;
            }
            if requested_union > DENSE_HALF_KNOT_THRESHOLD {
                let result = self.execute_dense_pair(
                    start,
                    end,
                    first_channel,
                    second_channel,
                    &caches[first_channel],
                    second_channel.map(|channel| &caches[channel]),
                )?;
                let retained_first = Self::apply_dense_result(&mut caches[first_channel], &result.first, result.first_m);
                let mut retained = retained_first;
                if let Some(second_channel) = second_channel {
                    retained += Self::apply_dense_result(&mut caches[second_channel], &result.second, result.first_m);
                }
                self.diagnostics.authoritative_coarse_values = self
                    .diagnostics
                    .authoritative_coarse_values
                    .saturating_add(retained as u64);
                self.diagnostics.dense_regions = self.diagnostics.dense_regions.saturating_add(1);
                self.diagnostics.dense_complete_regions = self.diagnostics.dense_complete_regions.saturating_add(1);
                self.diagnostics.dense_intermediate_cells = self
                    .diagnostics
                    .dense_intermediate_cells
                    .saturating_add(result.emitted_frames as u64);
                let active_channels = 1 + usize::from(second_channel.is_some());
                self.diagnostics.dense_phase_evaluations = self
                    .diagnostics
                    .dense_phase_evaluations
                    .saturating_add((result.emitted_frames * active_channels) as u64);
            } else {
                for channel in first_channel..=second_channel.unwrap_or(first_channel) {
                    let count = execute_direct_ranges(
                        &self.raw,
                        channel,
                        &ranges[channel],
                        &mut caches[channel],
                        self.metadata,
                        self.use_avx,
                    )?;
                    self.diagnostics.strict_coarse_evaluations = self
                        .diagnostics
                        .strict_coarse_evaluations
                        .saturating_add(count as u64);
                    self.diagnostics.authoritative_coarse_values = self
                        .diagnostics
                        .authoritative_coarse_values
                        .saturating_add(count as u64);
                }
            }
        }
        #[cfg(feature = "fast-stage-timing")]
        prefix_timer.finish(&mut self.diagnostics.fast_stage_prefix_block_ingest_nanos);

        for channel in 0..self.channels {
            for child in active[channel].clone() {
                self.process_active_child(channel, child, &mut caches[channel])?;
            }
        }
        self.diagnostics.tiles_processed = self.diagnostics.tiles_processed.saturating_add(1);
        Ok(())
    }

    fn finalize_certificate(mut self) -> Result<PeakCertificate, TruePeakError> {
        self.process_eof_tiles()?;
        #[cfg(feature = "fast-stage-timing")]
        let finalize_timer = StageTimer::start();

        let mut channel_intervals = Vec::with_capacity(self.channels);
        let mut channel_upper_linear_peaks = Vec::with_capacity(self.channels);
        let mut point_channels = Vec::with_capacity(self.channels);
        let mut overall_lower = 0.0;
        let mut overall_upper = 0.0;
        let mut overall_point = 0.0;
        let mut unresolved_upper = 0.0;
        let mut complete = true;

        for channel in 0..self.channels {
            let sample_peak = self.channel_sample_peaks[channel];
            let lower = max_nonnegative_finite(self.channel_lower_peaks[channel], sample_peak);
            let coverage_upper = max_nonnegative_finite(
                max_nonnegative_finite(self.channel_terminal_upper[channel], sample_peak),
                lower,
            );
            if !coverage_upper.is_finite() || coverage_upper < lower {
                return Err(TruePeakError::NumericalOverflow);
            }
            let norm_upper = upper_mul(HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER, sample_peak);
            let upper = coverage_upper.min(norm_upper);
            if !upper.is_finite() || lower > upper {
                return Err(TruePeakError::NumericalOverflow);
            }
            let point = self.channel_point_peaks[channel].min(upper);
            if !point.is_finite() {
                return Err(TruePeakError::NumericalOverflow);
            }
            channel_intervals.push(PeakInterval::new(lower, upper));
            channel_upper_linear_peaks.push(upper);
            point_channels.push(point);
            overall_lower = max_nonnegative_finite(overall_lower, lower);
            overall_upper = max_nonnegative_finite(overall_upper, upper);
            overall_point = max_nonnegative_finite(overall_point, point);

            let channel_unresolved = self.channel_terminal_upper[channel].min(norm_upper);
            unresolved_upper = max_nonnegative_finite(unresolved_upper, channel_unresolved);
            if channel_unresolved > lower {
                complete = false;
            }
        }

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
            finite_interval: PeakInterval::new(overall_lower, overall_upper),
            channel_intervals,
            channel_upper_linear_peaks,
            status: if complete { SearchStatus::Complete } else { SearchStatus::WorkLimited },
            diagnostics: self.diagnostics,
        })
    }
}

#[derive(Debug, Clone)]
pub(super) struct FastPeakMeterImpl {
    edge_policy: EdgePolicy,
    state: FastState,
    first_frame: Vec<f64>,
    last_frame: Vec<f64>,
    validation_peaks: Vec<f64>,
    edge_scratch: Vec<f64>,
    started: bool,
    next_input_index: i128,
}

impl FastPeakMeterImpl {
    pub(super) fn new(
        sample_rate_hz: u32,
        channels: usize,
        edge_policy: EdgePolicy,
    ) -> Result<Self, TruePeakError> {
        Self::new_with_execution_mode(sample_rate_hz, channels, edge_policy, FastExecutionMode::Production)
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
        Ok(Self {
            edge_policy,
            state: FastState::new(channels, execution_mode),
            first_frame: vec![0.0; channels],
            last_frame: vec![0.0; channels],
            validation_peaks: vec![0.0; channels],
            edge_scratch: Vec::with_capacity(RAW_HALO_FRAMES as usize * channels),
            started: false,
            next_input_index: 0,
        })
    }

    fn build_edge_extension(
        edge_policy: EdgePolicy,
        channels: usize,
        frame: &[f64],
        scratch: &mut Vec<f64>,
    ) {
        scratch.clear();
        scratch.resize(RAW_HALO_FRAMES as usize * channels, 0.0);
        if edge_policy == EdgePolicy::RepeatEndpoints {
            for destination in scratch.chunks_exact_mut(channels) {
                destination.copy_from_slice(frame);
            }
        }
    }

    pub(super) fn push_interleaved(&mut self, samples: &[f64]) -> Result<(), TruePeakError> {
        let channels = self.state.channels;
        if samples.len() % channels != 0 {
            return Err(TruePeakError::IncompleteFrame { samples: samples.len(), channels });
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
        if i128::from(new_frames).checked_mul(1024).is_none() {
            return Err(TruePeakError::InputTooLong);
        }

        // Validate the complete caller push before any measurement state moves.
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
            self.state.raw.append(-RAW_HALO_FRAMES, &self.edge_scratch);
            self.started = true;
        }
        self.last_frame.copy_from_slice(&samples[samples.len() - channels..]);
        for channel in 0..channels {
            self.state.channel_sample_peaks[channel] = max_nonnegative_finite(
                self.state.channel_sample_peaks[channel],
                self.validation_peaks[channel],
            );
        }

        // Commit in canonical 4096-frame boundaries after validation. This
        // bounds retained raw memory and makes caller chunking irrelevant to
        // tile readiness and pruning order.
        let mut consumed = 0usize;
        while consumed < chunk_frames {
            let absolute = self.next_input_index + consumed as i128;
            let within_tile = absolute.rem_euclid(TILE_INTERVALS) as usize;
            let until_boundary = TILE_INTERVALS as usize - within_tile;
            let take = until_boundary.min(chunk_frames - consumed);
            let first = consumed * channels;
            let last = (consumed + take) * channels;
            self.state.append_nominal(absolute, &samples[first..last])?;
            consumed += take;
        }
        self.next_input_index += chunk_frames as i128;
        debug_assert_eq!(self.state.nominal_frames_seen, new_frames);
        Ok(())
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
        self.state.raw.append(self.next_input_index, &self.edge_scratch);
        self.state.finalize_certificate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_raw_constants_are_frozen_bits() {
        assert_eq!(RAW_A_UPPER.to_bits(), 0x3ff0_2862_ce0a_81a1);
        assert_eq!(RAW_B_UPPER.to_bits(), 0x3cce_9970_cbf1_2e12);
        assert_eq!(FAST_ACCEPT_RATIO_DOWN.to_bits(), 0x3ff0_04b7_e9b5_ce5c);
        assert!(20.0 * FAST_ACCEPT_RATIO_DOWN.log10() < 0.01);
    }

    #[test]
    fn child_request_geometry_matches_frozen_first_stage_support() {
        let child = ActiveChild { start: 100, end: 132, raw_upper: 1.0 };
        let ranges = child_ranges(&[child]);
        assert_eq!(ranges, vec![(92, 139)]);
        assert_eq!(ranges[0].1 - ranges[0].0 + 1, 48);
        assert_eq!(ranges[0].0 - 767, 100 - 775);
        assert_eq!(ranges[0].1 + 768, 132 + 775);
    }

    #[test]
    fn commissioning_cut_points_are_intentionally_equivalent() {
        assert!(FastExecutionMode::SurveyBoundsOnly.stops_after_flat_bounds());
        assert!(FastExecutionMode::NominationOnly.stops_after_flat_bounds());
        assert!(!FastExecutionMode::Production.stops_after_flat_bounds());
    }

    #[test]
    fn raw_zero_support_rejects_at_zero_witness() {
        assert_eq!(raw_group_upper(0.0, 0.0).to_bits(), 0);
    }


    fn test_raw<F>(first: i128, last: i128, mut sample: F) -> RawStore
    where
        F: FnMut(i128) -> f64,
    {
        let mut raw = RawStore::new(1);
        let mut values = Vec::with_capacity(usize::try_from(last - first + 1).unwrap());
        for index in first..=last {
            values.push(sample(index));
        }
        raw.append(first, &values);
        raw
    }

    fn independent_half(raw: &RawStore, m: i128) -> f64 {
        // Full 1536-tap order with compensated accumulation. This deliberately
        // does not use the production symmetric pair graph.
        let mut sum = 0.0;
        let mut correction = 0.0;
        for tap in 0..1536usize {
            let coefficient = if tap < HQ1024_HALF_DELAY_COEFFICIENTS.len() {
                HQ1024_HALF_DELAY_COEFFICIENTS[tap]
            } else {
                HQ1024_HALF_DELAY_COEFFICIENTS[1535 - tap]
            };
            let product = coefficient * raw.sample(m + 768 - tap as i128, 0);
            let adjusted = product - correction;
            let next = sum + adjusted;
            correction = (next - sum) - adjusted;
            sum = next;
        }
        sum
    }

    fn independent_coarse(raw: &RawStore, coarse: i128) -> f64 {
        if coarse.rem_euclid(2) == 0 {
            raw.sample(coarse.div_euclid(2), 0)
        } else {
            independent_half(raw, (coarse - 1).div_euclid(2))
        }
    }

    fn independent_target(
        coarse_first: i128,
        coarse: &[f64],
        native: i128,
        phase: usize,
    ) -> f64 {
        debug_assert!(phase < 1024);
        let coarse_cell = 2 * native + (phase / HQ1024_TAIL_FACTOR) as i128;
        let lookup = |index: i128| coarse[usize::try_from(index - coarse_first).unwrap()];
        let tail_phase = phase % HQ1024_TAIL_FACTOR;
        if tail_phase == 0 {
            return lookup(coarse_cell);
        }
        let row = &HQ1024_TAIL_COEFFICIENTS[tail_phase];
        let mut sum = 0.0;
        let mut correction = 0.0;
        for (index, coefficient) in row.iter().copied().enumerate() {
            let offset = i128::from(HQ1024_TAIL_OFFSET_MIN) + index as i128;
            let product = coefficient * lookup(coarse_cell + offset);
            let adjusted = product - correction;
            let next = sum + adjusted;
            correction = (next - sum) - adjusted;
            sum = next;
        }
        sum
    }

    #[test]
    fn raw_screen_runtime_ranges_enclose_all_frozen_phases_and_support_edges() {
        let start = 0i128;
        let end = 3i128;
        let raw_first = start - RAW_HALO_FRAMES;
        let raw_last = end + RAW_HALO_FRAMES;
        let fixtures = [
            (start - 775, 0.73),
            (end + 775, -0.61),
            (-1, 0.47),
            (2, -0.83),
        ];

        for &(impulse, amplitude) in &fixtures {
            let raw = test_raw(raw_first, raw_last, |index| {
                let background = if index.rem_euclid(7) == 0 { 0.003 } else { -0.002 };
                if index == impulse { amplitude } else { background }
            });
            let summary = ChannelRawSummary::build(&raw, 0, start, end);
            assert!(summary.qualified);
            let d_upper = summary.root_d_upper(start, start, end);
            let sample_max = raw.max_magnitude(0, start, end);
            let upper = raw_group_upper(sample_max, d_upper);

            let coarse_first = 2 * start - 16;
            let coarse_last = 2 * end + 16;
            let coarse = (coarse_first..=coarse_last)
                .map(|index| independent_coarse(&raw, index))
                .collect::<Vec<_>>();
            let mut enumerated = exact_magnitude(raw.sample(end, 0));
            for native in start..end {
                for phase in 0..1024usize {
                    enumerated = max_exact_magnitude(
                        enumerated,
                        independent_target(coarse_first, &coarse, native, phase),
                    );
                }
            }
            assert!(
                enumerated <= upper,
                "raw screen escaped: impulse={impulse} enumerated={enumerated:.17e} upper={upper:.17e}",
            );
        }
    }

    #[test]
    fn raw_screen_rejection_is_backed_by_the_same_channel_l4_witness() {
        let start = 0i128;
        let end = 3i128;
        let raw = test_raw(start - RAW_HALO_FRAMES, end + RAW_HALO_FRAMES, |index| {
            let x = index as f64;
            0.11 * (0.017 * x).sin() - 0.07 * (0.043 * x).cos()
        });
        let summary = ChannelRawSummary::build(&raw, 0, start, end);
        assert!(summary.qualified);

        // Model a witness established by an earlier canonical tile. The real
        // screen must reject this one short root against L4, not against Lall.
        let witness = 2.0_f64;
        let mut state = FastState::new(1, FastExecutionMode::Production);
        state.raw = raw.clone();
        state.channel_l4_lower[0] = witness;
        state.channel_lower_peaks[0] = witness;
        let active = state.screen_channel(0, start, end, &summary);
        assert!(active.is_empty());
        assert_eq!(state.diagnostics.groups_rejected, 1);

        let coarse_first = 2 * start - 16;
        let coarse_last = 2 * end + 16;
        let coarse = (coarse_first..=coarse_last)
            .map(|index| independent_coarse(&raw, index))
            .collect::<Vec<_>>();
        let mut target_max = exact_magnitude(raw.sample(end, 0));
        for native in start..end {
            for phase in 0..1024usize {
                target_max = max_exact_magnitude(
                    target_max,
                    independent_target(coarse_first, &coarse, native, phase),
                );
            }
        }
        assert!(
            target_max <= witness,
            "raw-rejected target escaped L4 witness: target={target_max:.17e} witness={witness:.17e}",
        );
    }

    #[test]
    fn pruned_hq4_diagnostic_matches_independent_complete_hq4_survey() {
        const FRAMES: usize = 2049;
        let mut samples = vec![0.0_f64; FRAMES];
        samples[0] = 1.0;
        samples[17] = -0.2;

        let mut meter = FastPeakMeterImpl::new(192_000, 1, EdgePolicy::ZeroExtend).unwrap();
        meter.push_interleaved(&samples).unwrap();
        let certificate = meter.finalize().unwrap();
        assert!(
            certificate.diagnostics.groups_rejected > 0,
            "fixture must exercise native-domain rejection",
        );

        let final_native = FRAMES as i128 - 1;
        let raw = test_raw(-RAW_HALO_FRAMES, final_native + RAW_HALO_FRAMES, |index| {
            usize::try_from(index)
                .ok()
                .and_then(|index| samples.get(index))
                .copied()
                .unwrap_or(0.0)
        });
        let coarse_first = -16i128;
        let coarse_last = 2 * final_native + 16;
        let coarse = (coarse_first..=coarse_last)
            .map(|index| independent_coarse(&raw, index))
            .collect::<Vec<_>>();
        let mut oracle = exact_magnitude(samples[FRAMES - 1]);
        for native in 0..final_native {
            for phase in [0usize, 256, 512, 768] {
                oracle = max_exact_magnitude(
                    oracle,
                    independent_target(coarse_first, &coarse, native, phase),
                );
            }
        }

        let measured = certificate.diagnostics.fast_hq4_peak_linear;
        let error = certificate.numerical_envelope_linear;
        assert!(
            measured <= upper_add(oracle, error)
                && oracle <= upper_add(measured, error),
            "pruned HQ4 diagnostic escaped complete oracle: measured={measured:.17e} oracle={oracle:.17e} error={error:.17e}",
        );
    }

    #[test]
    fn direct_first_stage_scalar_and_avx_fit_the_declared_enclosure() {
        let raw = test_raw(-900, 900, |index| {
            let x = index as f64;
            let alternating = if index.rem_euclid(2) == 0 { 1.0 } else { -1.0 };
            0.71 * alternating + 0.17 * (0.031 * x).sin() - 0.09 * (0.047 * x).cos()
        });
        let ranges = [(-5, 6)];
        let mut scalar_cache = TileHalfCache::new(0, 32, &ranges);
        execute_direct_ranges(
            &raw,
            0,
            &ranges,
            &mut scalar_cache,
            fast_metadata(),
            false,
        )
        .unwrap();

        for m in ranges[0].0..=ranges[0].1 {
            let evaluation = scalar_cache.get(m);
            let oracle = independent_half(&raw, m);
            assert!(
                (evaluation.value - oracle).abs() <= evaluation.error,
                "scalar direct enclosure miss at m={m}",
            );
        }

        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx") {
            let mut avx_cache = TileHalfCache::new(0, 32, &ranges);
            execute_direct_ranges(
                &raw,
                0,
                &ranges,
                &mut avx_cache,
                fast_metadata(),
                true,
            )
            .unwrap();
            for m in ranges[0].0..=ranges[0].1 {
                let scalar = scalar_cache.get(m);
                let avx = avx_cache.get(m);
                let oracle = independent_half(&raw, m);
                assert!((avx.value - oracle).abs() <= avx.error, "AVX enclosure miss at m={m}");
                assert!((avx.value - scalar.value).abs() <= upper_add(avx.error, scalar.error));
            }
        }
    }

    #[test]
    fn sparse_and_dense_first_stage_cover_the_same_requested_span() {
        let tile_start = 0i128;
        let tile_end = 32i128;
        let window_start = tile_start - RAW_HALO_FRAMES;
        let window_end = tile_end + RAW_HALO_FRAMES;
        let mut raw = RawStore::new(2);
        let mut interleaved = Vec::with_capacity(
            usize::try_from(window_end - window_start + 1).unwrap() * 2,
        );
        for index in window_start..=window_end {
            let x = index as f64;
            interleaved.push(0.61 * (0.019 * x).sin() - 0.23 * (0.037 * x).cos());
            interleaved.push(1.0e-7 * (0.029 * x + 0.3).sin());
        }
        raw.append(window_start, &interleaved);

        let requested = [(tile_start - 8, tile_end + 7)];
        let empty: [(i128, i128); 0] = [];
        let first_cache = TileHalfCache::new(tile_start, tile_end, &requested);
        let second_cache = TileHalfCache::new(tile_start, tile_end, &empty);
        let mut state = FastState::new(2, FastExecutionMode::Production);
        state.raw = raw.clone();
        let dense = state
            .execute_dense_pair(
                tile_start,
                tile_end,
                0,
                Some(1),
                &first_cache,
                Some(&second_cache),
            )
            .unwrap();

        // The unused packed component is exactly inactive rather than inheriting
        // an unrelated loud-channel uncertainty.
        assert!(dense.second.iter().all(|value| value.is_none()));

        let mut direct = TileHalfCache::new(tile_start, tile_end, &requested);
        execute_direct_ranges(
            &raw,
            0,
            &requested,
            &mut direct,
            fast_metadata(),
            false,
        )
        .unwrap();
        for m in requested[0].0..=requested[0].1 {
            let dense_index = usize::try_from(m - dense.first_m).unwrap();
            let dense_evaluation = dense.first[dense_index]
                .expect("requested dense half knot must be retained");
            let direct_evaluation = direct.get(m);
            let oracle = independent_half(&raw, m);
            assert!(
                (direct_evaluation.value - oracle).abs() <= direct_evaluation.error,
                "direct FIR escaped oracle at m={m}",
            );
            assert!(
                (dense_evaluation.value - oracle).abs()
                    <= upper_add(dense_evaluation.error, direct_evaluation.error),
                "dense FIR escaped independent/direct enclosure at m={m}",
            );
        }
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
            // SAFETY: restore the complete control/status word captured on the
            // same test thread, including its rounding mode.
            unsafe { write_mxcsr(self.0) };
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn raw_screen_and_direct_enclosure_fail_closed_under_daz_ftz() {
        // SAFETY: test-local MXCSR mutation is restored by the guard.
        let original = unsafe { read_mxcsr() };
        let _restore = MxcsrRestore(original);
        // DAZ bit 6, FTZ bit 15. Preserve the caller's rounding mode and every
        // unrelated control bit.
        unsafe { write_mxcsr(original | (1 << 6) | (1 << 15)) };

        let tiny = f64::from_bits(7);
        let raw = test_raw(-RAW_HALO_FRAMES, 2 + RAW_HALO_FRAMES, |index| {
            if index == 0 { tiny } else { 0.0 }
        });
        let summary = ChannelRawSummary::build(&raw, 0, 0, 2);
        assert!(!summary.qualified, "a DAZ-sensitive raw support must survive screening");
        assert!(summary.root_d_upper(0, 0, 2).is_infinite());

        let ranges = [(-1, 2)];
        let mut cache = TileHalfCache::new(0, 2, &ranges);
        execute_direct_ranges(&raw, 0, &ranges, &mut cache, fast_metadata(), false).unwrap();
        assert!(
            (ranges[0].0..=ranges[0].1).any(|m| cache.get(m).upper() > 0.0),
            "direct numerical envelope must not collapse a nonzero subnormal support to exact zero",
        );
    }

    #[test]
    fn dyadic_child_bounds_replace_their_loose_parent() {
        let components = CellComponents { d2_upper: 0.05, magnitude_upper: 1.0 };
        let endpoint = Evaluation { value: 1.0, error: 0.0 };
        let midpoint = Evaluation { value: 1.0, error: 0.0 };
        let parent = node_upper(2, 256, endpoint, midpoint, components);
        let left = node_upper(4, 128, endpoint, midpoint, components);
        let right = node_upper(5, 128, midpoint, endpoint, components);
        assert!(parent > 1.0);
        assert!(max_nonnegative_finite(left, right) < parent);
        // The resolver stores only terminal descendants after splitting; this
        // regression makes the numerical consequence of retaining the parent
        // explicit without depending on a particular waveform fixture.
        let descendant_terminal = max_nonnegative_finite(left, right);
        assert!(descendant_terminal < parent);
    }

    #[test]
    fn mandatory_resolver_can_exceed_the_retired_104_evaluation_cap() {
        // Exercise the resolver directly so the regression proves the policy
        // property rather than depending on whether a particular public input
        // happens to survive today's native screen.  A zero-valued but
        // uncertain coarse support keeps the proven lower at zero while every
        // dyadic node remains competitive, forcing exhaustive subdivision.
        let uncertainty = 1.0e-6;
        let coarse = LocalCoarse {
            first: -16,
            values: vec![
                Evaluation {
                    value: 0.0,
                    error: uncertainty,
                };
                35
            ],
        };
        let survey = LocalSurvey {
            first_q4: 0,
            values: vec![
                Evaluation {
                    value: 0.0,
                    error: uncertainty,
                };
                5
            ],
        };
        let mut state = FastState::new(1, FastExecutionMode::Production);
        state.use_avx = false;

        let result = state.resolve_span(0, &coarse, &survey, 0, 1).unwrap();
        assert!(
            state.diagnostics.refined_cells > 104,
            "mandatory resolver stopped at or below the retired 104-evaluation cap: {}",
            state.diagnostics.refined_cells,
        );
        assert_eq!(
            state.diagnostics.phase_evaluations,
            state.diagnostics.refined_cells,
            "direct resolver exercise must account each tail evaluation in the generic counters",
        );
        assert!(result.needs_direct_tightening);
        assert!(result.terminal_upper > 0.0);
    }
}
