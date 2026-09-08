use std::fmt;
use std::sync::{Arc, OnceLock};

use rustfft::num_complex::Complex64;
use rustfft::{Fft, FftPlanner};

use super::{
    build_polyphase_filters, next_up_nonnegative, EdgePolicy, FastHeadroomTwoXStage, PeakLevel,
    TruePeakError, TruePeakResult, Window, HEADROOM64_HALF_DELAY_COEFFICIENTS,
    HEADROOM64_HALF_DELAY_TAPS, HEADROOM64_INTERPOLATION_CALIBRATION_LINEAR,
    HEADROOM64X_RECONSTRUCTION_NUMERIC_ERROR_PER_INPUT_PEAK_UPPER, HEADROOM64_STAGE_2_TAPS,
    HEADROOM64_STAGE_3_TAPS, HEADROOM64_STAGE_4_TAPS, HEADROOM64_STAGE_5_TAPS,
    HEADROOM64_STAGE_6_TAPS, HEADROOM_FAST_RECONSTRUCTION_NUMERIC_ERROR_PER_INPUT_PEAK_UPPER,
    HEADROOM_REFERENCE_FAST_BLOCK_FRAMES, HEADROOM_REFERENCE_FAST_COARSE_HALO,
    HEADROOM_REFERENCE_FAST_CUBIC_ERROR_GAIN_UPPER, HEADROOM_REFERENCE_FAST_FFT_SIZE,
    HEADROOM_REFERENCE_FAST_INPUT_HALO_FRAMES, HEADROOM_REFERENCE_FAST_MAX_REFINEMENTS_PER_TILE,
    HEADROOM_REFERENCE_FAST_PREFERRED_WIDTH_LINEAR, HEADROOM_REFERENCE_FAST_RESIDUAL_L1_UPPER,
    HEADROOM_REFERENCE_FAST_RESIDUAL_SUM_UPPER,
    HEADROOM_REFERENCE_FAST_SCREEN_NUMERIC_ERROR_PER_SCALE_UPPER,
    HEADROOM_REFERENCE_FAST_TAIL_LINF_GAIN_UPPER,
    HEADROOM_REFERENCE_FAST_TAIL_NUMERIC_ERROR_PER_SCALE_UPPER,
    HEADROOM_REFERENCE_FAST_TILE_INPUT_FRAMES,
    HEADROOM_STANDARD16X_EXECUTION_ERROR_PER_INPUT_PEAK_UPPER,
};

const TAIL_OFFSET_MIN: i32 = -9;
const TAIL_OFFSET_MAX: i32 = 9;
const TAIL_OFFSET_COUNT: usize = (TAIL_OFFSET_MAX - TAIL_OFFSET_MIN + 1) as usize;
const TAIL_FACTOR: i32 = 16;
const TAIL_DELAY_SUBFRAMES: i32 = 144;

