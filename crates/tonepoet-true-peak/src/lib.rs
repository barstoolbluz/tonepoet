//! Streaming true-peak measurement for decoded PCM audio.
//!
//! The public ceiling surface has exactly three HQ1024V1 tiers:
//! [`PeakTier::Reference`], [`PeakTier::Standard`], and [`PeakTier::Fast`].
//! They differ only in how the same certified HQ1024V1 reconstruction is
//! searched. [`ReportingPeakMeter`] is deliberately separate: it implements
//! the established libebur128-compatible reporting profile rather than the
//! finite HQ ceiling reconstruction.
//!
//! The old Headroom point-estimator ladder is not part of the public surface.
//! Its direct LegacyHeadroom64 implementation is retained only as an internal
//! independent oracle for HQ/legacy regression tests.

use std::error::Error;
use std::f64::consts::PI;
use std::fmt;

mod headroom64_coefficients;
mod hq1024_coefficients;
mod qualified_prefix_coefficients;
mod qualified_half_delay_fft;
mod certified_scan;
mod fast_scan;
#[cfg(feature = "fast-stage-timing")]
mod fast_stage_timing;

use headroom64_coefficients::HEADROOM64_HALF_DELAY_COEFFICIENTS;

const COEFFICIENT_EPSILON: f64 = 1.0e-15;
const REPORTING_TAPS: usize = 49;

/// Reference-tier commissioning objective for the achieved finite-peak interval.
///
/// This is an acceptance objective, not an additive safety reserve.
pub const REFERENCE_INTERVAL_OBJECTIVE_DB: f64 = 0.000_000_1;

/// Standard-tier commissioning objective for the achieved finite-peak interval.
///
/// This is an acceptance objective, not an additive safety reserve.
pub const STANDARD_INTERVAL_OBJECTIVE_DB: f64 = 0.000_1;

/// Fast066 algorithm revision identifier.
///
/// This is intentionally explicit because persisted application settings named
/// `fast` predate the current HQ1024V1 Fast implementation.
pub const FAST_ALGORITHM_REVISION: &str = "Fast066V2";

/// Fast-tier end-to-end performance target, expressed without fractional-unit
/// truncation: 0.66 seconds of wall time per minute of decoded programme audio.
///
/// Fast itself is deterministic and does not consult a clock. This is a
/// commissioning target for the shipping benchmark, not a runtime deadline.
pub const FAST_WALL_NANOS_PER_PROGRAMME_MINUTE: u64 = 660_000_000;

/// Commissioning-only Fast066 execution cut points.
///
/// This surface exists only with `fast-stage-timing`; normal builds cannot
/// select an ablation in place of the shipping Fast066V2 algorithm.
#[cfg(feature = "fast-stage-timing")]
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FastCommissioningMode {
    /// Qualified prefix, complete HQ4 survey, and flat certificate bounds only.
    SurveyBoundsOnly,
    /// Survey/bounds plus deterministic 64-entry nominee discovery and selection.
    NominationOnly,
    /// Complete shipping Fast066V2 proposal and finishing policy.
    Production,
}

// Legacy64 remains only as an independent internal oracle/descriptor. These
// constants are not user-selectable policy.
const HEADROOM64_HALF_DELAY_TAPS: usize = 384;
const HEADROOM64_STAGE_2_TAPS: usize = 49;
const HEADROOM64_STAGE_3_TAPS: usize = 25;
const HEADROOM64_STAGE_4_TAPS: usize = 17;
const HEADROOM64_STAGE_5_TAPS: usize = 13;
const HEADROOM64_STAGE_6_TAPS: usize = 9;
const HEADROOM64_INTERPOLATION_CALIBRATION_LINEAR: f64 = 0.999_539_589_003_087_8;
const HEADROOM64X_RECONSTRUCTION_LINF_GAIN_UPPER: f64 = 4.09;

/// Conservative induced L-infinity gain of the public HQ1024V1 reconstruction.
pub const HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER: f64 = 4.68;

/// Boundary convention for samples required outside a finite certified input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgePolicy {
    /// Extend the first and last decoded frame outward.
    RepeatEndpoints,
    /// Treat samples outside the finite stream as digital zero.
    ZeroExtend,
}

