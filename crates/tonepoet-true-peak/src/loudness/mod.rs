//! Streaming programme loudness measurements.
//!
//! This module deliberately keeps two named behaviors instead of a set of
//! compatibility switches. [`NativeEbu2023`] measures the full-resolution PCM
//! supplied by the caller. [`Libebur128126`] reproduces the pinned libebur128
//! 1.2.6 block clock, gate comparisons, default channel map (when explicitly
//! requested), and fourth-order K-weighting operation graph.  Legacy loudgain
//! S16 preparation is a consumer/front-end concern and is not performed here.

mod k_weighting;
mod simd;
mod statistics;
mod windows;

use std::error::Error;
use std::fmt;

use k_weighting::{FilterState, KWeightingCoefficients};
use statistics::{
    integrated_from_absolute_energies, lra_from_sorted_absolute_energies,
    passes_integrated_gate, sort_lra_energies, ABSOLUTE_GATE_ENERGY,
};
use windows::WindowClock;

/// Default admission limit for the meter's ring plus retained exact statistics.
/// Consumers with an application-wide worker budget should use
/// [`LoudnessMeter::with_roles_and_limit`] instead.
pub const DEFAULT_LOUDNESS_STORAGE_LIMIT_BYTES: usize = 256 * 1024 * 1024;

/// Constructor limits retained from libebur128 1.2.6.  The native profile uses
/// the same validated envelope until rates outside it have their own numerical
/// qualification rather than inheriting accidental coefficient behavior.
const MIN_SAMPLE_RATE_HZ: u32 = 16;
const MAX_SAMPLE_RATE_HZ: u32 = 2_822_400;
const MAX_CHANNELS: usize = 64;

/// The two supported loudness behaviors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoudnessProfile {
    /// Current standards-oriented profile: full-resolution PCM, explicit roles,
    /// rational 100 ms clocks, and the documented native LRA EOF continuation.
    NativeEbu2023,
    /// Compatibility profile matching libebur128 1.2.6 on identical prepared PCM.
    Libebur128126,
}

/// Metrics requested from a loudness observation or album reduction.
///
/// Integrated-only demand is intentionally limited to the native profile at
/// meter construction. Compatibility observations retain their historical
/// full-metric behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoudnessMetricDemand {
    IntegratedOnly,
    IntegratedAndRange,
}

impl LoudnessMetricDemand {
    #[inline]
    const fn includes_range(self) -> bool {
        matches!(self, Self::IntegratedAndRange)
    }
}

/// Metrics actually present in owned sufficient statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoudnessMetricCoverage {
    IntegratedOnly,
    IntegratedAndRange,
}

impl LoudnessMetricCoverage {
    #[must_use]
    pub const fn satisfies(self, demand: LoudnessMetricDemand) -> bool {
        match demand {
            LoudnessMetricDemand::IntegratedOnly => true,
            LoudnessMetricDemand::IntegratedAndRange => {
                matches!(self, Self::IntegratedAndRange)
            }
        }
    }
}

impl From<LoudnessMetricDemand> for LoudnessMetricCoverage {
    fn from(value: LoudnessMetricDemand) -> Self {
        match value {
            LoudnessMetricDemand::IntegratedOnly => Self::IntegratedOnly,
            LoudnessMetricDemand::IntegratedAndRange => Self::IntegratedAndRange,
        }
    }
}

/// Qualification-harness selector for the private loudness SIMD backend.
/// Hidden: it exists only so the timing example can pin a backend.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoudnessSimdBackend {
    Scalar,
    Sse2,
    Avx,
}

/// Loudness role of one decoded channel, in decoded channel order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelRole {
    Mono,
    Left,
    Right,
    Center,
    Lfe,
    LeftSurround,
    RightSurround,
    LeftBack,
    RightBack,
    /// Compatibility-only role used by libebur128's count-based default map.
    Unused,
}

impl ChannelRole {
    #[inline]
    const fn energy_weight(self) -> f64 {
        match self {
            Self::Lfe | Self::Unused => 0.0,
            Self::LeftSurround | Self::RightSurround | Self::LeftBack | Self::RightBack => 1.41,
            Self::Mono | Self::Left | Self::Right | Self::Center => 1.0,
        }
    }
}

/// Typed integrated-loudness outcome.  Non-finite legacy presentation is kept
/// outside this API so operationally different unavailable cases remain visible.
#[derive(Debug, Clone, PartialEq)]
pub enum IntegratedLoudness {
    Finite {
        lufs: f64,
        absolute_observations: usize,
        relative_observations: usize,
    },
    NoInput,
    InsufficientFrames { frames: u64, required_frames: u64 },
    BelowAbsoluteGate,
    BelowRelativeGate,
    NoEligibleBlocks,
    NumericalRange,
}

impl IntegratedLoudness {
    #[must_use]
    pub const fn finite_lufs(&self) -> Option<f64> {
        match self {
            Self::Finite { lufs, .. } => Some(*lufs),
            _ => None,
        }
    }
}

/// Typed LRA outcome.  A finite singleton distribution is `0 LU`; no eligible
/// observations is represented separately.
#[derive(Debug, Clone, PartialEq)]
pub enum LoudnessRange {
    /// Range was intentionally omitted because no declared consumer requested
    /// it. This is distinct from a requested range with no eligible windows.
    NotRequested,
    Finite { lu: f64, observations: usize },
    Unavailable { observations: usize },
}

impl LoudnessRange {
    #[must_use]
    pub const fn finite_lu(&self) -> Option<f64> {
        match self {
            Self::Finite { lu, .. } => Some(*lu),
            Self::NotRequested | Self::Unavailable { .. } => None,
        }
    }

    #[must_use]
    pub const fn observations(&self) -> usize {
        match self {
            Self::Finite { observations, .. } | Self::Unavailable { observations } => *observations,
            Self::NotRequested => 0,
        }
    }
}

/// Final per-track loudness result.
#[derive(Debug, Clone, PartialEq)]
pub struct LoudnessSummary {
    pub sample_rate_hz: u32,
    pub roles: Vec<ChannelRole>,
    pub profile: LoudnessProfile,
    pub metric_coverage: LoudnessMetricCoverage,
    pub real_frames: u64,
    pub integrated: IntegratedLoudness,
    pub range: LoudnessRange,
    pub absolute_integrated_observations: usize,
    pub absolute_lra_observations: usize,
    pub retained_storage_bytes: usize,
}

/// Owned sufficient statistics for deterministic album aggregation.
///
/// The energy arrays are intentionally private.  They are exact retained block
/// observations, not a public persistence format.
#[derive(Debug)]
pub struct LoudnessStatistics {
    profile: LoudnessProfile,
    metric_coverage: LoudnessMetricCoverage,
    integrated_energies: Vec<f64>,
    // Sorted at track finalization.  Temporal order is not needed by the LRA
    // rank calculation, and sorting in place avoids a permanent second copy.
    lra_energies: Vec<f64>,
    real_frames: u64,
}

impl LoudnessStatistics {
    #[must_use]
    pub const fn profile(&self) -> LoudnessProfile {
        self.profile
    }

    #[must_use]
    pub const fn metric_coverage(&self) -> LoudnessMetricCoverage {
        self.metric_coverage
    }

    #[must_use]
    pub const fn real_frames(&self) -> u64 {
        self.real_frames
    }

    #[must_use]
    pub fn integrated_observations(&self) -> usize {
        self.integrated_energies.len()
    }

    #[must_use]
    pub fn lra_observations(&self) -> usize {
        self.lra_energies.len()
    }

    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.integrated_energies.capacity() * std::mem::size_of::<f64>()
            + self.lra_energies.capacity() * std::mem::size_of::<f64>()
    }
}

/// Result of consuming a meter.
#[derive(Debug)]
pub struct LoudnessMeasurement {
    pub summary: LoudnessSummary,
    pub statistics: LoudnessStatistics,
}

/// Album result produced from independently windowed tracks.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumLoudnessSummary {
    pub profile: LoudnessProfile,
    pub metric_coverage: LoudnessMetricCoverage,
    pub track_count: usize,
    pub integrated: IntegratedLoudness,
    pub range: LoudnessRange,
    pub reporting_peak_linear: f64,
    pub retained_storage_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoudnessError {
    InvalidSampleRate,
    UnsupportedSampleRate { sample_rate_hz: u32 },
    InvalidChannelCount,
    AmbiguousChannelLayout { channels: usize },
    InvalidChannelLayout(String),
    IncompleteFrame { samples: usize, channels: usize },
    NonFiniteSample { sample_index: usize },
    NumericalRange,
    InputTooLong,
    ResourceLimit { required_bytes: usize, limit_bytes: usize },
    AllocationFailed,
    MeterFailed,
    UnsupportedMetricDemand {
        profile: LoudnessProfile,
        demand: LoudnessMetricDemand,
    },
    IncompleteMetricCoverage {
        required: LoudnessMetricDemand,
        available: LoudnessMetricCoverage,
    },
    IncompatibleProfile,
    InvalidReportingPeak,
    EmptyAlbum,
}

impl fmt::Display for LoudnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSampleRate => f.write_str("sample rate must be nonzero"),
            Self::UnsupportedSampleRate { sample_rate_hz } => write!(f, "sample rate {sample_rate_hz} Hz is outside the qualified {MIN_SAMPLE_RATE_HZ}..={MAX_SAMPLE_RATE_HZ} Hz range or does not yield finite K-weighting coefficients"),
            Self::InvalidChannelCount => write!(f, "channel count must be in 1..={MAX_CHANNELS}"),
            Self::AmbiguousChannelLayout { channels } => write!(
                f,
                "{channels}-channel loudness requires explicit channel roles"
            ),
            Self::InvalidChannelLayout(message) => f.write_str(message),
            Self::IncompleteFrame { samples, channels } => write!(
                f,
                "{samples} samples do not contain complete {channels}-channel frames"
            ),
            Self::NonFiniteSample { sample_index } => {
                write!(f, "non-finite PCM sample at interleaved index {sample_index}")
            }
            Self::NumericalRange => f.write_str("loudness arithmetic exceeded finite range"),
            Self::InputTooLong => f.write_str("loudness input length exceeds supported integer geometry"),
            Self::ResourceLimit { required_bytes, limit_bytes } => write!(
                f,
                "loudness storage requires {required_bytes} bytes, limit is {limit_bytes} bytes"
            ),
            Self::AllocationFailed => f.write_str("loudness buffer allocation failed"),
            Self::MeterFailed => f.write_str("loudness meter is terminal after a previous processing failure"),
            Self::UnsupportedMetricDemand { profile, demand } => write!(
                f,
                "loudness metric demand {demand:?} is not supported for profile {profile:?}"
            ),
            Self::IncompleteMetricCoverage { required, available } => write!(
                f,
                "loudness statistics provide {available:?}, but {required:?} is required"
            ),
            Self::IncompatibleProfile => f.write_str("album tracks use incompatible loudness profiles"),
            Self::InvalidReportingPeak => f.write_str("reporting peak must be finite and nonnegative"),
            Self::EmptyAlbum => f.write_str("album contains no tracks"),
        }
    }
}

impl Error for LoudnessError {}

/// Streaming K-weighted loudness meter.
#[derive(Debug)]
pub struct LoudnessMeter {
    sample_rate_hz: u32,
    channels: usize,
    roles: Vec<ChannelRole>,
    weights: Vec<f64>,
    profile: LoudnessProfile,
    metric_demand: LoudnessMetricDemand,
    simd_backend: simd::Backend,
    coefficients: KWeightingCoefficients,
    filters: Vec<FilterState>,
    ring: Vec<f64>,
    ring_frames: usize,
    write_frame: usize,
    clock: WindowClock,
    frames_seen: u64,
    real_frames: u64,
    integrated_energies: Vec<f64>,
    lra_energies: Vec<f64>,
    storage_limit_bytes: usize,
    fixed_storage_bytes: usize,
    history_storage_bytes: usize,
    failed: bool,
}

impl LoudnessMeter {
    /// Native convenience constructor for unambiguous mono/stereo streams.
    pub fn new(sample_rate_hz: u32, channels: usize) -> Result<Self, LoudnessError> {
        let roles = match channels {
            1 => vec![ChannelRole::Mono],
            2 => vec![ChannelRole::Left, ChannelRole::Right],
            0 => return Err(LoudnessError::InvalidChannelCount),
            _ => return Err(LoudnessError::AmbiguousChannelLayout { channels }),
        };
        Self::with_roles_and_limit(
            sample_rate_hz,
            &roles,
            LoudnessProfile::NativeEbu2023,
            DEFAULT_LOUDNESS_STORAGE_LIMIT_BYTES,
        )
    }

    /// Construct a meter from explicit decoded channel roles.
    pub fn with_roles(
        sample_rate_hz: u32,
        roles: &[ChannelRole],
        profile: LoudnessProfile,
    ) -> Result<Self, LoudnessError> {
        Self::with_roles_and_limit(
            sample_rate_hz,
            roles,
            profile,
            DEFAULT_LOUDNESS_STORAGE_LIMIT_BYTES,
        )
    }

