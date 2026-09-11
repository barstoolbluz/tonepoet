//! Streaming programme loudness measurements.
//!
//! This module deliberately keeps two named behaviors instead of a set of
//! compatibility switches. [`NativeEbu2023`] measures the full-resolution PCM
//! supplied by the caller. [`Libebur128126`] reproduces the pinned libebur128
//! 1.2.6 block clock, gate comparisons, default channel map (when explicitly
//! requested), and fourth-order K-weighting operation graph.  Legacy loudgain
//! S16 preparation is a consumer/front-end concern and is not performed here.

mod k_weighting;
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
    Finite { lu: f64, observations: usize },
    Unavailable { observations: usize },
}

impl LoudnessRange {
    #[must_use]
    pub const fn finite_lu(&self) -> Option<f64> {
        match self {
            Self::Finite { lu, .. } => Some(*lu),
            Self::Unavailable { .. } => None,
        }
    }

    #[must_use]
    pub const fn observations(&self) -> usize {
        match self {
            Self::Finite { observations, .. } | Self::Unavailable { observations } => *observations,
        }
    }
}

/// Final per-track loudness result.
#[derive(Debug, Clone, PartialEq)]
pub struct LoudnessSummary {
    pub sample_rate_hz: u32,
    pub roles: Vec<ChannelRole>,
    pub profile: LoudnessProfile,
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

        let channels = roles.len();
        let coefficients = KWeightingCoefficients::for_rate(sample_rate_hz)?;
        let clock = WindowClock::new(sample_rate_hz, profile)?;
        let ring_frames = usize::try_from(clock.ring_frames())
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

        Ok(Self {
            sample_rate_hz,
            channels,
            roles: owned_roles,
            weights,
            profile,
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

    #[must_use]
    pub const fn profile(&self) -> LoudnessProfile {
        self.profile
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
        self.preflight_history(target, true, true)?;

        for frame in samples.chunks_exact(self.channels) {
            if let Err(error) = self.process_frame(frame, true, true) {
                self.failed = true;
                return Err(error);
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
        if self.profile == LoudnessProfile::NativeEbu2023 && real_frames > 0 {
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
            for _ in 0..continuation_frames {
                self.process_frame(&zero_frame, false, true)?;
            }
        }

        sort_lra_energies(&mut self.lra_energies);
        let integrated = integrated_from_absolute_energies(
            &self.integrated_energies,
            self.profile,
            real_frames,
            self.clock.integrated_window_frames(),
        );
        let range = lra_from_sorted_absolute_energies(&self.lra_energies, self.profile);
        let summary = LoudnessSummary {
            sample_rate_hz: self.sample_rate_hz,
            roles: self.roles.clone(),
            profile: self.profile,
            real_frames,
            integrated,
            range,
            absolute_integrated_observations: self.integrated_energies.len(),
            absolute_lra_observations: self.lra_energies.len(),
            retained_storage_bytes: self.retained_storage_bytes(),
        };
        let statistics = LoudnessStatistics {
            profile: self.profile,
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

    fn process_frame(
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
        for channel in 0..self.channels {
            let filtered = self.filters[channel].process(frame[channel], self.coefficients);
            if !filtered.is_finite() {
                return Err(LoudnessError::NumericalRange);
            }
            self.ring[base + channel] = filtered;
        }
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

/// Builder for a loudgain/libebur128-style album: each track is independently
/// windowed and filtered, then its retained observations are pooled in declared
/// manifest order.  PCM is never concatenated.
#[derive(Debug)]
pub struct AlbumLoudnessBuilder {
    profile: Option<LoudnessProfile>,
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
        Self {
            profile: None,
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
        let needed_lra = self
            .lra_energies
            .len()
            .checked_add(statistics.lra_energies.len())
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
        let capacities_before = (
            self.integrated_energies.capacity(),
            self.lra_energies.capacity(),
        );
        let reserve_result = (|| {
            self.integrated_energies
                .try_reserve_exact(statistics.integrated_energies.len())
                .map_err(|_| LoudnessError::AllocationFailed)?;
            self.lra_energies
                .try_reserve_exact(statistics.lra_energies.len())
                .map_err(|_| LoudnessError::AllocationFailed)?;
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
        self.lra_energies.append(&mut statistics.lra_energies);
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
        sort_lra_energies(&mut self.lra_energies);
        let range = lra_from_sorted_absolute_energies(&self.lra_energies, profile);
        Ok(AlbumLoudnessSummary {
            profile,
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

}