/// A measured true-peak level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PeakLevel {
    /// Every represented sample was exactly zero.
    Silence,
    /// Finite true peak. `linear` is relative to digital full scale and is not
    /// clamped; values greater than 1.0 therefore produce positive dBTP.
    Finite { linear: f64, dbtp: f64 },
}

const F64_MAGNITUDE_MASK_FOR_DB: u64 = 0x7fff_ffff_ffff_ffff;
const F64_INFINITY_BITS_FOR_DB: u64 = 0x7ff0_0000_0000_0000;
const F64_MIN_NORMAL_BITS_FOR_DB: u64 = 0x0010_0000_0000_0000;

#[inline]
const fn f64_magnitude_bits_for_db(value: f64) -> u64 {
    value.to_bits() & F64_MAGNITUDE_MASK_FOR_DB
}

/// Convert a positive finite binary64 magnitude to dBTP without passing a
/// subnormal operand to libm. On x86 DAZ may otherwise reinterpret that
/// operand as zero inside `log10`, incorrectly producing `-inf`.
#[inline]
fn positive_finite_linear_to_dbtp(linear: f64) -> f64 {
    let bits = f64_magnitude_bits_for_db(linear);
    debug_assert!(bits != 0 && bits < F64_INFINITY_BITS_FOR_DB);
    if bits < F64_MIN_NORMAL_BITS_FOR_DB {
        // Every positive subnormal is k * 2^-1074 for an integer
        // 1 <= k < 2^52. `k` is exactly representable and normal.
        let k = bits as f64;
        20.0 * (k.log10() - 1074.0 * std::f64::consts::LOG10_2)
    } else {
        20.0 * linear.log10()
    }
}

impl PeakLevel {
    /// Linear full-scale value; exact silence maps to zero.
    #[must_use]
    pub const fn linear(self) -> f64 {
        match self {
            Self::Silence => 0.0,
            Self::Finite { linear, .. } => linear,
        }
    }
}

/// The finite waveform certified by every public ceiling tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CertifiedReconstruction {
    /// Frozen high-accuracy 1024x finite reconstruction.
    Hq1024V1,
}

/// User-facing ceiling measurement tier.
///
/// These names are intentionally API choices, not persistence migrations.
/// In particular, an older Tonepoet setting named `fast` meant the retired
/// 16x prefix estimator. Integration must not deserialize that stored value as
/// [`PeakTier::Fast`] without an explicit migration decision.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PeakTier {
    /// Exhaustive Reference9 search of HQ1024V1.
    Reference,
    /// Deterministic bounded-work Fast90 search of HQ1024V1.
    #[default]
    Standard,
    /// Deterministic bounded-work Fast066 search of HQ1024V1, with a release
    /// commissioning target of 0.66 seconds of wall time per programme minute.
    Fast,
}

