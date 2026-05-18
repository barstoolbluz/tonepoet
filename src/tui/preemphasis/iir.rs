//! IIR de-emphasis filter via bilinear transform.
//!
//! CD pre-emphasis (IEC 60908) uses time constants τ₁ = 50 µs and
//! τ₂ = 15 µs. The de-emphasis filter inverts this, restoring flat
//! response from pre-emphasized audio.

use std::f64::consts::PI;

/// Corner frequencies for CD pre-emphasis.
pub const F1: f64 = 1.0 / (2.0 * PI * 50e-6); // ≈ 3183.1 Hz
pub const F2: f64 = 1.0 / (2.0 * PI * 15e-6); // ≈ 10610.3 Hz

/// IIR de-emphasis filter coefficients.
#[derive(Debug, Clone, Copy)]
pub struct DeemphasisCoeffs {
    pub b0: f64,
    pub b1: f64,
    /// Stored as -a1 for the recurrence: y[n] = b0*x[n] + b1*x[n-1] + a1_neg*y[n-1]
    pub a1_neg: f64,
}

impl DeemphasisCoeffs {
    /// Compute de-emphasis filter coefficients via bilinear transform.
    pub fn new(sample_rate: u32) -> Self {
        let fs = sample_rate as f64;
        let a = 2.0 * 50e-6 * fs;
        let b = 2.0 * 15e-6 * fs;
        let b0 = (1.0 + b) / (1.0 + a);
        let b1 = (1.0 - b) / (1.0 + a);
        let a1 = (1.0 - a) / (1.0 + a);
        Self {
            b0,
            b1,
            a1_neg: -a1,
        }
    }
}

/// Stateful IIR de-emphasis filter.
#[derive(Debug, Clone)]
pub struct DeemphasisFilter {
    coeffs: DeemphasisCoeffs,
    x_prev: f64,
    y_prev: f64,
}

impl DeemphasisFilter {
    /// Create a new filter for the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            coeffs: DeemphasisCoeffs::new(sample_rate),
            x_prev: 0.0,
            y_prev: 0.0,
        }
    }

    /// Reset filter state (for processing a new segment).
    pub fn reset(&mut self) {
        self.x_prev = 0.0;
        self.y_prev = 0.0;
    }

    /// Process a single sample.
    #[inline]
    pub fn process_sample(&mut self, x: f64) -> f64 {
        let y =
            self.coeffs.b0 * x + self.coeffs.b1 * self.x_prev + self.coeffs.a1_neg * self.y_prev;
        self.x_prev = x;
        self.y_prev = y;
        y
    }

    /// Process a buffer of samples in-place.
    pub fn process_block(&mut self, samples: &mut [f64]) {
        for s in samples.iter_mut() {
            *s = self.process_sample(*s);
        }
    }

    /// Process a buffer, returning a new Vec with de-emphasized samples.
    pub fn process_to_vec(&mut self, samples: &[f64]) -> Vec<f64> {
        samples.iter().map(|&x| self.process_sample(x)).collect()
    }
}

/// Theoretical pre-emphasis gain at frequency f (dB).
/// This is the gain that pre-emphasis ADDS to the signal.
pub fn theoretical_gain_db(freq: f64) -> f64 {
    let ratio = (1.0 + (freq / F1).powi(2)) / (1.0 + (freq / F2).powi(2));
    10.0 * ratio.log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coefficients_44100() {
        let c = DeemphasisCoeffs::new(44100);
        // Known values from the design doc.
        assert!((c.b0 - 0.4294).abs() < 0.001);
        assert!((c.b1 - (-0.0597)).abs() < 0.001);
        assert!((c.a1_neg - 0.6303).abs() < 0.001);
    }

    #[test]
    fn test_theoretical_gain() {
        // At DC, gain should be ~0 dB.
        assert!(theoretical_gain_db(10.0).abs() < 0.01);
        // At 20 kHz, gain should be ~9.5 dB.
        let gain_20k = theoretical_gain_db(20000.0);
        assert!((gain_20k - 9.5).abs() < 0.2);
    }

    #[test]
    fn test_filter_unity_at_dc() {
        // At DC (constant input), output should equal input (0 dB gain).
        let mut filter = DeemphasisFilter::new(44100);
        // Feed constant 1.0 for many samples to reach steady state.
        let mut output = 0.0;
        for _ in 0..10000 {
            output = filter.process_sample(1.0);
        }
        // De-emphasis at DC should pass through (gain = 0 dB → ratio = 1.0).
        assert!((output - 1.0).abs() < 0.001);
    }
}
