//! Streaming true-peak measurement for decoded PCM audio.
//!
//! This crate deliberately has no file, process, or application-policy layer.
//! Callers provide decoded interleaved `f64` frames and receive a level.
//!
//! `Reporting4x` follows libebur128's public true-peak profile: the 49-tap
//! Hann-windowed interpolator runs at 4x below 96 kHz, 2x from 96 kHz up to
//! 192 kHz, and reports sample peak at 192 kHz and above. `Headroom64x` is a
//! separate high-accuracy point-estimate path for safety/headroom decisions.
//! Its safety authority is deliberately band-qualified rather than pretending
//! that a finite interpolator can prove a uniform bound at critical Nyquist.

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

const HEADROOM64X_AUTHORITY_LINEAR_SCALE: f64 = 1.003_459_849_147_839_3;

/// Oversampling mode used for true-peak evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruePeakMode {
    /// libebur128-compatible reporting profile, using up to four-times
    /// interpolation depending on input sample rate.
    Reporting4x,
    /// Sixty-four-times high-accuracy interpolation for headroom decisions.
    Headroom64x,
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
        }
    }
}

/// Boundary convention for samples required outside a finite input stream.
///
/// This setting is used by `Headroom64x`. `Reporting4x` deliberately ignores
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
    /// Finite-stream boundary convention for `Headroom64x`.
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
/// Headroom64x safety authority.
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
            linear: linear * HEADROOM64X_AUTHORITY_LINEAR_SCALE,
            dbtp: dbtp + HEADROOM64X_MAX_UNDERREAD_DB,
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
    delay: Vec<Vec<f64>>,
    delay_index: usize,
    channels: usize,
}

impl HeadroomHalfSampleStage {
    const GROUP_DELAY_INPUTS: i128 = (HEADROOM64_HALF_DELAY_TAPS / 2) as i128;

    fn new(channels: usize) -> Self {
        Self {
            delay: vec![vec![0.0; HEADROOM64_HALF_DELAY_TAPS]; channels],
            delay_index: 0,
            channels,
        }
    }

    #[inline]
    fn delayed(&self, channel: usize, frames_ago: usize) -> f64 {
        let len = HEADROOM64_HALF_DELAY_TAPS;
        let index = (self.delay_index + len - frames_ago) % len;
        self.delay[channel][index]
    }

    fn process_frame(&mut self, frame: &[f64], input_index: i128, output: &mut [f64]) -> i128 {
        debug_assert_eq!(frame.len(), self.channels);
        debug_assert_eq!(output.len(), self.channels * 2);

        for (channel, sample) in frame.iter().copied().enumerate() {
            self.delay[channel][self.delay_index] = sample;
        }

        for channel in 0..self.channels {
            // Integer phase is exact. This both preserves decoded samples and
            // avoids spending FIR work on a phase that needs no interpolation.
            output[channel] = self.delayed(channel, HEADROOM64_HALF_DELAY_TAPS / 2);

            // Type-II half-sample coefficients are symmetric. Pairing the
            // delay-line samples halves the multiply count without changing
            // the designed response.
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

        (input_index - Self::GROUP_DELAY_INPUTS) * 2
    }
}

#[derive(Debug, Clone)]
struct HeadroomEngine {
    stage1: HeadroomHalfSampleStage,
    stage2: InterpolatorStage,
    stage3: InterpolatorStage,
    stage4: InterpolatorStage,
    stage5: InterpolatorStage,
    stage6: InterpolatorStage,
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
        let stage2 = InterpolatorStage::new(
            HEADROOM64_STAGE_2_TAPS,
            2,
            channels,
            Window::Blackman,
            true,
        );
        let stage3 = InterpolatorStage::new(
            HEADROOM64_STAGE_3_TAPS,
            2,
            channels,
            Window::Blackman,
            true,
        );
        let stage4 = InterpolatorStage::new(
            HEADROOM64_STAGE_4_TAPS,
            2,
            channels,
            Window::Blackman,
            true,
        );
        let stage5 = InterpolatorStage::new(
            HEADROOM64_STAGE_5_TAPS,
            2,
            channels,
            Window::Blackman,
            true,
        );
        let stage6 = InterpolatorStage::new(
            HEADROOM64_STAGE_6_TAPS,
            2,
            channels,
            Window::Blackman,
            true,
        );

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
                                update_channel_peaks_scaled(
                                    channel_peaks,
                                    &self.scratch6[phase6 * self.channels..(phase6 + 1) * self.channels],
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

#[derive(Debug, Clone)]
enum MeterEngine {
    Reporting(ReportingEngine),
    Headroom(HeadroomEngine),
}

impl MeterEngine {
    fn pre_post_frames(&self) -> i128 {
        match self {
            Self::Reporting(engine) => engine.pre_post_frames(),
            Self::Headroom(engine) => engine.pre_post_frames,
        }
    }

    fn process_frame(
        &mut self,
        frame: &[f64],
        input_index: i128,
        channel_peaks: &mut [f64],
        upper_subframe: Option<i128>,
    ) {
        match self {
            Self::Reporting(engine) => {
                engine.process_frame(frame, input_index, channel_peaks, upper_subframe)
            }
            Self::Headroom(engine) => {
                engine.process_frame(frame, input_index, channel_peaks, upper_subframe)
            }
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
        };

        Ok(Self {
            config,
            factor,
            engine,
            channel_peaks: vec![0.0; config.channels],
            last_frame: vec![0.0; config.channels],
            started: false,
            next_input_index: 0,
            frames: 0,
        })
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
            self.engine
                .process_frame(frame, input_index, &mut self.channel_peaks, None);
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
    pub fn finalize(mut self) -> Result<TruePeakResult, TruePeakError> {
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
                Some(upper_subframe),
            );
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
        Ok(TruePeakResult {
            overall,
            channel_linear_peaks: self.channel_peaks,
            frames: self.frames,
        })
    }
}

fn update_channel_peaks(channel_peaks: &mut [f64], frame: &[f64]) {
    for (peak, sample) in channel_peaks.iter_mut().zip(frame.iter().copied()) {
        let magnitude = sample.abs();
        if magnitude > *peak {
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
    use super::HEADROOM64_HALF_DELAY_COEFFICIENTS;

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
}
