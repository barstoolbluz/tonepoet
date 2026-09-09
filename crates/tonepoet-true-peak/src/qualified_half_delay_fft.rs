use std::fmt;
use std::sync::OnceLock;

use num_complex::Complex64;

#[cfg(target_arch = "x86")]
use core::arch::x86::{
    __m256d, _mm256_add_pd, _mm256_addsub_pd, _mm256_loadu_pd, _mm256_mul_pd,
    _mm256_permute_pd, _mm256_setr_pd, _mm256_storeu_pd, _mm256_sub_pd,
};
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::{
    __m256d, _mm256_add_pd, _mm256_addsub_pd, _mm256_loadu_pd, _mm256_mul_pd,
    _mm256_permute_pd, _mm256_setr_pd, _mm256_storeu_pd, _mm256_sub_pd,
};

use super::qualified_prefix_coefficients::{
    HQ1024_QUALIFIED_PREFIX_ABSOLUTE_ERROR_UPPER,
    HQ1024_QUALIFIED_PREFIX_ERROR_PER_PACKED_COMPONENT_PEAK_UPPER,
    LEGACY_QUALIFIED_PREFIX_ABSOLUTE_ERROR_UPPER,
    LEGACY_QUALIFIED_PREFIX_ERROR_PER_PACKED_COMPONENT_PEAK_UPPER,
    QUALIFIED_PREFIX_MAX_FFT_SIZE, QUALIFIED_PREFIX_TWIDDLE_COMPLEX_ERROR_UPPER,
    QUALIFIED_PREFIX_TWIDDLES_8192,
};
use super::{
    ReconstructionId, HEADROOM64_HALF_DELAY_COEFFICIENTS, HEADROOM64_HALF_DELAY_TAPS,
};
use super::hq1024_coefficients::{
    HQ1024_FIRST_HALF_DELAY_TAPS, HQ1024_HALF_DELAY_COEFFICIENTS,
};

const LEGACY_FFT_SIZE: usize = 2048;
const HQ1024_FFT_SIZE: usize = 8192;

const F64_SIGN_MASK: u64 = 1_u64 << 63;
const F64_MAGNITUDE_MASK: u64 = !F64_SIGN_MASK;
const F64_INFINITY_BITS: u64 = 0x7ff0_0000_0000_0000;
const F64_MIN_NORMAL_BITS: u64 = 0x0010_0000_0000_0000;

#[inline]
fn magnitude_bits(value: f64) -> u64 {
    value.to_bits() & F64_MAGNITUDE_MASK
}

#[inline]
fn is_subnormal_magnitude_bits(bits: u64) -> bool {
    bits != 0 && bits < F64_MIN_NORMAL_BITS
}

/// Return the exact nonnegative magnitude encoding for a finite nonnegative
/// bound. Negative finite values and all NaNs/infinities fail closed. Negative
/// zero is accepted as exact zero. This classification is integer-only so DAZ
/// cannot reinterpret a subnormal operand as zero before the enclosure widens it.
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
    if is_subnormal_magnitude_bits(bits) {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QualifiedHalfDelayKind {
    Legacy384,
    Hq1024V1,
}

impl QualifiedHalfDelayKind {
    fn from_reconstruction(reconstruction: ReconstructionId) -> Self {
        match reconstruction {
            ReconstructionId::LegacyHeadroom64 => Self::Legacy384,
            ReconstructionId::Hq1024V1 => Self::Hq1024V1,
        }
    }

    const fn taps(self) -> usize {
        match self {
            Self::Legacy384 => HEADROOM64_HALF_DELAY_TAPS,
            Self::Hq1024V1 => HQ1024_FIRST_HALF_DELAY_TAPS,
        }
    }

    const fn fft_size(self) -> usize {
        match self {
            Self::Legacy384 => LEGACY_FFT_SIZE,
            Self::Hq1024V1 => HQ1024_FFT_SIZE,
        }
    }

    const fn block_frames(self) -> usize {
        self.fft_size() - self.taps() + 1
    }

    const fn group_delay_inputs(self) -> i128 {
        (self.taps() / 2) as i128
    }

    fn half_coefficients(self) -> &'static [f64] {
        match self {
            Self::Legacy384 => &HEADROOM64_HALF_DELAY_COEFFICIENTS,
            Self::Hq1024V1 => &HQ1024_HALF_DELAY_COEFFICIENTS,
        }
    }

    const fn relative_error_per_packed_component_peak_upper(self) -> f64 {
        match self {
            Self::Legacy384 => {
                LEGACY_QUALIFIED_PREFIX_ERROR_PER_PACKED_COMPONENT_PEAK_UPPER
            }
            Self::Hq1024V1 => {
                HQ1024_QUALIFIED_PREFIX_ERROR_PER_PACKED_COMPONENT_PEAK_UPPER
            }
        }
    }

    const fn absolute_error_upper(self) -> f64 {
        match self {
            Self::Legacy384 => LEGACY_QUALIFIED_PREFIX_ABSOLUTE_ERROR_UPPER,
            Self::Hq1024V1 => HQ1024_QUALIFIED_PREFIX_ABSOLUTE_ERROR_UPPER,
        }
    }
}