#[inline]
fn outward_nonnegative(value: f64) -> f64 {
    if value == 0.0 {
        0.0
    } else {
        next_up_nonnegative(value)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ReferenceFastScanStats {
    pub candidate_intervals: u64,
    pub refined_intervals: u64,
    pub budget_exhausted_tiles: u64,
}

#[derive(Debug, Clone)]
pub(super) struct ReferenceFastScanResult {
    pub point_estimate: TruePeakResult,
    pub reconstruction_upper: PeakLevel,
    pub reconstruction_channel_linear_peaks: Vec<f64>,
    pub lower_linear: f64,
    pub interval_width_db: Option<f64>,
    pub reference_search_complete: bool,
    pub stats: ReferenceFastScanStats,
}

#[derive(Debug)]
struct ReferenceTailKernels {
    coefficients: [[f64; TAIL_OFFSET_COUNT]; 16],
}

fn convolve(left: &[f64], right: &[f64]) -> Vec<f64> {
    let mut output = vec![0.0; left.len() + right.len() - 1];
    for (left_index, left_value) in left.iter().copied().enumerate() {
        if left_value == 0.0 {
            continue;
        }
        for (right_index, right_value) in right.iter().copied().enumerate() {
            if right_value != 0.0 {
                output[left_index + right_index] += left_value * right_value;
            }
        }
    }
    while output.len() > 1 && output.last() == Some(&0.0) {
        output.pop();
    }
    output
}

fn reference_two_x_impulse_response(taps: usize) -> Vec<f64> {
    let delay_frames = (taps + 1) / 2;
    let filters = build_polyphase_filters(taps, 2, delay_frames, Window::Blackman, true);
    let mut response = vec![0.0; delay_frames * 2];
    for (phase, filter) in filters.iter().enumerate() {
        for (&index, &coefficient) in filter.indices.iter().zip(&filter.coefficients) {
            response[index * 2 + phase] = coefficient;
        }
    }
    while response.len() > 1 && response.last() == Some(&0.0) {
        response.pop();
    }
    response
}

fn build_reference_tail_kernels() -> ReferenceTailKernels {
    let mut response = vec![1.0_f64];
    let mut expected_response_len = 1_usize;
    let mut delay = 0_i32;
    for taps in [
        HEADROOM64_STAGE_3_TAPS,
        HEADROOM64_STAGE_4_TAPS,
        HEADROOM64_STAGE_5_TAPS,
        HEADROOM64_STAGE_6_TAPS,
    ] {
        let stage_response = reference_two_x_impulse_response(taps);
        // Blackman is mathematically zero at both nominal FIR endpoints. The
        // shared filter builder drops those near-zero coefficients, and the
        // impulse-response helper trims the resulting trailing zero. Its
        // represented buffer is therefore taps - 1 samples long (indices
        // 0..taps-2), not the nominal taps samples. Keep that construction
        // invariant explicit so a future window/filter change cannot silently
        // alter the tail kernel.
        let expected_stage_response_len = taps - 1;
        debug_assert_eq!(stage_response.len(), expected_stage_response_len);

        let mut upsampled = vec![0.0; response.len() * 2 - 1];
        for (index, value) in response.iter().copied().enumerate() {
            upsampled[index * 2] = value;
        }
        response = convolve(&upsampled, &stage_response);
        expected_response_len = expected_response_len * 2 + expected_stage_response_len - 2;
        debug_assert_eq!(response.len(), expected_response_len);
        delay = (delay + ((taps - 1) / 4) as i32) * 2;
    }
    debug_assert_eq!(delay, TAIL_DELAY_SUBFRAMES);
    debug_assert!(response.iter().enumerate().all(|(index, coefficient)| {
        if *coefficient == 0.0 {
            return true;
        }
        let index = index as i32;
        let phase = (index - delay).rem_euclid(TAIL_FACTOR);
        let offset = (phase + delay - index) / TAIL_FACTOR;
        (TAIL_OFFSET_MIN..=TAIL_OFFSET_MAX).contains(&offset)
    }));

    let mut coefficients = [[0.0_f64; TAIL_OFFSET_COUNT]; 16];
    for phase in 0..16_i32 {
        for offset in TAIL_OFFSET_MIN..=TAIL_OFFSET_MAX {
            let response_index = phase + delay - TAIL_FACTOR * offset;
            if response_index >= 0 && (response_index as usize) < response.len() {
                coefficients[phase as usize][(offset - TAIL_OFFSET_MIN) as usize] =
                    response[response_index as usize];
            }
        }
        let l1 = coefficients[phase as usize]
            .iter()
            .copied()
            .map(f64::abs)
            .sum::<f64>();
        debug_assert!(l1 <= HEADROOM_REFERENCE_FAST_TAIL_LINF_GAIN_UPPER);
    }
    ReferenceTailKernels { coefficients }
}

fn reference_tail_kernels() -> &'static ReferenceTailKernels {
    static KERNELS: OnceLock<ReferenceTailKernels> = OnceLock::new();
    KERNELS.get_or_init(build_reference_tail_kernels)
}

#[derive(Clone)]
struct FftReference4Engine {
    forward: Arc<dyn Fft<f64>>,
    inverse: Arc<dyn Fft<f64>>,
    filter_spectrum: Vec<Complex64>,
    fft_buffer: Vec<Complex64>,
    forward_scratch: Vec<Complex64>,
    inverse_scratch: Vec<Complex64>,
    history: Vec<Vec<f64>>,
    pending: Vec<f64>,
    pending_start_index: Option<i128>,
    half_output: Vec<f64>,
    stage2: FastHeadroomTwoXStage,
    scratch1: Vec<f64>,
    scratch2: Vec<f64>,
    running_input_peaks: Vec<f64>,
    coarse_error_frame: Vec<f64>,
    channels: usize,
}

impl fmt::Debug for FftReference4Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FftReference4Engine")
            .field("channels", &self.channels)
            .field("pending_frames", &(self.pending.len() / self.channels))
            .finish_non_exhaustive()
    }
}

impl FftReference4Engine {
    fn new(channels: usize) -> Self {
        debug_assert!(HEADROOM_REFERENCE_FAST_FFT_SIZE
            >= HEADROOM_REFERENCE_FAST_BLOCK_FRAMES + HEADROOM64_HALF_DELAY_TAPS - 1);
        let mut planner = FftPlanner::<f64>::new();
        let forward = planner.plan_fft_forward(HEADROOM_REFERENCE_FAST_FFT_SIZE);
        let inverse = planner.plan_fft_inverse(HEADROOM_REFERENCE_FAST_FFT_SIZE);

        let mut filter_spectrum = vec![Complex64::new(0.0, 0.0); HEADROOM_REFERENCE_FAST_FFT_SIZE];
        for (index, coefficient) in HEADROOM64_HALF_DELAY_COEFFICIENTS
            .iter()
            .copied()
            .enumerate()
        {
            filter_spectrum[index].re = coefficient;
            filter_spectrum[HEADROOM64_HALF_DELAY_TAPS - 1 - index].re = coefficient;
        }
        let mut forward_scratch =
            vec![Complex64::new(0.0, 0.0); forward.get_inplace_scratch_len()];
        let inverse_scratch =
            vec![Complex64::new(0.0, 0.0); inverse.get_inplace_scratch_len()];
        forward.process_with_scratch(&mut filter_spectrum, &mut forward_scratch);

        Self {
            forward,
            inverse,
            filter_spectrum,
            fft_buffer: vec![Complex64::new(0.0, 0.0); HEADROOM_REFERENCE_FAST_FFT_SIZE],
            forward_scratch,
            inverse_scratch,
            history: vec![vec![0.0; HEADROOM64_HALF_DELAY_TAPS - 1]; channels],
            pending: Vec::with_capacity(HEADROOM_REFERENCE_FAST_BLOCK_FRAMES * channels),
            pending_start_index: None,
            half_output: vec![0.0; HEADROOM_REFERENCE_FAST_BLOCK_FRAMES * channels],
            // Use the repository's existing symmetric execution layout for the
            // frozen 49-tap Reference stage. Its paired-coefficient averaging
            // changes the generated branch only at sub-ULP scale; the coarse
            // execution enclosure below covers that perturbation while this
            // halves the unconditional half-phase multiplication count.
            stage2: FastHeadroomTwoXStage::new(HEADROOM64_STAGE_2_TAPS, channels),
            scratch1: vec![0.0; channels * 2],
            scratch2: vec![0.0; channels * 2],
            running_input_peaks: vec![0.0; channels],
            coarse_error_frame: vec![0.0; channels],
            channels,
        }
    }

