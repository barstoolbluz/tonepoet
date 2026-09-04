//! Streaming true-peak measurement for decoded PCM audio.
//!
//! This crate deliberately has no file, process, or application-policy layer.
//! Callers provide decoded interleaved `f64` frames and receive a level.
//!
//! `Reporting4x` follows libebur128's public true-peak profile: the 49-tap
//! Hann-windowed interpolator runs at 4x below 96 kHz, 2x from 96 kHz up to
//! 192 kHz, and reports sample peak at 192 kHz and above. The separate
//! headroom ladder provides `Headroom64x` as the default gold-standard point
//! estimate plus deliberate 16x and 8x speed/accuracy opt-ins. Every rung has
//! its own band-qualified one-sided authority rather than pretending that a
//! finite interpolator can prove a uniform bound at critical Nyquist. Album
//! hard-ceiling policy uses the separately named finite reconstruction meter,
//! whose governed 64x waveform contract is unchanged by the selected scan rung.

use std::error::Error;
use std::f64::consts::PI;
use std::fmt;

mod headroom64_coefficients;
use headroom64_coefficients::HEADROOM64_HALF_DELAY_COEFFICIENTS;

const COEFFICIENT_EPSILON: f64 = 1.0e-15;
const REPORTING_TAPS: usize = 49;

// Headroom64x is a six-stage 2x cascade. The expensive original-band stage is
// not another arbitrary windowed-sinc: integer samples pass through exactly,
// while only the half-sample phase uses a 384-tap Type-II equiripple
// fractional-delay FIR designed over 0..0.99 of original Nyquist
// (0..0.495 * Fs). Symmetry reduces that phase to 192 coefficient products
// per input frame/channel. Later 2x stages see an already-oversampled signal
// and remain short Blackman-windowed interpolation filters.
const HEADROOM64_HALF_DELAY_TAPS: usize = 384;
const HEADROOM64_STAGE_2_TAPS: usize = 49;
const HEADROOM64_STAGE_3_TAPS: usize = 25;
const HEADROOM64_STAGE_4_TAPS: usize = 17;
const HEADROOM64_STAGE_5_TAPS: usize = 13;
const HEADROOM64_STAGE_6_TAPS: usize = 9;

// Center the small residual interpolation ripple of the complete 64x cascade.
// Decoded sample peak is tracked independently and is never scaled or clamped.
const HEADROOM64_INTERPOLATION_CALIBRATION_LINEAR: f64 = 0.999_539_589_003_087_8;

// The opt-in fast paths reuse prefixes of the frozen Headroom64 cascade.
// The 16x path is biased upward by 0.007 dB: this spends a tiny portion of
// point-estimate symmetry to keep the complete response-plus-grid budget inside
// the declared 0.044 dB one-sided envelope while materially reducing work. The
// 8x path uses a larger +0.088 dB bias for the same one-sided purpose.
const HEADROOM16_INTERPOLATION_CALIBRATION_LINEAR: f64 = 1.000_806_229_611_061_6;
const HEADROOM8_INTERPOLATION_CALIBRATION_LINEAR: f64 = 1.010_182_870_544_833_2;

/// Highest input frequency for which the `Headroom64x` authority contract is
/// qualified, expressed as a fraction of the input sample rate.
///
/// Nyquist is 0.5. The deliberate 0.005 * Fs guard band is what lets a finite,
/// bounded-state interpolator make an honest small-error claim. Point
/// measurement is still available outside this band, but the crate refuses to
/// turn it into a safety authority.
pub const HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE: f64 = 0.495;

/// Analytic worst-case sampling-grid under-read for a 64x grid, in dB.
///
/// This is `-20*log10(cos(pi/(2*64)))`. It is only the grid component, not the
/// complete authority reserve.
pub const HEADROOM64X_GRID_MAX_UNDERREAD_DB: f64 = 0.002_616_421_594_233;

/// Qualified one-sided under-read reserve for `Headroom64x`, in dB.
///
/// Qualification combines analytically known aligned signals, deterministic
/// frequency/phase and multitone searches, finite-stream variants, and an
/// independent high-resolution reference. Three deterministic seeds across
/// the design and adversarial gates cover 16,000 exact-peak cases; their
/// worst observed under-read is below 0.018 dB after final calibration. A separate
/// complete-cascade response budget (interpolation + 64x grid + numerical
/// allowance) is below 0.021 dB. The 0.030 dB reserve retains roughly 0.009 dB
/// of engineering margin while staying well inside the requested 0.05 dB
/// authority ceiling. The reserve is valid only inside
/// `HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE`.
pub const HEADROOM64X_MAX_UNDERREAD_DB: f64 = 0.030_000_000;

/// Qualified one-sided under-read reserve for the opt-in middle-speed path.
///
/// This path evaluates the first four stages of the frozen Headroom64 cascade
/// on a 16x grid and applies a +0.007 dB one-sided calibration. Offline
/// qualification covers the complete prefix response, the analytic 16x
/// sampling-grid miss, deterministic exact-peak searches, and finite-stream
/// edge variants. The derived response-plus-grid component budget is about
/// 0.04125 dB, leaving explicit margin inside the operator-approved
/// 0.042-0.044 dB envelope.
pub const HEADROOM16X_MAX_UNDERREAD_DB: f64 = 0.044_000_000;

/// Qualified one-sided under-read reserve for the opt-in fastest path.
///
/// The 8x prefix is calibrated +0.088 dB so its coarser sampling grid remains
/// conservative enough for a useful one-sided authority.  Qualification
/// combines the complete-prefix response with the 8x grid miss and retains a
/// small margin below the operator-approved 0.082-0.084 dB envelope.
pub const HEADROOM8X_MAX_UNDERREAD_DB: f64 = 0.084_000_000;

/// The two fast headroom paths are qualified over the same source band as
/// Headroom64x because they reuse its frozen first-stage fractional-delay FIR.
pub const HEADROOM_FAST_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE: f64 =
    HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE;

/// Conservative induced L-infinity difference between the full uncalibrated
/// Headroom64 reconstruction and cubic interpolation of the fast 16x prefix.
///
/// Four-point cubic interpolation uses neighboring 16x prefix knots. The
/// independently recomputed maximum phase-wise absolute coefficient sum is
/// 0.0028500955108182; 0.0030 retains explicit coefficient-generation margin.
pub const HEADROOM16X_TO_64X_RECONSTRUCTION_LINF_ERROR_UPPER: f64 = 0.003_0;

/// Conservative induced L-infinity difference between the full uncalibrated
/// Headroom64 reconstruction and cubic interpolation of the fast 8x prefix.
///
/// Four-point cubic interpolation uses neighboring 8x prefix knots.  The
/// independently recomputed maximum phase-wise absolute coefficient sum is
/// 0.0029326006842505; 0.0030 retains explicit coefficient-generation margin.
pub const HEADROOM8X_TO_64X_RECONSTRUCTION_LINF_ERROR_UPPER: f64 = 0.003_0;

/// Floating-point enclosure for the fast-prefix reconstruction bridge,
/// expressed per unit decoded sample peak.
///
/// This is intentionally much looser than the full-64x 1e-11 evaluation
/// allowance: the fast stages pair symmetric taps and both fast ceiling
/// bridges evaluate cubic Bernstein controls. At 1e-9 the allowance remains
/// acoustically immaterial while comfortably covering binary64 rounding in
/// those extra operations.
const HEADROOM_FAST_RECONSTRUCTION_NUMERIC_ERROR_PER_INPUT_PEAK_UPPER: f64 = 1.0e-9;

const HEADROOM64X_AUTHORITY_LINEAR_SCALE: f64 = 1.003_459_849_147_839_3;
const HEADROOM16X_AUTHORITY_LINEAR_SCALE: f64 = 1.005_078_539_490_737;
const HEADROOM8X_AUTHORITY_LINEAR_SCALE: f64 = 1.009_717_771_242_342;

/// Conservative induced L-infinity gain of Tonepoet's uncalibrated
/// Headroom64 reconstruction from original-rate sample error to the 64x
/// reconstruction grid. Piecewise-linear interpolation between adjacent grid
/// points cannot increase this norm.
///
/// The independently derived maximum absolute coefficient sum of the complete
/// six-stage polyphase cascade is 4.089899431660599 on the qualified source
/// coefficients. The published upper is deliberately widened to 4.09 rather
/// than resting on a one-ULP margin: the five later Blackman stages are derived
/// from `sin`/`cos` at meter construction, and supported platform libm results
/// may differ at the last few bits. The extra 2.46e-5 relative margin is
/// negligible when applied only to terminal LSB-scale errors but keeps the
/// bound comfortably above such coefficient-generation variation.
///
/// This is used only to translate a deterministic terminal sample-error bound
/// into the reconstruction domain; it is not the published Headroom64x
/// point-estimation accuracy reserve.
pub const HEADROOM64X_RECONSTRUCTION_LINF_GAIN_UPPER: f64 = 4.09;

/// Conservative floating-point evaluation allowance, expressed as linear
/// reconstruction amplitude per unit decoded sample peak. A deliberately
/// pessimistic Higham-gamma propagation through all six stages remains below
/// 3.4e-12 on IEEE-754 binary64; 1e-11 retains nearly 3x margin. Keeping this
/// separate from the signal reconstruction norm makes numerical enclosure
/// explicit without spending interval-arithmetic work in the hot loop.
const HEADROOM64X_RECONSTRUCTION_NUMERIC_ERROR_PER_INPUT_PEAK_UPPER: f64 = 1.0e-11;

/// Oversampling mode used for true-peak evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruePeakMode {
    /// libebur128-compatible reporting profile, using up to four-times
    /// interpolation depending on input sample rate.
    Reporting4x,
    /// Sixty-four-times high-accuracy interpolation for headroom decisions.
    Headroom64x,
    /// Opt-in 16x headroom estimate with a qualified 0.044 dB one-sided bound.
    Headroom16x,
    /// Opt-in 8x headroom estimate with a qualified 0.084 dB one-sided bound.
    Headroom8x,
}

impl TruePeakMode {
    /// Maximum interpolation factor associated with this mode.
    ///
    /// `Reporting4x` intentionally reduces to 2x at 96-192 kHz and to sample
    /// peak at 192 kHz and above, matching libebur128's reporting profile.
    #[must_use]
    pub const fn oversample_factor(self) -> usize {
        match self {
            Self::Reporting4x => 4,
            Self::Headroom64x => 64,
            Self::Headroom16x => 16,
            Self::Headroom8x => 8,
        }
    }

    /// Effective interpolation factor for a concrete input sample rate.
    #[must_use]
    pub const fn oversample_factor_for_sample_rate(self, sample_rate_hz: u32) -> usize {
        match self {
            Self::Reporting4x if sample_rate_hz >= 192_000 => 1,
            Self::Reporting4x if sample_rate_hz >= 96_000 => 2,
            Self::Reporting4x => 4,
            Self::Headroom64x => 64,
            Self::Headroom16x => 16,
            Self::Headroom8x => 8,
        }
    }

    /// Qualified one-sided under-read reserve for headroom modes.
    ///
    /// `Reporting4x` answers the separate libebur128 reporting question and
    /// therefore deliberately has no headroom authority reserve.
    #[must_use]
    pub const fn max_underread_db(self) -> Option<f64> {
        match self {
            Self::Reporting4x => None,
            Self::Headroom64x => Some(HEADROOM64X_MAX_UNDERREAD_DB),
            Self::Headroom16x => Some(HEADROOM16X_MAX_UNDERREAD_DB),
            Self::Headroom8x => Some(HEADROOM8X_MAX_UNDERREAD_DB),
        }
    }
}