#[inline]
fn complex_mul(left: Complex64, right: Complex64) -> Complex64 {
    // Keep the arithmetic shape identical to the source-round derivation:
    // four binary64 products followed by two additions/subtractions. Rust's
    // ordinary floating operators do not request fused contraction.
    let rr = left.re * right.re;
    let ii = left.im * right.im;
    let ri = left.re * right.im;
    let ir = left.im * right.re;
    Complex64::new(rr - ii, ri + ir)
}

fn bit_reverse_permute(values: &mut [Complex64]) {
    let len = values.len();
    debug_assert!(len.is_power_of_two());
    let mut reversed = 0usize;
    for index in 1..len {
        let mut bit = len >> 1;
        while reversed & bit != 0 {
            reversed ^= bit;
            bit >>= 1;
        }
        reversed ^= bit;
        if index < reversed {
            values.swap(index, reversed);
        }
    }
}

fn bit_reverse_swaps(len: usize) -> Vec<(u16, u16)> {
    debug_assert!(len.is_power_of_two() && len <= u16::MAX as usize);
    let mut swaps = Vec::with_capacity(len / 2);
    let mut reversed = 0usize;
    for index in 1..len {
        let mut bit = len >> 1;
        while reversed & bit != 0 {
            reversed ^= bit;
            bit >>= 1;
        }
        reversed ^= bit;
        if index < reversed {
            swaps.push((index as u16, reversed as u16));
        }
    }
    swaps
}

#[inline]
fn bit_reverse_permute_precomputed(values: &mut [Complex64], swaps: &[(u16, u16)]) {
    for &(left, right) in swaps {
        values.swap(left as usize, right as usize);
    }
}

fn validate_fixed_fft_geometry(values: &[Complex64]) {
    let fft_size = values.len();
    assert!(
        fft_size.is_power_of_two()
            && fft_size <= QUALIFIED_PREFIX_MAX_FFT_SIZE
            && QUALIFIED_PREFIX_MAX_FFT_SIZE % fft_size == 0,
        "qualified prefix FFT geometry changed",
    );
    // Bind the generated root-error contract into the execution unit as well
    // as the offline verifier. The condition is constant-folded in release.
    debug_assert!(
        QUALIFIED_PREFIX_TWIDDLE_COMPLEX_ERROR_UPPER.is_finite()
            && QUALIFIED_PREFIX_TWIDDLE_COMPLEX_ERROR_UPPER > 0.0
    );
}