    fn process_frame(
        &mut self,
        frame: &[f64],
        input_index: i128,
        scanner: &mut ReferenceFastScanner,
    ) {
        debug_assert_eq!(frame.len(), self.channels);
        for (peak, sample) in self.running_input_peaks.iter_mut().zip(frame.iter().copied()) {
            *peak = (*peak).max(sample.abs());
        }
        let pending_frames = self.pending.len() / self.channels;
        match self.pending_start_index {
            None => self.pending_start_index = Some(input_index),
            Some(start) => debug_assert_eq!(input_index, start + pending_frames as i128),
        }
        self.pending.extend_from_slice(frame);
        if self.pending.len() == HEADROOM_REFERENCE_FAST_BLOCK_FRAMES * self.channels {
            self.process_pending(scanner);
        }
    }

    fn flush(&mut self, scanner: &mut ReferenceFastScanner) {
        if !self.pending.is_empty() {
            self.process_pending(scanner);
        }
    }

    fn process_pending(&mut self, scanner: &mut ReferenceFastScanner) {
        let count = self.pending.len() / self.channels;
        if count == 0 {
            return;
        }
        debug_assert!(count <= HEADROOM_REFERENCE_FAST_BLOCK_FRAMES);
        let start = self
            .pending_start_index
            .expect("non-empty FFT Reference block has a start index");
        let overlap = HEADROOM64_HALF_DELAY_TAPS - 1;
        let inverse_scale = 1.0 / HEADROOM_REFERENCE_FAST_FFT_SIZE as f64;

        for channel_pair_start in (0..self.channels).step_by(2) {
            let second_channel =
                (channel_pair_start + 1 < self.channels).then_some(channel_pair_start + 1);
            self.fft_buffer.fill(Complex64::new(0.0, 0.0));
            for history_index in 0..overlap {
                self.fft_buffer[history_index].re = self.history[channel_pair_start][history_index];
                if let Some(channel) = second_channel {
                    self.fft_buffer[history_index].im = self.history[channel][history_index];
                }
            }
            for frame_index in 0..count {
                self.fft_buffer[overlap + frame_index].re =
                    self.pending[frame_index * self.channels + channel_pair_start];
                if let Some(channel) = second_channel {
                    self.fft_buffer[overlap + frame_index].im =
                        self.pending[frame_index * self.channels + channel];
                }
            }
            self.forward
                .process_with_scratch(&mut self.fft_buffer, &mut self.forward_scratch);
            for (bin, filter) in self
                .fft_buffer
                .iter_mut()
                .zip(self.filter_spectrum.iter().copied())
            {
                *bin *= filter;
            }
            self.inverse
                .process_with_scratch(&mut self.fft_buffer, &mut self.inverse_scratch);
            for frame_index in 0..count {
                let output = self.fft_buffer[overlap + frame_index];
                self.half_output[frame_index * self.channels + channel_pair_start] =
                    output.re * inverse_scale;
                if let Some(channel) = second_channel {
                    self.half_output[frame_index * self.channels + channel] =
                        output.im * inverse_scale;
                }
            }
        }

        let execution_scale = HEADROOM_STANDARD16X_EXECUTION_ERROR_PER_INPUT_PEAK_UPPER
            + HEADROOM_FAST_RECONSTRUCTION_NUMERIC_ERROR_PER_INPUT_PEAK_UPPER;
        for (error, peak) in self
            .coarse_error_frame
            .iter_mut()
            .zip(self.running_input_peaks.iter().copied())
        {
            *error = outward_nonnegative(peak * execution_scale);
        }

        for frame_index in 0..count {
            for channel in 0..self.channels {
                self.scratch1[channel] = if frame_index >= HEADROOM64_HALF_DELAY_TAPS / 2 {
                    self.pending[(frame_index - HEADROOM64_HALF_DELAY_TAPS / 2) * self.channels
                        + channel]
                } else {
                    let history_index = overlap + frame_index - HEADROOM64_HALF_DELAY_TAPS / 2;
                    self.history[channel][history_index]
                };
                self.scratch1[self.channels + channel] =
                    self.half_output[frame_index * self.channels + channel];
            }
            let base1 =
                (start + frame_index as i128 - (HEADROOM64_HALF_DELAY_TAPS / 2) as i128) * 2;
            for phase1 in 0..2 {
                let frame1 = &self.scratch1[phase1 * self.channels..(phase1 + 1) * self.channels];
                let base2 = self.stage2.process_frame_symmetric(
                    frame1,
                    base1 + phase1 as i128,
                    &mut self.scratch2,
                );
                for phase2 in 0..2 {
                    let output_index = base2 + phase2 as i128;
                    let reconstructed =
                        &self.scratch2[phase2 * self.channels..(phase2 + 1) * self.channels];
                    scanner.observe_coarse(
                        output_index,
                        reconstructed,
                        &self.coarse_error_frame,
                    );
                }
            }
        }

        // Readiness is deliberately checked once per FFT block rather than
        // once per 4x knot. At high sample rates the latter would add billions
        // of avoidable control-flow calls to a 40-minute scan.
        scanner.process_ready_full_tiles();

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

#[derive(Debug, Clone, Copy)]
struct CandidateInterval {
    offset: usize,
    score: f64,
}

#[derive(Debug, Clone)]
struct ReferenceFastScanner {
    channels: usize,
    input_start_index: Option<i128>,
    input_values: Vec<f64>,
    coarse_start_index: Option<i128>,
    coarse_values: Vec<f64>,
    coarse_errors: Vec<f64>,
    nominal_frames_seen: i128,
    next_tile_start_frame: i128,
    point_channel_peaks: Vec<f64>,
    lower_channel_peaks: Vec<f64>,
    evaluated_upper_channel_peaks: Vec<f64>,
    unresolved_upper_channel_peaks: Vec<f64>,
    numerical_invalid: bool,
    stats: ReferenceFastScanStats,
}

impl ReferenceFastScanner {
    fn new(channels: usize) -> Self {
        Self {
            channels,
            input_start_index: None,
            input_values: Vec::with_capacity(
                (HEADROOM_REFERENCE_FAST_TILE_INPUT_FRAMES as usize
                    + 2 * HEADROOM_REFERENCE_FAST_INPUT_HALO_FRAMES as usize
                    + HEADROOM_REFERENCE_FAST_BLOCK_FRAMES)
                    * channels,
            ),
            coarse_start_index: None,
            coarse_values: Vec::with_capacity(
                (HEADROOM_REFERENCE_FAST_TILE_INPUT_FRAMES as usize * 4
                    + 2 * HEADROOM_REFERENCE_FAST_COARSE_HALO as usize
                    + HEADROOM_REFERENCE_FAST_BLOCK_FRAMES * 4)
                    * channels,
            ),
            coarse_errors: Vec::with_capacity(
                (HEADROOM_REFERENCE_FAST_TILE_INPUT_FRAMES as usize * 4
                    + 2 * HEADROOM_REFERENCE_FAST_COARSE_HALO as usize
                    + HEADROOM_REFERENCE_FAST_BLOCK_FRAMES * 4)
                    * channels,
            ),
            nominal_frames_seen: 0,
            next_tile_start_frame: 0,
            point_channel_peaks: vec![0.0; channels],
            lower_channel_peaks: vec![0.0; channels],
            evaluated_upper_channel_peaks: vec![0.0; channels],
            unresolved_upper_channel_peaks: vec![0.0; channels],
            numerical_invalid: false,
            stats: ReferenceFastScanStats::default(),
        }
    }

    fn observe_input(&mut self, input_index: i128, frame: &[f64], nominal: bool) {
        debug_assert_eq!(frame.len(), self.channels);
        let frames = self.input_values.len() / self.channels;
        match self.input_start_index {
            None => self.input_start_index = Some(input_index),
            Some(start) => debug_assert_eq!(input_index, start + frames as i128),
        }
        self.input_values.extend_from_slice(frame);
        if nominal {
            debug_assert_eq!(input_index, self.nominal_frames_seen);
            self.nominal_frames_seen += 1;
        }
    }

    #[inline]
    fn observe_coarse(&mut self, output_index: i128, frame: &[f64], error: &[f64]) {
        debug_assert_eq!(frame.len(), self.channels);
        debug_assert_eq!(error.len(), self.channels);
        let frames = self.coarse_values.len() / self.channels;
        match self.coarse_start_index {
            None => self.coarse_start_index = Some(output_index),
            Some(start) => debug_assert_eq!(output_index, start + frames as i128),
        }
        if frame.iter().chain(error).any(|value| !value.is_finite()) {
            self.numerical_invalid = true;
        }
        self.coarse_values.extend_from_slice(frame);
        self.coarse_errors.extend_from_slice(error);
    }

    fn input_last_index(&self) -> Option<i128> {
        self.input_start_index.map(|start| {
            start + (self.input_values.len() / self.channels) as i128 - 1
        })
    }

    fn coarse_last_index(&self) -> Option<i128> {
        self.coarse_start_index.map(|start| {
            start + (self.coarse_values.len() / self.channels) as i128 - 1
        })
    }

    fn ranges_available(
        &self,
        input_min: i128,
        input_max: i128,
        coarse_min: i128,
        coarse_max: i128,
    ) -> bool {
        self.input_start_index.is_some_and(|start| start <= input_min)
            && self.input_last_index().is_some_and(|end| end >= input_max)
            && self.coarse_start_index.is_some_and(|start| start <= coarse_min)
            && self.coarse_last_index().is_some_and(|end| end >= coarse_max)
    }

    fn process_ready_full_tiles(&mut self) {
        loop {
            let start_frame = self.next_tile_start_frame;
            let end_frame = start_frame + HEADROOM_REFERENCE_FAST_TILE_INPUT_FRAMES;
            // A full tile contains the four coarse intervals between each pair
            // of original frames, so its terminal endpoint must be nominal.
            if end_frame >= self.nominal_frames_seen {
                break;
            }
            let coarse_start = start_frame * 4;
            let coarse_end = end_frame * 4;
            if !self.ranges_available(
                start_frame - HEADROOM_REFERENCE_FAST_INPUT_HALO_FRAMES,
                end_frame + HEADROOM_REFERENCE_FAST_INPUT_HALO_FRAMES,
                coarse_start - HEADROOM_REFERENCE_FAST_COARSE_HALO,
                coarse_end - 1 + HEADROOM_REFERENCE_FAST_COARSE_HALO,
            ) {
                break;
            }
            self.process_tile(start_frame, end_frame, coarse_start, coarse_end);
            self.next_tile_start_frame = end_frame;
            self.discard_consumed_prefix();
        }
    }

    fn frame_offset(start: i128, channels: usize, index: i128) -> usize {
        debug_assert!(index >= start);
        usize::try_from(index - start).expect("bounded scanner index") * channels
    }

    fn overall_lower(&self) -> f64 {
        self.lower_channel_peaks
            .iter()
            .copied()
            .fold(0.0, f64::max)
    }

    fn process_tile(
        &mut self,
        start_frame: i128,
        end_frame: i128,
        coarse_start: i128,
        coarse_end: i128,
    ) {
        debug_assert!(coarse_end >= coarse_start);
        let input_start = self.input_start_index.expect("input tile is buffered");
        let coarse_buffer_start = self.coarse_start_index.expect("coarse tile is buffered");
        let input_min = start_frame - HEADROOM_REFERENCE_FAST_INPUT_HALO_FRAMES;
        let input_max = end_frame + HEADROOM_REFERENCE_FAST_INPUT_HALO_FRAMES;

        let mut tile_min = vec![f64::INFINITY; self.channels];
        let mut tile_max = vec![f64::NEG_INFINITY; self.channels];
        for index in input_min..=input_max {
            let base = Self::frame_offset(input_start, self.channels, index);
            for channel in 0..self.channels {
                let sample = self.input_values[base + channel];
                tile_min[channel] = tile_min[channel].min(sample);
                tile_max[channel] = tile_max[channel].max(sample);
            }
        }

        let mut residual_allowance = vec![0.0_f64; 4 * self.channels];
        for channel in 0..self.channels {
            let minimum = tile_min[channel];
            let maximum = tile_max[channel];
            let midpoint = 0.5 * minimum + 0.5 * maximum;
            // Bound deviations from the midpoint we actually rounded to,
            // rather than assuming `(max-min)/2` survived rounding outward.
            let radius = outward_nonnegative(
                (minimum - midpoint)
                    .abs()
                    .max((maximum - midpoint).abs()),
            );
            let magnitude = minimum.abs().max(maximum.abs());
            for phase in 0..4 {
                let plain = outward_nonnegative(
                    HEADROOM_REFERENCE_FAST_RESIDUAL_L1_UPPER[phase] * magnitude,
                );
                let centered_radius = outward_nonnegative(
                    HEADROOM_REFERENCE_FAST_RESIDUAL_L1_UPPER[phase] * radius,
                );
                let centered_dc = outward_nonnegative(
                    HEADROOM_REFERENCE_FAST_RESIDUAL_SUM_UPPER[phase] * midpoint.abs(),
                );
                let centered = outward_nonnegative(centered_radius + centered_dc);
                residual_allowance[phase * self.channels + channel] =
                    plain.min(centered);
            }
        }

        // Every coarse knot is itself a knot of the complete Reference
        // reconstruction. It therefore supplies both a measured point and a
        // certified lower bound after subtracting its execution enclosure.
        for index in coarse_start..=coarse_end {
            let base = Self::frame_offset(coarse_buffer_start, self.channels, index);
            for channel in 0..self.channels {
                let magnitude = self.coarse_values[base + channel].abs();
                let error = self.coarse_errors[base + channel];
                if !magnitude.is_finite() || !error.is_finite() {
                    self.numerical_invalid = true;
                    self.evaluated_upper_channel_peaks[channel] = f64::INFINITY;
                    continue;
                }
                self.point_channel_peaks[channel] =
                    self.point_channel_peaks[channel].max(magnitude);
                self.lower_channel_peaks[channel] = self.lower_channel_peaks[channel]
                    .max((magnitude - error).max(0.0));
                self.evaluated_upper_channel_peaks[channel] = self.evaluated_upper_channel_peaks
                    [channel]
                    .max(outward_nonnegative(magnitude + error));
            }
        }

        let interval_count = usize::try_from(coarse_end - coarse_start)
            .expect("canonical Reference tile interval count");
        if interval_count == 0 {
            return;
        }
        let mut interval_uppers = vec![0.0_f64; interval_count * self.channels];
        let mut candidates = Vec::<CandidateInterval>::new();
        let initial_threshold =
            HEADROOM_REFERENCE_FAST_PREFERRED_WIDTH_LINEAR * self.overall_lower();

        for offset in 0..interval_count {
            let interval_index = coarse_start + offset as i128;
            let phase = interval_index.rem_euclid(4) as usize;
            let mut score = 0.0_f64;
            for channel in 0..self.channels {
                let sample = |relative: i128| {
                    let base = Self::frame_offset(
                        coarse_buffer_start,
                        self.channels,
                        interval_index + relative,
                    );
                    self.coarse_values[base + channel]
                };
                let sample_error = |relative: i128| {
                    let base = Self::frame_offset(
                        coarse_buffer_start,
                        self.channels,
                        interval_index + relative,
                    );
                    self.coarse_errors[base + channel]
                };
                let ym1 = sample(-1);
                let y0 = sample(0);
                let y1 = sample(1);
                let y2 = sample(2);
                let d0 = y0 - ym1;
                let d1 = y1 - y0;
                let d2 = y2 - y1;
                let twice_d0 = d0 + d0;
                let twice_d1 = d1 + d1;
                let five_d1 = twice_d1 + twice_d1 + d1;
                let twice_d2 = d2 + d2;
                let b1 = y0 + (twice_d0 + five_d1 - d2) * (1.0 / 18.0);
                let b2 = y1 + (d0 - five_d1 - twice_d2) * (1.0 / 18.0);
                let bernstein = y0.abs().max(y1.abs()).max(b1.abs()).max(b2.abs());
                let coarse_error = sample_error(-1)
                    .max(sample_error(0))
                    .max(sample_error(1))
                    .max(sample_error(2));
                let scale = tile_min[channel]
                    .abs()
                    .max(tile_max[channel].abs())
                    .max(ym1.abs())
                    .max(y0.abs())
                    .max(y1.abs())
                    .max(y2.abs());
                let numerical =
                    scale * HEADROOM_REFERENCE_FAST_SCREEN_NUMERIC_ERROR_PER_SCALE_UPPER;
                let mut upper = bernstein
                    + residual_allowance[phase * self.channels + channel]
                    + HEADROOM_REFERENCE_FAST_CUBIC_ERROR_GAIN_UPPER * coarse_error
                    + numerical;
                if !upper.is_finite() {
                    upper = f64::INFINITY;
                } else {
                    upper = outward_nonnegative(upper);
                }
                interval_uppers[offset * self.channels + channel] = upper;
                score = score.max(upper);
            }
            if score > initial_threshold {
                candidates.push(CandidateInterval { offset, score });
            }
        }

        self.stats.candidate_intervals = self
            .stats
            .candidate_intervals
            .saturating_add(candidates.len() as u64);
        let candidate_order = |left: &CandidateInterval, right: &CandidateInterval| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.offset.cmp(&right.offset))
        };
        // Only the fixed refinement budget can be consumed. Partitioning the
        // best K in linear time avoids sorting every above-threshold interval
        // on adversarial flat/high-frequency material while preserving the
        // deterministic score/position ordering of the work we do perform.
        let best_omitted_score = if candidates.len()
            > HEADROOM_REFERENCE_FAST_MAX_REFINEMENTS_PER_TILE
        {
            let (_, best_omitted, _) = candidates.select_nth_unstable_by(
                HEADROOM_REFERENCE_FAST_MAX_REFINEMENTS_PER_TILE,
                candidate_order,
            );
            let score = best_omitted.score;
            candidates.truncate(HEADROOM_REFERENCE_FAST_MAX_REFINEMENTS_PER_TILE);
            Some(score)
        } else {
            None
        };
        candidates.sort_unstable_by(candidate_order);
        let mut refinements = 0_usize;
        let kernels = reference_tail_kernels();

        for candidate in &candidates {
            let threshold = HEADROOM_REFERENCE_FAST_PREFERRED_WIDTH_LINEAR * self.overall_lower();
            if candidate.score <= threshold {
                continue;
            }
            if refinements == HEADROOM_REFERENCE_FAST_MAX_REFINEMENTS_PER_TILE {
                break;
            }
            let interval_index = coarse_start + candidate.offset as i128;
            for channel in 0..self.channels {
                let coarse_base =
                    Self::frame_offset(coarse_buffer_start, self.channels, interval_index);
                let coarse_magnitude = self.coarse_values[coarse_base + channel].abs();
                let coarse_error = self.coarse_errors[coarse_base + channel];
                let mut exact_upper = outward_nonnegative(coarse_magnitude + coarse_error);
                for fine_phase in 1..16_usize {
                    let mut value = 0.0_f64;
                    let mut propagated_error = 0.0_f64;
                    let mut local_scale = 0.0_f64;
                    for offset in TAIL_OFFSET_MIN..=TAIL_OFFSET_MAX {
                        let coefficient = kernels.coefficients[fine_phase]
                            [(offset - TAIL_OFFSET_MIN) as usize];
                        if coefficient == 0.0 {
                            continue;
                        }
                        let base = Self::frame_offset(
                            coarse_buffer_start,
                            self.channels,
                            interval_index + i128::from(offset),
                        );
                        let sample = self.coarse_values[base + channel];
                        let error = self.coarse_errors[base + channel];
                        value += coefficient * sample;
                        propagated_error += coefficient.abs() * error;
                        local_scale = local_scale.max(sample.abs());
                    }
                    let error = propagated_error
                        + local_scale * HEADROOM_REFERENCE_FAST_TAIL_NUMERIC_ERROR_PER_SCALE_UPPER;
                    let magnitude = value.abs();
                    if !magnitude.is_finite() || !error.is_finite() {
                        exact_upper = f64::INFINITY;
                        continue;
                    }
                    self.point_channel_peaks[channel] =
                        self.point_channel_peaks[channel].max(magnitude);
                    self.lower_channel_peaks[channel] = self.lower_channel_peaks[channel]
                        .max((magnitude - error).max(0.0));
                    exact_upper = exact_upper.max(outward_nonnegative(magnitude + error));
                }
                self.evaluated_upper_channel_peaks[channel] =
                    self.evaluated_upper_channel_peaks[channel].max(exact_upper);
                // Zero marks this cell as resolved for the final unresolved-
                // upper reduction. The exact upper already lives in the
                // evaluated accumulator above.
                interval_uppers[candidate.offset * self.channels + channel] = 0.0;
            }
            refinements += 1;
            self.stats.refined_intervals = self.stats.refined_intervals.saturating_add(1);
        }

        if refinements == HEADROOM_REFERENCE_FAST_MAX_REFINEMENTS_PER_TILE {
            let threshold = HEADROOM_REFERENCE_FAST_PREFERRED_WIDTH_LINEAR * self.overall_lower();
            if best_omitted_score.is_some_and(|score| score > threshold) {
                self.stats.budget_exhausted_tiles =
                    self.stats.budget_exhausted_tiles.saturating_add(1);
            }
        }

        for offset in 0..interval_count {
            for channel in 0..self.channels {
                self.unresolved_upper_channel_peaks[channel] = self.unresolved_upper_channel_peaks
                    [channel]
                    .max(interval_uppers[offset * self.channels + channel]);
            }
        }
    }

