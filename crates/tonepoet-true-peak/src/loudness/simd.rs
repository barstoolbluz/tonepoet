//! Private same-graph SIMD candidates for loudness arithmetic.
//!
//! x86_64 SSE2 production dispatch is commissioned after bitwise differential
//! qualification and repeatable release-build net-benefit measurement. AVX
//! remains a test-only candidate pending a separately justified promotion.

use super::k_weighting::{FilterState, KWeightingCoefficients};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Backend(BackendKind);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendKind {
    Scalar,
    #[cfg(target_arch = "x86_64")]
    Sse2,
    /// Test-only and harness-only candidate pending separate production
    /// commissioning; never returned by `production()`.
    #[cfg(target_arch = "x86_64")]
    Avx,
}

impl Backend {
    pub(super) const fn scalar() -> Self {
        Self(BackendKind::Scalar)
    }

    /// Select only a backend with completed differential and performance
    /// qualification for this production target.
    pub(super) fn production() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            if std::is_x86_feature_detected!("sse2") {
                return Self(BackendKind::Sse2);
            }
        }
        Self::scalar()
    }

    pub(super) const fn is_scalar(self) -> bool {
        matches!(self.0, BackendKind::Scalar)
    }

    #[cfg(target_arch = "x86_64")]
    pub(super) fn sse2_if_available() -> Option<Self> {
        std::is_x86_feature_detected!("sse2").then_some(Self(BackendKind::Sse2))
    }

    #[cfg(target_arch = "x86_64")]
    pub(super) fn avx_if_available() -> Option<Self> {
        std::is_x86_feature_detected!("avx").then_some(Self(BackendKind::Avx))
    }

    #[cfg(test)]
    pub(super) fn best_available() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            if let Some(backend) = Self::avx_if_available() {
                return backend;
            }
            if let Some(backend) = Self::sse2_if_available() {
                return backend;
            }
        }
        Self::scalar()
    }
}

/// Filter one interleaved frame across independent channel lanes. The caller
/// owns whole-block validation and treats any non-finite returned real-channel
/// value as a terminal numerical failure; the output slice is the caller-owned
/// tentative destination for this frame.
pub(super) fn filter_frame(
    backend: Backend,
    frame: &[f64],
    filters: &mut [FilterState],
    coefficients: KWeightingCoefficients,
    output: &mut [f64],
) {
    // These are soundness checks for the raw-pointer loads/stores in the x86
    // implementations, not merely debug diagnostics.
    assert_eq!(frame.len(), filters.len());
    assert_eq!(frame.len(), output.len());

    match backend.0 {
        BackendKind::Scalar => filter_frame_scalar(frame, filters, coefficients, output),
        #[cfg(target_arch = "x86_64")]
        BackendKind::Sse2 => {
            // SAFETY: BackendKind::Sse2 is only constructed after the one-time
            // feature check at backend selection; slice geometry is checked by
            // the safe caller and this function never loads past it.
            unsafe { filter_frame_sse2(frame, filters, coefficients, output) }
        }
        #[cfg(target_arch = "x86_64")]
        BackendKind::Avx => {
            // SAFETY: BackendKind::Avx is only constructed after the one-time
            // feature check at backend selection; all groups contain real
            // channels and scalar/SSE2 tails stay in bounds.
            unsafe { filter_frame_avx(frame, filters, coefficients, output) }
        }
    }
}