/// Headroom scan ladder exposed to callers that need both a point estimate and
/// Tonepoet's finite hard-ceiling reconstruction authority.
///
/// Names describe the user-visible trade rather than an implementation factor;
/// callers can show the declared dB bounds through [`Self::max_underread_db`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HeadroomScanMode {
    /// Gold-standard Headroom64x scan. This remains the default.
    #[default]
    Reference,
    /// Materially faster 16x-prefix scan.
    Fast,
    /// Fastest 8x-prefix scan.
    Fastest,
}

impl HeadroomScanMode {
    /// Point-estimator implementation used by this scan rung.
    #[must_use]
    pub const fn point_mode(self) -> TruePeakMode {
        match self {
            Self::Reference => TruePeakMode::Headroom64x,
            Self::Fast => TruePeakMode::Headroom16x,
            Self::Fastest => TruePeakMode::Headroom8x,
        }
    }

    /// Declared qualified one-sided point under-read reserve in dB.
    #[must_use]
    pub const fn max_underread_db(self) -> f64 {
        match self {
            Self::Reference => HEADROOM64X_MAX_UNDERREAD_DB,
            Self::Fast => HEADROOM16X_MAX_UNDERREAD_DB,
            Self::Fastest => HEADROOM8X_MAX_UNDERREAD_DB,
        }
    }
}

/// Boundary convention for samples required outside a finite input stream.
///
/// This setting is used by all headroom modes. `Reporting4x` deliberately ignores
/// it and follows libebur128's finite-stream contract instead: zero-initialized
/// interpolation state, no synthetic pre-roll, and no synthetic end flush.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgePolicy {
    /// Extend the first and last decoded frame outward.
    RepeatEndpoints,
    /// Treat samples outside the finite stream as digital zero.
    ZeroExtend,
}

/// Configuration for one streaming meter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruePeakConfig {
    /// Sample rate of the decoded input frames.
    pub sample_rate_hz: u32,
    /// Number of interleaved channels in each decoded frame.
    pub channels: usize,
    /// Oversampling mode.
    pub mode: TruePeakMode,
    /// Finite-stream boundary convention for headroom modes.
    ///
    /// `Reporting4x` has fixed libebur128-compatible finite-stream semantics
    /// and ignores this field.
    pub edge_policy: EdgePolicy,
}

impl TruePeakConfig {
    /// Construct the default interoperable reporting configuration.
    #[must_use]
    pub const fn new(sample_rate_hz: u32, channels: usize) -> Self {
        Self {
            sample_rate_hz,
            channels,
            mode: TruePeakMode::Reporting4x,
            edge_policy: EdgePolicy::RepeatEndpoints,
        }
    }

    /// Select an oversampling mode.
    #[must_use]
    pub const fn with_mode(mut self, mode: TruePeakMode) -> Self {
        self.mode = mode;
        self
    }

    /// Select a finite-stream boundary convention.
    #[must_use]
    pub const fn with_edge_policy(mut self, edge_policy: EdgePolicy) -> Self {
        self.edge_policy = edge_policy;
        self
    }
}

/// A measured true-peak level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PeakLevel {
    /// Every decoded sample was exactly zero, so the logarithmic result is -inf.
    Silence,
    /// Finite true peak. `linear` is relative to digital full scale and is not
    /// clamped; values greater than 1.0 therefore produce positive dBTP.
    Finite { linear: f64, dbtp: f64 },
}

/// Error returned when a point estimate cannot be promoted to a qualified
/// headroom safety authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadroomAuthorityError {
    /// The caller supplied a negative, non-finite, or above-Nyquist band limit.
    InvalidBandLimit,
    /// The caller's declared signal bandwidth exceeds the qualified domain.
    OutsideQualifiedBand,
}

impl fmt::Display for HeadroomAuthorityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBandLimit => f.write_str(
                "headroom authority band limit must be finite and within 0..=0.5 of sample rate",
            ),
            Self::OutsideQualifiedBand => write!(
                f,
                "headroom authority is qualified only through {:.6} of sample rate",
                HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE,
            ),
        }
    }
}

impl Error for HeadroomAuthorityError {}

/// Promote a `Headroom64x` point estimate into a conservative safety authority.
///
/// `max_frequency_fraction_of_sample_rate` is a caller-supplied property of the
/// decoded signal path, not something the meter guesses from a finite block. A
/// value of 0.5 is Nyquist. The function refuses authority outside the frozen
/// qualified band instead of applying a misleading global reserve.
pub fn headroom64x_authority(
    point_estimate: PeakLevel,
    max_frequency_fraction_of_sample_rate: f64,
) -> Result<PeakLevel, HeadroomAuthorityError> {
    headroom_authority(
        point_estimate,
        max_frequency_fraction_of_sample_rate,
        HEADROOM64X_MAX_UNDERREAD_DB,
        HEADROOM64X_AUTHORITY_LINEAR_SCALE,
    )
}

/// Promote a `Headroom16x` point estimate into its qualified one-sided safety
/// authority.
pub fn headroom16x_authority(
    point_estimate: PeakLevel,
    max_frequency_fraction_of_sample_rate: f64,
) -> Result<PeakLevel, HeadroomAuthorityError> {
    headroom_authority(
        point_estimate,
        max_frequency_fraction_of_sample_rate,
        HEADROOM16X_MAX_UNDERREAD_DB,
        HEADROOM16X_AUTHORITY_LINEAR_SCALE,
    )
}

/// Promote a `Headroom8x` point estimate into its qualified one-sided safety
/// authority.
pub fn headroom8x_authority(
    point_estimate: PeakLevel,
    max_frequency_fraction_of_sample_rate: f64,
) -> Result<PeakLevel, HeadroomAuthorityError> {
    headroom_authority(
        point_estimate,
        max_frequency_fraction_of_sample_rate,
        HEADROOM8X_MAX_UNDERREAD_DB,
        HEADROOM8X_AUTHORITY_LINEAR_SCALE,
    )
}

fn headroom_authority(
    point_estimate: PeakLevel,
    max_frequency_fraction_of_sample_rate: f64,
    reserve_db: f64,
    linear_scale: f64,
) -> Result<PeakLevel, HeadroomAuthorityError> {
    if !max_frequency_fraction_of_sample_rate.is_finite()
        || !(0.0..=0.5).contains(&max_frequency_fraction_of_sample_rate)
    {
        return Err(HeadroomAuthorityError::InvalidBandLimit);
    }
    if max_frequency_fraction_of_sample_rate
        > HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE
    {
        return Err(HeadroomAuthorityError::OutsideQualifiedBand);
    }
    Ok(match point_estimate {
        PeakLevel::Silence => PeakLevel::Silence,
        PeakLevel::Finite { linear, dbtp } => PeakLevel::Finite {
            linear: linear * linear_scale,
            dbtp: dbtp + reserve_db,
        },
    })
}

impl PeakLevel {
    /// Return the logarithmic level, using negative infinity for silence.
    #[must_use]
    pub fn dbtp(self) -> f64 {
        match self {
            Self::Silence => f64::NEG_INFINITY,
            Self::Finite { dbtp, .. } => dbtp,
        }
    }

    /// Return the linear peak, using zero for silence.
    #[must_use]
    pub const fn linear(self) -> f64 {
        match self {
            Self::Silence => 0.0,
            Self::Finite { linear, .. } => linear,
        }
    }
}

/// Final result from a meter.
#[derive(Debug, Clone, PartialEq)]
pub struct TruePeakResult {
    /// Maximum level across all channels.
    pub overall: PeakLevel,
    /// Per-channel linear maxima, in input channel order.
    pub channel_linear_peaks: Vec<f64>,
    /// Number of decoded input frames consumed.
    pub frames: u64,
}

/// Input/configuration errors detected by the meter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TruePeakError {
    InvalidSampleRate,
    InvalidChannelCount,
    IncompleteFrame { samples: usize, channels: usize },
    NonFiniteSample { sample_index: usize },
    EmptyInput,
    InputTooLong,
}

impl fmt::Display for TruePeakError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSampleRate => f.write_str("sample rate must be greater than zero"),
            Self::InvalidChannelCount => f.write_str("channel count must be greater than zero"),
            Self::IncompleteFrame { samples, channels } => write!(
                f,
                "interleaved block has {samples} samples, not a whole number of {channels}-channel frames"
            ),
            Self::NonFiniteSample { sample_index } => write!(
                f,
                "decoded sample at block index {sample_index} is not finite"
            ),
            Self::EmptyInput => f.write_str("true-peak measurement requires at least one frame"),
            Self::InputTooLong => f.write_str("true-peak input frame count overflowed"),
        }
    }
}

impl Error for TruePeakError {}

#[derive(Debug, Clone, Copy)]
enum Window {
    Hann,
    Blackman,
}

#[derive(Debug, Clone)]
struct PhaseFilter {
    indices: Vec<usize>,
    coefficients: Vec<f64>,
}

#[derive(Debug, Clone)]
struct InterpolatorStage {
    factor: usize,
    group_delay_inputs: i128,
    filters: Vec<PhaseFilter>,
    delay: Vec<Vec<f64>>,
    delay_index: usize,
    channels: usize,
}

impl InterpolatorStage {
    fn new(
        taps: usize,
        factor: usize,
        channels: usize,
        window: Window,
        normalize_phase_dc: bool,
    ) -> Self {
        debug_assert!(taps > 0 && factor > 0);
        let delay_frames = (taps + factor - 1) / factor;
        let group_delay_inputs = ((taps - 1) / (2 * factor)) as i128;
        let filters = build_polyphase_filters(
            taps,
            factor,
            delay_frames,
            window,
            normalize_phase_dc,
        );
        Self {
            factor,
            group_delay_inputs,
            filters,
            delay: vec![vec![0.0; delay_frames]; channels],
            delay_index: 0,
            channels,
        }
    }

    fn process_frame(&mut self, frame: &[f64], input_index: i128, output: &mut [f64]) -> i128 {
        debug_assert_eq!(frame.len(), self.channels);
        debug_assert_eq!(output.len(), self.channels * self.factor);

        for (channel, sample) in frame.iter().copied().enumerate() {
            self.delay[channel][self.delay_index] = sample;
        }

        for (phase, filter) in self.filters.iter().enumerate() {
            let phase_output = &mut output[phase * self.channels..(phase + 1) * self.channels];
            for (channel, slot) in phase_output.iter_mut().enumerate() {
                let mut value = 0.0;
                for (&index, &coefficient) in filter.indices.iter().zip(&filter.coefficients) {
                    let delay_index = if self.delay_index >= index {
                        self.delay_index - index
                    } else {
                        self.delay[channel].len() + self.delay_index - index
                    };
                    value += self.delay[channel][delay_index] * coefficient;
                }
                *slot = value;
            }
        }

        self.delay_index += 1;
        if self.delay_index == self.delay[0].len() {
            self.delay_index = 0;
        }

        (input_index - self.group_delay_inputs) * self.factor as i128
    }
}

#[derive(Debug, Clone)]
struct ReportingEngine {
    factor: usize,
    stage: Option<InterpolatorStage>,
    scratch: Vec<f64>,
    channels: usize,
}