    /// Construct a meter with explicit metric demand using the default storage
    /// allowance. Integrated-only demand is available for the native profile;
    /// existing constructors continue to request integrated loudness and LRA.
    pub fn with_roles_and_metric_demand(
        sample_rate_hz: u32,
        roles: &[ChannelRole],
        profile: LoudnessProfile,
        metric_demand: LoudnessMetricDemand,
    ) -> Result<Self, LoudnessError> {
        Self::with_roles_and_metric_demand_and_limit(
            sample_rate_hz,
            roles,
            profile,
            metric_demand,
            DEFAULT_LOUDNESS_STORAGE_LIMIT_BYTES,
        )
    }

    /// Explicit libebur128 1.2.6 count-based default channel mapping.
    /// This constructor is compatibility behavior, not native role inference.
    pub fn libebur128_126_default_layout(
        sample_rate_hz: u32,
        channels: usize,
    ) -> Result<Self, LoudnessError> {
        let roles = legacy_default_roles(channels)?;
        Self::with_roles_and_limit(
            sample_rate_hz,
            &roles,
            LoudnessProfile::Libebur128126,
            DEFAULT_LOUDNESS_STORAGE_LIMIT_BYTES,
        )
    }

    /// Advanced constructor with an explicit retained-storage admission limit.
    pub fn with_roles_and_limit(
        sample_rate_hz: u32,
        roles: &[ChannelRole],
        profile: LoudnessProfile,
        storage_limit_bytes: usize,
    ) -> Result<Self, LoudnessError> {
        Self::with_roles_and_metric_demand_and_limit(
            sample_rate_hz,
            roles,
            profile,
            LoudnessMetricDemand::IntegratedAndRange,
            storage_limit_bytes,
        )
    }

    /// Advanced constructor with explicit metric demand and retained-storage
    /// admission limit.
    pub fn with_roles_and_metric_demand_and_limit(
        sample_rate_hz: u32,
        roles: &[ChannelRole],
        profile: LoudnessProfile,
        metric_demand: LoudnessMetricDemand,
        storage_limit_bytes: usize,
    ) -> Result<Self, LoudnessError> {
        if sample_rate_hz == 0 {
            return Err(LoudnessError::InvalidSampleRate);
        }
        if !(MIN_SAMPLE_RATE_HZ..=MAX_SAMPLE_RATE_HZ).contains(&sample_rate_hz) {
            return Err(LoudnessError::UnsupportedSampleRate { sample_rate_hz });
        }
        if roles.is_empty() || roles.len() > MAX_CHANNELS {
            return Err(LoudnessError::InvalidChannelCount);
        }
        validate_roles(roles, profile)?;
        if profile == LoudnessProfile::Libebur128126
            && metric_demand == LoudnessMetricDemand::IntegratedOnly
        {
            return Err(LoudnessError::UnsupportedMetricDemand {
                profile,
                demand: metric_demand,
            });
        }

        let channels = roles.len();
        let coefficients = KWeightingCoefficients::for_rate(sample_rate_hz)?;
        let clock = WindowClock::new(sample_rate_hz, profile)?;
        let requested_ring_frames = if metric_demand.includes_range() {
            clock.ring_frames()
        } else {
            clock.integrated_window_frames()
        };
        let ring_frames = usize::try_from(requested_ring_frames)
            .map_err(|_| LoudnessError::InputTooLong)?;
        let ring_samples = ring_frames
            .checked_mul(channels)
            .ok_or(LoudnessError::InputTooLong)?;
        let ring_bytes = ring_samples
            .checked_mul(std::mem::size_of::<f64>())
            .ok_or(LoudnessError::InputTooLong)?;
        let filter_bytes = channels
            .checked_mul(std::mem::size_of::<FilterState>())
            .ok_or(LoudnessError::InputTooLong)?;
        let weight_bytes = channels
            .checked_mul(std::mem::size_of::<f64>())
            .ok_or(LoudnessError::InputTooLong)?;
        let role_bytes = channels
            .checked_mul(std::mem::size_of::<ChannelRole>())
            .ok_or(LoudnessError::InputTooLong)?;
        let requested_fixed_storage_bytes = ring_bytes
            .checked_add(filter_bytes)
            .and_then(|value| value.checked_add(weight_bytes))
            .and_then(|value| value.checked_add(role_bytes))
            .ok_or(LoudnessError::InputTooLong)?;
        if requested_fixed_storage_bytes > storage_limit_bytes {
            return Err(LoudnessError::ResourceLimit {
                required_bytes: requested_fixed_storage_bytes,
                limit_bytes: storage_limit_bytes,
            });
        }

        let mut ring = Vec::new();
        ring.try_reserve_exact(ring_samples)
            .map_err(|_| LoudnessError::AllocationFailed)?;
        ring.resize(ring_samples, 0.0);

        let mut filters = Vec::new();
        filters
            .try_reserve_exact(channels)
            .map_err(|_| LoudnessError::AllocationFailed)?;
        filters.resize(channels, FilterState::default());

        let mut weights = Vec::new();
        weights
            .try_reserve_exact(channels)
            .map_err(|_| LoudnessError::AllocationFailed)?;
        weights.extend(roles.iter().copied().map(ChannelRole::energy_weight));

        let mut owned_roles = Vec::new();
        owned_roles
            .try_reserve_exact(channels)
            .map_err(|_| LoudnessError::AllocationFailed)?;
        owned_roles.extend_from_slice(roles);

        // `try_reserve_exact` may legally allocate more than requested. Account
        // the physical retained capacities, not only the logical dimensions,
        // before admitting the meter under a byte limit.
        let fixed_storage_bytes = ring
            .capacity()
            .checked_mul(std::mem::size_of::<f64>())
            .and_then(|value| {
                filters
                    .capacity()
                    .checked_mul(std::mem::size_of::<FilterState>())
                    .and_then(|bytes| value.checked_add(bytes))
            })
            .and_then(|value| {
                weights
                    .capacity()
                    .checked_mul(std::mem::size_of::<f64>())
                    .and_then(|bytes| value.checked_add(bytes))
            })
            .and_then(|value| {
                owned_roles
                    .capacity()
                    .checked_mul(std::mem::size_of::<ChannelRole>())
                    .and_then(|bytes| value.checked_add(bytes))
            })
            .ok_or(LoudnessError::InputTooLong)?;
        if fixed_storage_bytes > storage_limit_bytes {
            return Err(LoudnessError::ResourceLimit {
                required_bytes: fixed_storage_bytes,
                limit_bytes: storage_limit_bytes,
            });
        }

        let simd_backend = match profile {
            LoudnessProfile::NativeEbu2023 => simd::Backend::production(channels),
            LoudnessProfile::Libebur128126 => simd::Backend::scalar(),
        };

        Ok(Self {
            sample_rate_hz,
            channels,
            roles: owned_roles,
            weights,
            profile,
            metric_demand,
            simd_backend,
            coefficients,
            filters,
            ring,
            ring_frames,
            write_frame: 0,
            clock,
            frames_seen: 0,
            real_frames: 0,
            integrated_energies: Vec::new(),
            lra_energies: Vec::new(),
            storage_limit_bytes,
            fixed_storage_bytes,
            history_storage_bytes: 0,
            failed: false,
        })
    }

    /// Force the private SIMD backend for the qualification timing harness.
    ///
    /// Not part of the supported API: production selection stays with
    /// `Backend::production()`. Fails when the host lacks the requested ISA
    /// or when the meter has already consumed samples.
    #[doc(hidden)]
    pub fn force_simd_backend_for_qualification(
        &mut self,
        backend: LoudnessSimdBackend,
    ) -> Result<(), LoudnessError> {
        if self.frames_seen != 0 || self.failed {
            return Err(LoudnessError::MeterFailed);
        }
        let selected = match backend {
            LoudnessSimdBackend::Scalar => Some(simd::Backend::scalar()),
            #[cfg(target_arch = "x86_64")]
            LoudnessSimdBackend::Sse2 => simd::Backend::sse2_if_available(),
            #[cfg(target_arch = "x86_64")]
            LoudnessSimdBackend::Avx => simd::Backend::avx_if_available(),
            #[cfg(not(target_arch = "x86_64"))]
            LoudnessSimdBackend::Sse2 | LoudnessSimdBackend::Avx => None,
        };
        self.simd_backend = selected.ok_or(LoudnessError::MeterFailed)?;
        Ok(())
    }

    #[must_use]
    pub const fn profile(&self) -> LoudnessProfile {
        self.profile
    }

    #[must_use]
    pub const fn metric_demand(&self) -> LoudnessMetricDemand {
        self.metric_demand
    }

    #[must_use]
    pub const fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    #[must_use]
    pub fn roles(&self) -> &[ChannelRole] {
        &self.roles
    }

    #[must_use]
    pub const fn real_frames(&self) -> u64 {
        self.real_frames
    }

    #[must_use]
    pub fn retained_storage_bytes(&self) -> usize {
        self.fixed_storage_bytes.saturating_add(self.history_storage_bytes)
    }

    /// Validate an entire push before changing DSP state, then process complete
    /// interleaved frames.  Empty pushes are no-ops.
    pub fn push_interleaved(&mut self, samples: &[f64]) -> Result<(), LoudnessError> {
        if self.failed {
            return Err(LoudnessError::MeterFailed);
        }
        if samples.len() % self.channels != 0 {
            return Err(LoudnessError::IncompleteFrame {
                samples: samples.len(),
                channels: self.channels,
            });
        }
        if let Some((sample_index, _)) = samples
            .iter()
            .enumerate()
            .find(|(_, sample)| !sample.is_finite())
        {
            return Err(LoudnessError::NonFiniteSample { sample_index });
        }
        if samples.is_empty() {
            return Ok(());
        }

        let new_frames = u64::try_from(samples.len() / self.channels)
            .map_err(|_| LoudnessError::InputTooLong)?;
        let target = self
            .frames_seen
            .checked_add(new_frames)
            .ok_or(LoudnessError::InputTooLong)?;
        let include_lra = self.metric_demand.includes_range();
        self.preflight_history(target, true, include_lra)?;

        if self.simd_backend.is_scalar() {
            for frame in samples.chunks_exact(self.channels) {
                if let Err(error) = self.process_frame_scalar(frame, true, include_lra) {
                    self.failed = true;
                    return Err(error);
                }
            }
        } else {
            for frame in samples.chunks_exact(self.channels) {
                if let Err(error) = self.process_frame_simd(frame, true, include_lra) {
                    self.failed = true;
                    return Err(error);
                }
            }
        }
        self.real_frames = self
            .real_frames
            .checked_add(new_frames)
            .ok_or_else(|| {
                self.failed = true;
                LoudnessError::InputTooLong
            })?;
        Ok(())
    }

    /// Consume the meter and freeze both the summary and mergeable statistics.
    pub fn finalize(mut self) -> Result<LoudnessMeasurement, LoudnessError> {
        if self.failed {
            return Err(LoudnessError::MeterFailed);
        }
        let real_frames = self.real_frames;
        if self.profile == LoudnessProfile::NativeEbu2023
            && self.metric_demand.includes_range()
            && real_frames > 0
        {
            let continuation_frames = u64::from(self.sample_rate_hz)
                .checked_mul(3)
                .and_then(|value| value.checked_add(1))
                .ok_or(LoudnessError::InputTooLong)?
                / 2;
            let target = self
                .frames_seen
                .checked_add(continuation_frames)
                .ok_or(LoudnessError::InputTooLong)?;
            self.preflight_history(target, false, true)?;
            let mut zero_frame = Vec::new();
            zero_frame
                .try_reserve_exact(self.channels)
                .map_err(|_| LoudnessError::AllocationFailed)?;
            zero_frame.resize(self.channels, 0.0);
            if self.simd_backend.is_scalar() {
                for _ in 0..continuation_frames {
                    self.process_frame_scalar(&zero_frame, false, true)?;
                }
            } else {
                for _ in 0..continuation_frames {
                    self.process_frame_simd(&zero_frame, false, true)?;
                }
            }
        }

        if self.metric_demand.includes_range() {
            sort_lra_energies(&mut self.lra_energies);
        }
        let integrated = integrated_from_absolute_energies(
            &self.integrated_energies,
            self.profile,
            real_frames,
            self.clock.integrated_window_frames(),
        );
        let metric_coverage = LoudnessMetricCoverage::from(self.metric_demand);
        let range = if self.metric_demand.includes_range() {
            lra_from_sorted_absolute_energies(&self.lra_energies, self.profile)
        } else {
            LoudnessRange::NotRequested
        };
        let summary = LoudnessSummary {
            sample_rate_hz: self.sample_rate_hz,
            roles: self.roles.clone(),
            profile: self.profile,
            metric_coverage,
            real_frames,
            integrated,
            range,
            absolute_integrated_observations: self.integrated_energies.len(),
            absolute_lra_observations: self.lra_energies.len(),
            retained_storage_bytes: self.retained_storage_bytes(),
        };
        let statistics = LoudnessStatistics {
            profile: self.profile,
            metric_coverage,
            integrated_energies: self.integrated_energies,
            lra_energies: self.lra_energies,
            real_frames,
        };
        Ok(LoudnessMeasurement { summary, statistics })
    }