fn filter_frame_scalar(
    frame: &[f64],
    filters: &mut [FilterState],
    coefficients: KWeightingCoefficients,
    output: &mut [f64],
) {
    for ((sample, filter), out) in frame.iter().zip(filters.iter_mut()).zip(output.iter_mut()) {
        *out = filter.process(*sample, coefficients);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn filter_frame_sse2(
    frame: &[f64],
    filters: &mut [FilterState],
    coefficients: KWeightingCoefficients,
    output: &mut [f64],
) {
    use std::arch::x86_64::*;

    let a1 = _mm_set1_pd(coefficients.a[1]);
    let a2 = _mm_set1_pd(coefficients.a[2]);
    let a3 = _mm_set1_pd(coefficients.a[3]);
    let a4 = _mm_set1_pd(coefficients.a[4]);
    let b0 = _mm_set1_pd(coefficients.b[0]);
    let b1 = _mm_set1_pd(coefficients.b[1]);
    let b2 = _mm_set1_pd(coefficients.b[2]);
    let b3 = _mm_set1_pd(coefficients.b[3]);
    let b4 = _mm_set1_pd(coefficients.b[4]);

    let mut channel = 0;
    while channel + 2 <= frame.len() {
        let d0 = _mm_set_pd(filters[channel + 1].delay[0], filters[channel].delay[0]);
        let d1 = _mm_set_pd(filters[channel + 1].delay[1], filters[channel].delay[1]);
        let d2 = _mm_set_pd(filters[channel + 1].delay[2], filters[channel].delay[2]);
        let d3 = _mm_set_pd(filters[channel + 1].delay[3], filters[channel].delay[3]);
        let input = _mm_loadu_pd(frame.as_ptr().add(channel));

        // Preserve the scalar left-associated recurrence with separate packed
        // operations. SSE2 has no fused multiply-add instruction.
        let mut z = _mm_sub_pd(input, _mm_mul_pd(a1, d0));
        z = _mm_sub_pd(z, _mm_mul_pd(a2, d1));
        z = _mm_sub_pd(z, _mm_mul_pd(a3, d2));
        z = _mm_sub_pd(z, _mm_mul_pd(a4, d3));

        let mut out = _mm_mul_pd(b0, z);
        out = _mm_add_pd(out, _mm_mul_pd(b1, d0));
        out = _mm_add_pd(out, _mm_mul_pd(b2, d1));
        out = _mm_add_pd(out, _mm_mul_pd(b3, d2));
        out = _mm_add_pd(out, _mm_mul_pd(b4, d3));

        let mut z_values = [0.0_f64; 2];
        _mm_storeu_pd(z_values.as_mut_ptr(), z);
        _mm_storeu_pd(output.as_mut_ptr().add(channel), out);
        for lane in 0..2 {
            let state = &mut filters[channel + lane];
            state.delay[3] = state.delay[2];
            state.delay[2] = state.delay[1];
            state.delay[1] = state.delay[0];
            state.delay[0] = z_values[lane];
        }
        channel += 2;
    }
    if channel < frame.len() {
        output[channel] = filters[channel].process(frame[channel], coefficients);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
unsafe fn filter_frame_avx(
    frame: &[f64],
    filters: &mut [FilterState],
    coefficients: KWeightingCoefficients,
    output: &mut [f64],
) {
    use std::arch::x86_64::*;

    let a1 = _mm256_set1_pd(coefficients.a[1]);
    let a2 = _mm256_set1_pd(coefficients.a[2]);
    let a3 = _mm256_set1_pd(coefficients.a[3]);
    let a4 = _mm256_set1_pd(coefficients.a[4]);
    let b0 = _mm256_set1_pd(coefficients.b[0]);
    let b1 = _mm256_set1_pd(coefficients.b[1]);
    let b2 = _mm256_set1_pd(coefficients.b[2]);
    let b3 = _mm256_set1_pd(coefficients.b[3]);
    let b4 = _mm256_set1_pd(coefficients.b[4]);

    let mut channel = 0;
    while channel + 4 <= frame.len() {
        let d0 = _mm256_set_pd(
            filters[channel + 3].delay[0],
            filters[channel + 2].delay[0],
            filters[channel + 1].delay[0],
            filters[channel].delay[0],
        );
        let d1 = _mm256_set_pd(
            filters[channel + 3].delay[1],
            filters[channel + 2].delay[1],
            filters[channel + 1].delay[1],
            filters[channel].delay[1],
        );
        let d2 = _mm256_set_pd(
            filters[channel + 3].delay[2],
            filters[channel + 2].delay[2],
            filters[channel + 1].delay[2],
            filters[channel].delay[2],
        );
        let d3 = _mm256_set_pd(
            filters[channel + 3].delay[3],
            filters[channel + 2].delay[3],
            filters[channel + 1].delay[3],
            filters[channel].delay[3],
        );
        let input = _mm256_loadu_pd(frame.as_ptr().add(channel));

        // AVX alone cannot contract these operations into FMA. Keep every
        // multiply and subtract/add in the scalar source order.
        let mut z = _mm256_sub_pd(input, _mm256_mul_pd(a1, d0));
        z = _mm256_sub_pd(z, _mm256_mul_pd(a2, d1));
        z = _mm256_sub_pd(z, _mm256_mul_pd(a3, d2));
        z = _mm256_sub_pd(z, _mm256_mul_pd(a4, d3));

        let mut out = _mm256_mul_pd(b0, z);
        out = _mm256_add_pd(out, _mm256_mul_pd(b1, d0));
        out = _mm256_add_pd(out, _mm256_mul_pd(b2, d1));
        out = _mm256_add_pd(out, _mm256_mul_pd(b3, d2));
        out = _mm256_add_pd(out, _mm256_mul_pd(b4, d3));

        let mut z_values = [0.0_f64; 4];
        _mm256_storeu_pd(z_values.as_mut_ptr(), z);
        _mm256_storeu_pd(output.as_mut_ptr().add(channel), out);
        for lane in 0..4 {
            let state = &mut filters[channel + lane];
            state.delay[3] = state.delay[2];
            state.delay[2] = state.delay[1];
            state.delay[1] = state.delay[0];
            state.delay[0] = z_values[lane];
        }
        channel += 4;
    }

    if channel + 2 <= frame.len() {
        // SAFETY: AVX-capable x86-64 also has SSE2; the remaining two real
        // channels are in bounds.
        filter_frame_sse2(
            &frame[channel..channel + 2],
            &mut filters[channel..channel + 2],
            coefficients,
            &mut output[channel..channel + 2],
        );
        channel += 2;
    }
    if channel < frame.len() {
        output[channel] = filters[channel].process(frame[channel], coefficients);
    }
}

/// Accumulate per-channel square sums for the supplied ring spans. `spans` are
/// ordered exactly as the scalar profile requires; each tuple is `(start_frame,
/// frame_count)`. Only channels with nonzero loudness weight are evaluated.
pub(super) fn window_channel_sums(
    backend: Backend,
    ring: &[f64],
    channels: usize,
    weights: &[f64],
    spans: &[(usize, usize)],
    sums: &mut [f64],
) {
    assert_eq!(weights.len(), channels);
    assert!(sums.len() >= channels);
    sums[..channels].fill(0.0);

    match backend.0 {
        BackendKind::Scalar => window_channel_sums_scalar(ring, channels, weights, spans, sums),
        #[cfg(target_arch = "x86_64")]
        BackendKind::Sse2 => {
            // SAFETY: selection performed the one-time feature check. Only real
            // weighted channels are assembled into lanes and span loads stay
            // within caller-validated ring geometry.
            unsafe { window_channel_sums_sse2(ring, channels, weights, spans, sums) }
        }
        #[cfg(target_arch = "x86_64")]
        BackendKind::Avx => {
            // SAFETY: selection performed the one-time feature check and the
            // same bounds/real-lane invariants apply as in the SSE2 path.
            unsafe { window_channel_sums_avx(ring, channels, weights, spans, sums) }
        }
    }
}

fn window_channel_sums_scalar(
    ring: &[f64],
    channels: usize,
    weights: &[f64],
    spans: &[(usize, usize)],
    sums: &mut [f64],
) {
    for channel in 0..channels {
        if weights[channel] == 0.0 {
            continue;
        }
        let mut sum = 0.0;
        for &(start_frame, frame_count) in spans {
            for frame in start_frame..start_frame + frame_count {
                let sample = ring[frame * channels + channel];
                sum += sample * sample;
            }
        }
        sums[channel] = sum;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn accumulate_window_pair(
    ring: &[f64],
    channels: usize,
    spans: &[(usize, usize)],
    c0: usize,
    c1: usize,
    sums: &mut [f64],
) {
    use std::arch::x86_64::*;
    let mut acc = _mm_setzero_pd();
    for &(start_frame, frame_count) in spans {
        for frame in start_frame..start_frame + frame_count {
            let base = frame * channels;
            let samples = _mm_set_pd(ring[base + c1], ring[base + c0]);
            acc = _mm_add_pd(acc, _mm_mul_pd(samples, samples));
        }
    }
    let mut values = [0.0_f64; 2];
    _mm_storeu_pd(values.as_mut_ptr(), acc);
    sums[c0] = values[0];
    sums[c1] = values[1];
}

fn accumulate_window_scalar(
    ring: &[f64],
    channels: usize,
    spans: &[(usize, usize)],
    channel: usize,
    sums: &mut [f64],
) {
    let mut sum = 0.0;
    for &(start_frame, frame_count) in spans {
        for frame in start_frame..start_frame + frame_count {
            let sample = ring[frame * channels + channel];
            sum += sample * sample;
        }
    }
    sums[channel] = sum;
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn window_channel_sums_sse2(
    ring: &[f64],
    channels: usize,
    weights: &[f64],
    spans: &[(usize, usize)],
    sums: &mut [f64],
) {
    let mut pending = [0_usize; 2];
    let mut pending_len = 0;
    for (channel, weight) in weights.iter().copied().enumerate() {
        if weight == 0.0 {
            continue;
        }
        pending[pending_len] = channel;
        pending_len += 1;
        if pending_len == 2 {
            accumulate_window_pair(ring, channels, spans, pending[0], pending[1], sums);
            pending_len = 0;
        }
    }
    if pending_len == 1 {
        accumulate_window_scalar(ring, channels, spans, pending[0], sums);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
unsafe fn window_channel_sums_avx(
    ring: &[f64],
    channels: usize,
    weights: &[f64],
    spans: &[(usize, usize)],
    sums: &mut [f64],
) {
    use std::arch::x86_64::*;

    // Keep the grouping buffer on the stack and only for real weighted
    // channels. No persistent padding or per-window heap allocation is needed.
    let mut pending = [0_usize; 4];
    let mut pending_len = 0;
    for (channel, weight) in weights.iter().copied().enumerate() {
        if weight == 0.0 {
            continue;
        }
        pending[pending_len] = channel;
        pending_len += 1;
        if pending_len == 4 {
            let [c0, c1, c2, c3] = pending;
            let mut acc = _mm256_setzero_pd();
            for &(start_frame, frame_count) in spans {
                for frame in start_frame..start_frame + frame_count {
                    let base = frame * channels;
                    let samples = _mm256_set_pd(
                        ring[base + c3],
                        ring[base + c2],
                        ring[base + c1],
                        ring[base + c0],
                    );
                    acc = _mm256_add_pd(acc, _mm256_mul_pd(samples, samples));
                }
            }
            let mut values = [0.0_f64; 4];
            _mm256_storeu_pd(values.as_mut_ptr(), acc);
            sums[c0] = values[0];
            sums[c1] = values[1];
            sums[c2] = values[2];
            sums[c3] = values[3];
            pending_len = 0;
        }
    }

    if pending_len >= 2 {
        accumulate_window_pair(ring, channels, spans, pending[0], pending[1], sums);
    }
    if pending_len == 1 {
        accumulate_window_scalar(ring, channels, spans, pending[0], sums);
    } else if pending_len == 3 {
        accumulate_window_scalar(ring, channels, spans, pending[2], sums);
    }
}