impl ReportingEngine {
    fn new(sample_rate_hz: u32, channels: usize) -> Self {
        let factor = if sample_rate_hz >= 192_000 {
            1
        } else if sample_rate_hz >= 96_000 {
            2
        } else {
            4
        };
        let stage = (factor > 1).then(|| {
            InterpolatorStage::new(REPORTING_TAPS, factor, channels, Window::Hann, false)
        });
        Self {
            factor,
            stage,
            scratch: vec![0.0; factor * channels],
            channels,
        }
    }

    fn pre_post_frames(&self) -> i128 {
        // libebur128 starts with calloc-zeroed interpolation state and does
        // not synthesize samples before or after a finite stream.  Returning
        // zero here keeps the shared streaming shell from adding either.
        0
    }

    fn process_frame(
        &mut self,
        frame: &[f64],
        input_index: i128,
        channel_peaks: &mut [f64],
        _upper_subframe: Option<i128>,
    ) {
        if let Some(stage) = &mut self.stage {
            // libebur128 evaluates every phase produced while each supplied
            // frame advances the zero-initialized delay line.  It does not
            // recenter the finite stream by the FIR group delay and clip away
            // the startup region; that startup response is part of its
            // interoperable reporting contract.
            let _base = stage.process_frame(frame, input_index, &mut self.scratch);
            for phase in 0..self.factor {
                update_channel_peaks(
                    channel_peaks,
                    &self.scratch[phase * self.channels..(phase + 1) * self.channels],
                );
            }
        } else {
            update_channel_peaks(channel_peaks, frame);
        }
    }
}

#[derive(Debug, Clone)]
struct HeadroomHalfSampleStage {
    // Each channel stores two identical copies of the circular delay line.
    // This makes every 384-frame history window contiguous and removes `%`
    // from the 192-product half-phase loop without changing coefficient or
    // accumulation order.
    delay: Vec<Vec<f64>>,
    delay_index: usize,
    channels: usize,
}

impl HeadroomHalfSampleStage {
    const GROUP_DELAY_INPUTS: i128 = (HEADROOM64_HALF_DELAY_TAPS / 2) as i128;

    fn new(channels: usize) -> Self {
        Self {
            delay: vec![vec![0.0; HEADROOM64_HALF_DELAY_TAPS * 2]; channels],
            delay_index: 0,
            channels,
        }
    }

    #[inline]
    fn process_frame(&mut self, frame: &[f64], input_index: i128, output: &mut [f64]) -> i128 {
        debug_assert_eq!(frame.len(), self.channels);
        debug_assert_eq!(output.len(), self.channels * 2);

        for (channel, sample) in frame.iter().copied().enumerate() {
            self.delay[channel][self.delay_index] = sample;
            self.delay[channel][self.delay_index + HEADROOM64_HALF_DELAY_TAPS] = sample;
        }

        for channel in 0..self.channels {
            let delay = &self.delay[channel];
            let base = self.delay_index;

            // Integer phase is exact. With the doubled ring, delayed(192) is
            // always directly addressable as base + 192.
            output[channel] = delay[base + HEADROOM64_HALF_DELAY_TAPS / 2];

            // Preserve the original summation order exactly. For coefficient
            // i, recent[i] is delayed(i) and old[i] is delayed(383-i).
            let recent = &delay[base + HEADROOM64_HALF_DELAY_TAPS / 2 + 1
                ..base + HEADROOM64_HALF_DELAY_TAPS + 1];
            let old = &delay[base + 1..base + HEADROOM64_HALF_DELAY_TAPS / 2 + 1];
            let mut half = 0.0;
            for ((recent_sample, old_sample), coefficient) in recent
                .iter()
                .rev()
                .zip(old.iter())
                .zip(HEADROOM64_HALF_DELAY_COEFFICIENTS.iter().copied())
            {
                half += coefficient * (*recent_sample + *old_sample);
            }
            output[self.channels + channel] = half;
        }

        self.delay_index += 1;
        if self.delay_index == HEADROOM64_HALF_DELAY_TAPS {
            self.delay_index = 0;
        }

        (input_index - Self::GROUP_DELAY_INPUTS) * 2
    }

    /// Same frozen first-stage filter with a fast-only accumulation schedule.
    ///
    /// Headroom64x keeps `process_frame` above, including its historical
    /// summation order. The opt-in modes split the 192-term half-phase dot
    /// product across four independent accumulators, shortening its serialized
    /// floating-point dependency chain from 192 products to 48 without
    /// changing coefficients, samples, or streaming state. The final reduction
    /// changes only last-bit rounding and is covered by the fast qualification's
    /// explicit numerical allowance.
    #[inline]
    fn process_frame_fast(
        &mut self,
        frame: &[f64],
        input_index: i128,
        output: &mut [f64],
    ) -> i128 {
        debug_assert_eq!(frame.len(), self.channels);
        debug_assert_eq!(output.len(), self.channels * 2);
        debug_assert_eq!(HEADROOM64_HALF_DELAY_COEFFICIENTS.len() % 4, 0);

        for (channel, sample) in frame.iter().copied().enumerate() {
            self.delay[channel][self.delay_index] = sample;
            self.delay[channel][self.delay_index + HEADROOM64_HALF_DELAY_TAPS] = sample;
        }

        for channel in 0..self.channels {
            let delay = &self.delay[channel];
            let base = self.delay_index;
            output[channel] = delay[base + HEADROOM64_HALF_DELAY_TAPS / 2];

            let recent = &delay[base + HEADROOM64_HALF_DELAY_TAPS / 2 + 1
                ..base + HEADROOM64_HALF_DELAY_TAPS + 1];
            let old = &delay[base + 1..base + HEADROOM64_HALF_DELAY_TAPS / 2 + 1];
            let mut sum0 = 0.0_f64;
            let mut sum1 = 0.0_f64;
            let mut sum2 = 0.0_f64;
            let mut sum3 = 0.0_f64;
            let mut index = 0_usize;
            while index < HEADROOM64_HALF_DELAY_COEFFICIENTS.len() {
                let i0 = index;
                let i1 = index + 1;
                let i2 = index + 2;
                let i3 = index + 3;
                sum0 += HEADROOM64_HALF_DELAY_COEFFICIENTS[i0]
                    * (recent[recent.len() - 1 - i0] + old[i0]);
                sum1 += HEADROOM64_HALF_DELAY_COEFFICIENTS[i1]
                    * (recent[recent.len() - 1 - i1] + old[i1]);
                sum2 += HEADROOM64_HALF_DELAY_COEFFICIENTS[i2]
                    * (recent[recent.len() - 1 - i2] + old[i2]);
                sum3 += HEADROOM64_HALF_DELAY_COEFFICIENTS[i3]
                    * (recent[recent.len() - 1 - i3] + old[i3]);
                index += 4;
            }
            output[self.channels + channel] = (sum0 + sum1) + (sum2 + sum3);
        }

        self.delay_index += 1;
        if self.delay_index == HEADROOM64_HALF_DELAY_TAPS {
            self.delay_index = 0;
        }

        (input_index - Self::GROUP_DELAY_INPUTS) * 2
    }
}

/// Fixed 2x interpolation stage used only by Headroom64x. The checked-in
/// mathematical filter is still generated by `build_polyphase_filters`; this
/// execution layout merely specializes its exact identity phase and stores a
/// doubled circular delay so the nontrivial phase is one contiguous dot
/// product.
#[derive(Debug, Clone)]
struct HeadroomTwoXStage {
    group_delay_inputs: i128,
    identity_index: usize,
    half_coefficients: Vec<f64>,
    delay: Vec<Vec<f64>>,
    delay_frames: usize,
    delay_index: usize,
    channels: usize,
}

impl HeadroomTwoXStage {
    fn new(taps: usize, channels: usize) -> Self {
        let delay_frames = (taps + 1) / 2;
        let filters = build_polyphase_filters(taps, 2, delay_frames, Window::Blackman, true);
        debug_assert_eq!(filters.len(), 2);
        debug_assert_eq!(filters[0].indices.len(), 1);
        debug_assert_eq!(filters[0].coefficients.len(), 1);
        debug_assert_eq!(filters[0].coefficients[0], 1.0);

        debug_assert!(filters[1]
            .indices
            .iter()
            .copied()
            .eq(0..filters[1].indices.len()));

        Self {
            group_delay_inputs: ((taps - 1) / 4) as i128,
            identity_index: filters[0].indices[0],
            half_coefficients: filters[1].coefficients.clone(),
            delay: vec![vec![0.0; delay_frames * 2]; channels],
            delay_frames,
            delay_index: 0,
            channels,
        }
    }

    #[inline]
    fn process_frame(&mut self, frame: &[f64], input_index: i128, output: &mut [f64]) -> i128 {
        debug_assert_eq!(frame.len(), self.channels);
        debug_assert_eq!(output.len(), self.channels * 2);

        for (channel, sample) in frame.iter().copied().enumerate() {
            self.delay[channel][self.delay_index] = sample;
            self.delay[channel][self.delay_index + self.delay_frames] = sample;
        }

        for channel in 0..self.channels {
            let delay = &self.delay[channel];
            let base = self.delay_index;
            // The generic reference computes `0.0 + sample * 1.0` for
            // this exact phase. `sample + 0.0` removes the redundant multiply
            // while preserving its +0.0 result for an input -0.0 as well as
            // every finite nonzero bit pattern.
            output[channel] = delay[base + self.delay_frames - self.identity_index] + 0.0;

            let history = &delay[base + self.delay_frames + 1 - self.half_coefficients.len()
                ..base + self.delay_frames + 1];
            let mut value = 0.0;
            for (sample, coefficient) in history
                .iter()
                .rev()
                .zip(self.half_coefficients.iter().copied())
            {
                value += *sample * coefficient;
            }
            output[self.channels + channel] = value;
        }

        self.delay_index += 1;
        if self.delay_index == self.delay_frames {
            self.delay_index = 0;
        }

        (input_index - self.group_delay_inputs) * 2
    }
}

/// Fast-only wrapper around the unchanged Headroom64 2x stage state.
///
/// The mathematical Blackman half phase is symmetric. Platform libm can
/// leave paired generated coefficients a few ULPs apart, so the opt-in paths
/// freeze each execution pair to its arithmetic mean at construction. Keeping
/// this data outside `HeadroomTwoXStage` means the reference Headroom64 object,
/// allocations, coefficients, and hot loop remain exactly unchanged.
#[derive(Debug, Clone)]
struct FastHeadroomTwoXStage {
    inner: HeadroomTwoXStage,
    symmetric_half_coefficients: Vec<f64>,
}

impl FastHeadroomTwoXStage {
    fn new(taps: usize, channels: usize) -> Self {
        let inner = HeadroomTwoXStage::new(taps, channels);
        debug_assert_eq!(inner.half_coefficients.len() % 2, 0);
        let half_len = inner.half_coefficients.len() / 2;
        let symmetric_half_coefficients = (0..half_len)
            .map(|index| {
                let mirror = inner.half_coefficients.len() - 1 - index;
                0.5 * (inner.half_coefficients[index] + inner.half_coefficients[mirror])
            })
            .collect();
        Self {
            inner,
            symmetric_half_coefficients,
        }
    }

    #[inline]
    fn group_delay_inputs(&self) -> i128 {
        self.inner.group_delay_inputs
    }