    fn discard_consumed_prefix(&mut self) {
        let input_keep = self.next_tile_start_frame - HEADROOM_REFERENCE_FAST_INPUT_HALO_FRAMES;
        if let Some(start) = self.input_start_index {
            if input_keep > start {
                let frames = usize::try_from(input_keep - start).expect("bounded input drain");
                let samples = frames * self.channels;
                self.input_values.drain(..samples);
                self.input_start_index = Some(input_keep);
            }
        }

        let coarse_keep = self.next_tile_start_frame * 4 - HEADROOM_REFERENCE_FAST_COARSE_HALO;
        if let Some(start) = self.coarse_start_index {
            if coarse_keep > start {
                let frames = usize::try_from(coarse_keep - start).expect("bounded coarse drain");
                let samples = frames * self.channels;
                self.coarse_values.drain(..samples);
                self.coarse_errors.drain(..samples);
                self.coarse_start_index = Some(coarse_keep);
            }
        }
    }

    fn finalize(
        mut self,
        frames: u64,
        input_channel_peaks: &[f64],
        input_sample_peak: f64,
    ) -> Result<ReferenceFastScanResult, TruePeakError> {
        debug_assert_eq!(input_channel_peaks.len(), self.channels);
        self.process_ready_full_tiles();
        if self.numerical_invalid {
            return Err(TruePeakError::NumericalOverflow);
        }
        let last_frame = i128::from(frames - 1);
        let coarse_end = last_frame * 4;
        let coarse_start = self.next_tile_start_frame * 4;
        if coarse_start < coarse_end {
            if !self.ranges_available(
                self.next_tile_start_frame - HEADROOM_REFERENCE_FAST_INPUT_HALO_FRAMES,
                last_frame + HEADROOM_REFERENCE_FAST_INPUT_HALO_FRAMES,
                coarse_start - HEADROOM_REFERENCE_FAST_COARSE_HALO,
                coarse_end - 1 + HEADROOM_REFERENCE_FAST_COARSE_HALO,
            ) {
                return Err(TruePeakError::NumericalOverflow);
            }
            self.process_tile(self.next_tile_start_frame, last_frame, coarse_start, coarse_end);
        } else if coarse_start == 0 && coarse_end == 0 {
            let start = self.coarse_start_index.ok_or(TruePeakError::NumericalOverflow)?;
            if self.coarse_last_index().is_none_or(|end| end < 0) || start > 0 {
                return Err(TruePeakError::NumericalOverflow);
            }
            let base = Self::frame_offset(start, self.channels, 0);
            for channel in 0..self.channels {
                let magnitude = self.coarse_values[base + channel].abs();
                let error = self.coarse_errors[base + channel];
                let sample_peak = input_channel_peaks[channel];
                self.point_channel_peaks[channel] = magnitude.max(sample_peak);
                self.lower_channel_peaks[channel] =
                    (magnitude - error).max(0.0).max(sample_peak);
                self.evaluated_upper_channel_peaks[channel] =
                    outward_nonnegative(magnitude + error).max(sample_peak);
            }
        }

        if self.numerical_invalid {
            return Err(TruePeakError::NumericalOverflow);
        }

        for channel in 0..self.channels {
            let sample_peak = input_channel_peaks[channel];
            self.point_channel_peaks[channel] = self.point_channel_peaks[channel].max(sample_peak);
            self.lower_channel_peaks[channel] = self.lower_channel_peaks[channel].max(sample_peak);
            self.evaluated_upper_channel_peaks[channel] =
                self.evaluated_upper_channel_peaks[channel].max(sample_peak);
        }

        let numerical_allowance = input_sample_peak
            * HEADROOM64X_RECONSTRUCTION_NUMERIC_ERROR_PER_INPUT_PEAK_UPPER;
        let mut reconstruction_channel_linear_peaks = vec![0.0; self.channels];
        for channel in 0..self.channels {
            let sample_peak = input_channel_peaks[channel];
            let raw_upper = self.evaluated_upper_channel_peaks[channel]
                .max(self.unresolved_upper_channel_peaks[channel])
                .max(sample_peak);
            let upper = if raw_upper == 0.0 {
                0.0
            } else {
                next_up_nonnegative(raw_upper + numerical_allowance)
            };
            if !upper.is_finite() {
                return Err(TruePeakError::NumericalOverflow);
            }
            reconstruction_channel_linear_peaks[channel] = upper;
        }

        let raw_point_channels = self.point_channel_peaks;
        let point_channels = raw_point_channels
            .iter()
            .copied()
            .zip(input_channel_peaks.iter().copied())
            .map(|(raw, sample_peak)| {
                (raw * HEADROOM64_INTERPOLATION_CALIBRATION_LINEAR).max(sample_peak)
            })
            .collect::<Vec<_>>();
        let point_linear = point_channels.iter().copied().fold(0.0, f64::max);
        let point_overall = if point_linear == 0.0 {
            PeakLevel::Silence
        } else {
            PeakLevel::Finite {
                linear: point_linear,
                dbtp: 20.0 * point_linear.log10(),
            }
        };
        let reconstruction_linear = reconstruction_channel_linear_peaks
            .iter()
            .copied()
            .fold(0.0, f64::max);
        let reconstruction_upper = if reconstruction_linear == 0.0 {
            PeakLevel::Silence
        } else {
            PeakLevel::Finite {
                linear: reconstruction_linear,
                dbtp: 20.0 * reconstruction_linear.log10(),
            }
        };
        let lower_linear = self.lower_channel_peaks.iter().copied().fold(0.0, f64::max);
        let unresolved_linear = self
            .unresolved_upper_channel_peaks
            .iter()
            .copied()
            .fold(0.0, f64::max);
        let interval_width_db = if lower_linear > 0.0 {
            Some(20.0 * (reconstruction_linear / lower_linear).log10())
        } else {
            None
        };
        let reference_search_complete = unresolved_linear <= lower_linear;

        Ok(ReferenceFastScanResult {
            point_estimate: TruePeakResult {
                overall: point_overall,
                channel_linear_peaks: point_channels,
                frames,
            },
            reconstruction_upper,
            reconstruction_channel_linear_peaks,
            lower_linear,
            interval_width_db,
            reference_search_complete,
            stats: self.stats,
        })
    }
}