    fn preflight_history(
        &mut self,
        target_frames: u64,
        include_integrated: bool,
        include_lra: bool,
    ) -> Result<(), LoudnessError> {
        let (integrated, lra) = self
            .clock
            .events_due_through(target_frames, include_integrated, include_lra)?;
        let capacities_before = (
            self.integrated_energies.capacity(),
            self.lra_energies.capacity(),
        );
        let result = (|| {
            reserve_history_vector(
                &mut self.integrated_energies,
                integrated,
                self.fixed_storage_bytes,
                &mut self.history_storage_bytes,
                self.storage_limit_bytes,
            )?;
            reserve_history_vector(
                &mut self.lra_energies,
                lra,
                self.fixed_storage_bytes,
                &mut self.history_storage_bytes,
                self.storage_limit_bytes,
            )?;
            Ok(())
        })();
        if result.is_err()
            && capacities_before
                != (
                    self.integrated_energies.capacity(),
                    self.lra_energies.capacity(),
                )
        {
            // Allocation is the only preflight operation that can mutate the
            // meter.  If one reserve succeeded before a later admission error,
            // make the meter terminal rather than exposing a half-admitted
            // allocator state as reusable.  Pure arithmetic/limit rejection
            // remains retryable because no DSP or allocator state changed.
            self.failed = true;
        }
        result
    }

    fn process_frame_scalar(
        &mut self,
        frame: &[f64],
        emit_integrated: bool,
        emit_lra: bool,
    ) -> Result<(), LoudnessError> {
        debug_assert_eq!(frame.len(), self.channels);
        let base = self
            .write_frame
            .checked_mul(self.channels)
            .ok_or(LoudnessError::InputTooLong)?;
        // This remains the authority graph and portable fallback.
        for channel in 0..self.channels {
            let filtered = self.filters[channel].process(frame[channel], self.coefficients);
            if !filtered.is_finite() {
                return Err(LoudnessError::NumericalRange);
            }
            self.ring[base + channel] = filtered;
        }
        self.finish_frame(emit_integrated, emit_lra)
    }

    fn process_frame_simd(
        &mut self,
        frame: &[f64],
        emit_integrated: bool,
        emit_lra: bool,
    ) -> Result<(), LoudnessError> {
        debug_assert_eq!(frame.len(), self.channels);
        debug_assert!(!self.simd_backend.is_scalar());
        let base = self
            .write_frame
            .checked_mul(self.channels)
            .ok_or(LoudnessError::InputTooLong)?;
        let channels = self.channels;
        let backend = self.simd_backend;
        let coefficients = self.coefficients;
        simd::filter_frame(
            backend,
            frame,
            &mut self.filters,
            coefficients,
            &mut self.ring[base..base + channels],
        );
        if self.ring[base..base + channels]
            .iter()
            .any(|filtered| !filtered.is_finite())
        {
            return Err(LoudnessError::NumericalRange);
        }
        self.finish_frame(emit_integrated, emit_lra)
    }

    fn finish_frame(
        &mut self,
        emit_integrated: bool,
        emit_lra: bool,
    ) -> Result<(), LoudnessError> {
        self.write_frame += 1;
        if self.write_frame == self.ring_frames {
            self.write_frame = 0;
        }
        self.frames_seen = self
            .frames_seen
            .checked_add(1)
            .ok_or(LoudnessError::InputTooLong)?;

        if emit_integrated && self.clock.take_integrated_if_due(self.frames_seen)? {
            let energy = self.window_energy(self.clock.integrated_window_frames())?;
            if passes_integrated_gate(energy, ABSOLUTE_GATE_ENERGY, self.profile) {
                self.integrated_energies.push(energy);
            }
        }
        if emit_lra && self.clock.take_lra_if_due(self.frames_seen)? {
            let energy = self.window_energy(self.clock.lra_window_frames())?;
            // EBU Tech 3342 and libebur128 use an inclusive absolute LRA gate.
            if energy >= ABSOLUTE_GATE_ENERGY {
                self.lra_energies.push(energy);
            }
        }
        Ok(())
    }

    fn window_energy(&self, window_frames: u64) -> Result<f64, LoudnessError> {
        if self.simd_backend.is_scalar() {
            return self.window_energy_scalar(window_frames);
        }

        let window_frames = usize::try_from(window_frames).map_err(|_| LoudnessError::InputTooLong)?;
        if window_frames == 0 || window_frames > self.ring_frames {
            return Err(LoudnessError::InputTooLong);
        }
        let start_frame = if self.write_frame >= window_frames {
            self.write_frame - window_frames
        } else {
            self.ring_frames - (window_frames - self.write_frame)
        };
        let first_frames = window_frames.min(self.ring_frames - start_frame);
        let second_frames = window_frames - first_frames;

        let mut spans = [(0_usize, 0_usize); 2];
        let span_count;
        if self.profile == LoudnessProfile::Libebur128126 && second_frames > 0 {
            spans[0] = (0, second_frames);
            spans[1] = (start_frame, first_frames);
            span_count = 2;
        } else {
            spans[0] = (start_frame, first_frames);
            if second_frames > 0 {
                spans[1] = (0, second_frames);
                span_count = 2;
            } else {
                span_count = 1;
            }
        }

        let mut channel_sums = [0.0_f64; MAX_CHANNELS];
        simd::window_channel_sums(
            self.simd_backend,
            &self.ring,
            self.channels,
            &self.weights,
            &spans[..span_count],
            &mut channel_sums,
        );

        let mut total = 0.0;
        for channel in 0..self.channels {
            if self.weights[channel] == 0.0 {
                continue;
            }
            total += self.weights[channel] * channel_sums[channel];
        }
        let energy = total / window_frames as f64;
        if energy.is_finite() && energy >= 0.0 {
            Ok(energy)
        } else {
            Err(LoudnessError::NumericalRange)
        }
    }

    fn window_energy_scalar(&self, window_frames: u64) -> Result<f64, LoudnessError> {
        let window_frames = usize::try_from(window_frames).map_err(|_| LoudnessError::InputTooLong)?;
        if window_frames == 0 || window_frames > self.ring_frames {
            return Err(LoudnessError::InputTooLong);
        }
        let start_frame = if self.write_frame >= window_frames {
            self.write_frame - window_frames
        } else {
            self.ring_frames - (window_frames - self.write_frame)
        };
        let first_frames = window_frames.min(self.ring_frames - start_frame);
        let second_frames = window_frames - first_frames;

        let mut total = 0.0;
        for channel in 0..self.channels {
            if self.weights[channel] == 0.0 {
                continue;
            }
            let mut channel_sum = 0.0;
            let first_start = start_frame * self.channels + channel;
            let first_end = (start_frame + first_frames) * self.channels;
            let second_end = second_frames * self.channels;

            if self.profile == LoudnessProfile::Libebur128126 && second_frames > 0 {
                // libebur128's wrapped gating block deliberately traverses the
                // physical ring from frame 0 to audio_data_index first, then
                // the tail at the end of the ring. That is not chronological
                // order. Preserve it because the summation order is observable
                // at binary64 rounding boundaries.
                for sample in self.ring[channel..second_end]
                    .iter()
                    .step_by(self.channels)
                {
                    channel_sum += *sample * *sample;
                }
                for sample in self.ring[first_start..first_end]
                    .iter()
                    .step_by(self.channels)
                {
                    channel_sum += *sample * *sample;
                }
            } else {
                for sample in self.ring[first_start..first_end]
                    .iter()
                    .step_by(self.channels)
                {
                    channel_sum += *sample * *sample;
                }
                if second_frames > 0 {
                    for sample in self.ring[channel..second_end]
                        .iter()
                        .step_by(self.channels)
                    {
                        channel_sum += *sample * *sample;
                    }
                }
            }
            total += self.weights[channel] * channel_sum;
        }
        let energy = total / window_frames as f64;
        if energy.is_finite() && energy >= 0.0 {
            Ok(energy)
        } else {
            Err(LoudnessError::NumericalRange)
        }
    }
}

/// Builder for album loudness reduction: each track is independently windowed
/// and filtered, then the demanded retained observations are pooled in declared
/// manifest order. PCM is never concatenated.
#[derive(Debug)]
pub struct AlbumLoudnessBuilder {
    profile: Option<LoudnessProfile>,
    metric_demand: LoudnessMetricDemand,
    tracks: usize,
    real_frames: u64,
    integrated_energies: Vec<f64>,
    lra_energies: Vec<f64>,
    reporting_peak_linear: f64,
    storage_limit_bytes: usize,
    storage_bytes: usize,
    failed: bool,
}

impl AlbumLoudnessBuilder {
    #[must_use]
    pub fn new(storage_limit_bytes: usize) -> Self {
        Self::with_metric_demand(
            storage_limit_bytes,
            LoudnessMetricDemand::IntegratedAndRange,
        )
    }

    #[must_use]
    pub fn with_metric_demand(
        storage_limit_bytes: usize,
        metric_demand: LoudnessMetricDemand,
    ) -> Self {
        Self {
            profile: None,
            metric_demand,
            tracks: 0,
            real_frames: 0,
            integrated_energies: Vec::new(),
            lra_energies: Vec::new(),
            reporting_peak_linear: 0.0,
            storage_limit_bytes,
            storage_bytes: 0,
            failed: false,
        }
    }

    #[must_use]
    pub const fn metric_demand(&self) -> LoudnessMetricDemand {
        self.metric_demand
    }

    pub fn push_track(
        &mut self,
        mut statistics: LoudnessStatistics,
        reporting_peak_linear: f64,
    ) -> Result<(), LoudnessError> {
        if self.failed {
            return Err(LoudnessError::MeterFailed);
        }
        if !reporting_peak_linear.is_finite() || reporting_peak_linear < 0.0 {
            return Err(LoudnessError::InvalidReportingPeak);
        }
        match self.profile {
            Some(profile) if profile != statistics.profile => {
                return Err(LoudnessError::IncompatibleProfile)
            }
            _ => {}
        }
        if !statistics.metric_coverage.satisfies(self.metric_demand) {
            return Err(LoudnessError::IncompleteMetricCoverage {
                required: self.metric_demand,
                available: statistics.metric_coverage,
            });
        }

        let next_real_frames = self
            .real_frames
            .checked_add(statistics.real_frames)
            .ok_or(LoudnessError::InputTooLong)?;
        let next_tracks = self
            .tracks
            .checked_add(1)
            .ok_or(LoudnessError::InputTooLong)?;
        let needed_integrated = self
            .integrated_energies
            .len()
            .checked_add(statistics.integrated_energies.len())
            .ok_or(LoudnessError::InputTooLong)?;
        let incoming_lra_len = if self.metric_demand.includes_range() {
            statistics.lra_energies.len()
        } else {
            0
        };
        let needed_lra = self
            .lra_energies
            .len()
            .checked_add(incoming_lra_len)
            .ok_or(LoudnessError::InputTooLong)?;
        let needed_bytes = needed_integrated
            .checked_add(needed_lra)
            .and_then(|count| count.checked_mul(std::mem::size_of::<f64>()))
            .ok_or(LoudnessError::InputTooLong)?;
        if needed_bytes > self.storage_limit_bytes {
            return Err(LoudnessError::ResourceLimit {
                required_bytes: needed_bytes,
                limit_bytes: self.storage_limit_bytes,
            });
        }


        // The first participant already owns exactly the buffers the album
        // reducer needs. Reuse them only when every consumed buffer is tight;
        // retaining spare capacity here could make a later track fail where the
        // established compact-copy path would still fit.
        let can_adopt_first = self.tracks == 0
            && self.integrated_energies.is_empty()
            && self.lra_energies.is_empty()
            && statistics.integrated_energies.capacity()
                == statistics.integrated_energies.len()
            && (!self.metric_demand.includes_range()
                || statistics.lra_energies.capacity() == statistics.lra_energies.len());
        if can_adopt_first {
            let adopted_bytes = statistics
                .integrated_energies
                .capacity()
                .checked_add(if self.metric_demand.includes_range() {
                    statistics.lra_energies.capacity()
                } else {
                    0
                })
                .and_then(|count| count.checked_mul(std::mem::size_of::<f64>()))
                .ok_or(LoudnessError::InputTooLong)?;
            if adopted_bytes <= self.storage_limit_bytes {
                if self.profile.is_none() {
                    self.profile = Some(statistics.profile);
                }
                self.real_frames = next_real_frames;
                self.reporting_peak_linear =
                    self.reporting_peak_linear.max(reporting_peak_linear);
                self.integrated_energies = statistics.integrated_energies;
                if self.metric_demand.includes_range() {
                    self.lra_energies = statistics.lra_energies;
                }
                self.tracks = next_tracks;
                self.storage_bytes = adopted_bytes;
                return Ok(());
            }
        }

        let capacities_before = (
            self.integrated_energies.capacity(),
            self.lra_energies.capacity(),
        );
        let reserve_result = (|| {
            self.integrated_energies
                .try_reserve_exact(statistics.integrated_energies.len())
                .map_err(|_| LoudnessError::AllocationFailed)?;
            if self.metric_demand.includes_range() {
                self.lra_energies
                    .try_reserve_exact(statistics.lra_energies.len())
                    .map_err(|_| LoudnessError::AllocationFailed)?;
            }
            Ok::<(), LoudnessError>(())
        })();
        if let Err(error) = reserve_result {
            if capacities_before
                != (
                    self.integrated_energies.capacity(),
                    self.lra_energies.capacity(),
                )
            {
                self.failed = true;
            }
            return Err(error);
        }

        let actual_storage_bytes = self
            .integrated_energies
            .capacity()
            .checked_add(self.lra_energies.capacity())
            .and_then(|count| count.checked_mul(std::mem::size_of::<f64>()))
            .ok_or_else(|| {
                self.failed = true;
                LoudnessError::InputTooLong
            })?;
        if actual_storage_bytes > self.storage_limit_bytes {
            // Vec is permitted to allocate more than reserve_exact requests.
            // Once that happens the builder's physical resource state changed,
            // so reject the track and make the builder terminal.
            self.failed = true;
            self.storage_bytes = actual_storage_bytes;
            return Err(LoudnessError::ResourceLimit {
                required_bytes: actual_storage_bytes,
                limit_bytes: self.storage_limit_bytes,
            });
        }

        // All fallible admission work is complete. Commit logical album state
        // only after the track is known to fit, so an error never creates a
        // partially-added track.
        if self.profile.is_none() {
            self.profile = Some(statistics.profile);
        }
        self.real_frames = next_real_frames;
        self.reporting_peak_linear = self.reporting_peak_linear.max(reporting_peak_linear);
        self.integrated_energies
            .append(&mut statistics.integrated_energies);
        if self.metric_demand.includes_range() {
            self.lra_energies.append(&mut statistics.lra_energies);
        }
        self.tracks = next_tracks;
        self.storage_bytes = actual_storage_bytes;
        Ok(())
    }