    #[inline]
    fn process_frame_symmetric(
        &mut self,
        frame: &[f64],
        input_index: i128,
        output: &mut [f64],
    ) -> i128 {
        let Self {
            inner: stage,
            symmetric_half_coefficients,
        } = self;
        debug_assert_eq!(frame.len(), stage.channels);
        debug_assert_eq!(output.len(), stage.channels * 2);

        for (channel, sample) in frame.iter().copied().enumerate() {
            stage.delay[channel][stage.delay_index] = sample;
            stage.delay[channel][stage.delay_index + stage.delay_frames] = sample;
        }

        let coefficient_len = stage.half_coefficients.len();
        for channel in 0..stage.channels {
            let delay = &stage.delay[channel];
            let base = stage.delay_index;
            output[channel] = delay[base + stage.delay_frames - stage.identity_index] + 0.0;

            let mut value = 0.0;
            for (index, coefficient) in symmetric_half_coefficients
                .iter()
                .copied()
                .enumerate()
            {
                let mirror = coefficient_len - 1 - index;
                let recent = delay[base + stage.delay_frames - index];
                let old = delay[base + stage.delay_frames - mirror];
                value += coefficient * (recent + old);
            }
            output[stage.channels + channel] = value;
        }

        stage.delay_index += 1;
        if stage.delay_index == stage.delay_frames {
            stage.delay_index = 0;
        }

        (input_index - stage.group_delay_inputs) * 2
    }
}

#[derive(Debug, Clone)]
struct HeadroomEngine {
    stage1: HeadroomHalfSampleStage,
    stage2: HeadroomTwoXStage,
    stage3: HeadroomTwoXStage,
    stage4: HeadroomTwoXStage,
    stage5: HeadroomTwoXStage,
    stage6: HeadroomTwoXStage,
    scratch1: Vec<f64>,
    scratch2: Vec<f64>,
    scratch3: Vec<f64>,
    scratch4: Vec<f64>,
    scratch5: Vec<f64>,
    scratch6: Vec<f64>,
    channels: usize,
    pre_post_frames: i128,
}

impl HeadroomEngine {
    fn new(channels: usize) -> Self {
        let stage1 = HeadroomHalfSampleStage::new(channels);
        let stage2 = HeadroomTwoXStage::new(HEADROOM64_STAGE_2_TAPS, channels);
        let stage3 = HeadroomTwoXStage::new(HEADROOM64_STAGE_3_TAPS, channels);
        let stage4 = HeadroomTwoXStage::new(HEADROOM64_STAGE_4_TAPS, channels);
        let stage5 = HeadroomTwoXStage::new(HEADROOM64_STAGE_5_TAPS, channels);
        let stage6 = HeadroomTwoXStage::new(HEADROOM64_STAGE_6_TAPS, channels);

        let mut delay_subframes = HeadroomHalfSampleStage::GROUP_DELAY_INPUTS * 2;
        for group_delay in [
            stage2.group_delay_inputs,
            stage3.group_delay_inputs,
            stage4.group_delay_inputs,
            stage5.group_delay_inputs,
            stage6.group_delay_inputs,
        ] {
            // `group_delay` is expressed in this stage's *input* frames.
            // Add it before converting the accumulated delay to the next 2x
            // output grid. This matches convolution of the complete cascade
            // and yields 12_816 final subframes = 200.25 input frames.
            delay_subframes = (delay_subframes + group_delay) * 2;
        }
        debug_assert_eq!(delay_subframes, 12_816);
        let pre_post_frames = (delay_subframes + 63) / 64;
        debug_assert_eq!(pre_post_frames, 201);

        Self {
            stage1,
            stage2,
            stage3,
            stage4,
            stage5,
            stage6,
            scratch1: vec![0.0; channels * 2],
            scratch2: vec![0.0; channels * 2],
            scratch3: vec![0.0; channels * 2],
            scratch4: vec![0.0; channels * 2],
            scratch5: vec![0.0; channels * 2],
            scratch6: vec![0.0; channels * 2],
            channels,
            pre_post_frames,
        }
    }