#[derive(Debug, Clone)]
pub(super) struct ReferenceFastCeilingMeter {
    edge_policy: EdgePolicy,
    engine: FftReference4Engine,
    scanner: ReferenceFastScanner,
    first_frame: Vec<f64>,
    last_frame: Vec<f64>,
    started: bool,
    next_input_index: i128,
    frames: u64,
    input_channel_peaks: Vec<f64>,
    input_sample_peak: f64,
}

impl ReferenceFastCeilingMeter {
    pub fn new(
        sample_rate_hz: u32,
        channels: usize,
        edge_policy: EdgePolicy,
    ) -> Result<Self, TruePeakError> {
        if sample_rate_hz == 0 {
            return Err(TruePeakError::InvalidSampleRate);
        }
        if channels == 0 {
            return Err(TruePeakError::InvalidChannelCount);
        }
        Ok(Self {
            edge_policy,
            engine: FftReference4Engine::new(channels),
            scanner: ReferenceFastScanner::new(channels),
            first_frame: vec![0.0; channels],
            last_frame: vec![0.0; channels],
            started: false,
            next_input_index: 0,
            frames: 0,
            input_channel_peaks: vec![0.0; channels],
            input_sample_peak: 0.0,
        })
    }

    pub fn push_interleaved(&mut self, samples: &[f64]) -> Result<(), TruePeakError> {
        let channels = self.last_frame.len();
        if samples.len() % channels != 0 {
            return Err(TruePeakError::IncompleteFrame {
                samples: samples.len(),
                channels,
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
            self.first_frame.copy_from_slice(&samples[..channels]);
            let extension = match self.edge_policy {
                EdgePolicy::RepeatEndpoints => self.first_frame.clone(),
                EdgePolicy::ZeroExtend => vec![0.0; channels],
            };
            for input_index in -HEADROOM_REFERENCE_FAST_INPUT_HALO_FRAMES..0 {
                self.scanner.observe_input(input_index, &extension, false);
                self.engine.process_frame(&extension, input_index, &mut self.scanner);
            }
            self.started = true;
        }

        for frame in samples.chunks_exact(channels) {
            let input_index = self.next_input_index;
            for (peak, sample) in self.input_channel_peaks.iter_mut().zip(frame.iter().copied()) {
                *peak = (*peak).max(sample.abs());
            }
            self.input_sample_peak = frame
                .iter()
                .copied()
                .map(f64::abs)
                .fold(self.input_sample_peak, f64::max);
            self.scanner.observe_input(input_index, frame, true);
            self.engine.process_frame(frame, input_index, &mut self.scanner);
            self.last_frame.copy_from_slice(frame);
            self.next_input_index += 1;
            self.frames = self.frames.checked_add(1).ok_or(TruePeakError::InputTooLong)?;
        }
        Ok(())
    }

    pub fn finalize(mut self) -> Result<ReferenceFastScanResult, TruePeakError> {
        if self.frames == 0 {
            return Err(TruePeakError::EmptyInput);
        }
        let channels = self.last_frame.len();
        let extension = match self.edge_policy {
            EdgePolicy::RepeatEndpoints => self.last_frame.clone(),
            EdgePolicy::ZeroExtend => vec![0.0; channels],
        };
        let stop = self.next_input_index + HEADROOM_REFERENCE_FAST_INPUT_HALO_FRAMES;
        for input_index in self.next_input_index..stop {
            self.scanner.observe_input(input_index, &extension, false);
            self.engine.process_frame(&extension, input_index, &mut self.scanner);
        }
        self.engine.flush(&mut self.scanner);
        self.scanner
            .finalize(self.frames, &self.input_channel_peaks, self.input_sample_peak)
    }
}