fn fixed_radix2_fft_scalar_stages(values: &mut [Complex64], inverse: bool) {
    let fft_size = values.len();
    let mut span = 2usize;
    while span <= fft_size {
        let half = span / 2;
        let twiddle_stride = QUALIFIED_PREFIX_MAX_FFT_SIZE / span;
        for base in (0..fft_size).step_by(span) {
            for offset in 0..half {
                let (re, stored_im) =
                    QUALIFIED_PREFIX_TWIDDLES_8192[offset * twiddle_stride];
                let twiddle = Complex64::new(re, if inverse { -stored_im } else { stored_im });
                let upper = values[base + offset];
                let rotated = complex_mul(values[base + offset + half], twiddle);
                values[base + offset] = Complex64::new(
                    upper.re + rotated.re,
                    upper.im + rotated.im,
                );
                values[base + offset + half] = Complex64::new(
                    upper.re - rotated.re,
                    upper.im - rotated.im,
                );
            }
        }
        span *= 2;
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx")]
unsafe fn complex_mul_pair_avx(left: __m256d, right: __m256d) -> __m256d {
    // Lanes hold two AoS complex values: [re0, im0, re1, im1].  This is the
    // exact same four-multiply/two-add-sub graph as `complex_mul`, merely
    // executed for two independent complex values at once.  No FMA is used.
    let left_re = _mm256_permute_pd::<0b0000>(left);
    let left_im = _mm256_permute_pd::<0b1111>(left);
    let swapped_right = _mm256_permute_pd::<0b0101>(right);
    let direct = _mm256_mul_pd(left_re, right);
    let crossed = _mm256_mul_pd(left_im, swapped_right);
    _mm256_addsub_pd(direct, crossed)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx")]
#[inline]
unsafe fn butterfly_pair_avx(
    values: &mut [Complex64],
    base: usize,
    half: usize,
    offset: usize,
    twiddles: __m256d,
) {
    let upper_ptr = values.as_mut_ptr().add(base + offset);
    let lower_ptr = values.as_mut_ptr().add(base + offset + half);
    let upper = _mm256_loadu_pd(upper_ptr.cast::<f64>());
    let lower = _mm256_loadu_pd(lower_ptr.cast::<f64>());
    let rotated = complex_mul_pair_avx(lower, twiddles);
    _mm256_storeu_pd(
        upper_ptr.cast::<f64>(),
        _mm256_add_pd(upper, rotated),
    );
    _mm256_storeu_pd(
        lower_ptr.cast::<f64>(),
        _mm256_sub_pd(upper, rotated),
    );
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx")]
unsafe fn fixed_radix2_fft_avx_stages(values: &mut [Complex64], inverse: bool) {
    // `num_complex::Complex<f64>` is #[repr(C)] and consists of two adjacent
    // f64 fields.  Unaligned AVX loads/stores therefore consume exactly two
    // consecutive complex values without adding an alignment requirement.
    debug_assert_eq!(core::mem::size_of::<Complex64>(), 2 * core::mem::size_of::<f64>());
    let fft_size = values.len();
    let mut span = 2usize;
    while span <= fft_size {
        let half = span / 2;
        let twiddle_stride = QUALIFIED_PREFIX_MAX_FFT_SIZE / span;
        for base in (0..fft_size).step_by(span) {
            let mut offset = 0usize;
            while offset + 1 < half {
                let (re0, stored_im0) =
                    QUALIFIED_PREFIX_TWIDDLES_8192[offset * twiddle_stride];
                let (re1, stored_im1) =
                    QUALIFIED_PREFIX_TWIDDLES_8192[(offset + 1) * twiddle_stride];
                let im0 = if inverse { -stored_im0 } else { stored_im0 };
                let im1 = if inverse { -stored_im1 } else { stored_im1 };
                let twiddles = _mm256_setr_pd(re0, im0, re1, im1);
                butterfly_pair_avx(values, base, half, offset, twiddles);
                offset += 2;
            }

            // The span-2 stage has one butterfly; every larger radix-2 stage
            // has an even half-span. Keep the scalar tail for completeness and
            // to make this routine valid if a smaller supported FFT is
            // introduced without changing the proof graph.
            while offset < half {
                let (re, stored_im) =
                    QUALIFIED_PREFIX_TWIDDLES_8192[offset * twiddle_stride];
                let twiddle = Complex64::new(re, if inverse { -stored_im } else { stored_im });
                let upper = values[base + offset];
                let rotated = complex_mul(values[base + offset + half], twiddle);
                values[base + offset] = Complex64::new(
                    upper.re + rotated.re,
                    upper.im + rotated.im,
                );
                values[base + offset + half] = Complex64::new(
                    upper.re - rotated.re,
                    upper.im - rotated.im,
                );
                offset += 1;
            }
        }
        span *= 2;
    }
}

fn fixed_radix2_fft(values: &mut [Complex64], inverse: bool) {
    validate_fixed_fft_geometry(values);
    bit_reverse_permute(values);

    fixed_radix2_fft_scalar_stages(values, inverse);
}

fn fixed_radix2_fft_fast90(
    values: &mut [Complex64],
    inverse: bool,
    bit_reverse_swaps: &[(u16, u16)],
) {
    validate_fixed_fft_geometry(values);
    bit_reverse_permute_precomputed(values, bit_reverse_swaps);

    // The caller selects this routine only after AVX runtime detection. AVX is
    // deliberately an executor for the *same* certified arithmetic graph, not
    // a different FFT implementation. This keeps every source-round operation
    // count and twiddle enclosure valid while processing two independent
    // butterflies per vector on AVX-without-AVX2 machines too.
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    // SAFETY: `QualifiedHalfDelayFft::new_with_fast90_executor` enables this
    // path only after `is_x86_feature_detected!("avx")` succeeds. The function
    // accepts the same validated slice and uses unaligned loads.
    unsafe {
        fixed_radix2_fft_avx_stages(values, inverse);
        return;
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    fixed_radix2_fft_scalar_stages(values, inverse);
}

fn pointwise_multiply_scalar(values: &mut [Complex64], filter: &[Complex64]) {
    debug_assert_eq!(values.len(), filter.len());
    for (bin, filter) in values.iter_mut().zip(filter.iter().copied()) {
        *bin = complex_mul(*bin, filter);
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx")]
unsafe fn pointwise_multiply_avx(values: &mut [Complex64], filter: &[Complex64]) {
    debug_assert_eq!(values.len(), filter.len());
    debug_assert_eq!(values.len() % 2, 0);
    let mut index = 0usize;
    while index + 3 < values.len() {
        let values_ptr = values.as_mut_ptr().add(index);
        let filter_ptr = filter.as_ptr().add(index);
        let left = _mm256_loadu_pd(values_ptr.cast::<f64>());
        let right = _mm256_loadu_pd(filter_ptr.cast::<f64>());
        _mm256_storeu_pd(
            values_ptr.cast::<f64>(),
            complex_mul_pair_avx(left, right),
        );
        let values_ptr_2 = values.as_mut_ptr().add(index + 2);
        let filter_ptr_2 = filter.as_ptr().add(index + 2);
        let left_2 = _mm256_loadu_pd(values_ptr_2.cast::<f64>());
        let right_2 = _mm256_loadu_pd(filter_ptr_2.cast::<f64>());
        _mm256_storeu_pd(
            values_ptr_2.cast::<f64>(),
            complex_mul_pair_avx(left_2, right_2),
        );
        index += 4;
    }
    while index + 1 < values.len() {
        let values_ptr = values.as_mut_ptr().add(index);
        let filter_ptr = filter.as_ptr().add(index);
        let left = _mm256_loadu_pd(values_ptr.cast::<f64>());
        let right = _mm256_loadu_pd(filter_ptr.cast::<f64>());
        _mm256_storeu_pd(values_ptr.cast::<f64>(), complex_mul_pair_avx(left, right));
        index += 2;
    }
    if index < values.len() {
        values[index] = complex_mul(values[index], filter[index]);
    }
}

fn pointwise_multiply_fast90(values: &mut [Complex64], filter: &[Complex64]) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    // SAFETY: the caller reaches this routine only through an executor whose
    // constructor already established AVX support. Both slices have identical
    // FFT geometry and the implementation uses unaligned loads.
    unsafe {
        pointwise_multiply_avx(values, filter);
        return;
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    pointwise_multiply_scalar(values, filter);
}

#[inline]
fn fast90_avx_available() -> bool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        std::arch::is_x86_feature_detected!("avx")
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        false
    }
}

struct QualifiedPlan {
    filter_spectrum: Vec<Complex64>,
    bit_reverse_swaps: Vec<(u16, u16)>,
}

fn build_plan(kind: QualifiedHalfDelayKind) -> QualifiedPlan {
    let fft_size = kind.fft_size();
    let taps = kind.taps();
    let half = kind.half_coefficients();
    assert_eq!(half.len() * 2, taps, "qualified prefix filter geometry changed");

    let mut filter_spectrum = vec![Complex64::new(0.0, 0.0); fft_size];
    for (index, coefficient) in half.iter().copied().enumerate() {
        filter_spectrum[index].re = coefficient;
        filter_spectrum[taps - 1 - index].re = coefficient;
    }
    fixed_radix2_fft(&mut filter_spectrum, false);

    // The derivation explicitly charges sqrt(2)*MIN_NORMAL for this
    // canonicalization. It prevents a DAZ backend from silently treating a
    // stored subnormal filter-spectrum component differently at multiply time.
    for bin in &mut filter_spectrum {
        // Classify with the IEEE encoding rather than FP comparisons: the plan
        // may be initialized on a caller thread that already has DAZ enabled.
        if is_subnormal_magnitude_bits(magnitude_bits(bin.re)) {
            bin.re = 0.0;
        }
        if is_subnormal_magnitude_bits(magnitude_bits(bin.im)) {
            bin.im = 0.0;
        }
        assert!(
            bin.re.is_finite() && bin.im.is_finite(),
            "qualified prefix filter spectrum overflowed",
        );
    }
    QualifiedPlan {
        filter_spectrum,
        bit_reverse_swaps: bit_reverse_swaps(fft_size),
    }
}

fn shared_plan(kind: QualifiedHalfDelayKind) -> &'static QualifiedPlan {
    static LEGACY: OnceLock<QualifiedPlan> = OnceLock::new();
    static HQ: OnceLock<QualifiedPlan> = OnceLock::new();
    match kind {
        QualifiedHalfDelayKind::Legacy384 => LEGACY.get_or_init(|| build_plan(kind)),
        QualifiedHalfDelayKind::Hq1024V1 => HQ.get_or_init(|| build_plan(kind)),
    }
}

#[inline]
fn outward_mul(left: f64, right: f64) -> f64 {
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
        return f64::MIN_POSITIVE;
    }
    next_up_nonnegative_bits(value_bits)
}

#[inline]
fn outward_add(left: f64, right: f64) -> f64 {
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

fn first_stage_l1_upper(kind: QualifiedHalfDelayKind) -> f64 {
    static LEGACY: OnceLock<f64> = OnceLock::new();
    static HQ: OnceLock<f64> = OnceLock::new();
    let slot = match kind {
        QualifiedHalfDelayKind::Legacy384 => &LEGACY,
        QualifiedHalfDelayKind::Hq1024V1 => &HQ,
    };
    *slot.get_or_init(|| {
        let half = kind
            .half_coefficients()
            .iter()
            .copied()
            .fold(0.0_f64, |sum, coefficient| outward_add(sum, coefficient.abs()));
        // The frozen half-delay FIR is symmetric and `half_coefficients` stores
        // exactly one side. Outward addition makes this a source-derived bound
        // on the complete binary64 coefficient L1 norm.
        outward_add(half, half)
    })
}

fn normal_power_of_two(exponent: i32) -> f64 {
    debug_assert!((-1022..=1023).contains(&exponent));
    f64::from_bits(((exponent + 1023) as u64) << 52)
}

/// Scale only very large blocks, and only by a normal exact binary power.
///
/// The target peak after scaling is at most four. This keeps every forward,
/// product and inverse intermediate far from overflow without perturbing
/// ordinary audio-scale arithmetic. The inverse scale is also normal.
fn overflow_safe_scale(component_peak: f64) -> (f64, f64) {
    if component_peak < 4.0 {
        return (1.0, 1.0);
    }
    let exponent_bits = ((component_peak.to_bits() >> 52) & 0x7ff) as i32;
    if exponent_bits == 0 || exponent_bits == 0x7ff {
        return (1.0, 1.0);
    }
    let unbiased = exponent_bits - 1023;
    let shift = (unbiased - 1).clamp(0, 1022);
    if shift == 0 {
        (1.0, 1.0)
    } else {
        (normal_power_of_two(-shift), normal_power_of_two(shift))
    }
}

fn block_error_upper(
    kind: QualifiedHalfDelayKind,
    component_peak: f64,
    inverse_block_scale: f64,
) -> f64 {
    if magnitude_bits(component_peak) == 0 {
        return 0.0;
    }
    let relative = outward_mul(
        kind.relative_error_per_packed_component_peak_upper(),
        component_peak,
    );
    let absolute = outward_mul(kind.absolute_error_upper(), inverse_block_scale);
    outward_add(relative, absolute)
}

/// Fixed-size authoritative 2x first-stage executor used by the certified
/// scanners.
///
/// Integer phases are exact delayed source samples. Half phases are produced
/// by the owned fixed radix-2 overlap-save transform and carry a per-channel
/// absolute enclosure. For a packed stereo pair the enclosure scales from the
/// maximum absolute component across both channels and the full contributing
/// history/pending block, never from the quiet channel alone.
#[derive(Clone)]
pub(crate) struct QualifiedHalfDelayFft {
    kind: QualifiedHalfDelayKind,
    plan: &'static QualifiedPlan,
    fft_buffer: Vec<Complex64>,
    history: Vec<Vec<f64>>,
    pending: Vec<f64>,
    pending_start_index: Option<i128>,
    half_output: Vec<f64>,
    half_error: Vec<f64>,
    integer_output: Vec<f64>,
    integer_error: Vec<f64>,
    channels: usize,
    fast90_same_graph_avx: bool,
}

impl fmt::Debug for QualifiedHalfDelayFft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QualifiedHalfDelayFft")
            .field("kind", &self.kind)
            .field("channels", &self.channels)
            .field("pending_frames", &(self.pending.len() / self.channels))
            .field("fast90_same_graph_avx", &self.fast90_same_graph_avx)
            .finish_non_exhaustive()
    }
}

impl QualifiedHalfDelayFft {
    pub(crate) fn new(reconstruction: ReconstructionId, channels: usize) -> Self {
        Self::new_with_fast90_executor(reconstruction, channels, false)
    }

    pub(crate) fn new_fast90(reconstruction: ReconstructionId, channels: usize) -> Self {
        Self::new_with_fast90_executor(reconstruction, channels, true)
    }

    fn new_with_fast90_executor(
        reconstruction: ReconstructionId,
        channels: usize,
        request_fast90_same_graph_avx: bool,
    ) -> Self {
        assert!(channels > 0, "qualified half-delay FFT requires channels");
        let kind = QualifiedHalfDelayKind::from_reconstruction(reconstruction);
        let plan = shared_plan(kind);
        let block_frames = kind.block_frames();
        let taps = kind.taps();
        Self {
            kind,
            plan,
            fft_buffer: vec![Complex64::new(0.0, 0.0); kind.fft_size()],
            history: vec![vec![0.0; taps - 1]; channels],
            pending: Vec::with_capacity(block_frames * channels),
            pending_start_index: None,
            half_output: vec![0.0; block_frames * channels],
            half_error: vec![0.0; block_frames * channels],
            integer_output: vec![0.0; channels],
            integer_error: vec![0.0; channels],
            channels,
            fast90_same_graph_avx: request_fast90_same_graph_avx && fast90_avx_available(),
        }
    }

    pub(crate) const fn block_frames(&self) -> usize {
        self.kind.block_frames()
    }

    pub(crate) const fn fast90_same_graph_avx_active(&self) -> bool {
        self.fast90_same_graph_avx
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub(crate) fn next_frame_completes_block(&self) -> bool {
        self.pending.len() / self.channels + 1 == self.kind.block_frames()
    }

    pub(crate) fn process_frame<F>(
        &mut self,
        frame: &[f64],
        input_index: i128,
        emit: F,
    ) -> bool
    where
        F: FnMut(i128, &[f64], &[f64], &[f64], &[f64]),
    {
        self.process_frame_with_fft_permission(frame, input_index, true, emit)
    }

    /// Feed one frame while allowing a time-bounded caller to replace an FFT
    /// block with a conservative coefficient-norm enclosure. Skipping the FFT
    /// does not skip input state: overlap history advances identically, integer
    /// knots remain exact, and every half knot is emitted as `0 +/- L1*peak`.
    pub(crate) fn process_frame_with_fft_permission<F>(
        &mut self,
        frame: &[f64],
        input_index: i128,
        execute_fft: bool,
        mut emit: F,
    ) -> bool
    where
        F: FnMut(i128, &[f64], &[f64], &[f64], &[f64]),
    {
        debug_assert_eq!(frame.len(), self.channels);
        let pending_frames = self.pending.len() / self.channels;
        match self.pending_start_index {
            None => self.pending_start_index = Some(input_index),
            Some(start) => debug_assert_eq!(input_index, start + pending_frames as i128),
        }
        self.pending.extend_from_slice(frame);
        if self.pending.len() == self.kind.block_frames() * self.channels {
            self.process_pending(&mut emit, execute_fft);
            true
        } else {
            false
        }
    }

    pub(crate) fn flush<F>(&mut self, emit: F) -> bool
    where
        F: FnMut(i128, &[f64], &[f64], &[f64], &[f64]),
    {
        self.flush_with_fft_permission(true, emit)
    }

    pub(crate) fn flush_with_fft_permission<F>(&mut self, execute_fft: bool, mut emit: F) -> bool
    where
        F: FnMut(i128, &[f64], &[f64], &[f64], &[f64]),
    {
        if self.pending.is_empty() {
            false
        } else {
            self.process_pending(&mut emit, execute_fft);
            true
        }
    }

    fn process_pending<F>(&mut self, emit: &mut F, execute_fft: bool)
    where
        F: FnMut(i128, &[f64], &[f64], &[f64], &[f64]),
    {
        let count = self.pending.len() / self.channels;
        if count == 0 {
            return;
        }
        debug_assert!(count <= self.kind.block_frames());
        let start = self
            .pending_start_index
            .expect("non-empty qualified FFT block has a start index");
        let taps = self.kind.taps();
        let overlap = taps - 1;
        let fft_size = self.kind.fft_size();
        let inverse_fft_scale = 1.0 / fft_size as f64;

        for channel_pair_start in (0..self.channels).step_by(2) {
            let second =
                (channel_pair_start + 1 < self.channels).then_some(channel_pair_start + 1);

            // IEEE positive finite encodings are monotonically ordered. Keep
            // the packed-pair/component maximum in integer space so DAZ cannot
            // collapse a nonzero subnormal source block to exact silence.
            let mut component_peak_bits = 0_u64;
            for history_index in 0..overlap {
                component_peak_bits = component_peak_bits.max(magnitude_bits(
                    self.history[channel_pair_start][history_index],
                ));
                if let Some(channel) = second {
                    component_peak_bits = component_peak_bits
                        .max(magnitude_bits(self.history[channel][history_index]));
                }
            }
            for frame_index in 0..count {
                component_peak_bits = component_peak_bits.max(magnitude_bits(
                    self.pending[frame_index * self.channels + channel_pair_start],
                ));
                if let Some(channel) = second {
                    component_peak_bits = component_peak_bits.max(magnitude_bits(
                        self.pending[frame_index * self.channels + channel],
                    ));
                }
            }

            if component_peak_bits == 0 {
                for frame_index in 0..count {
                    self.half_output[frame_index * self.channels + channel_pair_start] = 0.0;
                    self.half_error[frame_index * self.channels + channel_pair_start] = 0.0;
                    if let Some(channel) = second {
                        self.half_output[frame_index * self.channels + channel] = 0.0;
                        self.half_error[frame_index * self.channels + channel] = 0.0;
                    }
                }
                continue;
            }

            let component_peak = f64::from_bits(component_peak_bits);
            if !execute_fft {
                let half_upper = outward_mul(first_stage_l1_upper(self.kind), component_peak);
                for frame_index in 0..count {
                    self.half_output[frame_index * self.channels + channel_pair_start] = 0.0;
                    self.half_error[frame_index * self.channels + channel_pair_start] = half_upper;
                    if let Some(channel) = second {
                        self.half_output[frame_index * self.channels + channel] = 0.0;
                        self.half_error[frame_index * self.channels + channel] = half_upper;
                    }
                }
                continue;
            }

            let (block_scale, inverse_block_scale) = overflow_safe_scale(component_peak);
            self.fft_buffer.fill(Complex64::new(0.0, 0.0));
            for history_index in 0..overlap {
                self.fft_buffer[history_index].re =
                    self.history[channel_pair_start][history_index] * block_scale;
                if let Some(channel) = second {
                    self.fft_buffer[history_index].im =
                        self.history[channel][history_index] * block_scale;
                }
            }
            for frame_index in 0..count {
                self.fft_buffer[overlap + frame_index].re =
                    self.pending[frame_index * self.channels + channel_pair_start] * block_scale;
                if let Some(channel) = second {
                    self.fft_buffer[overlap + frame_index].im =
                        self.pending[frame_index * self.channels + channel] * block_scale;
                }
            }

            if self.fast90_same_graph_avx {
                fixed_radix2_fft_fast90(
                    &mut self.fft_buffer,
                    false,
                    &self.plan.bit_reverse_swaps,
                );
                pointwise_multiply_fast90(&mut self.fft_buffer, &self.plan.filter_spectrum);
                fixed_radix2_fft_fast90(
                    &mut self.fft_buffer,
                    true,
                    &self.plan.bit_reverse_swaps,
                );
            } else {
                fixed_radix2_fft(&mut self.fft_buffer, false);
                pointwise_multiply_scalar(&mut self.fft_buffer, &self.plan.filter_spectrum);
                fixed_radix2_fft(&mut self.fft_buffer, true);
            }

            let error = block_error_upper(self.kind, component_peak, inverse_block_scale);
            for frame_index in 0..count {
                let output = self.fft_buffer[overlap + frame_index];
                let re = (output.re * inverse_fft_scale) * inverse_block_scale;
                let im = (output.im * inverse_fft_scale) * inverse_block_scale;
                self.half_output[frame_index * self.channels + channel_pair_start] = re;
                self.half_error[frame_index * self.channels + channel_pair_start] = error;
                if let Some(channel) = second {
                    self.half_output[frame_index * self.channels + channel] = im;
                    self.half_error[frame_index * self.channels + channel] = error;
                }
            }
        }

        for frame_index in 0..count {
            for channel in 0..self.channels {
                self.integer_output[channel] = if frame_index >= taps / 2 {
                    self.pending[(frame_index - taps / 2) * self.channels + channel]
                } else {
                    let history_index = overlap + frame_index - taps / 2;
                    self.history[channel][history_index]
                };
            }
            let base = (start + frame_index as i128 - self.kind.group_delay_inputs()) * 2;
            let half = &self.half_output
                [frame_index * self.channels..(frame_index + 1) * self.channels];
            let half_error = &self.half_error
                [frame_index * self.channels..(frame_index + 1) * self.channels];
            emit(
                base,
                &self.integer_output,
                &self.integer_error,
                half,
                half_error,
            );
        }

        for channel in 0..self.channels {
            if count >= overlap {
                for history_index in 0..overlap {
                    let source_frame = count - overlap + history_index;
                    self.history[channel][history_index] =
                        self.pending[source_frame * self.channels + channel];
                }
            } else {
                self.history[channel].copy_within(count..overlap, 0);
                for frame_index in 0..count {
                    self.history[channel][overlap - count + frame_index] =
                        self.pending[frame_index * self.channels + channel];
                }
            }
        }
        self.pending.clear();
        self.pending_start_index = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            // SAFETY: restore the control word captured on this test thread.
            unsafe { write_mxcsr(self.0) };
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn skipped_fft_l1_enclosure_survives_daz_ftz_subnormal_input() {
        const DAZ: u32 = 1 << 6;
        const FTZ: u32 = 1 << 15;
        // SAFETY: MXCSR is thread-local; the guard restores it on every exit.
        let original = unsafe { read_mxcsr() };
        let _restore = MxcsrRestore(original);
        unsafe { write_mxcsr(original | DAZ | FTZ) };

        let mut bounded = QualifiedHalfDelayFft::new_fast90(ReconstructionId::Hq1024V1, 1);
        let block_frames = bounded.block_frames();
        let sample = f64::from_bits(7);
        let mut observed_nonzero_bound = false;
        for frame_index in 0..block_frames {
            bounded.process_frame_with_fft_permission(
                &[sample],
                frame_index as i128,
                false,
                |_base, _integer, _integer_error, half, half_error| {
                    assert_eq!(half[0].to_bits(), 0.0_f64.to_bits());
                    assert!(half_error[0].is_finite());
                    observed_nonzero_bound |= magnitude_bits(half_error[0]) != 0;
                },
            );
        }
        assert!(observed_nonzero_bound, "DAZ/FTZ collapsed a nonzero skipped-prefix enclosure");
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn avx_executor_is_bitwise_identical_to_scalar_authority_graph() {
        if !std::arch::is_x86_feature_detected!("avx") {
            return;
        }
        for fft_size in [LEGACY_FFT_SIZE, HQ1024_FFT_SIZE] {
            let swaps = bit_reverse_swaps(fft_size);
            for inverse in [false, true] {
                let source = (0..fft_size)
                    .map(|index| {
                        let x = index as f64;
                        Complex64::new(
                            (0.017 * x).sin() + 0.031 * (0.003 * x).cos(),
                            (0.029 * x).cos() - 0.013 * (0.007 * x).sin(),
                        )
                    })
                    .collect::<Vec<_>>();
                let mut scalar = source.clone();
                validate_fixed_fft_geometry(&scalar);
                bit_reverse_permute(&mut scalar);
                fixed_radix2_fft_scalar_stages(&mut scalar, inverse);

                let mut dispatched = source;
                fixed_radix2_fft_fast90(&mut dispatched, inverse, &swaps);
                for (index, (expected, actual)) in
                    scalar.iter().zip(dispatched.iter()).enumerate()
                {
                    assert_eq!(
                        expected.re.to_bits(),
                        actual.re.to_bits(),
                        "real lane drift at fft_size={fft_size} inverse={inverse} index={index}",
                    );
                    assert_eq!(
                        expected.im.to_bits(),
                        actual.im.to_bits(),
                        "imag lane drift at fft_size={fft_size} inverse={inverse} index={index}",
                    );
                }
            }
        }

        let mut scalar = (0..LEGACY_FFT_SIZE)
            .map(|index| {
                let x = index as f64;
                Complex64::new((0.11 * x).sin(), (0.07 * x).cos())
            })
            .collect::<Vec<_>>();
        let filter = (0..LEGACY_FFT_SIZE)
            .map(|index| {
                let x = index as f64;
                Complex64::new((0.05 * x).cos(), (0.09 * x).sin())
            })
            .collect::<Vec<_>>();
        let mut dispatched = scalar.clone();
        pointwise_multiply_scalar(&mut scalar, &filter);
        pointwise_multiply_fast90(&mut dispatched, &filter);
        for (index, (expected, actual)) in scalar.iter().zip(dispatched.iter()).enumerate() {
            assert_eq!(expected.re.to_bits(), actual.re.to_bits(), "pointwise real drift at {index}");
            assert_eq!(expected.im.to_bits(), actual.im.to_bits(), "pointwise imag drift at {index}");
        }
    }

    #[test]
    fn skipped_fft_block_is_conservative_and_does_not_break_later_fft_state() {
        for reconstruction in [
            ReconstructionId::LegacyHeadroom64,
            ReconstructionId::Hq1024V1,
        ] {
            let mut authority = QualifiedHalfDelayFft::new_fast90(reconstruction, 2);
            let mut bounded = QualifiedHalfDelayFft::new_fast90(reconstruction, 2);
            let block_frames = authority.block_frames();
            let mut authority_first = Vec::with_capacity(block_frames);
            let mut bounded_first = Vec::with_capacity(block_frames);

            for frame_index in 0..block_frames {
                let x = frame_index as f64;
                let frame = [
                    0.61 * (0.017 * x).sin() + 0.19 * (0.071 * x).cos(),
                    0.57 * (0.023 * x).cos() - 0.13 * (0.049 * x).sin(),
                ];
                authority.process_frame_with_fft_permission(
                    &frame,
                    frame_index as i128,
                    true,
                    |base, integer, integer_error, half, half_error| {
                        authority_first.push((
                            base,
                            integer.to_vec(),
                            integer_error.to_vec(),
                            half.to_vec(),
                            half_error.to_vec(),
                        ));
                    },
                );
                bounded.process_frame_with_fft_permission(
                    &frame,
                    frame_index as i128,
                    false,
                    |base, integer, integer_error, half, half_error| {
                        bounded_first.push((
                            base,
                            integer.to_vec(),
                            integer_error.to_vec(),
                            half.to_vec(),
                            half_error.to_vec(),
                        ));
                    },
                );
            }

            assert_eq!(authority_first.len(), block_frames);
            assert_eq!(bounded_first.len(), block_frames);
            for (expected, skipped) in authority_first.iter().zip(&bounded_first) {
                assert_eq!(expected.0, skipped.0);
                for channel in 0..2 {
                    assert_eq!(expected.1[channel].to_bits(), skipped.1[channel].to_bits());
                    assert_eq!(expected.2[channel].to_bits(), skipped.2[channel].to_bits());
                    assert_eq!(skipped.3[channel].to_bits(), 0.0_f64.to_bits());
                    assert!(skipped.4[channel].is_finite() && skipped.4[channel] > 0.0);
                    assert!(
                        f64::from_bits(magnitude_bits(expected.3[channel])) <= skipped.4[channel],
                        "qualified half knot escaped skipped-block L1 enclosure for {reconstruction:?}",
                    );
                }
            }

            // Skipping arithmetic must not skip stream state. The following
            // exact FFT block must therefore be bitwise identical to an engine
            // that executed the preceding block normally.
            let mut authority_second = Vec::with_capacity(block_frames);
            let mut bounded_second = Vec::with_capacity(block_frames);
            for local_index in 0..block_frames {
                let frame_index = block_frames + local_index;
                let x = frame_index as f64;
                let frame = [
                    0.61 * (0.017 * x).sin() + 0.19 * (0.071 * x).cos(),
                    0.57 * (0.023 * x).cos() - 0.13 * (0.049 * x).sin(),
                ];
                authority.process_frame_with_fft_permission(
                    &frame,
                    frame_index as i128,
                    true,
                    |base, integer, integer_error, half, half_error| {
                        authority_second.push((
                            base,
                            integer.to_vec(),
                            integer_error.to_vec(),
                            half.to_vec(),
                            half_error.to_vec(),
                        ));
                    },
                );
                bounded.process_frame_with_fft_permission(
                    &frame,
                    frame_index as i128,
                    true,
                    |base, integer, integer_error, half, half_error| {
                        bounded_second.push((
                            base,
                            integer.to_vec(),
                            integer_error.to_vec(),
                            half.to_vec(),
                            half_error.to_vec(),
                        ));
                    },
                );
            }
            assert_eq!(authority_second.len(), block_frames);
            assert_eq!(bounded_second.len(), block_frames);
            for (expected, actual) in authority_second.iter().zip(&bounded_second) {
                assert_eq!(expected.0, actual.0);
                for channel in 0..2 {
                    assert_eq!(expected.1[channel].to_bits(), actual.1[channel].to_bits());
                    assert_eq!(expected.2[channel].to_bits(), actual.2[channel].to_bits());
                    assert_eq!(expected.3[channel].to_bits(), actual.3[channel].to_bits());
                    assert_eq!(expected.4[channel].to_bits(), actual.4[channel].to_bits());
                }
            }
        }
    }

    #[test]
    fn fixed_fft_roundtrip_has_expected_power_of_two_scale() {
        for fft_size in [LEGACY_FFT_SIZE, HQ1024_FFT_SIZE] {
            let mut values = (0..fft_size)
                .map(|index| {
                    let x = index as f64;
                    Complex64::new((0.017 * x).sin(), (0.031 * x).cos())
                })
                .collect::<Vec<_>>();
            let original = values.clone();
            fixed_radix2_fft(&mut values, false);
            fixed_radix2_fft(&mut values, true);
            let inverse_scale = 1.0 / fft_size as f64;
            let max_error = values
                .iter()
                .zip(original.iter())
                .map(|(actual, expected)| {
                    let re = actual.re * inverse_scale - expected.re;
                    let im = actual.im * inverse_scale - expected.im;
                    re.hypot(im)
                })
                .fold(0.0_f64, f64::max);
            assert!(max_error < 1.0e-10, "roundtrip error {max_error:e}");
        }
    }
}