    pub fn finalize(mut self) -> Result<AlbumLoudnessSummary, LoudnessError> {
        if self.failed {
            return Err(LoudnessError::MeterFailed);
        }
        let profile = self.profile.ok_or(LoudnessError::EmptyAlbum)?;
        let integrated = if self.integrated_energies.is_empty() {
            if self.real_frames == 0 {
                IntegratedLoudness::NoInput
            } else {
                IntegratedLoudness::NoEligibleBlocks
            }
        } else {
            // For an album, every stored block came from a complete independent
            // track window.  Passing 0 here suppresses a meaningless single-file
            // minimum-length classification while preserving the same gates.
            integrated_from_absolute_energies(
                &self.integrated_energies,
                profile,
                self.real_frames.max(1),
                0,
            )
        };
        let metric_coverage = LoudnessMetricCoverage::from(self.metric_demand);
        let range = if self.metric_demand.includes_range() {
            sort_lra_energies(&mut self.lra_energies);
            lra_from_sorted_absolute_energies(&self.lra_energies, profile)
        } else {
            LoudnessRange::NotRequested
        };
        Ok(AlbumLoudnessSummary {
            profile,
            metric_coverage,
            track_count: self.tracks,
            integrated,
            range,
            reporting_peak_linear: self.reporting_peak_linear,
            retained_storage_bytes: self.storage_bytes,
        })
    }
}

fn validate_roles(roles: &[ChannelRole], profile: LoudnessProfile) -> Result<(), LoudnessError> {
    if profile == LoudnessProfile::NativeEbu2023 {
        if roles.contains(&ChannelRole::Unused) {
            return Err(LoudnessError::InvalidChannelLayout(
                "native loudness layouts may not contain compatibility-only unused channels".to_string(),
            ));
        }
        for (index, role) in roles.iter().enumerate() {
            if roles[..index].contains(role) {
                return Err(LoudnessError::InvalidChannelLayout(format!(
                    "native loudness layout repeats channel role {role:?}"
                )));
            }
        }
        if roles.len() > 1 && roles.contains(&ChannelRole::Mono) {
            return Err(LoudnessError::InvalidChannelLayout(
                "mono is a single-channel role and may not appear in a multichannel native layout"
                    .to_string(),
            ));
        }
    }
    let audible = roles
        .iter()
        .copied()
        .any(|role| role.energy_weight() > 0.0);
    if !audible {
        return Err(LoudnessError::InvalidChannelLayout(
            "loudness layout contains no weighted programme channels".to_string(),
        ));
    }
    Ok(())
}

fn legacy_default_roles(channels: usize) -> Result<Vec<ChannelRole>, LoudnessError> {
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(LoudnessError::InvalidChannelCount);
    }
    let mut roles = match channels {
        1 => vec![ChannelRole::Mono],
        2 => vec![ChannelRole::Left, ChannelRole::Right],
        3 => vec![ChannelRole::Left, ChannelRole::Right, ChannelRole::Center],
        4 => vec![
            ChannelRole::Left,
            ChannelRole::Right,
            ChannelRole::LeftSurround,
            ChannelRole::RightSurround,
        ],
        5 => vec![
            ChannelRole::Left,
            ChannelRole::Right,
            ChannelRole::Center,
            ChannelRole::LeftSurround,
            ChannelRole::RightSurround,
        ],
        _ => {
            let mut mapped = Vec::new();
            mapped
                .try_reserve_exact(channels)
                .map_err(|_| LoudnessError::AllocationFailed)?;
            mapped.extend([
                ChannelRole::Left,
                ChannelRole::Right,
                ChannelRole::Center,
                ChannelRole::Unused,
                ChannelRole::LeftSurround,
                ChannelRole::RightSurround,
            ]);
            mapped.resize(channels, ChannelRole::Unused);
            mapped
        }
    };
    // `match` arms above already have the exact requested length, except the
    // >=6 compatibility branch which explicitly resizes.
    roles.truncate(channels);
    Ok(roles)
}