    fn process_frame(
        &mut self,
        frame: &[f64],
        input_index: i128,
        channel_peaks: &mut [f64],
        mut reconstruction_peaks: Option<&mut [f64]>,
        upper_subframe: Option<i128>,
    ) {
        let base1 = self.stage1.process_frame(frame, input_index, &mut self.scratch1);
        for phase1 in 0..2 {
            let frame1 = &self.scratch1[phase1 * self.channels..(phase1 + 1) * self.channels];
            let base2 = self.stage2.process_frame(frame1, base1 + phase1 as i128, &mut self.scratch2);
            for phase2 in 0..2 {
                let frame2 = &self.scratch2[phase2 * self.channels..(phase2 + 1) * self.channels];
                let base3 = self.stage3.process_frame(frame2, base2 + phase2 as i128, &mut self.scratch3);
                for phase3 in 0..2 {
                    let frame3 = &self.scratch3[phase3 * self.channels..(phase3 + 1) * self.channels];
                    let base4 = self.stage4.process_frame(frame3, base3 + phase3 as i128, &mut self.scratch4);
                    for phase4 in 0..2 {
                        let frame4 = &self.scratch4[phase4 * self.channels..(phase4 + 1) * self.channels];
                        let base5 = self.stage5.process_frame(frame4, base4 + phase4 as i128, &mut self.scratch5);
                        for phase5 in 0..2 {
                            let frame5 = &self.scratch5[phase5 * self.channels..(phase5 + 1) * self.channels];
                            let base6 = self.stage6.process_frame(frame5, base5 + phase5 as i128, &mut self.scratch6);
                            for phase6 in 0..2 {
                                let output_index = base6 + phase6 as i128;
                                if output_index < 0
                                    || upper_subframe
                                        .map(|upper| output_index > upper)
                                        .unwrap_or(false)
                                {
                                    continue;
                                }
                                let reconstructed = &self.scratch6
                                    [phase6 * self.channels..(phase6 + 1) * self.channels];
                                if let Some(peaks) = reconstruction_peaks.as_deref_mut() {
                                    // Ceiling mode needs the uncalibrated knot
                                    // maximum. Derive its ordinary calibrated
                                    // point estimate once at finalize instead
                                    // of doing two peak updates in this 64x hot
                                    // loop.
                                    update_channel_peaks(peaks, reconstructed);
                                } else {
                                    update_channel_peaks_scaled(
                                        channel_peaks,
                                        reconstructed,
                                        HEADROOM64_INTERPOLATION_CALIBRATION_LINEAR,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Prefix executor for the opt-in 16x and 8x headroom modes.
///
/// It intentionally owns a separate execution path so the Headroom64x gold
/// standard remains untouched. Stage one uses the frozen qualified filter with
/// a fast-only accumulation schedule; later stages pair the mathematically
/// symmetric Blackman half-phase taps. That reduces the 16x path to 272 FIR
/// coefficient products per original frame/channel and the 8x path to 240,
/// versus 576 for the reference path.
#[derive(Debug, Clone)]
struct FastHeadroomEngine {
    factor: usize,
    stage1: HeadroomHalfSampleStage,
    stage2: FastHeadroomTwoXStage,
    stage3: FastHeadroomTwoXStage,
    stage4: Option<FastHeadroomTwoXStage>,
    scratch1: Vec<f64>,
    scratch2: Vec<f64>,
    scratch3: Vec<f64>,
    scratch4: Vec<f64>,
    channels: usize,
    pre_post_frames: i128,
    calibration_linear: f64,
}

impl FastHeadroomEngine {
    fn new(mode: TruePeakMode, channels: usize) -> Self {
        debug_assert!(matches!(mode, TruePeakMode::Headroom16x | TruePeakMode::Headroom8x));
        let stage1 = HeadroomHalfSampleStage::new(channels);
        let stage2 = FastHeadroomTwoXStage::new(HEADROOM64_STAGE_2_TAPS, channels);
        let stage3 = FastHeadroomTwoXStage::new(HEADROOM64_STAGE_3_TAPS, channels);
        let (factor, stage4, calibration_linear) = match mode {
            TruePeakMode::Headroom16x => (
                16,
                Some(FastHeadroomTwoXStage::new(HEADROOM64_STAGE_4_TAPS, channels)),
                HEADROOM16_INTERPOLATION_CALIBRATION_LINEAR,
            ),
            TruePeakMode::Headroom8x => (8, None, HEADROOM8_INTERPOLATION_CALIBRATION_LINEAR),
            _ => unreachable!("fast headroom engine requires a fast headroom mode"),
        };

        let mut delay_subframes = HeadroomHalfSampleStage::GROUP_DELAY_INPUTS * 2;
        for group_delay in [stage2.group_delay_inputs(), stage3.group_delay_inputs()] {
            delay_subframes = (delay_subframes + group_delay) * 2;
        }
        if let Some(stage) = stage4.as_ref() {
            delay_subframes = (delay_subframes + stage.group_delay_inputs()) * 2;
        }
        let pre_post_frames = (delay_subframes + factor as i128 - 1) / factor as i128;
        debug_assert_eq!(
            (factor, delay_subframes, pre_post_frames),
            if factor == 16 {
                (16, 3_200, 200)
            } else {
                (8, 1_596, 200)
            },
        );

        Self {
            factor,
            stage1,
            stage2,
            stage3,
            stage4,
            scratch1: vec![0.0; channels * 2],
            scratch2: vec![0.0; channels * 2],
            scratch3: vec![0.0; channels * 2],
            scratch4: vec![0.0; channels * 2],
            channels,
            pre_post_frames,
            calibration_linear,
        }
    }

    fn process_frame(
        &mut self,
        frame: &[f64],
        input_index: i128,
        channel_peaks: &mut [f64],
        mut bridge: Option<&mut FastReconstructionBridge>,
        upper_subframe: Option<i128>,
    ) {
        let base1 = self
            .stage1
            .process_frame_fast(frame, input_index, &mut self.scratch1);
        for phase1 in 0..2 {
            let frame1 = &self.scratch1[phase1 * self.channels..(phase1 + 1) * self.channels];
            let base2 = self.stage2.process_frame_symmetric(
                frame1,
                base1 + phase1 as i128,
                &mut self.scratch2,
            );
            for phase2 in 0..2 {
                let frame2 = &self.scratch2[phase2 * self.channels..(phase2 + 1) * self.channels];
                let base3 = self.stage3.process_frame_symmetric(
                    frame2,
                    base2 + phase2 as i128,
                    &mut self.scratch3,
                );
                for phase3 in 0..2 {
                    let frame3 =
                        &self.scratch3[phase3 * self.channels..(phase3 + 1) * self.channels];
                    let index3 = base3 + phase3 as i128;
                    if self.factor == 8 {
                        observe_fast_headroom_output(
                            index3,
                            frame3,
                            self.calibration_linear,
                            channel_peaks,
                            bridge.as_deref_mut(),
                            upper_subframe,
                        );
                        continue;
                    }

                    let stage4 = self.stage4.as_mut().expect("16x path has stage 4");
                    let base4 = stage4.process_frame_symmetric(
                        frame3,
                        index3,
                        &mut self.scratch4,
                    );
                    for phase4 in 0..2 {
                        let output_index = base4 + phase4 as i128;
                        let reconstructed = &self.scratch4
                            [phase4 * self.channels..(phase4 + 1) * self.channels];
                        observe_fast_headroom_output(
                            output_index,
                            reconstructed,
                            self.calibration_linear,
                            channel_peaks,
                            bridge.as_deref_mut(),
                            upper_subframe,
                        );
                    }
                }
            }
        }
    }
}

#[inline]
fn observe_fast_headroom_output(
    output_index: i128,
    reconstructed: &[f64],
    calibration_linear: f64,
    channel_peaks: &mut [f64],
    bridge: Option<&mut FastReconstructionBridge>,
    upper_subframe: Option<i128>,
) {
    if let Some(bridge) = bridge {
        // Ceiling mode already tracks every uncalibrated prefix knot in the
        // bridge. Derive the calibrated point maximum once at finalize rather
        // than multiplying and comparing the same hot-loop output twice.
        bridge.observe(output_index, reconstructed, upper_subframe);
        return;
    }
    if output_index < 0
        || upper_subframe
            .map(|upper| output_index > upper)
            .unwrap_or(false)
    {
        return;
    }
    update_channel_peaks_scaled(channel_peaks, reconstructed, calibration_linear);
}

#[derive(Debug, Clone)]
enum MeterEngine {
    Reporting(ReportingEngine),
    Headroom(HeadroomEngine),
    FastHeadroom(FastHeadroomEngine),
}

impl MeterEngine {
    fn pre_post_frames(&self) -> i128 {
        match self {
            Self::Reporting(engine) => engine.pre_post_frames(),
            Self::Headroom(engine) => engine.pre_post_frames,
            Self::FastHeadroom(engine) => engine.pre_post_frames,
        }
    }

    fn process_frame(
        &mut self,
        frame: &[f64],
        input_index: i128,
        channel_peaks: &mut [f64],
        reconstruction_peaks: Option<&mut [f64]>,
        fast_reconstruction_bridge: Option<&mut FastReconstructionBridge>,
        upper_subframe: Option<i128>,
    ) {
        match self {
            Self::Reporting(engine) => {
                engine.process_frame(frame, input_index, channel_peaks, upper_subframe)
            }
            Self::Headroom(engine) => engine.process_frame(
                frame,
                input_index,
                channel_peaks,
                reconstruction_peaks,
                upper_subframe,
            ),
            Self::FastHeadroom(engine) => engine.process_frame(
                frame,
                input_index,
                channel_peaks,
                fast_reconstruction_bridge,
                upper_subframe,
            ),
        }
    }
}

/// Bounded-state incremental true-peak meter.
#[derive(Debug, Clone)]
pub struct TruePeakMeter {
    config: TruePeakConfig,
    factor: usize,
    engine: MeterEngine,
    channel_peaks: Vec<f64>,
    reconstruction_peaks: Option<Vec<f64>>,
    fast_reconstruction_bridge: Option<FastReconstructionBridge>,
    input_sample_peak: f64,
    last_frame: Vec<f64>,
    started: bool,
    next_input_index: i128,
    frames: u64,
}

impl TruePeakMeter {
    /// Create a new meter with bounded memory independent of stream length.
    pub fn new(config: TruePeakConfig) -> Result<Self, TruePeakError> {
        if config.sample_rate_hz == 0 {
            return Err(TruePeakError::InvalidSampleRate);
        }
        if config.channels == 0 {
            return Err(TruePeakError::InvalidChannelCount);
        }

        let factor = config
            .mode
            .oversample_factor_for_sample_rate(config.sample_rate_hz);
        let engine = match config.mode {
            TruePeakMode::Reporting4x => MeterEngine::Reporting(ReportingEngine::new(
                config.sample_rate_hz,
                config.channels,
            )),
            TruePeakMode::Headroom64x => {
                MeterEngine::Headroom(HeadroomEngine::new(config.channels))
            }
            TruePeakMode::Headroom16x | TruePeakMode::Headroom8x => {
                MeterEngine::FastHeadroom(FastHeadroomEngine::new(config.mode, config.channels))
            }
        };

        Ok(Self {
            config,
            factor,
            engine,
            channel_peaks: vec![0.0; config.channels],
            reconstruction_peaks: None,
            fast_reconstruction_bridge: None,
            input_sample_peak: 0.0,
            last_frame: vec![0.0; config.channels],
            started: false,
            next_input_index: 0,
            frames: 0,
        })
    }

    fn new_with_headroom_reconstruction(config: TruePeakConfig) -> Result<Self, TruePeakError> {
        debug_assert_eq!(config.mode, TruePeakMode::Headroom64x);
        let channels = config.channels;
        let mut meter = Self::new(config)?;
        meter.reconstruction_peaks = Some(vec![0.0; channels]);
        Ok(meter)
    }

    fn new_with_fast_reconstruction(config: TruePeakConfig) -> Result<Self, TruePeakError> {
        debug_assert!(matches!(
            config.mode,
            TruePeakMode::Headroom16x | TruePeakMode::Headroom8x
        ));
        let channels = config.channels;
        let mode = config.mode;
        let mut meter = Self::new(config)?;
        meter.fast_reconstruction_bridge = Some(FastReconstructionBridge::new(mode, channels));
        Ok(meter)
    }

    /// Feed complete interleaved decoded frames.
    ///
    /// Block boundaries have no effect on the result. The call validates the
    /// complete block before mutating meter state, so a rejected block is not
    /// partially consumed.
    pub fn push_interleaved(&mut self, samples: &[f64]) -> Result<(), TruePeakError> {
        if samples.len() % self.config.channels != 0 {
            return Err(TruePeakError::IncompleteFrame {
                samples: samples.len(),
                channels: self.config.channels,
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

        if !self.started {
            let first = samples[..self.config.channels].to_vec();
            let extension = match self.config.edge_policy {
                EdgePolicy::RepeatEndpoints => first,
                EdgePolicy::ZeroExtend => vec![0.0; self.config.channels],
            };
            let pre_frames = self.engine.pre_post_frames();
            for input_index in -pre_frames..0 {
                self.engine.process_frame(
                    &extension,
                    input_index,
                    &mut self.channel_peaks,
                    self.reconstruction_peaks.as_deref_mut(),
                    self.fast_reconstruction_bridge.as_mut(),
                    None,
                );
            }
            self.started = true;
        }

        for frame in samples.chunks_exact(self.config.channels) {
            let input_index = self.next_input_index;
            // Preserve decoded sample peak as an independent authority.  This
            // matches libebur128's TRUE_PEAK contract and also guarantees that
            // Headroom64x calibration can never hide an above-unity input.
            update_channel_peaks(&mut self.channel_peaks, frame);
            if let Some(peaks) = self.reconstruction_peaks.as_deref_mut() {
                update_channel_peaks(peaks, frame);
            }
            self.input_sample_peak = frame
                .iter()
                .copied()
                .map(f64::abs)
                .fold(self.input_sample_peak, f64::max);
            self.engine.process_frame(
                frame,
                input_index,
                &mut self.channel_peaks,
                self.reconstruction_peaks.as_deref_mut(),
                self.fast_reconstruction_bridge.as_mut(),
                None,
            );
            self.last_frame.copy_from_slice(frame);
            self.next_input_index += 1;
            self.frames = self
                .frames
                .checked_add(1)
                .ok_or(TruePeakError::InputTooLong)?;
        }
        Ok(())
    }

    /// Flush the interpolation filter according to the selected edge policy.
    pub fn finalize(self) -> Result<TruePeakResult, TruePeakError> {
        self.finalize_internal().map(|(result, _, _, _)| result)
    }

    fn finalize_internal(
        mut self,
    ) -> Result<
        (
            TruePeakResult,
            Option<Vec<f64>>,
            Option<FastReconstructionBridge>,
            f64,
        ),
        TruePeakError,
    > {
        if self.frames == 0 {
            return Err(TruePeakError::EmptyInput);
        }

        let upper_subframe = i128::from(self.frames - 1) * self.factor as i128;
        let extension = match self.config.edge_policy {
            EdgePolicy::RepeatEndpoints => self.last_frame.clone(),
            EdgePolicy::ZeroExtend => vec![0.0; self.config.channels],
        };
        let stop = self.next_input_index + self.engine.pre_post_frames();
        for input_index in self.next_input_index..stop {
            self.engine.process_frame(
                &extension,
                input_index,
                &mut self.channel_peaks,
                self.reconstruction_peaks.as_deref_mut(),
                self.fast_reconstruction_bridge.as_mut(),
                Some(upper_subframe),
            );
        }

        if let Some(reconstruction_peaks) = self.reconstruction_peaks.as_ref() {
            for (point_peak, reconstruction_peak) in self
                .channel_peaks
                .iter_mut()
                .zip(reconstruction_peaks.iter().copied())
            {
                let calibrated = reconstruction_peak * HEADROOM64_INTERPOLATION_CALIBRATION_LINEAR;
                if calibrated > *point_peak {
                    *point_peak = calibrated;
                }
            }
        }
        if let Some(bridge) = self.fast_reconstruction_bridge.as_ref() {
            let calibration = match self.config.mode {
                TruePeakMode::Headroom16x => HEADROOM16_INTERPOLATION_CALIBRATION_LINEAR,
                TruePeakMode::Headroom8x => HEADROOM8_INTERPOLATION_CALIBRATION_LINEAR,
                _ => unreachable!("fast bridge requires a fast headroom mode"),
            };
            for (point_peak, reconstruction_peak) in self
                .channel_peaks
                .iter_mut()
                .zip(bridge.point_channel_peaks().iter().copied())
            {
                let calibrated = reconstruction_peak * calibration;
                if calibrated > *point_peak {
                    *point_peak = calibrated;
                }
            }
        }

        let linear = self.channel_peaks.iter().copied().fold(0.0, f64::max);
        let overall = if linear == 0.0 {
            PeakLevel::Silence
        } else {
            PeakLevel::Finite {
                linear,
                dbtp: 20.0 * linear.log10(),
            }
        };
        Ok((
            TruePeakResult {
                overall,
                channel_linear_peaks: self.channel_peaks,
                frames: self.frames,
            },
            self.reconstruction_peaks,
            self.fast_reconstruction_bridge,
            self.input_sample_peak,
        ))
    }
}

#[derive(Debug, Clone)]
struct FastReconstructionBridge {
    channel_peaks: Vec<f64>,
    cubic_envelope_peaks: Vec<f64>,
    window: Vec<f64>,
    window_len: usize,
    last_index: Option<i128>,
    final_upper: Option<i128>,
    highest_index: i128,
    channels: usize,
    bridge_error_upper: f64,
}

impl FastReconstructionBridge {
    fn new(mode: TruePeakMode, channels: usize) -> Self {
        let bridge_error_upper = match mode {
            TruePeakMode::Headroom16x => HEADROOM16X_TO_64X_RECONSTRUCTION_LINF_ERROR_UPPER,
            TruePeakMode::Headroom8x => HEADROOM8X_TO_64X_RECONSTRUCTION_LINF_ERROR_UPPER,
            _ => unreachable!("fast reconstruction bridge requires a fast headroom mode"),
        };
        Self {
            channel_peaks: vec![0.0; channels],
            cubic_envelope_peaks: vec![0.0; channels],
            window: vec![0.0; channels * 4],
            window_len: 0,
            last_index: None,
            final_upper: None,
            highest_index: i128::MIN,
            channels,
            bridge_error_upper,
        }
    }

    fn observe(&mut self, output_index: i128, frame: &[f64], upper_subframe: Option<i128>) {
        if let Some(upper) = upper_subframe {
            self.final_upper = Some(upper);
            if output_index > upper + 1 {
                return;
            }
        }
        if output_index < -1 {
            return;
        }
        if let Some(previous) = self.last_index {
            debug_assert_eq!(output_index, previous + 1);
        }
        self.last_index = Some(output_index);

        if output_index >= 0
            && upper_subframe
                .map(|upper| output_index <= upper)
                .unwrap_or(true)
        {
            update_channel_peaks_fail_closed(&mut self.channel_peaks, frame);
        }
        self.highest_index = self.highest_index.max(output_index);

        if self.window_len < 4 {
            let start = self.window_len * self.channels;
            self.window[start..start + self.channels].copy_from_slice(frame);
            self.window_len += 1;
        } else {
            self.window
                .copy_within(self.channels..4 * self.channels, 0);
            self.window[3 * self.channels..4 * self.channels].copy_from_slice(frame);
        }
        if self.window_len < 4 {
            return;
        }

        // With window indices [k-1, k, k+1, k+2], this evaluates the interval
        // k..k+1. A cubic polynomial expressed in Bernstein form lies inside
        // the convex hull of its four controls, so the largest absolute
        // control is a rigorous continuous-interval bound regardless of whether
        // the coarse prefix grid is 16x or 8x.
        let interval_index = output_index - 2;
        if interval_index < 0
            || upper_subframe
                .map(|upper| interval_index >= upper)
                .unwrap_or(false)
        {
            return;
        }
        for channel in 0..self.channels {
            let ym1 = self.window[channel];
            let y0 = self.window[self.channels + channel];
            let y1 = self.window[2 * self.channels + channel];
            let y2 = self.window[3 * self.channels + channel];

            // The endpoint Bernstein controls b0=y0 and b3=y1 are already
            // covered by channel_peaks. Compute only the two interior controls.
            // Rewriting through adjacent first differences needs two floating
            // multiplications per coarse interval/channel.
            let d0 = y0 - ym1;
            let d1 = y1 - y0;
            let d2 = y2 - y1;
            let twice_d0 = d0 + d0;
            let twice_d1 = d1 + d1;
            let five_d1 = twice_d1 + twice_d1 + d1;
            let twice_d2 = d2 + d2;
            let b1 = y0 + (twice_d0 + five_d1 - d2) * (1.0 / 18.0);
            let b2 = y1 + (d0 - five_d1 - twice_d2) * (1.0 / 18.0);
            let bound = b1.abs().max(b2.abs());
            if !bound.is_finite() {
                self.cubic_envelope_peaks[channel] = f64::INFINITY;
            } else if bound > self.cubic_envelope_peaks[channel] {
                self.cubic_envelope_peaks[channel] = bound;
            }
        }
    }

    fn point_channel_peaks(&self) -> &[f64] {
        &self.channel_peaks
    }

    fn into_reconstruction_upper(self, input_sample_peak: f64) -> Vec<f64> {
        let complete = self
            .final_upper
            .is_some_and(|upper| self.highest_index >= upper + 1);
        let mut peaks = self
            .channel_peaks
            .into_iter()
            .zip(self.cubic_envelope_peaks)
            .map(|(coarse, cubic)| coarse.max(cubic))
            .collect::<Vec<_>>();
        if !complete {
            // A future filter-delay edit that fails to supply the bridge's
            // required terminal samples must fail closed rather than return an
            // under-bound. Production validation rejects non-finite authority.
            peaks.fill(f64::INFINITY);
            return peaks;
        }
        if input_sample_peak == 0.0 {
            return peaks;
        }
        let allowance = input_sample_peak
            * (self.bridge_error_upper
                + HEADROOM64X_RECONSTRUCTION_NUMERIC_ERROR_PER_INPUT_PEAK_UPPER
                + HEADROOM_FAST_RECONSTRUCTION_NUMERIC_ERROR_PER_INPUT_PEAK_UPPER);
        for peak in &mut peaks {
            *peak = next_up_nonnegative((*peak).max(input_sample_peak) + allowance);
        }
        peaks
    }
}

/// Result of the finite Headroom64 ceiling reconstruction.
///
/// The point estimate may come from any headroom scan rung, but the ceiling
/// authority always governs the same full finite Headroom64 reconstruction.
/// It is intentionally distinct from the band-qualified point authorities.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadroomCeilingResult {
    /// Calibrated point estimate from the selected headroom scan rung.
    pub point_estimate: TruePeakResult,
    /// Conservative peak of Tonepoet's declared finite reconstruction model.
    pub reconstruction_upper: PeakLevel,
    /// Conservative per-channel reconstruction peaks in input channel order.
    pub reconstruction_channel_linear_peaks: Vec<f64>,
}

/// Streaming evaluator for Tonepoet's hard-ceiling waveform contract.
///
/// The governed signal is the final-rate interleaved Float64 PCM stream fed to
/// this meter. Each channel is extended outside the finite stream using the
/// configured Headroom64 edge policy. `Reference` evaluates the full six-stage
/// 2x FIR cascade directly. The fast scan rungs evaluate a qualified prefix and
/// add a conservative interpolation/difference enclosure that still bounds the
/// same uncalibrated full-64x reconstruction. Over the nominal finite interval
/// from the first decoded
/// frame through the last, the continuous waveform is defined as straight-line
/// interpolation between adjacent 64x reconstruction knots. The absolute value
/// of a linear segment reaches its maximum at an endpoint, so the real-valued
/// continuous peak under this convention is bounded by the maximum knot plus
/// the explicit binary64 evaluation allowance. Channels are independent and
/// the reported ceiling peak is their maximum.
///
/// This deliberately does not claim to bound an arbitrary ideal DAC, an
/// unspecified sinc reconstruction, or decoded output of a lossy codec. It is a
/// finite, auditable reconstruction convention that requires no fabricated
/// <=0.495*Fs spectral-support assertion.
#[derive(Debug, Clone)]
pub struct HeadroomCeilingMeter {
    meter: TruePeakMeter,
}

impl HeadroomCeilingMeter {
    /// Create a bounded-state ceiling evaluator.
    pub fn new(
        sample_rate_hz: u32,
        channels: usize,
        edge_policy: EdgePolicy,
    ) -> Result<Self, TruePeakError> {
        let config = TruePeakConfig::new(sample_rate_hz, channels)
            .with_mode(TruePeakMode::Headroom64x)
            .with_edge_policy(edge_policy);
        Ok(Self {
            meter: TruePeakMeter::new_with_headroom_reconstruction(config)?,
        })
    }

    /// Create a ceiling evaluator using one rung of the headroom scan ladder.
    ///
    /// `Reference` is exactly [`Self::new`]. The two fast modes scan only a
    /// prefix of the Headroom64 cascade for their point estimate. Their ceiling
    /// result nevertheless remains an upper bound on the same full finite 64x
    /// reconstruction: both modes bound a four-point cubic interpolant through
    /// Bernstein controls and add the independently qualified full-64x
    /// difference norm for their prefix.
    pub fn new_with_scan_mode(
        sample_rate_hz: u32,
        channels: usize,
        edge_policy: EdgePolicy,
        scan_mode: HeadroomScanMode,
    ) -> Result<Self, TruePeakError> {
        if scan_mode == HeadroomScanMode::Reference {
            return Self::new(sample_rate_hz, channels, edge_policy);
        }
        let config = TruePeakConfig::new(sample_rate_hz, channels)
            .with_mode(scan_mode.point_mode())
            .with_edge_policy(edge_policy);
        Ok(Self {
            meter: TruePeakMeter::new_with_fast_reconstruction(config)?,
        })
    }

    /// Feed complete interleaved final-rate Float64 frames.
    pub fn push_interleaved(&mut self, samples: &[f64]) -> Result<(), TruePeakError> {
        self.meter.push_interleaved(samples)
    }

    /// Finalize the selected-rung point estimate and the full-64x ceiling authority.
    pub fn finalize(self) -> Result<HeadroomCeilingResult, TruePeakError> {
        let is_reference = self.meter.config.mode == TruePeakMode::Headroom64x;
        let (point_estimate, reconstruction, fast_bridge, input_sample_peak) =
            self.meter.finalize_internal()?;
        let mut reconstruction_channel_linear_peaks = match (reconstruction, fast_bridge) {
            (Some(peaks), None) => peaks,
            (None, Some(bridge)) => bridge.into_reconstruction_upper(input_sample_peak),
            _ => unreachable!("ceiling meter enables exactly one reconstruction authority"),
        };
        if reconstruction_channel_linear_peaks
            .iter()
            .all(|peak| peak.is_finite())
        {
            if is_reference {
                let numerical_allowance = input_sample_peak
                    * HEADROOM64X_RECONSTRUCTION_NUMERIC_ERROR_PER_INPUT_PEAK_UPPER;
                if input_sample_peak > 0.0 {
                    for peak in &mut reconstruction_channel_linear_peaks {
                        *peak = next_up_nonnegative(*peak + numerical_allowance);
                    }
                }
            }
        }
        let linear = reconstruction_channel_linear_peaks
            .iter()
            .copied()
            .fold(0.0, f64::max);
        let reconstruction_upper = if linear == 0.0 {
            PeakLevel::Silence
        } else {
            PeakLevel::Finite {
                linear,
                dbtp: 20.0 * linear.log10(),
            }
        };
        Ok(HeadroomCeilingResult {
            point_estimate,
            reconstruction_upper,
            reconstruction_channel_linear_peaks,
        })
    }
}

fn next_up_nonnegative(value: f64) -> f64 {
    debug_assert!(value >= 0.0 && !value.is_nan());
    if value == f64::INFINITY {
        return value;
    }
    if value == 0.0 {
        return f64::from_bits(1);
    }
    f64::from_bits(value.to_bits() + 1)
}

fn update_channel_peaks(channel_peaks: &mut [f64], frame: &[f64]) {
    for (peak, sample) in channel_peaks.iter_mut().zip(frame.iter().copied()) {
        let magnitude = sample.abs();
        if magnitude > *peak {
            *peak = magnitude;
        }
    }
}

/// Peak update for proof-carrying fast reconstruction state.
///
/// Ordinary meter paths preserve their historical IEEE-754 behavior. The fast
/// ceiling bridge is newer authority code, so an arithmetic overflow/NaN must
/// conservatively poison the bound instead of being skipped by comparisons.
fn update_channel_peaks_fail_closed(channel_peaks: &mut [f64], frame: &[f64]) {
    for (peak, sample) in channel_peaks.iter_mut().zip(frame.iter().copied()) {
        let magnitude = sample.abs();
        if !magnitude.is_finite() {
            *peak = f64::INFINITY;
        } else if magnitude > *peak {
            *peak = magnitude;
        }
    }
}

fn update_channel_peaks_scaled(channel_peaks: &mut [f64], frame: &[f64], scale: f64) {
    for (peak, sample) in channel_peaks.iter_mut().zip(frame.iter().copied()) {
        let magnitude = (sample * scale).abs();
        if magnitude > *peak {
            *peak = magnitude;
        }
    }
}

fn build_polyphase_filters(
    taps: usize,
    factor: usize,
    delay_frames: usize,
    window: Window,
    normalize_phase_dc: bool,
) -> Vec<PhaseFilter> {
    let mut filters = (0..factor)
        .map(|_| PhaseFilter {
            indices: Vec::with_capacity(delay_frames),
            coefficients: Vec::with_capacity(delay_frames),
        })
        .collect::<Vec<_>>();

    for tap in 0..taps {
        let centered = tap as f64 - (taps - 1) as f64 / 2.0;
        let sinc = if centered.abs() < f64::EPSILON {
            1.0
        } else {
            let argument = centered * PI / factor as f64;
            argument.sin() / argument
        };
        let phase_fraction = tap as f64 / (taps - 1) as f64;
        let window = match window {
            Window::Hann => 0.5 * (1.0 - (2.0 * PI * phase_fraction).cos()),
            Window::Blackman => {
                0.42 - 0.5 * (2.0 * PI * phase_fraction).cos()
                    + 0.08 * (4.0 * PI * phase_fraction).cos()
            }
        };
        let coefficient = sinc * window;
        if coefficient.abs() <= COEFFICIENT_EPSILON {
            continue;
        }
        let phase = tap % factor;
        filters[phase].indices.push(tap / factor);
        filters[phase].coefficients.push(coefficient);
    }

    if normalize_phase_dc {
        for filter in &mut filters {
            let sum = filter.coefficients.iter().copied().sum::<f64>();
            debug_assert!(sum.abs() > f64::EPSILON);
            for coefficient in &mut filter.coefficients {
                *coefficient /= sum;
            }
        }
    }

    filters
}

#[cfg(test)]
mod coefficient_integrity_tests {
    use super::*;

    const FNV1A64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV1A64_PRIME: u64 = 0x0000_0100_0000_01b3;
    const EXPECTED_COEFFICIENT_COUNT: usize = 192;
    const EXPECTED_COEFFICIENT_CHECKSUM: u64 = 0xdca3_0520_f06a_0210;

    fn mix_u64(mut hash: u64, value: u64) -> u64 {
        for byte in value.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV1A64_PRIME);
        }
        hash
    }

    #[test]
    fn headroom64_coefficients_match_frozen_checksum() {
        assert_eq!(
            HEADROOM64_HALF_DELAY_COEFFICIENTS.len(),
            EXPECTED_COEFFICIENT_COUNT,
        );

        let mut hash = FNV1A64_OFFSET_BASIS;
        hash = mix_u64(hash, HEADROOM64_HALF_DELAY_COEFFICIENTS.len() as u64);
        for coefficient in HEADROOM64_HALF_DELAY_COEFFICIENTS {
            hash = mix_u64(hash, coefficient.to_bits());
        }
        assert_eq!(hash, EXPECTED_COEFFICIENT_CHECKSUM);
    }


    #[derive(Clone)]
    struct ReferenceHalfStage {
        delay: Vec<Vec<f64>>,
        delay_index: usize,
        channels: usize,
    }

    impl ReferenceHalfStage {
        fn new(channels: usize) -> Self {
            Self {
                delay: vec![vec![0.0; HEADROOM64_HALF_DELAY_TAPS]; channels],
                delay_index: 0,
                channels,
            }
        }

        fn delayed(&self, channel: usize, frames_ago: usize) -> f64 {
            let len = HEADROOM64_HALF_DELAY_TAPS;
            let index = (self.delay_index + len - frames_ago) % len;
            self.delay[channel][index]
        }

        fn process_frame(&mut self, frame: &[f64], input_index: i128, output: &mut [f64]) -> i128 {
            for (channel, sample) in frame.iter().copied().enumerate() {
                self.delay[channel][self.delay_index] = sample;
            }
            for channel in 0..self.channels {
                output[channel] = self.delayed(channel, HEADROOM64_HALF_DELAY_TAPS / 2);
                let mut half = 0.0;
                for (index, coefficient) in HEADROOM64_HALF_DELAY_COEFFICIENTS
                    .iter()
                    .copied()
                    .enumerate()
                {
                    half += coefficient
                        * (self.delayed(channel, index)
                            + self.delayed(channel, HEADROOM64_HALF_DELAY_TAPS - 1 - index));
                }
                output[self.channels + channel] = half;
            }
            self.delay_index += 1;
            if self.delay_index == HEADROOM64_HALF_DELAY_TAPS {
                self.delay_index = 0;
            }
            (input_index - HeadroomHalfSampleStage::GROUP_DELAY_INPUTS) * 2
        }
    }

    fn pseudo_random_frame(state: &mut u64, channels: usize) -> Vec<f64> {
        (0..channels)
            .map(|_| {
                *state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let unit = ((*state >> 11) as f64) * (1.0 / ((1_u64 << 53) as f64));
                unit * 2.5 - 1.25
            })
            .collect()
    }

    fn convolve(left: &[f64], right: &[f64]) -> Vec<f64> {
        let mut output = vec![0.0; left.len() + right.len() - 1];
        for (left_index, left_value) in left.iter().copied().enumerate() {
            if left_value == 0.0 {
                continue;
            }
            for (right_index, right_value) in right.iter().copied().enumerate() {
                output[left_index + right_index] += left_value * right_value;
            }
        }
        output
    }

    fn headroom_stage_impulse_response(taps: usize) -> Vec<f64> {
        let delay_frames = (taps + 1) / 2;
        let filters = build_polyphase_filters(taps, 2, delay_frames, Window::Blackman, true);
        let mut response = vec![0.0; delay_frames * 2];
        for (phase, filter) in filters.iter().enumerate() {
            for (&index, &coefficient) in filter.indices.iter().zip(&filter.coefficients) {
                response[index * 2 + phase] = coefficient;
            }
        }
        response
    }

    fn complete_headroom_impulse_response() -> Vec<f64> {
        let mut response = vec![0.0; HEADROOM64_HALF_DELAY_TAPS * 2];
        response[(HEADROOM64_HALF_DELAY_TAPS / 2) * 2] = 1.0;
        for (index, coefficient) in HEADROOM64_HALF_DELAY_COEFFICIENTS
            .iter()
            .copied()
            .enumerate()
        {
            response[index * 2 + 1] += coefficient;
            response[(HEADROOM64_HALF_DELAY_TAPS - 1 - index) * 2 + 1] += coefficient;
        }

        for taps in [
            HEADROOM64_STAGE_2_TAPS,
            HEADROOM64_STAGE_3_TAPS,
            HEADROOM64_STAGE_4_TAPS,
            HEADROOM64_STAGE_5_TAPS,
            HEADROOM64_STAGE_6_TAPS,
        ] {
            let mut upsampled = vec![0.0; response.len() * 2 - 1];
            for (index, value) in response.iter().copied().enumerate() {
                upsampled[index * 2] = value;
            }
            response = convolve(&upsampled, &headroom_stage_impulse_response(taps));
        }
        response
    }

    fn headroom_stage_impulse_response_fast(taps: usize) -> Vec<f64> {
        let delay_frames = (taps + 1) / 2;
        let filters = build_polyphase_filters(taps, 2, delay_frames, Window::Blackman, true);
        let mut response = vec![0.0; delay_frames * 2];
        for (phase, filter) in filters.iter().enumerate() {
            if phase == 0 {
                for (&index, &coefficient) in filter.indices.iter().zip(&filter.coefficients) {
                    response[index * 2 + phase] = coefficient;
                }
                continue;
            }
            let half_len = filter.coefficients.len() / 2;
            for index in 0..half_len {
                let mirror = filter.coefficients.len() - 1 - index;
                let average = 0.5 * (filter.coefficients[index] + filter.coefficients[mirror]);
                response[filter.indices[index] * 2 + phase] = average;
                response[filter.indices[mirror] * 2 + phase] = average;
            }
        }
        response
    }

    #[test]
    fn symmetric_two_x_runtime_matches_its_independent_impulse_response() {
        for taps in [
            HEADROOM64_STAGE_2_TAPS,
            HEADROOM64_STAGE_3_TAPS,
            HEADROOM64_STAGE_4_TAPS,
        ] {
            let expected = headroom_stage_impulse_response_fast(taps);
            let delay_frames = (taps + 1) / 2;
            let mut stage = FastHeadroomTwoXStage::new(taps, 1);
            let mut output = [0.0_f64; 2];
            let mut actual = Vec::with_capacity(expected.len());
            for input_index in 0..delay_frames as i128 {
                let sample = if input_index == 0 { 1.0 } else { 0.0 };
                stage.process_frame_symmetric(&[sample], input_index, &mut output);
                actual.extend_from_slice(&output);
            }
            assert_eq!(actual.len(), expected.len());
            for (index, (actual, expected)) in actual
                .iter()
                .copied()
                .zip(expected.iter().copied())
                .enumerate()
            {
                assert!(
                    (actual - expected).abs() <= 2.0e-15,
                    "taps={taps} index={index}: runtime={actual:.17e} expected={expected:.17e}",
                );
            }
        }
    }

    fn complete_fast_headroom_impulse_response(mode: TruePeakMode) -> (Vec<f64>, i128) {
        let mut response = vec![0.0; HEADROOM64_HALF_DELAY_TAPS * 2];
        response[(HEADROOM64_HALF_DELAY_TAPS / 2) * 2] = 1.0;
        for (index, coefficient) in HEADROOM64_HALF_DELAY_COEFFICIENTS
            .iter()
            .copied()
            .enumerate()
        {
            response[index * 2 + 1] += coefficient;
            response[(HEADROOM64_HALF_DELAY_TAPS - 1 - index) * 2 + 1] += coefficient;
        }

        let taps: &[usize] = match mode {
            TruePeakMode::Headroom16x => &[
                HEADROOM64_STAGE_2_TAPS,
                HEADROOM64_STAGE_3_TAPS,
                HEADROOM64_STAGE_4_TAPS,
            ],
            TruePeakMode::Headroom8x => &[HEADROOM64_STAGE_2_TAPS, HEADROOM64_STAGE_3_TAPS],
            _ => unreachable!("fast impulse response requires a fast mode"),
        };
        let mut delay = HeadroomHalfSampleStage::GROUP_DELAY_INPUTS * 2;
        for &stage_taps in taps {
            let mut upsampled = vec![0.0; response.len() * 2 - 1];
            for (index, value) in response.iter().copied().enumerate() {
                upsampled[index * 2] = value;
            }
            response = convolve(&upsampled, &headroom_stage_impulse_response_fast(stage_taps));
            let group_delay = ((stage_taps - 1) / 4) as i128;
            delay = (delay + group_delay) * 2;
        }
        (response, delay)
    }

    fn impulse_sample(response: &[f64], delay: i128, physical_index: i128) -> f64 {
        let index = physical_index + delay;
        if index < 0 || index >= response.len() as i128 {
            0.0
        } else {
            response[index as usize]
        }
    }

    fn cubic_sample(
        response: &[f64],
        delay: i128,
        fine_index: i128,
        ratio: i128,
    ) -> f64 {
        let q = fine_index.div_euclid(ratio);
        let t = fine_index.rem_euclid(ratio) as f64 / ratio as f64;
        let ym1 = impulse_sample(response, delay, q - 1);
        let y0 = impulse_sample(response, delay, q);
        let y1 = impulse_sample(response, delay, q + 1);
        let y2 = impulse_sample(response, delay, q + 2);
        let wm1 = -t * (t - 1.0) * (t - 2.0) / 6.0;
        let w0 = (t + 1.0) * (t - 1.0) * (t - 2.0) / 2.0;
        let w1 = -(t + 1.0) * t * (t - 2.0) / 2.0;
        let w2 = (t + 1.0) * t * (t - 1.0) / 6.0;
        wm1 * ym1 + w0 * y0 + w1 * y1 + w2 * y2
    }

    fn bridge_phase_l1(
        full: &[f64],
        full_delay: i128,
        coarse: &[f64],
        coarse_delay: i128,
        ratio: i128,
    ) -> f64 {
        let full_min = -full_delay;
        let full_max = full.len() as i128 - 1 - full_delay;
        let coarse_min = -coarse_delay;
        let coarse_max = coarse.len() as i128 - 1 - coarse_delay;
        let min_physical = full_min.min(ratio * (coarse_min - 2)) - 128;
        let max_physical = full_max.max(ratio * (coarse_max + 2) + ratio - 1) + 128;

        let mut worst = 0.0_f64;
        for phase in 0_i128..64 {
            let mut physical = min_physical + (phase - min_physical).rem_euclid(64);
            let mut total = 0.0_f64;
            while physical <= max_physical {
                total += (impulse_sample(full, full_delay, physical)
                    - cubic_sample(coarse, coarse_delay, physical, ratio))
                .abs();
                physical += 64;
            }
            worst = worst.max(total);
        }
        worst
    }

    #[test]
    fn fast_reconstruction_bridge_constants_cover_exact_runtime_filters() {
        let full = complete_headroom_impulse_response();
        let full_delay = 12_816_i128;
        let (fast16, fast16_delay) =
            complete_fast_headroom_impulse_response(TruePeakMode::Headroom16x);
        let (fast8, fast8_delay) =
            complete_fast_headroom_impulse_response(TruePeakMode::Headroom8x);
        assert_eq!(fast16_delay, 3_200);
        assert_eq!(fast8_delay, 1_596);

        let fast16_cubic_l1 = bridge_phase_l1(&full, full_delay, &fast16, fast16_delay, 4);
        let fast8_cubic_l1 = bridge_phase_l1(&full, full_delay, &fast8, fast8_delay, 8);

        assert!((fast16_cubic_l1 - 0.002_850_095_510_818_164).abs() < 5.0e-13);
        assert!((fast8_cubic_l1 - 0.002_932_600_684_250_417_7).abs() < 5.0e-13);
        assert!(fast16_cubic_l1 <= HEADROOM16X_TO_64X_RECONSTRUCTION_LINF_ERROR_UPPER);
        assert!(fast8_cubic_l1 <= HEADROOM8X_TO_64X_RECONSTRUCTION_LINF_ERROR_UPPER);
    }

    #[test]
    fn reconstruction_linf_constant_covers_complete_cascade() {
        let response = complete_headroom_impulse_response();
        let maximum_phase_l1 = (0..64)
            .map(|phase| {
                response
                    .iter()
                    .skip(phase)
                    .step_by(64)
                    .map(|value| value.abs())
                    .sum::<f64>()
            })
            .fold(0.0, f64::max);

        // Frozen independently by qualification/verify_ceiling_contract.py.
        let independent_reference = 4.089_899_431_660_599_f64;
        assert!((maximum_phase_l1 - independent_reference).abs() < 5.0e-15);
        assert!(maximum_phase_l1 <= HEADROOM64X_RECONSTRUCTION_LINF_GAIN_UPPER);
    }

    #[test]
    fn fast_reconstruction_numeric_allowance_exceeds_pessimistic_gamma_budget() {
        let unit_roundoff = f64::EPSILON / 2.0;
        let gamma = |operations: usize| {
            let ku = operations as f64 * unit_roundoff;
            ku / (1.0 - ku)
        };

        let first_stage_l1 = 2.0
            * HEADROOM64_HALF_DELAY_COEFFICIENTS
                .iter()
                .copied()
                .map(f64::abs)
                .sum::<f64>();
        let stage_l1 = |taps: usize| {
            let response = headroom_stage_impulse_response_fast(taps);
            (0..2)
                .map(|phase| {
                    response
                        .iter()
                        .skip(phase)
                        .step_by(2)
                        .copied()
                        .map(f64::abs)
                        .sum::<f64>()
                })
                .fold(0.0, f64::max)
        };
        let stages = [
            (first_stage_l1, 4 * 192 + 8),
            (stage_l1(HEADROOM64_STAGE_2_TAPS), 3 * 24 + 4),
            (stage_l1(HEADROOM64_STAGE_3_TAPS), 3 * 12 + 4),
            (stage_l1(HEADROOM64_STAGE_4_TAPS), 3 * 8 + 4),
        ];

        let propagate = |count: usize| {
            let mut real_magnitude_bound = 1.0_f64;
            let mut numerical_error_bound = 0.0_f64;
            for &(l1, operations) in &stages[..count] {
                let local_error = gamma(operations)
                    * l1
                    * (real_magnitude_bound + numerical_error_bound);
                numerical_error_bound = l1 * numerical_error_bound + local_error;
                real_magnitude_bound *= l1;
            }
            (real_magnitude_bound, numerical_error_bound)
        };

        let (fast16_magnitude, fast16_error) = propagate(4);
        let (fast8_magnitude, fast8_error) = propagate(3);
        // Each interior cubic Bernstein control has coefficient L1 norm 4/3.
        // Charge 32 rounding operations, twice the runtime expression's count,
        // to keep this enclosure intentionally pessimistic.
        let cubic_error = |magnitude: f64, error: f64| {
            (4.0 / 3.0) * error + gamma(32) * (4.0 / 3.0) * (magnitude + error)
        };
        let fast16_cubic_error = cubic_error(fast16_magnitude, fast16_error);
        let fast8_cubic_error = cubic_error(fast8_magnitude, fast8_error);

        assert!(fast16_cubic_error < 4.0e-12);
        assert!(fast8_cubic_error < 4.0e-12);
        assert!(fast16_cubic_error < HEADROOM_FAST_RECONSTRUCTION_NUMERIC_ERROR_PER_INPUT_PEAK_UPPER);
        assert!(fast8_cubic_error < HEADROOM_FAST_RECONSTRUCTION_NUMERIC_ERROR_PER_INPUT_PEAK_UPPER);
    }

    #[test]
    fn reconstruction_numeric_allowance_exceeds_pessimistic_gamma_budget() {
        let unit_roundoff = f64::EPSILON / 2.0;
        let gamma = |operations: usize| {
            let ku = operations as f64 * unit_roundoff;
            ku / (1.0 - ku)
        };

        let mut real_magnitude_bound = 1.0_f64;
        let mut numerical_error_bound = 0.0_f64;
        let stage_specs = [
            (4.089_899_431_660_599_f64, 4 * 192 + 8),
            (2.182_305_364_025_995_5_f64, 3 * 24 + 4),
            (1.738_382_235_555_464_f64, 3 * 12 + 4),
            (1.475_916_404_745_172_f64, 3 * 8 + 4),
            (1.288_719_089_050_186_f64, 3 * 6 + 4),
            (1.058_953_251_382_957_2_f64, 3 * 4 + 4),
        ];
        for (stage_l1, operations) in stage_specs {
            let local_error = gamma(operations)
                * stage_l1
                * (real_magnitude_bound + numerical_error_bound);
            numerical_error_bound = stage_l1 * numerical_error_bound + local_error;
            real_magnitude_bound *= stage_l1;
        }

        assert!(numerical_error_bound < 3.4e-12);
        assert!(
            numerical_error_bound
                < HEADROOM64X_RECONSTRUCTION_NUMERIC_ERROR_PER_INPUT_PEAK_UPPER
        );
    }

    #[test]
    fn doubled_first_stage_is_bit_identical_to_modulo_reference() {
        let channels = 3;
        let mut optimized = HeadroomHalfSampleStage::new(channels);
        let mut reference = ReferenceHalfStage::new(channels);
        let mut optimized_output = vec![0.0; channels * 2];
        let mut reference_output = vec![0.0; channels * 2];
        let mut state = 0x72f0_4a81_d3c6_195b_u64;

        for input_index in -250_i128..2_000 {
            let frame = pseudo_random_frame(&mut state, channels);
            let optimized_base = optimized.process_frame(&frame, input_index, &mut optimized_output);
            let reference_base = reference.process_frame(&frame, input_index, &mut reference_output);
            assert_eq!(optimized_base, reference_base);
            assert_eq!(
                optimized_output.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
                reference_output.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
            );
        }
    }

    #[test]
    fn fast_first_stage_preserves_the_frozen_filter_with_only_rounding_reorder() {
        let channels = 3;
        let mut reference = HeadroomHalfSampleStage::new(channels);
        let mut fast = HeadroomHalfSampleStage::new(channels);
        let mut reference_output = vec![0.0; channels * 2];
        let mut fast_output = vec![0.0; channels * 2];
        let mut state = 0x6d2a_1e93_9f71_c4b5_u64;

        for input_index in -250_i128..2_000 {
            let frame = pseudo_random_frame(&mut state, channels);
            let reference_base =
                reference.process_frame(&frame, input_index, &mut reference_output);
            let fast_base = fast.process_frame_fast(&frame, input_index, &mut fast_output);
            assert_eq!(reference_base, fast_base);
            for (channel_phase, (reference, fast)) in reference_output
                .iter()
                .copied()
                .zip(fast_output.iter().copied())
                .enumerate()
            {
                assert!(
                    (reference - fast).abs() <= 1.0e-12,
                    "index={input_index} channel_phase={channel_phase}: reference={reference:.17e} fast={fast:.17e}",
                );
            }
        }
    }

    #[test]
    fn specialized_two_x_stages_are_bit_identical_to_generic_filters() {
        let channels = 3;
        for taps in [
            HEADROOM64_STAGE_2_TAPS,
            HEADROOM64_STAGE_3_TAPS,
            HEADROOM64_STAGE_4_TAPS,
            HEADROOM64_STAGE_5_TAPS,
            HEADROOM64_STAGE_6_TAPS,
        ] {
            let mut optimized = HeadroomTwoXStage::new(taps, channels);
            let mut reference = InterpolatorStage::new(taps, 2, channels, Window::Blackman, true);
            let mut optimized_output = vec![0.0; channels * 2];
            let mut reference_output = vec![0.0; channels * 2];
            let mut state = 0x3109_b765_a77d_38e1_u64 ^ taps as u64;

            for input_index in -80_i128..500 {
                let frame = pseudo_random_frame(&mut state, channels);
                let optimized_base = optimized.process_frame(&frame, input_index, &mut optimized_output);
                let reference_base = reference.process_frame(&frame, input_index, &mut reference_output);
                assert_eq!(optimized_base, reference_base, "taps={taps}");
                assert_eq!(
                    optimized_output.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
                    reference_output.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
                    "taps={taps}",
                );
            }
        }
    }

    #[test]
    fn ceiling_meter_preserves_silence_and_exposes_uncalibrated_reconstruction() {
        let mut silent = HeadroomCeilingMeter::new(48_000, 2, EdgePolicy::RepeatEndpoints).unwrap();
        silent.push_interleaved(&[0.0; 128]).unwrap();
        let silent = silent.finalize().unwrap();
        assert_eq!(silent.point_estimate.overall, PeakLevel::Silence);
        assert_eq!(silent.reconstruction_upper, PeakLevel::Silence);

        let mut meter = HeadroomCeilingMeter::new(48_000, 1, EdgePolicy::RepeatEndpoints).unwrap();
        meter.push_interleaved(&[0.5; 512]).unwrap();
        let result = meter.finalize().unwrap();
        assert!(result.reconstruction_upper.linear() >= 0.5);
        assert!(result.reconstruction_upper.linear() >= result.point_estimate.overall.linear());
        assert!(result.reconstruction_upper.linear() - 0.5 < 1.0e-9);
    }
}