impl PeakTier {
    /// Commissioning width objective where the tier has one.
    ///
    /// Fast intentionally returns `None`: its fixed work budget is an
    /// execution policy, not an accuracy floor. The returned interval remains
    /// authoritative even when optional proposal/finishing work does not find the winner.
    #[must_use]
    pub const fn interval_objective_db(self) -> Option<f64> {
        match self {
            Self::Reference => Some(REFERENCE_INTERVAL_OBJECTIVE_DB),
            Self::Standard => Some(STANDARD_INTERVAL_OBJECTIVE_DB),
            Self::Fast => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ReconstructionId {
    LegacyHeadroom64,
    Hq1024V1,
}

impl ReconstructionId {
    #[must_use]
    pub(crate) const fn target_factor(self) -> usize {
        match self {
            Self::LegacyHeadroom64 => 64,
            Self::Hq1024V1 => 1024,
        }
    }

    #[must_use]
    pub(crate) const fn reconstruction_linf_gain_upper(self) -> f64 {
        match self {
            Self::LegacyHeadroom64 => HEADROOM64X_RECONSTRUCTION_LINF_GAIN_UPPER,
            Self::Hq1024V1 => HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SearchPolicy {
    Reference9,
    Fast90,
    RetiredClockFast1s,
}

impl SearchPolicy {
    #[must_use]
    pub(crate) const fn public_tier(self) -> PeakTier {
        match self {
            Self::Reference9 => PeakTier::Reference,
            Self::Fast90 => PeakTier::Standard,
            Self::RetiredClockFast1s => PeakTier::Fast,
        }
    }

    #[must_use]
    pub(crate) const fn uses_accelerated_prefix(self) -> bool {
        !matches!(self, Self::Reference9)
    }
}

/// Whether a certified search fully resolved its competitive regions or
/// returned safely with unresolved bounded work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchStatus {
    /// No unresolved region can exceed the proven lower winner.
    Complete,
    /// A deterministic bounded-work search completed its allotted work while
    /// a truthful unresolved interval remained.
    WorkLimited,
    /// Retained for compatibility with certificates produced by the retired
    /// clock-bounded Fast implementation. The current public Fast066 backend
    /// is deterministic and never returns this status.
    TimeLimited,
}

/// Closed nonnegative interval enclosing a finite reconstruction peak.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeakInterval {
    pub lower_linear: f64,
    pub upper_linear: f64,
}

impl PeakInterval {
    pub(crate) fn new(lower_linear: f64, upper_linear: f64) -> Self {
        assert!(lower_linear >= 0.0, "peak interval lower must be nonnegative");
        assert!(upper_linear >= lower_linear, "peak interval upper must not be below lower");
        Self { lower_linear, upper_linear }
    }

    #[must_use]
    pub fn width_db(self) -> Option<f64> {
        let upper_bits = f64_magnitude_bits_for_db(self.upper_linear);
        let lower_bits = f64_magnitude_bits_for_db(self.lower_linear);
        if upper_bits == 0 { return None; }
        if lower_bits == 0 { return Some(f64::INFINITY); }
        if upper_bits < F64_MIN_NORMAL_BITS_FOR_DB || lower_bits < F64_MIN_NORMAL_BITS_FOR_DB {
            Some(positive_finite_linear_to_dbtp(self.upper_linear)
                - positive_finite_linear_to_dbtp(self.lower_linear))
        } else {
            Some(20.0 * (self.upper_linear / self.lower_linear).log10())
        }
    }

    #[must_use]
    pub fn upper_level(self) -> PeakLevel {
        if f64_magnitude_bits_for_db(self.upper_linear) == 0 {
            PeakLevel::Silence
        } else {
            PeakLevel::Finite {
                linear: self.upper_linear,
                dbtp: positive_finite_linear_to_dbtp(self.upper_linear),
            }
        }
    }

    #[must_use]
    pub const fn is_silence(self) -> bool {
        f64_magnitude_bits_for_db(self.lower_linear) == 0
            && f64_magnitude_bits_for_db(self.upper_linear) == 0
    }
}

/// Work and numerical diagnostics attached to a certified search.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SearchDiagnostics {
    pub tiles_processed: u64,
    pub groups_rejected: u64,
    pub groups_expanded: u64,
    pub candidate_cells: u64,
    pub refined_cells: u64,
    pub phase_evaluations: u64,
    pub authoritative_coarse_values: u64,
    pub authoritative_coarse_groups: u64,
    /// Standard/retired-Fast roots tested by the cheap whole-support L1 bound.
    pub accelerated_l1_groups_tested: u64,
    /// Accelerated roots rejected by the L1 bound.
    pub accelerated_l1_groups_rejected: u64,
    /// Accelerated roots that required the curvature hierarchy.
    pub accelerated_curvature_roots: u64,
    /// Whether the same-graph AVX qualified-prefix executor was active.
    pub accelerated_same_graph_avx_prefix_active: bool,
    pub strict_coarse_evaluations: u64,
    pub dense_regions: u64,
    pub dense_intermediate_cells: u64,
    pub dense_complete_regions: u64,
    pub dense_phase_evaluations: u64,
    pub direct_rescore_evaluations: u64,
    /// Deterministic modeled work consumed by the shared certified-search engine.
    /// Standard uses it as a limit; Reference records it; Fast066 leaves it zero.
    pub work_credits_consumed: u64,
    /// Tiles in which Standard exhausted deterministic credits.
    pub work_limited_tiles: u64,
    /// Qualified first-stage FFT blocks replaced by the retired clock-bounded
    /// Fast implementation. Fast066 leaves this at zero.
    pub time_bounded_prefix_blocks_skipped: u64,
    /// Tiles in which the retired clock-bounded Fast implementation reached
    /// its cumulative processing-time allowance. Fast066 leaves this at zero.
    pub time_limited_tiles: u64,
    /// Complete HQ 4x survey knots reduced by Fast066.
    pub fast_survey_knots: u64,
    /// Local HQ 4x maxima observed by Fast066 before quota selection.
    pub fast_candidates_observed: u64,
    /// Flat 256-frame algebraic envelopes evaluated by Fast066.
    pub fast_flat_groups: u64,
    /// HQ4 nominees retained by Fast066V2's spatial-plus-global 64-entry selection.
    pub fast_candidates_selected: u64,
    /// Channel-tiles whose local-maxima population exceeded the 64-nominee quota.
    pub fast_candidate_saturated_tiles: u64,
    /// Surviving nominees for which Fast066V2 evaluated or reused the proposed HQ1024 knot.
    pub fast_proposals_evaluated: u64,
    /// Proposal-stage non-HQ4 HQ1024 knots actually evaluated by the short-tail bank.
    pub fast_proposal_fine_knots_evaluated: u64,
    /// Proposal records admitted to the at-most-eight finishing pass after its bound recheck.
    pub fast_finishing_candidates: u64,
    /// Finishing-stage non-HQ4 HQ1024 knots actually evaluated by the short-tail bank.
    pub fast_finishing_fine_knots_evaluated: u64,
    /// New non-HQ4 HQ1024 knots evaluated by all Fast066V2 optional work.
    pub fast_fine_knots_evaluated: u64,
    /// Channel-tiles whose complete optional-work domain was dominated by the same-channel lower witness.
    pub fast_bound_pruned_channel_tiles: u64,
    /// Selected nominees skipped because their complete possible-work neighborhood was dominated.
    pub fast_bound_pruned_nominees: u64,
    /// Top-eight proposal records skipped after the lower witness improved before finishing.
    pub fast_bound_pruned_finishers: u64,
    /// Probed nominees whose HQ4 three-point fit was invalid and therefore proposed their center knot.
    pub fast_invalid_proposal_fits: u64,
    /// Finishing candidates stopped because an endpoint probe was missing or the center did not win.
    pub fast_unbracketed_finishers: u64,
    /// Center-winning finishing candidates stopped because their signed concave fit was invalid.
    pub fast_invalid_finishing_fits: u64,
    /// Exact maximum input-sample magnitude observed by Fast066.
    pub fast_input_sample_peak_linear: f64,
    /// Complete HQ 4x survey point maximum observed by Fast066.
    pub fast_hq4_peak_linear: f64,
    /// Commissioning-only time spent in qualified-prefix execution plus block ingestion.
    #[cfg(feature = "fast-stage-timing")]
    pub fast_stage_prefix_block_ingest_nanos: u64,
    /// Commissioning-only time spent building the complete HQ 4x survey.
    #[cfg(feature = "fast-stage-timing")]
    pub fast_stage_midpoint_survey_nanos: u64,
    /// Commissioning-only time spent constructing flat-group certificate envelopes.
    #[cfg(feature = "fast-stage-timing")]
    pub fast_stage_flat_envelope_nanos: u64,
    /// Commissioning-only inclusive time spent in nomination, proposal, and finishing work.
    /// This retained aggregate overlaps the three V2 stage counters below.
    #[cfg(feature = "fast-stage-timing")]
    pub fast_stage_candidate_refinement_nanos: u64,
    /// Commissioning-only time spent discovering and selecting at most 64 HQ4 nominees.
    #[cfg(feature = "fast-stage-timing")]
    pub fast_stage_nomination_nanos: u64,
    /// Commissioning-only time spent screening nominees and evaluating one proposed HQ knot each.
    #[cfg(feature = "fast-stage-timing")]
    pub fast_stage_proposal_nanos: u64,
    /// Commissioning-only time spent ranking measured proposals and finishing at most eight.
    #[cfg(feature = "fast-stage-timing")]
    pub fast_stage_finishing_nanos: u64,
    /// Commissioning-only time spent in final certificate reduction after all tile work.
    #[cfg(feature = "fast-stage-timing")]
    pub fast_stage_finalize_nanos: u64,
    pub max_evaluation_error_linear: f64,
    pub unresolved_upper_linear: f64,
}

/// Public HQ1024V1 certificate.
#[derive(Debug, Clone, PartialEq)]
pub struct PeakCertificate {
    pub reconstruction: CertifiedReconstruction,
    pub tier: PeakTier,
    pub reconstruction_linf_gain_upper: f64,
    pub numerical_envelope_linear: f64,
    pub reported_point_estimate: TruePeakResult,
    pub finite_interval: PeakInterval,
    pub channel_intervals: Vec<PeakInterval>,
    pub channel_upper_linear_peaks: Vec<f64>,
    pub status: SearchStatus,
    pub diagnostics: SearchDiagnostics,
}

impl PeakCertificate {
    #[must_use]
    pub fn upper_level(&self) -> PeakLevel { self.finite_interval.upper_level() }
    #[must_use]
    pub fn interval_width_db(&self) -> Option<f64> { self.finite_interval.width_db() }
    #[must_use]
    pub const fn reconstruction_linf_gain_upper(&self) -> f64 {
        self.reconstruction_linf_gain_upper
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InternalPeakCertificate {
    pub reconstruction: ReconstructionId,
    pub search_policy: SearchPolicy,
    pub reconstruction_linf_gain_upper: f64,
    pub numerical_envelope_linear: f64,
    pub reported_point_estimate: TruePeakResult,
    pub finite_interval: PeakInterval,
    pub channel_intervals: Vec<PeakInterval>,
    pub channel_upper_linear_peaks: Vec<f64>,
    pub status: SearchStatus,
    pub diagnostics: SearchDiagnostics,
}

impl InternalPeakCertificate {
    #[cfg(test)]
    pub(crate) fn upper_level(&self) -> PeakLevel { self.finite_interval.upper_level() }
    #[cfg(test)]
    pub(crate) fn interval_width_db(&self) -> Option<f64> { self.finite_interval.width_db() }
}

/// Streaming certified HQ1024V1 meter.
///
/// Caller chunk sizes do not define FFT blocks or search tiles. All three tiers
/// are deterministic for a fixed backend. Fast uses its own fixed-work Fast066
/// execution graph; host scheduling never changes which signal locations are
/// measured.
#[derive(Debug, Clone)]
pub struct CertifiedPeakMeter {
    inner: CertifiedPeakBackend,
}

#[derive(Debug, Clone)]
enum CertifiedPeakBackend {
    Certified(certified_scan::CertifiedPeakMeterImpl),
    Fast(fast_scan::FastPeakMeterImpl),
}

impl CertifiedPeakMeter {
    /// Create one of the three public HQ1024V1 tiers.
    pub fn new(
        sample_rate_hz: u32,
        channels: usize,
        edge_policy: EdgePolicy,
        tier: PeakTier,
    ) -> Result<Self, TruePeakError> {
        let inner = match tier {
            PeakTier::Reference => CertifiedPeakBackend::Certified(
                certified_scan::CertifiedPeakMeterImpl::new(
                    sample_rate_hz,
                    channels,
                    edge_policy,
                    ReconstructionId::Hq1024V1,
                    SearchPolicy::Reference9,
                )?,
            ),
            PeakTier::Standard => CertifiedPeakBackend::Certified(
                certified_scan::CertifiedPeakMeterImpl::new(
                    sample_rate_hz,
                    channels,
                    edge_policy,
                    ReconstructionId::Hq1024V1,
                    SearchPolicy::Fast90,
                )?,
            ),
            PeakTier::Fast => CertifiedPeakBackend::Fast(
                fast_scan::FastPeakMeterImpl::new(sample_rate_hz, channels, edge_policy)?,
            ),
        };
        Ok(Self { inner })
    }

    /// Construct a Fast066 commissioning ablation. This method is unavailable
    /// in normal builds so production callers cannot accidentally select a
    /// partial execution graph.
    #[cfg(feature = "fast-stage-timing")]
    #[doc(hidden)]
    pub fn new_fast_commissioning(
        sample_rate_hz: u32,
        channels: usize,
        edge_policy: EdgePolicy,
        mode: FastCommissioningMode,
    ) -> Result<Self, TruePeakError> {
        let execution_mode = match mode {
            FastCommissioningMode::SurveyBoundsOnly =>
                fast_scan::FastExecutionMode::SurveyBoundsOnly,
            FastCommissioningMode::NominationOnly =>
                fast_scan::FastExecutionMode::NominationOnly,
            FastCommissioningMode::Production => fast_scan::FastExecutionMode::Production,
        };
        Ok(Self {
            inner: CertifiedPeakBackend::Fast(fast_scan::FastPeakMeterImpl::new_with_execution_mode(
                sample_rate_hz,
                channels,
                edge_policy,
                execution_mode,
            )?),
        })
    }

    pub fn push_interleaved(&mut self, samples: &[f64]) -> Result<(), TruePeakError> {
        match &mut self.inner {
            CertifiedPeakBackend::Certified(inner) => inner.push_interleaved(samples),
            CertifiedPeakBackend::Fast(inner) => inner.push_interleaved(samples),
        }
    }

    pub fn finalize(self) -> Result<PeakCertificate, TruePeakError> {
        match self.inner {
            CertifiedPeakBackend::Fast(inner) => inner.finalize(),
            CertifiedPeakBackend::Certified(inner) => {
                let internal = inner.finalize()?;
                debug_assert_eq!(internal.reconstruction, ReconstructionId::Hq1024V1);
                Ok(PeakCertificate {
                    reconstruction: CertifiedReconstruction::Hq1024V1,
                    tier: internal.search_policy.public_tier(),
                    reconstruction_linf_gain_upper: internal.reconstruction_linf_gain_upper,
                    numerical_envelope_linear: internal.numerical_envelope_linear,
                    reported_point_estimate: internal.reported_point_estimate,
                    finite_interval: internal.finite_interval,
                    channel_intervals: internal.channel_intervals,
                    channel_upper_linear_peaks: internal.channel_upper_linear_peaks,
                    status: internal.status,
                    diagnostics: internal.diagnostics,
                })
            }
        }
    }
}

/// Final result from the standards-compatible reporting meter.
#[derive(Debug, Clone, PartialEq)]
pub struct TruePeakResult {
    pub overall: PeakLevel,
    pub channel_linear_peaks: Vec<f64>,
    pub frames: u64,
}

/// Input/configuration errors detected by a meter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TruePeakError {
    InvalidSampleRate,
    InvalidChannelCount,
    IncompleteFrame { samples: usize, channels: usize },
    NonFiniteSample { sample_index: usize },
    NumericalOverflow,
    /// Internal Reference invariant failure: a live unresolved upper survived
    /// an exhaustive search. Returning a partial Reference certificate would
    /// violate the tier contract, so finalization fails closed instead.
    ReferenceSearchIncomplete,
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
            Self::NumericalOverflow => f.write_str("true-peak reconstruction exceeded finite binary64 range"),
            Self::ReferenceSearchIncomplete => f.write_str(
                "reference true-peak search ended with a live unresolved region",
            ),
            Self::EmptyInput => f.write_str("true-peak measurement requires at least one frame"),
            Self::InputTooLong => f.write_str("true-peak input frame count overflowed"),
        }
    }
}
impl Error for TruePeakError {}

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

/// Dedicated standards-compatible reporting meter.
///
/// This answers the interoperable 4x/2x/1x reporting question used by
/// libebur128 consumers. It is deliberately not a low-accuracy ceiling tier.
#[derive(Debug, Clone)]
pub struct ReportingPeakMeter {
    sample_rate_hz: u32,
    channels: usize,
    engine: ReportingEngine,
    channel_peaks: Vec<f64>,
    next_input_index: i128,
    frames: u64,
}

impl ReportingPeakMeter {
    pub fn new(sample_rate_hz: u32, channels: usize) -> Result<Self, TruePeakError> {
        if sample_rate_hz == 0 { return Err(TruePeakError::InvalidSampleRate); }
        if channels == 0 { return Err(TruePeakError::InvalidChannelCount); }
        Ok(Self {
            sample_rate_hz,
            channels,
            engine: ReportingEngine::new(sample_rate_hz, channels),
            channel_peaks: vec![0.0; channels],
            next_input_index: 0,
            frames: 0,
        })
    }

    /// Effective interpolation factor for the libebur128-compatible profile.
    #[must_use]
    pub const fn oversample_factor(&self) -> usize {
        if self.sample_rate_hz >= 192_000 { 1 }
        else if self.sample_rate_hz >= 96_000 { 2 }
        else { 4 }
    }

    /// Validate the whole block before mutation, then consume complete frames.
    pub fn push_interleaved(&mut self, samples: &[f64]) -> Result<(), TruePeakError> {
        if samples.len() % self.channels != 0 {
            return Err(TruePeakError::IncompleteFrame { samples: samples.len(), channels: self.channels });
        }
        if let Some((sample_index, _)) = samples.iter().enumerate().find(|(_, sample)| !sample.is_finite()) {
            return Err(TruePeakError::NonFiniteSample { sample_index });
        }
        for frame in samples.chunks_exact(self.channels) {
            update_channel_peaks(&mut self.channel_peaks, frame);
            self.engine.process_frame(frame, self.next_input_index, &mut self.channel_peaks, None);
            self.next_input_index += 1;
            self.frames = self.frames.checked_add(1).ok_or(TruePeakError::InputTooLong)?;
        }
        Ok(())
    }

    pub fn finalize(self) -> Result<TruePeakResult, TruePeakError> {
        if self.frames == 0 { return Err(TruePeakError::EmptyInput); }
        let linear = self.channel_peaks.iter().copied().fold(0.0, max_nonnegative_by_bits);
        let overall = if f64_magnitude_bits_for_db(linear) == 0 {
            PeakLevel::Silence
        } else {
            PeakLevel::Finite { linear, dbtp: positive_finite_linear_to_dbtp(linear) }
        };
        Ok(TruePeakResult { overall, channel_linear_peaks: self.channel_peaks, frames: self.frames })
    }
}

#[cfg(test)]
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

#[cfg(test)]
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
}

/// Fixed 2x interpolation stage used only by Headroom64x. The checked-in
/// mathematical filter is still generated by `build_polyphase_filters`; this
/// execution layout merely specializes its exact identity phase and stores a
/// doubled circular delay so the nontrivial phase is one contiguous dot
/// product.
#[cfg(test)]
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

#[cfg(test)]
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
#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
pub(crate) fn legacy_headroom64_direct_oracle_peak(
    samples: &[f64],
    channels: usize,
    edge_policy: EdgePolicy,
) -> Result<f64, TruePeakError> {
    if channels == 0 { return Err(TruePeakError::InvalidChannelCount); }
    if samples.len() % channels != 0 {
        return Err(TruePeakError::IncompleteFrame { samples: samples.len(), channels });
    }
    if let Some((sample_index, _)) = samples.iter().enumerate().find(|(_, sample)| !sample.is_finite()) {
        return Err(TruePeakError::NonFiniteSample { sample_index });
    }
    let frames = samples.len() / channels;
    if frames == 0 { return Err(TruePeakError::EmptyInput); }

    let mut engine = HeadroomEngine::new(channels);
    let mut point_sink = vec![0.0; channels];
    let mut reconstruction_peaks = vec![0.0; channels];
    let first = samples[..channels].to_vec();
    let pre_extension = match edge_policy {
        EdgePolicy::RepeatEndpoints => first,
        EdgePolicy::ZeroExtend => vec![0.0; channels],
    };
    for input_index in -engine.pre_post_frames..0 {
        engine.process_frame(
            &pre_extension,
            input_index,
            &mut point_sink,
            Some(&mut reconstruction_peaks),
            None,
        );
    }

    for (index, frame) in samples.chunks_exact(channels).enumerate() {
        engine.process_frame(
            frame,
            index as i128,
            &mut point_sink,
            Some(&mut reconstruction_peaks),
            None,
        );
    }

    let last = samples[samples.len() - channels..].to_vec();
    let post_extension = match edge_policy {
        EdgePolicy::RepeatEndpoints => last,
        EdgePolicy::ZeroExtend => vec![0.0; channels],
    };
    let upper_subframe = (frames as i128 - 1) * 64;
    let stop = frames as i128 + engine.pre_post_frames;
    for input_index in frames as i128..stop {
        engine.process_frame(
            &post_extension,
            input_index,
            &mut point_sink,
            Some(&mut reconstruction_peaks),
            Some(upper_subframe),
        );
    }

    Ok(reconstruction_peaks.into_iter().fold(0.0, max_nonnegative_by_bits))
}

#[inline]
fn max_nonnegative_by_bits(left: f64, right: f64) -> f64 {
    let left_bits = f64_magnitude_bits_for_db(left);
    let right_bits = f64_magnitude_bits_for_db(right);
    f64::from_bits(left_bits.max(right_bits))
}

fn update_channel_peaks(channel_peaks: &mut [f64], frame: &[f64]) {
    for (peak, sample) in channel_peaks.iter_mut().zip(frame.iter().copied()) {
        let magnitude_bits = f64_magnitude_bits_for_db(sample);
        if magnitude_bits > f64_magnitude_bits_for_db(*peak) {
            *peak = f64::from_bits(magnitude_bits);
        }
    }
}

#[cfg(test)]
#[inline]
fn update_channel_peaks_scaled(channel_peaks: &mut [f64], frame: &[f64], scale: f64) {
    debug_assert!(scale.is_finite() && scale >= 0.0);
    for (peak, sample) in channel_peaks.iter_mut().zip(frame.iter().copied()) {
        let magnitude_bits = f64_magnitude_bits_for_db(sample * scale);
        if magnitude_bits > f64_magnitude_bits_for_db(*peak) {
            *peak = f64::from_bits(magnitude_bits);
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
mod public_surface_tests {
    use super::*;

    #[test]
    fn tier_surface_is_exactly_hq_reference_standard_fast() {
        let tiers = [PeakTier::Reference, PeakTier::Standard, PeakTier::Fast];
        assert_eq!(tiers.len(), 3);
        assert_eq!(PeakTier::Reference.interval_objective_db(), Some(REFERENCE_INTERVAL_OBJECTIVE_DB));
        assert_eq!(PeakTier::Standard.interval_objective_db(), Some(STANDARD_INTERVAL_OBJECTIVE_DB));
        assert_eq!(PeakTier::Fast.interval_objective_db(), None);
    }

    #[test]
    fn reporting_profile_factor_is_rate_dependent_and_separate() {
        assert_eq!(ReportingPeakMeter::new(48_000, 1).unwrap().oversample_factor(), 4);
        assert_eq!(ReportingPeakMeter::new(96_000, 1).unwrap().oversample_factor(), 2);
        assert_eq!(ReportingPeakMeter::new(192_000, 1).unwrap().oversample_factor(), 1);
    }

    #[test]
    fn reporting_rejected_block_is_not_partially_consumed() {
        let mut meter = ReportingPeakMeter::new(48_000, 2).unwrap();
        assert!(matches!(
            meter.push_interleaved(&[0.1, 0.2, 0.3]),
            Err(TruePeakError::IncompleteFrame { .. })
        ));
        meter.push_interleaved(&[0.25, -0.5]).unwrap();
        assert_eq!(meter.finalize().unwrap().frames, 1);
    }

    #[test]
    fn logarithmic_view_keeps_nonzero_subnormal_finite() {
        let level = PeakInterval::new(f64::from_bits(7), f64::from_bits(11)).upper_level();
        match level {
            PeakLevel::Finite { dbtp, .. } => assert!(dbtp.is_finite()),
            PeakLevel::Silence => panic!("nonzero subnormal must not become silence"),
        }
    }
}