fn reserve_history_vector(
    vector: &mut Vec<f64>,
    additional_items: usize,
    fixed_storage_bytes: usize,
    history_storage_bytes: &mut usize,
    limit_bytes: usize,
) -> Result<(), LoudnessError> {
    if additional_items == 0 {
        return Ok(());
    }
    let required_len = vector
        .len()
        .checked_add(additional_items)
        .ok_or(LoudnessError::InputTooLong)?;
    if required_len <= vector.capacity() {
        return Ok(());
    }
    let required_capacity_growth = required_len - vector.capacity();
    let additional_bytes = required_capacity_growth
        .checked_mul(std::mem::size_of::<f64>())
        .ok_or(LoudnessError::InputTooLong)?;
    let requested_total = fixed_storage_bytes
        .checked_add(*history_storage_bytes)
        .and_then(|value| value.checked_add(additional_bytes))
        .ok_or(LoudnessError::InputTooLong)?;
    if requested_total > limit_bytes {
        return Err(LoudnessError::ResourceLimit {
            required_bytes: requested_total,
            limit_bytes,
        });
    }
    let old_capacity = vector.capacity();
    // Vec::try_reserve_exact takes an amount relative to *len*, not capacity.
    // Reserve enough for the complete post-push length even when the vector
    // currently has unused capacity.
    vector
        .try_reserve_exact(required_len - vector.len())
        .map_err(|_| LoudnessError::AllocationFailed)?;
    let actual_added = vector.capacity().saturating_sub(old_capacity);
    let actual_bytes = actual_added
        .checked_mul(std::mem::size_of::<f64>())
        .ok_or(LoudnessError::InputTooLong)?;
    *history_storage_bytes = (*history_storage_bytes)
        .checked_add(actual_bytes)
        .ok_or(LoudnessError::InputTooLong)?;
    let actual_total = fixed_storage_bytes
        .checked_add(*history_storage_bytes)
        .ok_or(LoudnessError::InputTooLong)?;
    if actual_total > limit_bytes {
        return Err(LoudnessError::ResourceLimit {
            required_bytes: actual_total,
            limit_bytes,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const S16_TEST_SCALE: f64 = 1.0 / 32_768.0;

    fn tone(rate: u32, frames: usize, amplitude: f64, start: usize) -> Vec<f64> {
        let mut pcm = Vec::with_capacity(frames * 2);
        for n in start..start + frames {
            let value = amplitude
                * (2.0 * std::f64::consts::PI * 997.0 * n as f64 / f64::from(rate)).sin();
            pcm.extend([value, value]);
        }
        pcm
    }

    #[test]
    fn libebur128_first_integrated_boundary_matches_reference_probe() {
        let mut short = LoudnessMeter::libebur128_126_default_layout(48_000, 2).unwrap();
        short.push_interleaved(&tone(48_000, 19_199, 0.1, 0)).unwrap();
        let short = short.finalize().unwrap();
        assert!(matches!(
            short.summary.integrated,
            IntegratedLoudness::InsufficientFrames { .. }
        ));

        let mut exact = LoudnessMeter::libebur128_126_default_layout(48_000, 2).unwrap();
        exact.push_interleaved(&tone(48_000, 19_200, 0.1, 0)).unwrap();
        let exact = exact.finalize().unwrap();
        let actual = exact.summary.integrated.finite_lufs().unwrap();
        let expected = -19.99903532982239_f64;
        assert!((actual - expected).abs() <= 2.0e-13, "{actual:?} != {expected:?}");
    }

    #[test]
    fn libebur128_wrapped_ring_matches_reference_operation_order() {
        const RATE: u32 = 48_000;
        const FRAMES: usize = 5 * RATE as usize + 1_234;
        let mut state = 0x1234_5678_u32;
        let mut pcm = Vec::with_capacity(FRAMES * 2);
        for n in 0..FRAMES {
            state = state
                .wrapping_mul(1_664_525)
                .wrapping_add(1_013_904_223);
            let mut sample = ((state >> 16) & 0xffff) as i32 - 32_768;
            if n < RATE as usize {
                sample /= 16;
            } else if n < 3 * RATE as usize {
                sample /= 4;
            }
            let right = if n % 7 == 0 { sample } else { -sample / 2 };
            pcm.extend([f64::from(sample) * S16_TEST_SCALE, f64::from(right) * S16_TEST_SCALE]);
        }

        let mut meter = LoudnessMeter::libebur128_126_default_layout(RATE, 2).unwrap();
        meter.push_interleaved(&pcm).unwrap();
        let result = meter.finalize().unwrap().summary;
        let integrated = result.integrated.finite_lufs().unwrap();
        let lra = result.range.finite_lu().unwrap();
        assert_eq!(integrated, -3.135_889_382_593_765_f64);
        assert_eq!(lra, 12.034_492_096_604_762_f64);
    }

    #[test]
    fn native_eof_continuation_does_not_change_real_frame_count_or_integrated_windows() {
        let pcm = tone(48_000, 4 * 48_000, 0.1, 0);
        let mut meter = LoudnessMeter::new(48_000, 2).unwrap();
        meter.push_interleaved(&pcm).unwrap();
        let result = meter.finalize().unwrap();
        assert_eq!(result.summary.real_frames, 4 * 48_000);
        assert_eq!(result.summary.integrated, integrated_from_absolute_energies(
            &result.statistics.integrated_energies,
            LoudnessProfile::NativeEbu2023,
            4 * 48_000,
            19_200,
        ));
        assert!(result.summary.absolute_lra_observations > 0);
    }

    #[test]
    fn constructors_reject_unqualified_rate_and_channel_geometry() {
        assert!(matches!(
            LoudnessMeter::libebur128_126_default_layout(15, 2),
            Err(LoudnessError::UnsupportedSampleRate { sample_rate_hz: 15 })
        ));
        assert!(matches!(
            LoudnessMeter::libebur128_126_default_layout(2_822_401, 2),
            Err(LoudnessError::UnsupportedSampleRate { sample_rate_hz: 2_822_401 })
        ));
        assert!(matches!(
            LoudnessMeter::libebur128_126_default_layout(48_000, 65),
            Err(LoudnessError::InvalidChannelCount)
        ));
    }

    #[test]
    fn native_role_validation_rejects_duplicate_or_multichannel_mono_roles() {
        assert!(matches!(
            LoudnessMeter::with_roles(
                48_000,
                &[ChannelRole::Left, ChannelRole::Left],
                LoudnessProfile::NativeEbu2023,
            ),
            Err(LoudnessError::InvalidChannelLayout(_))
        ));
        assert!(matches!(
            LoudnessMeter::with_roles(
                48_000,
                &[ChannelRole::Mono, ChannelRole::Right],
                LoudnessProfile::NativeEbu2023,
            ),
            Err(LoudnessError::InvalidChannelLayout(_))
        ));
    }

    #[test]
    fn resource_rejection_before_growth_is_retryable() {
        let mut meter = LoudnessMeter::with_roles_and_limit(
            48_000,
            &[ChannelRole::Left, ChannelRole::Right],
            LoudnessProfile::Libebur128126,
            usize::MAX,
        )
        .unwrap();
        // Pin the test to the allocator's actual retained fixed capacity rather
        // than assuming `try_reserve_exact` chooses a particular capacity.
        meter.storage_limit_bytes = meter.fixed_storage_bytes;
        let block = tone(48_000, 19_200, 0.1, 0);
        assert!(matches!(
            meter.push_interleaved(&block),
            Err(LoudnessError::ResourceLimit { .. })
        ));
        // The limit failure happened before any history allocation or DSP
        // mutation, so an empty retry remains a valid no-op rather than a
        // terminal-meter error.
        assert_eq!(meter.real_frames(), 0);
        assert!(meter.push_interleaved(&[]).is_ok());
    }

    #[test]
    fn pushes_are_chunk_invariant() {
        let pcm = [
            tone(48_000, 144_000, 0.01, 0),
            tone(48_000, 64_800, 0.2, 144_000),
        ]
        .concat();
        let mut whole = LoudnessMeter::libebur128_126_default_layout(48_000, 2).unwrap();
        whole.push_interleaved(&pcm).unwrap();
        let whole = whole.finalize().unwrap();

        let mut split = LoudnessMeter::libebur128_126_default_layout(48_000, 2).unwrap();
        let frame_chunks = [1usize, 31, 4_800, 4_093, 777];
        let mut offset = 0usize;
        let mut index = 0usize;
        while offset < pcm.len() {
            let samples = (frame_chunks[index % frame_chunks.len()] * 2).min(pcm.len() - offset);
            split.push_interleaved(&pcm[offset..offset + samples]).unwrap();
            offset += samples;
            index += 1;
        }
        let split = split.finalize().unwrap();
        assert_eq!(whole.summary.integrated, split.summary.integrated);
        assert_eq!(whole.summary.range, split.summary.range);
        assert_eq!(
            whole.statistics.integrated_energies,
            split.statistics.integrated_energies
        );
        assert_eq!(whole.statistics.lra_energies, split.statistics.lra_energies);
    }

    #[test]
    fn album_does_not_concatenate_short_tracks() {
        let mut album = AlbumLoudnessBuilder::new(8 * 1024 * 1024);
        for start in [0usize, 14_400] {
            let mut meter = LoudnessMeter::libebur128_126_default_layout(48_000, 2).unwrap();
            meter
                .push_interleaved(&tone(48_000, 14_400, 0.1, start))
                .unwrap();
            let measurement = meter.finalize().unwrap();
            album
                .push_track(measurement.statistics, 0.1)
                .expect("album track");
        }
        let album = album.finalize().unwrap();
        assert!(matches!(album.integrated, IntegratedLoudness::NoEligibleBlocks));

        let mut continuous = LoudnessMeter::libebur128_126_default_layout(48_000, 2).unwrap();
        continuous
            .push_interleaved(&tone(48_000, 28_800, 0.1, 0))
            .unwrap();
        assert!(continuous
            .finalize()
            .unwrap()
            .summary
            .integrated
            .finite_lufs()
            .is_some());
    }

    #[test]
    fn legacy_eight_channel_default_excludes_3_6_7_from_loudness() {
        let meter = LoudnessMeter::libebur128_126_default_layout(48_000, 8).unwrap();
        assert_eq!(
            meter.roles(),
            &[
                ChannelRole::Left,
                ChannelRole::Right,
                ChannelRole::Center,
                ChannelRole::Unused,
                ChannelRole::LeftSurround,
                ChannelRole::RightSurround,
                ChannelRole::Unused,
                ChannelRole::Unused,
            ]
        );
    }

    #[test]
    fn rejected_push_does_not_mutate_meter() {
        let mut meter = LoudnessMeter::new(48_000, 2).unwrap();
        assert!(matches!(
            meter.push_interleaved(&[0.0]),
            Err(LoudnessError::IncompleteFrame { .. })
        ));
        assert_eq!(meter.real_frames(), 0);
        assert!(matches!(
            meter.push_interleaved(&[0.0, f64::NAN]),
            Err(LoudnessError::NonFiniteSample { .. })
        ));
        assert_eq!(meter.real_frames(), 0);
    }

    #[test]
    fn history_reservation_crossing_spare_capacity_reaches_required_length() {
        let mut values = Vec::with_capacity(8);
        values.extend([1.0, 2.0, 3.0, 4.0]);
        let fixed = 128usize;
        let mut history = values.capacity() * std::mem::size_of::<f64>();
        let old_capacity = values.capacity();
        reserve_history_vector(
            &mut values,
            8,
            fixed,
            &mut history,
            usize::MAX,
        )
        .unwrap();
        assert!(values.capacity() >= 12);
        assert_eq!(
            history,
            old_capacity * std::mem::size_of::<f64>()
                + (values.capacity() - old_capacity) * std::mem::size_of::<f64>()
        );
    }

    #[test]
    fn storage_limit_is_checked_before_large_ring_allocation() {
        let error = LoudnessMeter::with_roles_and_limit(
            384_000,
            &[
                ChannelRole::Left,
                ChannelRole::Right,
                ChannelRole::Center,
                ChannelRole::Lfe,
                ChannelRole::LeftSurround,
                ChannelRole::RightSurround,
                ChannelRole::LeftBack,
                ChannelRole::RightBack,
            ],
            LoudnessProfile::NativeEbu2023,
            1_000_000,
        )
        .unwrap_err();
        assert!(matches!(error, LoudnessError::ResourceLimit { .. }));
    }
    fn mono_tone(rate: u32, frames: usize, amplitude: f64, start: usize) -> Vec<f64> {
        (start..start + frames)
            .map(|n| amplitude * (2.0 * std::f64::consts::PI * 997.0 * n as f64 / f64::from(rate)).sin())
            .collect()
    }

    fn artificial_statistics(
        profile: LoudnessProfile,
        integrated_energies: &[f64],
        lra_energies: &[f64],
        real_frames: u64,
    ) -> LoudnessStatistics {
        LoudnessStatistics {
            profile,
            metric_coverage: LoudnessMetricCoverage::IntegratedAndRange,
            integrated_energies: integrated_energies.to_vec(),
            lra_energies: lra_energies.to_vec(),
            real_frames,
        }
    }

    #[test]
    fn pcm_validity_matrix_is_fail_closed_without_rejecting_finite_float_headroom() {
        for profile in [LoudnessProfile::NativeEbu2023, LoudnessProfile::Libebur128126] {
            let mut empty = LoudnessMeter::with_roles(48_000, &[ChannelRole::Left, ChannelRole::Right], profile).unwrap();
            empty.push_interleaved(&[]).unwrap();
            let result = empty.finalize().unwrap();
            assert_eq!(result.summary.real_frames, 0);
            assert_eq!(result.summary.integrated, IntegratedLoudness::NoInput);
        }

        let mut partial = LoudnessMeter::new(48_000, 2).unwrap();
        assert_eq!(
            partial.push_interleaved(&[0.0]),
            Err(LoudnessError::IncompleteFrame { samples: 1, channels: 2 }),
        );
        assert_eq!(partial.real_frames(), 0);

        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut nonfinite = LoudnessMeter::new(48_000, 2).unwrap();
            assert_eq!(
                nonfinite.push_interleaved(&[0.0, bad]),
                Err(LoudnessError::NonFiniteSample { sample_index: 1 }),
            );
            assert_eq!(nonfinite.real_frames(), 0);
        }

        let mut headroom = LoudnessMeter::new(48_000, 2).unwrap();
        headroom.push_interleaved(&tone(48_000, 19_200, 2.0, 0)).unwrap();
        assert!(headroom.finalize().unwrap().summary.integrated.finite_lufs().is_some());

        let mut overflow = LoudnessMeter::new(48_000, 2).unwrap();
        assert!(matches!(
            overflow.push_interleaved(&[f64::MAX, f64::MAX]),
            Err(LoudnessError::NumericalRange)
        ));
        assert_eq!(overflow.push_interleaved(&[0.0, 0.0]), Err(LoudnessError::MeterFailed));
    }

    #[test]
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[allow(deprecated)]
    fn loudness_subnormal_stream_is_stable_with_daz_and_ftz_enabled() {
        #[cfg(target_arch = "x86")]
        unsafe fn read_mxcsr() -> u32 { core::arch::x86::_mm_getcsr() }
        #[cfg(target_arch = "x86_64")]
        unsafe fn read_mxcsr() -> u32 { core::arch::x86_64::_mm_getcsr() }
        #[cfg(target_arch = "x86")]
        unsafe fn write_mxcsr(value: u32) { core::arch::x86::_mm_setcsr(value); }
        #[cfg(target_arch = "x86_64")]
        unsafe fn write_mxcsr(value: u32) { core::arch::x86_64::_mm_setcsr(value); }
        std::thread::spawn(|| {
            const DAZ_FTZ: u32 = (1 << 6) | (1 << 15);
            let original = unsafe { read_mxcsr() };
            unsafe { write_mxcsr(original | DAZ_FTZ) };

            let subnormal = f64::from_bits(11);
            let mut meter = LoudnessMeter::new(48_000, 2).unwrap();
            let pcm = vec![subnormal; 19_200 * 2];
            meter.push_interleaved(&pcm).unwrap();
            let result = meter.finalize().unwrap();
            unsafe { write_mxcsr(original) };
            assert_eq!(result.summary.real_frames, 19_200);
            assert_eq!(result.summary.integrated, IntegratedLoudness::BelowAbsoluteGate);
        })
        .join()
        .expect("DAZ/FTZ loudness test thread");
    }

    #[test]
    fn mono_stereo_and_explicit_dual_mono_policy_are_distinct() {
        let mono_pcm = mono_tone(48_000, 48_000, 0.1, 0);
        let mut mono = LoudnessMeter::new(48_000, 1).unwrap();
        mono.push_interleaved(&mono_pcm).unwrap();
        let mono = mono.finalize().unwrap().summary.integrated.finite_lufs().unwrap();

        let stereo_pcm: Vec<f64> = mono_pcm.iter().flat_map(|sample| [*sample, *sample]).collect();
        let mut stereo = LoudnessMeter::new(48_000, 2).unwrap();
        assert_eq!(stereo.roles(), &[ChannelRole::Left, ChannelRole::Right]);
        stereo.push_interleaved(&stereo_pcm).unwrap();
        let stereo = stereo.finalize().unwrap().summary.integrated.finite_lufs().unwrap();

        let expected_delta = 10.0 * 2.0_f64.log10();
        assert!(((stereo - mono) - expected_delta).abs() <= 2.0e-12);
        assert!(matches!(
            LoudnessMeter::with_roles(
                48_000,
                &[ChannelRole::Mono, ChannelRole::Right],
                LoudnessProfile::NativeEbu2023,
            ),
            Err(LoudnessError::InvalidChannelLayout(_))
        ));
    }

    #[test]
    fn isolated_channel_weights_and_lfe_peak_exclusion_are_explicit() {
        let roles = [
            ChannelRole::Left,
            ChannelRole::Right,
            ChannelRole::Center,
            ChannelRole::Lfe,
            ChannelRole::LeftSurround,
            ChannelRole::RightSurround,
        ];
        let frames = 19_200usize;
        let channels = roles.len();
        let mut energies = Vec::new();
        for active in 0..channels {
            let mut pcm = vec![0.0; frames * channels];
            for n in 0..frames {
                pcm[n * channels + active] = 0.1
                    * (2.0 * std::f64::consts::PI * 997.0 * n as f64 / 48_000.0).sin();
            }
            let mut meter = LoudnessMeter::with_roles(48_000, &roles, LoudnessProfile::NativeEbu2023).unwrap();
            meter.push_interleaved(&pcm).unwrap();
            let measurement = meter.finalize().unwrap();
            energies.push(measurement.statistics.integrated_energies.first().copied());
        }

        let left = energies[0].unwrap();
        assert!((energies[1].unwrap() / left - 1.0).abs() <= 2.0e-13);
        assert!((energies[2].unwrap() / left - 1.0).abs() <= 2.0e-13);
        assert!(energies[3].is_none(), "LFE entered loudness energy");
        assert!((energies[4].unwrap() / left - 1.41).abs() <= 2.0e-13);
        assert!((energies[5].unwrap() / left - 1.41).abs() <= 2.0e-13);

        let mut lfe_pcm = vec![0.0; frames * channels];
        for n in 0..frames {
            lfe_pcm[n * channels + 3] = 0.25
                * (2.0 * std::f64::consts::PI * 997.0 * n as f64 / 48_000.0).sin();
        }
        let mut peak = crate::ReportingPeakMeter::new(48_000, channels).unwrap();
        peak.push_interleaved(&lfe_pcm).unwrap();
        let peak = peak.finalize().unwrap();
        assert!(peak.channel_linear_peaks[3] >= 0.24, "excluded loudness channel lost reporting peak");
    }

    #[test]
    fn legacy_default_layout_matrix_covers_four_five_six_and_eight_channels() {
        let fixtures: &[&[ChannelRole]] = &[
            &[ChannelRole::Left, ChannelRole::Right, ChannelRole::LeftSurround, ChannelRole::RightSurround],
            &[ChannelRole::Left, ChannelRole::Right, ChannelRole::Center, ChannelRole::LeftSurround, ChannelRole::RightSurround],
            &[ChannelRole::Left, ChannelRole::Right, ChannelRole::Center, ChannelRole::Unused, ChannelRole::LeftSurround, ChannelRole::RightSurround],
            &[ChannelRole::Left, ChannelRole::Right, ChannelRole::Center, ChannelRole::Unused, ChannelRole::LeftSurround, ChannelRole::RightSurround, ChannelRole::Unused, ChannelRole::Unused],
        ];
        for expected in fixtures {
            let meter = LoudnessMeter::libebur128_126_default_layout(48_000, expected.len()).unwrap();
            assert_eq!(meter.roles(), *expected);
        }
    }

    #[test]
    fn native_measurement_is_stable_when_roles_and_pcm_are_permuted_together() {
        let roles = [
            ChannelRole::Left,
            ChannelRole::Right,
            ChannelRole::Center,
            ChannelRole::Lfe,
            ChannelRole::LeftSurround,
            ChannelRole::RightSurround,
        ];
        let permuted = [
            ChannelRole::RightSurround,
            ChannelRole::LeftSurround,
            ChannelRole::Lfe,
            ChannelRole::Center,
            ChannelRole::Right,
            ChannelRole::Left,
        ];
        fn role_sample(role: ChannelRole, n: usize) -> f64 {
            let (amplitude, frequency) = match role {
                ChannelRole::Left => (0.10, 503.0),
                ChannelRole::Right => (0.08, 701.0),
                ChannelRole::Center => (0.06, 997.0),
                ChannelRole::Lfe => (0.20, 83.0),
                ChannelRole::LeftSurround => (0.04, 1301.0),
                ChannelRole::RightSurround => (0.03, 1601.0),
                _ => unreachable!(),
            };
            amplitude * (2.0 * std::f64::consts::PI * frequency * n as f64 / 48_000.0).sin()
        }
        fn render(roles: &[ChannelRole], frames: usize) -> Vec<f64> {
            let mut pcm = Vec::with_capacity(frames * roles.len());
            for n in 0..frames {
                for role in roles {
                    pcm.push(role_sample(*role, n));
                }
            }
            pcm
        }
        fn measure(roles: &[ChannelRole], pcm: &[f64]) -> LoudnessMeasurement {
            let mut meter = LoudnessMeter::with_roles(48_000, roles, LoudnessProfile::NativeEbu2023).unwrap();
            meter.push_interleaved(pcm).unwrap();
            meter.finalize().unwrap()
        }

        let a = measure(&roles, &render(&roles, 4 * 48_000));
        let b = measure(&permuted, &render(&permuted, 4 * 48_000));
        let a_lufs = a.summary.integrated.finite_lufs().unwrap();
        let b_lufs = b.summary.integrated.finite_lufs().unwrap();
        assert!((a_lufs - b_lufs).abs() <= 2.0e-12, "permutation changed loudness: {a_lufs} vs {b_lufs}");
        assert_eq!(a.statistics.integrated_observations(), b.statistics.integrated_observations());
        assert_eq!(a.statistics.lra_observations(), b.statistics.lra_observations());
    }

    #[test]
    fn streaming_whole_framewise_partitioned_and_repeated_scans_agree() {
        let pcm = tone(48_000, 48_000, 0.125, 0);
        fn measure_with_chunks(pcm: &[f64], chunks: &[usize]) -> LoudnessMeasurement {
            let mut meter = LoudnessMeter::libebur128_126_default_layout(48_000, 2).unwrap();
            let mut offset = 0usize;
            let mut index = 0usize;
            while offset < pcm.len() {
                let requested = chunks[index % chunks.len()] * 2;
                let count = requested.min(pcm.len() - offset);
                meter.push_interleaved(&pcm[offset..offset + count]).unwrap();
                offset += count;
                index += 1;
            }
            meter.finalize().unwrap()
        }

        let whole = measure_with_chunks(&pcm, &[48_000]);
        let framewise = measure_with_chunks(&pcm, &[1]);
        let partitioned = measure_with_chunks(&pcm, &[37, 4_799, 2, 9_601, 113, 7, 2_047]);
        let repeated = measure_with_chunks(&pcm, &[48_000]);
        for other in [&framewise, &partitioned, &repeated] {
            assert_eq!(whole.summary.integrated, other.summary.integrated);
            assert_eq!(whole.summary.range, other.summary.range);
            assert_eq!(whole.statistics.integrated_energies, other.statistics.integrated_energies);
            assert_eq!(whole.statistics.lra_energies, other.statistics.lra_energies);
        }
    }

    #[test]
    fn album_matrix_covers_mixed_rates_levels_silence_gate_and_peak_only_tracks() {
        let mut mixed = AlbumLoudnessBuilder::new(DEFAULT_LOUDNESS_STORAGE_LIMIT_BYTES);
        for (rate, frames, amplitude, peak) in [(44_100, 44_100, 0.05, 0.05), (48_000, 96_000, 0.2, 0.2)] {
            let mut meter = LoudnessMeter::libebur128_126_default_layout(rate, 2).unwrap();
            meter.push_interleaved(&tone(rate, frames, amplitude, 0)).unwrap();
            mixed.push_track(meter.finalize().unwrap().statistics, peak).unwrap();
        }
        let mixed = mixed.finalize().unwrap();
        assert_eq!(mixed.track_count, 2);
        assert!(mixed.integrated.finite_lufs().is_some());
        assert_eq!(mixed.reporting_peak_linear, 0.2);

        let mut silent_plus_active = AlbumLoudnessBuilder::new(DEFAULT_LOUDNESS_STORAGE_LIMIT_BYTES);
        let mut silent = LoudnessMeter::libebur128_126_default_layout(48_000, 2).unwrap();
        silent.push_interleaved(&vec![0.0; 48_000 * 2]).unwrap();
        let silent = silent.finalize().unwrap();
        assert!(silent.summary.integrated.finite_lufs().is_none());
        silent_plus_active.push_track(silent.statistics, 0.9).unwrap();
        let mut active = LoudnessMeter::libebur128_126_default_layout(48_000, 2).unwrap();
        active.push_interleaved(&tone(48_000, 48_000, 0.1, 0)).unwrap();
        silent_plus_active.push_track(active.finalize().unwrap().statistics, 0.1).unwrap();
        let album = silent_plus_active.finalize().unwrap();
        assert!(album.integrated.finite_lufs().is_some());
        assert_eq!(album.reporting_peak_linear, 0.9, "peak from loudness-unavailable track was lost");

        let mut cross_track_gate = AlbumLoudnessBuilder::new(DEFAULT_LOUDNESS_STORAGE_LIMIT_BYTES);
        cross_track_gate.push_track(artificial_statistics(LoudnessProfile::Libebur128126, &[1.0], &[], 48_000), 0.1).unwrap();
        cross_track_gate.push_track(artificial_statistics(LoudnessProfile::Libebur128126, &[100.0], &[], 48_000), 0.2).unwrap();
        let album = cross_track_gate.finalize().unwrap();
        let IntegratedLoudness::Finite { absolute_observations, relative_observations, .. } = album.integrated else {
            panic!("cross-track gate should remain finite");
        };
        assert_eq!(absolute_observations, 2);
        assert_eq!(relative_observations, 1, "album relative gate did not re-evaluate pooled track observations");
    }

    #[test]
    fn native_completion_at_exact_one_point_five_seconds_adds_only_lra_history() {
        let frames = 72_000usize;
        let pcm = tone(48_000, frames, 0.1, 0);
        let mut meter = LoudnessMeter::new(48_000, 2).unwrap();
        meter.push_interleaved(&pcm).unwrap();
        let integrated_before = meter.integrated_energies.clone();
        let result = meter.finalize().unwrap();
        assert_eq!(result.summary.real_frames, frames as u64);
        assert_eq!(result.statistics.integrated_energies, integrated_before);
        assert_eq!(result.statistics.lra_observations(), 1, "1.5 s real + 1.5 s virtual tail must hit exactly one 3 s LRA boundary");

        let mut peak = crate::ReportingPeakMeter::new(48_000, 2).unwrap();
        peak.push_interleaved(&pcm).unwrap();
        let peak = peak.finalize().unwrap();
        assert_eq!(peak.frames, frames as u64);
        assert!(peak.channel_linear_peaks.iter().all(|value| *value > 0.0));

        let mut one_short = LoudnessMeter::new(48_000, 2).unwrap();
        one_short.push_interleaved(&tone(48_000, frames - 1, 0.1, 0)).unwrap();
        assert_eq!(one_short.finalize().unwrap().statistics.lra_observations(), 0);
    }

    fn native_meter_with_demand(
        rate: u32,
        roles: &[ChannelRole],
        demand: LoudnessMetricDemand,
    ) -> LoudnessMeter {
        LoudnessMeter::with_roles_and_metric_demand(
            rate,
            roles,
            LoudnessProfile::NativeEbu2023,
            demand,
        )
        .unwrap()
    }

    fn assert_bits_eq(left: &[f64], right: &[f64]) {
        assert_eq!(left.len(), right.len());
        for (index, (left, right)) in left.iter().zip(right).enumerate() {
            assert_eq!(
                left.to_bits(),
                right.to_bits(),
                "binary64 mismatch at index {index}: {left:?} != {right:?}"
            );
        }
    }

    #[test]
    fn perf06_native_integrated_only_preserves_integrated_bits_and_real_extent() {
        const RATE: u32 = 44_101;
        const FRAMES: usize = RATE as usize * 4 + 777;
        let roles = [ChannelRole::Left, ChannelRole::Right, ChannelRole::Lfe];
        let mut pcm = Vec::with_capacity(FRAMES * roles.len());
        for n in 0..FRAMES {
            let phase = 2.0 * std::f64::consts::PI * 997.0 * n as f64 / f64::from(RATE);
            let tiny = if n % 997 == 0 { f64::from_bits(1) } else { 0.0 };
            pcm.extend([
                0.125 * phase.sin() + tiny,
                -0.0625 * (1.7 * phase).cos(),
                if n % 211 == 0 { -0.9 } else { 0.0 },
            ]);
        }

        let mut full = native_meter_with_demand(
            RATE,
            &roles,
            LoudnessMetricDemand::IntegratedAndRange,
        );
        let mut reduced = native_meter_with_demand(
            RATE,
            &roles,
            LoudnessMetricDemand::IntegratedOnly,
        );
        let pattern = [1usize, 37, 4_411, 2, 8_903, 113, 7, 2_047];
        let mut offset_frames = 0usize;
        let mut chunk_index = 0usize;
        while offset_frames < FRAMES {
            let chunk_frames = pattern[chunk_index % pattern.len()];
            let count = chunk_frames.min(FRAMES - offset_frames);
            let start = offset_frames * roles.len();
            let end = (offset_frames + count) * roles.len();
            full.push_interleaved(&pcm[start..end]).unwrap();
            reduced.push_interleaved(&pcm[start..end]).unwrap();
            offset_frames += count;
            chunk_index += 1;
        }

        assert_eq!(full.real_frames(), FRAMES as u64);
        assert_eq!(reduced.real_frames(), FRAMES as u64);
        assert_bits_eq(&full.integrated_energies, &reduced.integrated_energies);
        assert_eq!(reduced.lra_energies.len(), 0);
        assert_eq!(reduced.frames_seen, reduced.real_frames);

        let full = full.finalize().unwrap();
        let reduced = reduced.finalize().unwrap();
        assert_eq!(full.summary.integrated, reduced.summary.integrated);
        assert_bits_eq(
            &full.statistics.integrated_energies,
            &reduced.statistics.integrated_energies,
        );
        assert_eq!(
            reduced.summary.metric_coverage,
            LoudnessMetricCoverage::IntegratedOnly
        );
        assert_eq!(reduced.summary.range, LoudnessRange::NotRequested);
        assert_eq!(reduced.summary.absolute_lra_observations, 0);
        assert_eq!(reduced.statistics.lra_observations(), 0);
    }

    #[test]
    fn perf07_metric_coverage_is_explicit_and_album_admission_is_subset_safe() {
        let compatibility_error = LoudnessMeter::with_roles_and_metric_demand(
            48_000,
            &[ChannelRole::Left, ChannelRole::Right],
            LoudnessProfile::Libebur128126,
            LoudnessMetricDemand::IntegratedOnly,
        )
        .unwrap_err();
        assert!(matches!(
            compatibility_error,
            LoudnessError::UnsupportedMetricDemand { .. }
        ));

        let pcm = tone(48_000, 48_000, 0.1, 0);
        let mut reduced_meter = native_meter_with_demand(
            48_000,
            &[ChannelRole::Left, ChannelRole::Right],
            LoudnessMetricDemand::IntegratedOnly,
        );
        reduced_meter.push_interleaved(&pcm).unwrap();
        let reduced = reduced_meter.finalize().unwrap();

        let mut full_builder = AlbumLoudnessBuilder::new(DEFAULT_LOUDNESS_STORAGE_LIMIT_BYTES);
        assert!(matches!(
            full_builder.push_track(reduced.statistics, 0.75),
            Err(LoudnessError::IncompleteMetricCoverage {
                required: LoudnessMetricDemand::IntegratedAndRange,
                available: LoudnessMetricCoverage::IntegratedOnly,
            })
        ));
        assert_eq!(full_builder.tracks, 0);
        assert!(full_builder.profile.is_none());

        let mut full_meter = LoudnessMeter::new(48_000, 2).unwrap();
        full_meter.push_interleaved(&pcm).unwrap();
        let full = full_meter.finalize().unwrap();
        full_builder.push_track(full.statistics, 0.5).unwrap();
        assert_eq!(full_builder.tracks, 1);

        let mut subset_builder = AlbumLoudnessBuilder::with_metric_demand(
            DEFAULT_LOUDNESS_STORAGE_LIMIT_BYTES,
            LoudnessMetricDemand::IntegratedOnly,
        );
        let short = LoudnessStatistics {
            profile: LoudnessProfile::NativeEbu2023,
            metric_coverage: LoudnessMetricCoverage::IntegratedOnly,
            integrated_energies: Vec::new(),
            lra_energies: Vec::new(),
            real_frames: 100,
        };
        subset_builder.push_track(short, 0.9).unwrap();

        let mut full_meter = LoudnessMeter::new(48_000, 2).unwrap();
        full_meter.push_interleaved(&pcm).unwrap();
        let full = full_meter.finalize().unwrap();
        subset_builder.push_track(full.statistics, 0.25).unwrap();
        let subset = subset_builder.finalize().unwrap();
        assert_eq!(subset.metric_coverage, LoudnessMetricCoverage::IntegratedOnly);
        assert_eq!(subset.range, LoudnessRange::NotRequested);
        assert_eq!(subset.track_count, 2);
        assert_eq!(subset.reporting_peak_linear.to_bits(), 0.9_f64.to_bits());

        // Requested-but-mathematically-unavailable range remains distinct
        // from a deliberately omitted range on the same short programme.
        let short_pcm = tone(48_000, 100, 0.1, 0);
        let mut short_full = LoudnessMeter::new(48_000, 2).unwrap();
        short_full.push_interleaved(&short_pcm).unwrap();
        let short_full = short_full.finalize().unwrap();
        assert_eq!(
            short_full.summary.metric_coverage,
            LoudnessMetricCoverage::IntegratedAndRange
        );
        assert_eq!(
            short_full.summary.range,
            LoudnessRange::Unavailable { observations: 0 }
        );

        let mut short_reduced = native_meter_with_demand(
            48_000,
            &[ChannelRole::Left, ChannelRole::Right],
            LoudnessMetricDemand::IntegratedOnly,
        );
        short_reduced.push_interleaved(&short_pcm).unwrap();
        let short_reduced = short_reduced.finalize().unwrap();
        assert_eq!(short_reduced.summary.range, LoudnessRange::NotRequested);
    }

    #[test]
    fn perf08_integrated_only_ring_and_history_use_the_reduced_resource_geometry() {
        let roles = [ChannelRole::Left, ChannelRole::Right];
        let full = LoudnessMeter::with_roles(
            48_000,
            &roles,
            LoudnessProfile::NativeEbu2023,
        )
        .unwrap();
        let reduced = native_meter_with_demand(
            48_000,
            &roles,
            LoudnessMetricDemand::IntegratedOnly,
        );
        assert_eq!(full.ring_frames, 144_000);
        assert_eq!(reduced.ring_frames, 19_200);
        assert!(reduced.fixed_storage_bytes < full.fixed_storage_bytes);

        let exact_reduced_limit = reduced.fixed_storage_bytes;
        drop(reduced);
        let reduced = LoudnessMeter::with_roles_and_metric_demand_and_limit(
            48_000,
            &roles,
            LoudnessProfile::NativeEbu2023,
            LoudnessMetricDemand::IntegratedOnly,
            exact_reduced_limit,
        )
        .unwrap();
        assert_eq!(reduced.fixed_storage_bytes, exact_reduced_limit);
        assert!(matches!(
            LoudnessMeter::with_roles_and_limit(
                48_000,
                &roles,
                LoudnessProfile::NativeEbu2023,
                exact_reduced_limit,
            ),
            Err(LoudnessError::ResourceLimit { .. })
        ));

        let mut reduced = LoudnessMeter::with_roles_and_metric_demand_and_limit(
            48_000,
            &roles,
            LoudnessProfile::NativeEbu2023,
            LoudnessMetricDemand::IntegratedOnly,
            DEFAULT_LOUDNESS_STORAGE_LIMIT_BYTES,
        )
        .unwrap();
        reduced
            .push_interleaved(&tone(48_000, 5 * 48_000, 0.1, 0))
            .unwrap();
        assert_eq!(reduced.lra_energies.len(), 0);
        assert_eq!(reduced.lra_energies.capacity(), 0);
    }

    #[test]
    fn perf10_first_track_adopts_only_tight_consumed_buffers() {
        let tight_integrated = vec![1.0, 2.0, 3.0].into_boxed_slice().into_vec();
        let tight_lra = vec![4.0, 5.0].into_boxed_slice().into_vec();
        assert_eq!(tight_integrated.capacity(), tight_integrated.len());
        assert_eq!(tight_lra.capacity(), tight_lra.len());
        let integrated_ptr = tight_integrated.as_ptr();
        let lra_ptr = tight_lra.as_ptr();
        let stats = LoudnessStatistics {
            profile: LoudnessProfile::NativeEbu2023,
            metric_coverage: LoudnessMetricCoverage::IntegratedAndRange,
            integrated_energies: tight_integrated,
            lra_energies: tight_lra,
            real_frames: 48_000,
        };
        let mut builder = AlbumLoudnessBuilder::new(5 * std::mem::size_of::<f64>());
        builder.push_track(stats, 0.4).unwrap();
        assert_eq!(builder.integrated_energies.as_ptr(), integrated_ptr);
        assert_eq!(builder.lra_energies.as_ptr(), lra_ptr);
        assert_eq!(builder.storage_bytes, 5 * std::mem::size_of::<f64>());

        // Admission failures happen before ownership transfer can alter the
        // logical album. Exact allowance succeeds above; one f64 less and an
        // invalid peak both leave a fresh builder empty and reusable.
        let insufficient = LoudnessStatistics {
            profile: LoudnessProfile::NativeEbu2023,
            metric_coverage: LoudnessMetricCoverage::IntegratedAndRange,
            integrated_energies: vec![1.0, 2.0, 3.0].into_boxed_slice().into_vec(),
            lra_energies: vec![4.0, 5.0].into_boxed_slice().into_vec(),
            real_frames: 48_000,
        };
        let mut too_small = AlbumLoudnessBuilder::new(4 * std::mem::size_of::<f64>());
        assert!(matches!(
            too_small.push_track(insufficient, 0.4),
            Err(LoudnessError::ResourceLimit { .. })
        ));
        assert_eq!(too_small.tracks, 0);
        assert!(too_small.profile.is_none());
        assert_eq!(too_small.storage_bytes, 0);

        let invalid_peak = LoudnessStatistics {
            profile: LoudnessProfile::NativeEbu2023,
            metric_coverage: LoudnessMetricCoverage::IntegratedAndRange,
            integrated_energies: vec![1.0].into_boxed_slice().into_vec(),
            lra_energies: vec![2.0].into_boxed_slice().into_vec(),
            real_frames: 48_000,
        };
        let mut invalid_peak_builder = AlbumLoudnessBuilder::new(16 * std::mem::size_of::<f64>());
        assert_eq!(
            invalid_peak_builder.push_track(invalid_peak, f64::NAN),
            Err(LoudnessError::InvalidReportingPeak)
        );
        assert_eq!(invalid_peak_builder.tracks, 0);
        assert!(invalid_peak_builder.profile.is_none());

        let mut slack = Vec::with_capacity(16);
        slack.extend([1.0, 2.0, 3.0]);
        let incoming_ptr = slack.as_ptr();
        let stats = LoudnessStatistics {
            profile: LoudnessProfile::NativeEbu2023,
            metric_coverage: LoudnessMetricCoverage::IntegratedOnly,
            integrated_energies: slack,
            lra_energies: Vec::new(),
            real_frames: 48_000,
        };
        let mut builder = AlbumLoudnessBuilder::with_metric_demand(
            64 * std::mem::size_of::<f64>(),
            LoudnessMetricDemand::IntegratedOnly,
        );
        builder.push_track(stats, 0.2).unwrap();
        assert_ne!(builder.integrated_energies.as_ptr(), incoming_ptr);
        assert_eq!(builder.integrated_energies.as_slice(), &[1.0, 2.0, 3.0]);
        assert_eq!(
            builder.storage_bytes,
            builder.integrated_energies.capacity() * std::mem::size_of::<f64>()
        );

        let first = LoudnessStatistics {
            profile: LoudnessProfile::NativeEbu2023,
            metric_coverage: LoudnessMetricCoverage::IntegratedAndRange,
            integrated_energies: vec![1.0, 2.0].into_boxed_slice().into_vec(),
            lra_energies: vec![3.0].into_boxed_slice().into_vec(),
            real_frames: 48_000,
        };
        let second = LoudnessStatistics {
            profile: LoudnessProfile::NativeEbu2023,
            metric_coverage: LoudnessMetricCoverage::IntegratedAndRange,
            integrated_energies: vec![4.0].into_boxed_slice().into_vec(),
            lra_energies: vec![5.0].into_boxed_slice().into_vec(),
            real_frames: 48_000,
        };
        let mut builder = AlbumLoudnessBuilder::new(16 * std::mem::size_of::<f64>());
        builder.push_track(first, 0.2).unwrap();
        builder.push_track(second, 0.8).unwrap();
        assert_eq!(builder.integrated_energies.as_slice(), &[1.0, 2.0, 4.0]);
        assert_eq!(builder.lra_energies.as_slice(), &[3.0, 5.0]);
        assert_eq!(builder.tracks, 2);
        assert_eq!(builder.reporting_peak_linear.to_bits(), 0.8_f64.to_bits());
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn simd01_runtime_selection_respects_profile_commissioning_boundary() {
        let sse2_available = std::is_x86_feature_detected!("sse2");
        let native = LoudnessMeter::with_roles(
            48_000,
            &[ChannelRole::Left, ChannelRole::Right],
            LoudnessProfile::NativeEbu2023,
        )
        .unwrap();
        let compatibility_default =
            LoudnessMeter::libebur128_126_default_layout(48_000, 2).unwrap();
        let compatibility_roles = LoudnessMeter::with_roles(
            48_000,
            &[ChannelRole::Left, ChannelRole::Right],
            LoudnessProfile::Libebur128126,
        )
        .unwrap();

        let native_mono = LoudnessMeter::with_roles(
            48_000,
            &[ChannelRole::Mono],
            LoudnessProfile::NativeEbu2023,
        )
        .unwrap();

        assert_eq!(native.simd_backend.is_scalar(), !sse2_available);
        assert!(native_mono.simd_backend.is_scalar());
        assert!(compatibility_default.simd_backend.is_scalar());
        assert!(compatibility_roles.simd_backend.is_scalar());
        assert_eq!(
            simd::Backend::sse2_if_available().is_some(),
            sse2_available
        );

        if let Some(avx) = simd::Backend::avx_if_available() {
            assert_ne!(native.simd_backend, avx);
            assert_ne!(compatibility_default.simd_backend, avx);
            assert_ne!(compatibility_roles.simd_backend, avx);
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn simd02_k_weighting_matches_scalar_outputs_and_delay_bits() {
        let coefficients = KWeightingCoefficients::for_rate(48_000).unwrap();
        for backend in [
            simd::Backend::sse2_if_available(),
            simd::Backend::avx_if_available(),
        ]
        .into_iter()
        .flatten()
        {
            for channels in [1usize, 2, 3, 4, 5, 7] {
                let mut scalar_states = vec![FilterState::default(); channels];
                let mut vector_states = scalar_states.clone();
                let mut scalar_out = vec![0.0; channels];
                let mut vector_out = vec![0.0; channels];
                for frame_index in 0..2_003usize {
                    let mut frame = vec![0.0; channels];
                    for (channel, sample) in frame.iter_mut().enumerate() {
                        *sample = match (frame_index + channel) % 11 {
                            0 => -0.0,
                            1 => f64::from_bits(1),
                            _ => ((frame_index * (channel + 3)) as f64 * 0.013).sin() * 1.75,
                        };
                    }
                    simd::filter_frame(
                        simd::Backend::scalar(),
                        &frame,
                        &mut scalar_states,
                        coefficients,
                        &mut scalar_out,
                    );
                    simd::filter_frame(
                        backend,
                        &frame,
                        &mut vector_states,
                        coefficients,
                        &mut vector_out,
                    );
                    assert_bits_eq(&scalar_out, &vector_out);
                    for (left, right) in scalar_states.iter().zip(&vector_states) {
                        assert_bits_eq(&left.delay, &right.delay);
                    }
                }
            }
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn simd03_window_energy_matches_scalar_for_wrapped_profiles_and_zero_weights() {
        for backend in [
            simd::Backend::sse2_if_available(),
            simd::Backend::avx_if_available(),
        ]
        .into_iter()
        .flatten()
        {
            for profile in [LoudnessProfile::NativeEbu2023, LoudnessProfile::Libebur128126] {
                let roles = if profile == LoudnessProfile::NativeEbu2023 {
                    vec![
                        ChannelRole::Left,
                        ChannelRole::Right,
                        ChannelRole::Center,
                        ChannelRole::Lfe,
                        ChannelRole::LeftSurround,
                    ]
                } else {
                    vec![
                        ChannelRole::Left,
                        ChannelRole::Right,
                        ChannelRole::Center,
                        ChannelRole::Unused,
                        ChannelRole::LeftSurround,
                    ]
                };
                let mut meter = LoudnessMeter::with_roles(48_000, &roles, profile).unwrap();
                meter.write_frame = 13;
                for (index, sample) in meter.ring.iter_mut().enumerate() {
                    *sample = match index % 17 {
                        0 => -0.0,
                        1 => f64::from_bits(2),
                        _ => ((index as f64) * 0.001_003).sin() * 0.25,
                    };
                }
                let windows = [
                    meter.clock.integrated_window_frames(),
                    meter.clock.lra_window_frames(),
                ];
                for window in windows {
                    let scalar = meter.window_energy_scalar(window).unwrap();
                    meter.simd_backend = backend;
                    let vector = meter.window_energy(window).unwrap();
                    meter.simd_backend = simd::Backend::scalar();
                    assert_eq!(scalar.to_bits(), vector.to_bits(), "{profile:?} {window}");
                }
            }
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn simd03_per_channel_window_sums_preserve_rounding_sensitive_order() {
        for backend in [
            simd::Backend::sse2_if_available(),
            simd::Backend::avx_if_available(),
        ]
        .into_iter()
        .flatten()
        {
            let channels = 5usize;
            let frames = 37usize;
            let weights = [1.0, 1.0, 1.0, 0.0, 1.41];
            let mut ring = vec![0.0_f64; frames * channels];
            for frame in 0..frames {
                for channel in 0..channels {
                    ring[frame * channels + channel] = match (frame + channel) % 8 {
                        0 => 1.0,
                        1 => f64::from_bits(1),
                        2 => -0.0,
                        3 => 2.0_f64.powi(-27),
                        4 => -2.0_f64.powi(-27),
                        5 => 0.5,
                        6 => -0.25,
                        _ => 1.0_f64 + 2.0_f64.powi(-52),
                    };
                }
            }
            for spans in [
                &[(3usize, 29usize)][..],
                &[(0usize, 11usize), (19usize, 18usize)][..],
                &[(19usize, 18usize), (0usize, 11usize)][..],
            ] {
                let mut scalar = [0.0_f64; MAX_CHANNELS];
                let mut vector = [0.0_f64; MAX_CHANNELS];
                simd::window_channel_sums(
                    simd::Backend::scalar(),
                    &ring,
                    channels,
                    &weights,
                    spans,
                    &mut scalar,
                );
                simd::window_channel_sums(
                    backend,
                    &ring,
                    channels,
                    &weights,
                    spans,
                    &mut vector,
                );
                assert_bits_eq(&scalar[..channels], &vector[..channels]);
                assert_eq!(scalar[3].to_bits(), 0.0_f64.to_bits());
                assert_eq!(vector[3].to_bits(), 0.0_f64.to_bits());

                let scalar_total = weights
                    .iter()
                    .zip(&scalar)
                    .filter(|(weight, _)| **weight != 0.0)
                    .fold(0.0_f64, |total, (weight, sum)| total + *weight * *sum);
                let vector_total = weights
                    .iter()
                    .zip(&vector)
                    .filter(|(weight, _)| **weight != 0.0)
                    .fold(0.0_f64, |total, (weight, sum)| total + *weight * *sum);
                assert_eq!(scalar_total.to_bits(), vector_total.to_bits());
            }
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn simd04_full_and_reduced_meter_state_matches_scalar_authority() {
        let backend = simd::Backend::best_available();
        if backend.is_scalar() {
            return;
        }
        let roles = [
            ChannelRole::Left,
            ChannelRole::Right,
            ChannelRole::Center,
            ChannelRole::Lfe,
            ChannelRole::LeftSurround,
        ];
        let frames = 48_000usize + 137;
        let mut pcm = Vec::with_capacity(frames * roles.len());
        for n in 0..frames {
            for channel in 0..roles.len() {
                pcm.push(((n * (channel + 5)) as f64 * 0.0097).sin() * (0.05 + channel as f64 * 0.01));
            }
        }
        for demand in [
            LoudnessMetricDemand::IntegratedOnly,
            LoudnessMetricDemand::IntegratedAndRange,
        ] {
            let mut scalar = native_meter_with_demand(48_000, &roles, demand);
            let mut vector = native_meter_with_demand(48_000, &roles, demand);
            vector.simd_backend = backend;
            let pattern = [1usize, 511, 4_800, 17, 9_601];
            let mut offset = 0usize;
            let mut chunk_index = 0usize;
            while offset < frames {
                let chunk_frames = pattern[chunk_index % pattern.len()];
                let count = chunk_frames.min(frames - offset);
                let start = offset * roles.len();
                let end = (offset + count) * roles.len();
                scalar.push_interleaved(&pcm[start..end]).unwrap();
                vector.push_interleaved(&pcm[start..end]).unwrap();
                offset += count;
                chunk_index += 1;
            }
            assert_bits_eq(&scalar.ring, &vector.ring);
            for (left, right) in scalar.filters.iter().zip(&vector.filters) {
                assert_bits_eq(&left.delay, &right.delay);
            }
            assert_bits_eq(&scalar.integrated_energies, &vector.integrated_energies);
            assert_bits_eq(&scalar.lra_energies, &vector.lra_energies);
            let scalar = scalar.finalize().unwrap();
            let vector = vector.finalize().unwrap();
            assert_eq!(scalar.summary.integrated, vector.summary.integrated);
            assert_eq!(scalar.summary.range, vector.summary.range);
            assert_bits_eq(
                &scalar.statistics.integrated_energies,
                &vector.statistics.integrated_energies,
            );
            assert_bits_eq(&scalar.statistics.lra_energies, &vector.statistics.lra_energies);
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn simd05_invalid_block_is_rejected_before_any_state_mutation() {
        let backend = simd::Backend::best_available();
        if backend.is_scalar() {
            return;
        }
        let mut meter = LoudnessMeter::new(48_000, 2).unwrap();
        meter.simd_backend = backend;
        let ring_before = meter.ring.clone();
        let filters_before = meter.filters.clone();
        assert_eq!(
            meter.push_interleaved(&[0.25, 0.5, f64::NAN, 0.0]),
            Err(LoudnessError::NonFiniteSample { sample_index: 2 })
        );
        assert_bits_eq(&ring_before, &meter.ring);
        for (left, right) in filters_before.iter().zip(&meter.filters) {
            assert_bits_eq(&left.delay, &right.delay);
        }
        assert_eq!(meter.frames_seen, 0);
        assert_eq!(meter.real_frames, 0);

        let mut overflow = LoudnessMeter::new(48_000, 2).unwrap();
        overflow.simd_backend = backend;
        assert_eq!(
            overflow.push_interleaved(&[f64::MAX, f64::MAX]),
            Err(LoudnessError::NumericalRange)
        );
        assert_eq!(
            overflow.push_interleaved(&[0.0, 0.0]),
            Err(LoudnessError::MeterFailed)
        );
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    #[allow(deprecated)]
    fn simd06_daz_ftz_preserves_scalar_vector_filter_bits_and_restores_mxcsr() {
        unsafe fn read_mxcsr() -> u32 {
            core::arch::x86_64::_mm_getcsr()
        }
        unsafe fn write_mxcsr(value: u32) {
            core::arch::x86_64::_mm_setcsr(value);
        }

        std::thread::spawn(|| {
            let backend = simd::Backend::best_available();
            if backend.is_scalar() {
                return;
            }
            let coefficients = KWeightingCoefficients::for_rate(48_000).unwrap();
            let original = unsafe { read_mxcsr() };
            const DAZ_FTZ: u32 = (1 << 6) | (1 << 15);
            unsafe { write_mxcsr(original | DAZ_FTZ) };

            let channels = 5usize;
            let mut scalar_states = vec![FilterState::default(); channels];
            let mut vector_states = scalar_states.clone();
            let mut scalar_out = vec![0.0_f64; channels];
            let mut vector_out = vec![0.0_f64; channels];
            let mut equal = true;
            for frame_index in 0..257usize {
                let mut frame = vec![0.0_f64; channels];
                for (channel, sample) in frame.iter_mut().enumerate() {
                    *sample = match (frame_index + channel) % 5 {
                        0 => f64::from_bits(1),
                        1 => f64::from_bits(11),
                        2 => -0.0,
                        3 => -f64::from_bits(7),
                        _ => 0.25,
                    };
                }
                simd::filter_frame(
                    simd::Backend::scalar(),
                    &frame,
                    &mut scalar_states,
                    coefficients,
                    &mut scalar_out,
                );
                simd::filter_frame(
                    backend,
                    &frame,
                    &mut vector_states,
                    coefficients,
                    &mut vector_out,
                );
                equal &= scalar_out
                    .iter()
                    .zip(&vector_out)
                    .all(|(left, right)| left.to_bits() == right.to_bits());
                equal &= scalar_states.iter().zip(&vector_states).all(|(left, right)| {
                    left.delay
                        .iter()
                        .zip(right.delay.iter())
                        .all(|(left, right)| left.to_bits() == right.to_bits())
                });
            }

            unsafe { write_mxcsr(original) };
            assert_eq!(unsafe { read_mxcsr() }, original);
            assert!(equal, "DAZ/FTZ changed scalar/vector same-graph bits");
        })
        .join()
        .expect("SIMD DAZ/FTZ test thread");
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn simd10_reconstructed_meters_reset_all_authoritative_state() {
        let backend = simd::Backend::best_available();
        if backend.is_scalar() {
            return;
        }
        let roles = [ChannelRole::Left, ChannelRole::Right, ChannelRole::Center];
        let frames = 48_000usize + 311;
        let mut pcm = Vec::with_capacity(frames * roles.len());
        for n in 0..frames {
            pcm.extend([
                ((n as f64) * 0.011).sin() * 0.11,
                -((n as f64) * 0.017).sin() * 0.07,
                ((n as f64) * 0.023).cos() * 0.03,
            ]);
        }

        let observe = |backend: simd::Backend| {
            let mut meter = LoudnessMeter::with_roles(
                48_000,
                &roles,
                LoudnessProfile::NativeEbu2023,
            )
            .unwrap();
            meter.simd_backend = backend;
            let pattern = [37usize, 1, 4_799, 503, 8_003];
            let mut offset = 0usize;
            let mut part = 0usize;
            while offset < frames {
                let count = pattern[part % pattern.len()].min(frames - offset);
                let start = offset * roles.len();
                let end = (offset + count) * roles.len();
                meter.push_interleaved(&pcm[start..end]).unwrap();
                offset += count;
                part += 1;
            }
            meter.finalize().unwrap()
        };

        let scalar = observe(simd::Backend::scalar());
        let first = observe(backend);
        let second = observe(backend);
        assert_eq!(first.summary.integrated, scalar.summary.integrated);
        assert_eq!(first.summary.range, scalar.summary.range);
        assert_eq!(second.summary.integrated, scalar.summary.integrated);
        assert_eq!(second.summary.range, scalar.summary.range);
        assert_bits_eq(
            &first.statistics.integrated_energies,
            &second.statistics.integrated_energies,
        );
        assert_bits_eq(&first.statistics.lra_energies, &second.statistics.lra_energies);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn simd11_backend_adds_no_persistent_storage_or_history_penalty() {
        let backend = simd::Backend::best_available();
        if backend.is_scalar() {
            return;
        }
        let roles = [ChannelRole::Left, ChannelRole::Right, ChannelRole::Center];
        let mut probe = LoudnessMeter::with_roles(
            48_000,
            &roles,
            LoudnessProfile::NativeEbu2023,
        )
        .unwrap();
        let fixed_storage_bytes = probe.fixed_storage_bytes;

        let frames = 72_000usize;
        let mut pcm = Vec::with_capacity(frames * roles.len());
        for n in 0..frames {
            let sample = ((n as f64) * 0.01).sin() * 0.1;
            pcm.extend([sample, -sample, sample * 0.5]);
        }
        // Treat the source as unknown extent by feeding unrelated chunk sizes;
        // the meter has no predeclared total duration.
        let pattern = [1usize, 997, 4_801, 31, 8_003];
        let mut offset = 0usize;
        let mut part = 0usize;
        while offset < frames {
            let count = pattern[part % pattern.len()].min(frames - offset);
            let start = offset * roles.len();
            let end = (offset + count) * roles.len();
            probe.push_interleaved(&pcm[start..end]).unwrap();
            offset += count;
            part += 1;
        }
        let history_after_push = probe.history_storage_bytes;
        let probe = probe.finalize().unwrap();
        let final_history = probe
            .summary
            .retained_storage_bytes
            .checked_sub(fixed_storage_bytes)
            .unwrap();
        assert!(final_history > history_after_push);
        // This is the allocator-observed scalar boundary. A hypothetical
        // persistent 64-byte lane pad would fit at construction but steal
        // headroom required by final LRA continuation.
        let limit = fixed_storage_bytes + final_history;

        let mut scalar = LoudnessMeter::with_roles_and_limit(
            48_000,
            &roles,
            LoudnessProfile::NativeEbu2023,
            limit,
        )
        .unwrap();
        let mut vector = LoudnessMeter::with_roles_and_limit(
            48_000,
            &roles,
            LoudnessProfile::NativeEbu2023,
            limit,
        )
        .unwrap();
        vector.simd_backend = backend;
        assert_eq!(scalar.fixed_storage_bytes, vector.fixed_storage_bytes);
        assert_eq!(scalar.retained_storage_bytes(), vector.retained_storage_bytes());

        let mut offset = 0usize;
        let mut part = 0usize;
        while offset < frames {
            let count = pattern[part % pattern.len()].min(frames - offset);
            let start = offset * roles.len();
            let end = (offset + count) * roles.len();
            let scalar_result = scalar.push_interleaved(&pcm[start..end]);
            let vector_result = vector.push_interleaved(&pcm[start..end]);
            assert_eq!(scalar_result, vector_result);
            offset += count;
            part += 1;
        }
        assert_eq!(scalar.retained_storage_bytes(), vector.retained_storage_bytes());
        let scalar = scalar.finalize().unwrap();
        let vector = vector.finalize().unwrap();
        assert_eq!(scalar.summary.integrated, vector.summary.integrated);
        assert_eq!(scalar.summary.range, vector.summary.range);
        assert_eq!(
            scalar.summary.retained_storage_bytes,
            vector.summary.retained_storage_bytes
        );

        assert!(matches!(
            LoudnessMeter::with_roles_and_limit(
                48_000,
                &roles,
                LoudnessProfile::NativeEbu2023,
                fixed_storage_bytes - 1,
            ),
            Err(LoudnessError::ResourceLimit { .. })
        ));
    }

}
