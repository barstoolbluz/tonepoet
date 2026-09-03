//! Submitted-batch DSD peak-normalization primitives.
//!
//! Album scope is deliberately outside the qualified DSD Reference contract.
//! This module borrows the established reconstruction mechanics, but it does
//! not create or consume Reference attestations and never modifies the frozen
//! qualification corpus.

use crate::dsd_reference::{resolve_reference_profile, DbNano};
use crate::enums::{DsdAutoGainScope, DsdLowpassMethod, RateTarget};
use crate::error::{PlanningError, Result};
use crate::mapping;
use crate::plan::{
    CommandEnvironmentPolicy, InputSource, OutputSink, PlannedCommand,
};
use crate::settings::PipelineSettings;
use crate::source::SourceInfo;
use crate::tools::ToolIdentifier;
use std::path::Path;

/// One deterministic post-reconstruction peak report for album aggregation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AlbumPeakMeasurement {
    /// Finite calibrated Headroom64x point plus the independently named upper
    /// value for Tonepoet's declared finite reconstruction waveform.
    Finite {
        /// Existing Headroom64x point estimate, retained for reporting only.
        point_db: DbNano,
        /// Conservative linear peak upper bound of the declared signal-domain
        /// reconstruction. This value, not `point_db`, drives hard-ceiling gain.
        signal_upper_linear: f64,
    },
    /// The analyzer reported a completely silent signal (`-inf`).
    Silence,
}

/// Signal domain governed by album NormalizePeak after terminal realization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumCeilingDomain {
    /// Final stored lossless PCM, including deterministic quantization/dither
    /// error and its reconstructed-waveform contribution.
    LosslessStoredPcm,
    /// PCM presented to a lossy encoder. Decoded codec output is explicitly
    /// outside this ceiling contract because a lossy codec can create peaks.
    LossyEncoderInputPcm,
    /// Aggregate submitted scope contains both lossless stored-PCM and lossy
    /// encoder-input participants. The numeric fields are component-wise
    /// worst cases and therefore satisfy both domains.
    MixedPcm,
}

/// Deterministic terminal realization bound used by hard-ceiling arithmetic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlbumTerminalBound {
    /// Maximum reconstructed-waveform contribution introduced before the
    /// fixed gain is applied (for example SoX's Float64 -> internal Int32
    /// realization). It belongs inside the gain term of the ceiling algebra.
    pub pre_gain_reconstructed_error_linear: f64,
    /// Maximum absolute error of a final stored sample where that concept
    /// exists. Lossy encoder-input contracts use `None` because decoded codec
    /// output is not a stored PCM sample governed by this authority.
    pub stored_sample_error_linear: Option<f64>,
    /// Maximum reconstructed-waveform contribution introduced after the fixed
    /// gain (quantization, dither, target sample-format realization).
    pub post_gain_reconstructed_error_linear: f64,
    /// Exact terminal domain governed by the authority.
    pub domain: AlbumCeilingDomain,
}

/// Resolved fixed gain shared by every DSD track in one submitted batch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlbumGainAuthority {
    /// Peak target selected by the user-facing NormalizePeak control. This is
    /// preserved exactly; it is never overwritten with an internal reserve.
    pub target_dbfs: DbNano,
    /// Loudest calibrated Headroom64x point, for reporting only.
    pub loudest_peak_dbfs: Option<DbNano>,
    /// Loudest proved signal-domain upper bound in linear full-scale amplitude.
    pub loudest_signal_upper_linear: f64,
    /// Worst terminal realization bound across participating outputs.
    pub terminal_bound: AlbumTerminalBound,
    /// Maximum linear gain permitted by the proved inequality before fixed-
    /// point conversion.
    pub maximum_linear_gain: f64,
    /// Fixed conservatively rounded gain applied uniformly to all tracks.
    pub gain_db: DbNano,
    /// Number of DSD tracks represented by this authority.
    pub track_count: usize,
}

/// Derive one fixed album gain from a complete measurement set.
///
/// For finite material the hard-ceiling inequality is evaluated in linear
/// amplitude:
///
/// `G * (P_signal + E_pre) + E_post <= C`
///
/// where `C` is the requested ceiling, `P_signal` is the loudest conservative
/// reconstruction bound, `E_pre` is any terminal realization error introduced
/// before gain, and `E_post` is the reconstructed error introduced after gain. The resulting `G` is converted to `DbNano` only in the conservative
/// direction. The calibrated Headroom64 point remains available for reporting
/// but has no hard-upper-bound semantics here.
///
/// An all-silent set receives exactly 0 dB, provided the terminal realization
/// bound itself fits beneath the requested ceiling.
pub fn resolve_album_gain(
    target_dbfs: DbNano,
    measurements: &[AlbumPeakMeasurement],
    terminal_bound: AlbumTerminalBound,
) -> std::result::Result<AlbumGainAuthority, String> {
    let participants = measurements
        .iter()
        .copied()
        .map(|measurement| (measurement, terminal_bound))
        .collect::<Vec<_>>();
    resolve_album_gain_constraints(target_dbfs, &participants)
}

/// Derive one fixed album gain when submitted participants have different
/// terminal realizations.
///
/// Each measurement is paired with the terminal bound of the output that will
/// consume it. The shared album gain is the minimum permitted gain across the
/// individual inequalities
///
/// `G * (P_signal_i + E_pre_i) + E_post_i <= C`.
///
/// Pairing the terms avoids the unnecessary conservatism of combining the
/// loudest signal from one track with the largest terminal error from another.
/// The returned `terminal_bound` is only a component-wise diagnostic summary;
/// the gain itself is resolved from the paired constraints above.
pub fn resolve_album_gain_constraints(
    target_dbfs: DbNano,
    participants: &[(AlbumPeakMeasurement, AlbumTerminalBound)],
) -> std::result::Result<AlbumGainAuthority, String> {
    if participants.is_empty() {
        return Err("album DSD gain requires at least one measured DSD track".to_string());
    }
    if !(DbNano::MIN_NORMALIZE_TARGET..=DbNano::MAX_NORMALIZE_TARGET).contains(&target_dbfs) {
        return Err(
            "album DSD NormalizePeak target must be between -12.000000000 and 0.000000000 dBTP"
                .to_string(),
        );
    }

    let ceiling_linear = db_nano_to_linear_lower(target_dbfs)?;
    let mut loudest_peak_dbfs = None;
    let mut loudest_signal_upper_linear = 0.0_f64;
    let mut permitted_linear_gain: Option<f64> = None;
    let mut aggregate_terminal_bound: Option<AlbumTerminalBound> = None;

    for (measurement, terminal_bound) in participants.iter().copied() {
        validate_terminal_bound(terminal_bound)?;
        if terminal_bound.post_gain_reconstructed_error_linear >= ceiling_linear {
            return Err(format!(
                "album DSD post-gain terminal realization bound ({:.12e} FS) leaves no room beneath requested ceiling {} dBTP",
                terminal_bound.post_gain_reconstructed_error_linear,
                target_dbfs.render(false),
            ));
        }

        aggregate_terminal_bound = Some(match aggregate_terminal_bound {
            None => terminal_bound,
            Some(current) => merge_terminal_bounds(current, terminal_bound),
        });

        let AlbumPeakMeasurement::Finite {
            point_db,
            signal_upper_linear,
        } = measurement
        else {
            continue;
        };
        if !signal_upper_linear.is_finite() || signal_upper_linear <= 0.0 {
            return Err("album DSD signal ceiling bound is invalid".to_string());
        }

        loudest_peak_dbfs = Some(
            loudest_peak_dbfs
                .map(|current: DbNano| current.max(point_db))
                .unwrap_or(point_db),
        );
        loudest_signal_upper_linear = loudest_signal_upper_linear.max(signal_upper_linear);

        let numerator = next_down_positive(
            ceiling_linear - terminal_bound.post_gain_reconstructed_error_linear,
        );
        if !(numerator > 0.0) {
            return Err("album DSD hard-ceiling numerator is not positive".to_string());
        }
        let effective_signal_upper = next_up_nonnegative(
            signal_upper_linear + terminal_bound.pre_gain_reconstructed_error_linear,
        );
        let participant_gain = next_down_positive(numerator / effective_signal_upper);
        if !(participant_gain > 0.0) || !participant_gain.is_finite() {
            return Err("album DSD hard-ceiling linear gain is invalid".to_string());
        }
        permitted_linear_gain = Some(
            permitted_linear_gain
                .map(|current| current.min(participant_gain))
                .unwrap_or(participant_gain),
        );
    }

    let terminal_bound = aggregate_terminal_bound
        .expect("nonempty participant set always produces a terminal-bound summary");
    let (maximum_linear_gain, gain_db) = match permitted_linear_gain {
        None => (1.0, DbNano::ZERO),
        Some(maximum_linear_gain) => (
            maximum_linear_gain,
            linear_gain_to_db_nano_down(maximum_linear_gain)?,
        ),
    };

    Ok(AlbumGainAuthority {
        target_dbfs,
        loudest_peak_dbfs,
        loudest_signal_upper_linear,
        terminal_bound,
        maximum_linear_gain,
        gain_db,
        track_count: participants.len(),
    })
}

fn validate_terminal_bound(terminal_bound: AlbumTerminalBound) -> std::result::Result<(), String> {
    if !terminal_bound.pre_gain_reconstructed_error_linear.is_finite()
        || terminal_bound.pre_gain_reconstructed_error_linear < 0.0
        || !terminal_bound.post_gain_reconstructed_error_linear.is_finite()
        || terminal_bound.post_gain_reconstructed_error_linear < 0.0
        || terminal_bound
            .stored_sample_error_linear
            .is_some_and(|value| !value.is_finite() || value < 0.0)
    {
        return Err("album DSD terminal ceiling bound is invalid".to_string());
    }
    Ok(())
}

fn merge_terminal_bounds(left: AlbumTerminalBound, right: AlbumTerminalBound) -> AlbumTerminalBound {
    AlbumTerminalBound {
        pre_gain_reconstructed_error_linear: left
            .pre_gain_reconstructed_error_linear
            .max(right.pre_gain_reconstructed_error_linear),
        stored_sample_error_linear: match (
            left.stored_sample_error_linear,
            right.stored_sample_error_linear,
        ) {
            (Some(left), Some(right)) => Some(left.max(right)),
            _ => None,
        },
        post_gain_reconstructed_error_linear: left
            .post_gain_reconstructed_error_linear
            .max(right.post_gain_reconstructed_error_linear),
        domain: if left.domain == right.domain {
            left.domain
        } else {
            AlbumCeilingDomain::MixedPcm
        },
    }
}

// `ln(10)` enclosure used by the directed exponential below. These decimal
// constants straddle the mathematical value
// 2.3025850929940456840179914546843642...
const LN_10_LOWER: f64 = 2.302_585_092_994_045;
const LN_10_UPPER: f64 = 2.302_585_092_994_046;
const DB_NANO_EXP_DENOMINATOR: f64 = 20_000_000_000.0;
const EXP_TAYLOR_TERMS: u32 = 18;
const EXP_REDUCED_MAX: f64 = 0.0625;
// Far beyond any meaningful audio gain while keeping the integer magnitude
// exactly representable as binary64 during interval conversion.
const MAX_INTERVAL_DB_NANO_MAGNITUDE: u64 = 6_000_000_000_000;
// Runtime SoX/FFmpeg parse + binary64 gain realization is many orders of
// magnitude smaller on supported platforms. Keeping sixteen additional
// nanodecibels below the mathematical bound makes that implementation layer
// explicit without creating audible or practically measurable headroom.
const GAIN_REALIZATION_GUARD_NANODB: i64 = 16;

fn db_nano_to_linear_lower(db: DbNano) -> std::result::Result<f64, String> {
    db_nano_to_linear_interval(db).map(|(lower, _)| lower)
}

fn db_nano_to_linear_upper(db: DbNano) -> std::result::Result<f64, String> {
    db_nano_to_linear_interval(db).map(|(_, upper)| upper)
}

/// Outward-rounded interval for the mathematical amplitude `10^(dB/20)`.
///
/// No `powf`, `exp`, or `log` correctness assumption participates in the
/// ceiling proof. The dB exponent is reduced until <= 1/16, enclosed with a
/// positive Taylor series plus a geometric remainder, then restored by exact-
/// count repeated squaring with every arithmetic operation rounded outward by
/// one binary64 representable value. Negative exponents are handled by
/// reciprocal interval inversion.
fn db_nano_to_linear_interval(db: DbNano) -> std::result::Result<(f64, f64), String> {
    if db == DbNano::ZERO {
        return Ok((1.0, 1.0));
    }

    let magnitude = db.0.unsigned_abs();
    if magnitude > MAX_INTERVAL_DB_NANO_MAGNITUDE {
        return Err("album DSD dB value is outside the directed linear-conversion domain".to_string());
    }
    let magnitude = magnitude as f64;
    let product_lower = mul_down_positive(magnitude, LN_10_LOWER);
    let product_upper = mul_up_nonnegative(magnitude, LN_10_UPPER);
    let y_lower = div_down_positive(product_lower, DB_NANO_EXP_DENOMINATOR);
    let y_upper = div_up_nonnegative(product_upper, DB_NANO_EXP_DENOMINATOR);
    let (exp_lower, exp_upper) = exp_positive_interval(y_lower, y_upper)?;

    if db.0 > 0 {
        Ok((exp_lower, exp_upper))
    } else {
        let lower = div_down_positive(1.0, exp_upper);
        let upper = div_up_nonnegative(1.0, exp_lower);
        Ok((lower, upper))
    }
}

fn exp_positive_interval(
    mut y_lower: f64,
    mut y_upper: f64,
) -> std::result::Result<(f64, f64), String> {
    if !y_lower.is_finite() || !y_upper.is_finite() || y_lower < 0.0 || y_lower > y_upper {
        return Err("album DSD directed exponential input is invalid".to_string());
    }
    if y_upper == 0.0 {
        return Ok((1.0, 1.0));
    }

    let mut squarings = 0_u32;
    while y_upper > EXP_REDUCED_MAX {
        // Division by two is exact for normal binary64 values; outward helpers
        // keep the invariant explicit if this domain ever expands to subnormal.
        y_lower = div_down_positive(y_lower, 2.0);
        y_upper = div_up_nonnegative(y_upper, 2.0);
        squarings += 1;
        if squarings > 20 {
            return Err("album DSD directed exponential range reduction overflowed".to_string());
        }
    }

    let mut term_lower = 1.0;
    let mut term_upper = 1.0;
    let mut sum_lower = 1.0;
    let mut sum_upper = 1.0;
    for n in 1..=EXP_TAYLOR_TERMS {
        term_lower = div_down_positive(
            mul_down_positive(term_lower, y_lower),
            f64::from(n),
        );
        term_upper = div_up_nonnegative(
            mul_up_nonnegative(term_upper, y_upper),
            f64::from(n),
        );
        sum_lower = add_down_positive(sum_lower, term_lower);
        sum_upper = add_up_nonnegative(sum_upper, term_upper);
    }

    // All omitted exp terms are positive. Bound their tail by the first
    // omitted term times a geometric series whose ratio dominates every later
    // term ratio on z <= 1/16.
    let first_tail_upper = div_up_nonnegative(
        mul_up_nonnegative(term_upper, y_upper),
        f64::from(EXP_TAYLOR_TERMS + 1),
    );
    let ratio_upper = div_up_nonnegative(y_upper, f64::from(EXP_TAYLOR_TERMS + 2));
    let denominator_lower = sub_down_positive(1.0, ratio_upper);
    let tail_upper = div_up_nonnegative(first_tail_upper, denominator_lower);
    sum_upper = add_up_nonnegative(sum_upper, tail_upper);

    for _ in 0..squarings {
        sum_lower = mul_down_positive(sum_lower, sum_lower);
        sum_upper = mul_up_nonnegative(sum_upper, sum_upper);
        if !sum_upper.is_finite() {
            return Err("album DSD directed exponential overflowed".to_string());
        }
    }
    Ok((sum_lower, sum_upper))
}

fn linear_gain_to_db_nano_down(maximum_linear_gain: f64) -> std::result::Result<DbNano, String> {
    let db = 20.0 * maximum_linear_gain.log10();
    if !db.is_finite() {
        return Err("album DSD gain could not be represented in decibels".to_string());
    }
    let scaled = db * 1_000_000_000.0;
    if scaled < i64::MIN as f64 || scaled > i64::MAX as f64 {
        return Err("album DSD gain is outside DbNano range".to_string());
    }

    // log10 is only a seed: it does not participate in the safety decision.
    // Move below the seed by an explicit runtime-realization guard, then prove
    // the candidate's mathematical linear amplitude with the directed exp
    // interval. If the seed happened to round high, walk downward until the
    // upper endpoint is within the proved maximum.
    let mut nano = (scaled.floor() as i64)
        .checked_sub(GAIN_REALIZATION_GUARD_NANODB)
        .ok_or_else(|| "album DSD conservative gain rounding overflowed".to_string())?;
    loop {
        let candidate_upper = db_nano_to_linear_upper(DbNano(nano))?;
        if candidate_upper <= maximum_linear_gain {
            return Ok(DbNano(nano));
        }
        nano = nano
            .checked_sub(1)
            .ok_or_else(|| "album DSD conservative gain rounding overflowed".to_string())?;
    }
}

fn next_down_positive(value: f64) -> f64 {
    debug_assert!(value.is_finite() && value > 0.0);
    f64::from_bits(value.to_bits() - 1)
}

fn next_up_nonnegative(value: f64) -> f64 {
    debug_assert!(value >= 0.0 && !value.is_nan());
    if value == f64::INFINITY {
        value
    } else if value == 0.0 {
        f64::from_bits(1)
    } else {
        f64::from_bits(value.to_bits() + 1)
    }
}

fn add_down_positive(left: f64, right: f64) -> f64 {
    next_down_positive(left + right)
}

fn add_up_nonnegative(left: f64, right: f64) -> f64 {
    next_up_nonnegative(left + right)
}

fn sub_down_positive(left: f64, right: f64) -> f64 {
    let value = left - right;
    debug_assert!(value > 0.0);
    next_down_positive(value)
}

fn mul_down_positive(left: f64, right: f64) -> f64 {
    let value = left * right;
    debug_assert!(value > 0.0);
    next_down_positive(value)
}

fn mul_up_nonnegative(left: f64, right: f64) -> f64 {
    next_up_nonnegative(left * right)
}

fn div_down_positive(numerator: f64, denominator: f64) -> f64 {
    let value = numerator / denominator;
    debug_assert!(value > 0.0);
    next_down_positive(value)
}

fn div_up_nonnegative(numerator: f64, denominator: f64) -> f64 {
    next_up_nonnegative(numerator / denominator)
}

/// Resolve the PCM rate at which album peak authority must be measured.
///
/// Measurement happens after the same DSD reconstruction/rate conversion the
/// ordinary track path would use. For lossless output that is the requested
/// final PCM rate; for lossy hard-ceiling output the rate must also be a
/// direct input rate of the configured encoder.
pub fn album_gain_target_rate_hz(
    settings: &PipelineSettings,
    source: &SourceInfo,
) -> Result<u32> {
    if !source.is_dsd() {
        return Err(PlanningError::invalid_source(
            "source",
            "album DSD peak analysis requires a DSD source",
        ));
    }
    let target_rate_hz = match settings.target_sample_rate {
        RateTarget::PcmHz(hz) => Ok(hz),
        RateTarget::Source => source
            .dsd_rate()
            .map(crate::enums::DsdRate::default_pcm_target_hz)
            .ok_or_else(|| {
                PlanningError::invalid_source(
                    "sample_rate_hz",
                    "album DSD peak analysis requires a known DSD rate or explicit PCM target rate",
                )
            }),
        RateTarget::Dsd(_) => Err(PlanningError::invalid_settings(
            "target_sample_rate",
            "album DSD peak normalization requires a PCM target rate",
        )),
    }?;

    if settings.target_format.is_lossy()
        && mapping::ffmpeg_lossy_encoder_accepts_rate_directly(
            &settings.target_format,
            target_rate_hz,
        ) != Some(true)
    {
        return Err(PlanningError::invalid_settings(
            "target_sample_rate",
            format!(
                "album-scoped DSD NormalizePeak requires the retained carrier rate to equal the lossy encoder-input PCM rate; {} at {} Hz would require FFmpeg to resample after the proved gain",
                settings.target_format,
                target_rate_hz,
            ),
        ));
    }

    Ok(target_rate_hz)
}

/// Build the single expensive decode used by album-scoped DSD normalization.
///
/// The command writes a headerless little-endian Float64 carrier. The root
/// crate streams that retained carrier through the standalone true-peak meter
/// after validating frame alignment; SoX text output is not measurement
/// authority for album gain.
/// Headerless PCM deliberately avoids both container-size ceilings and the
/// cross-tool Float64 container interpretation differences exercised by this
/// pipeline. The orchestrator retains the authoritative sample rate and
/// channel count beside the carrier, so consumers never need to infer them
/// from the file. No normalization gain or output dither is applied in this
/// pass; the submitted-batch barrier binds one fixed gain only after every
/// participating track has reported its peak.
pub fn build_album_gain_analysis_command(
    settings: &PipelineSettings,
    source: &SourceInfo,
    input: &Path,
    output: &Path,
    duration: Option<std::time::Duration>,
) -> Result<PlannedCommand> {
    if settings.dsd.auto_gain_scope() != DsdAutoGainScope::Album
        || !settings.dsd.album_auto_gain_selected()
    {
        return Err(PlanningError::invalid_settings(
            "dsd.auto_gain_scope",
            "album DSD peak analysis requires an active album-scoped automatic gain mode",
        ));
    }
    let target_rate_hz = album_gain_target_rate_hz(settings, source)?;
    let mut args = vec![
        "-S".to_string(),
        "-D".to_string(),
        input.display().to_string(),
        "-t".to_string(),
        "raw".to_string(),
        "-e".to_string(),
        "floating-point".to_string(),
        "-b".to_string(),
        "64".to_string(),
        "-L".to_string(),
        output.display().to_string(),
    ];

    if settings.dsd.is_native_v2() {
        let source_rate = source.dsd_rate().ok_or_else(|| {
            PlanningError::invalid_source(
                "sample_rate_hz",
                "native album DSD peak analysis requires a recognized DSD source rate",
            )
        })?;
        let profile = resolve_reference_profile(
            source_rate,
            target_rate_hz,
            settings.dsd.from_dsd.profile,
        )?;
        // Match the native reconstruction front end while remaining outside
        // qualified Reference policy/attestation. The later shared fixed gain
        // restores whatever level the aggregate target requires.
        args.extend([
            "gain".to_string(),
            DbNano::REFERENCE_HEADROOM.render(false),
            "rate".to_string(),
            "-u".to_string(),
            target_rate_hz.to_string(),
        ]);
        if let Some((transition_hz, center_hz)) = profile.sinc() {
            args.extend([
                "sinc".to_string(),
                "-a".to_string(),
                "180".to_string(),
                "-L".to_string(),
                "-t".to_string(),
                transition_hz.to_string(),
                format!("-{center_hz}"),
            ]);
        }
    } else {
        add_legacy_reconstruction_effects(settings, source, &mut args, target_rate_hz);
    }
    let mut command = PlannedCommand::new(
        ToolIdentifier::Sox,
        args,
        InputSource::Path(input.to_path_buf()),
        OutputSink::Path(output.to_path_buf()),
        duration,
        "Decode DSD once for submitted-batch album true-peak analysis",
    );
    command.environment_policy = CommandEnvironmentPolicy::ClearAndSet;
    command.environment.insert("LC_ALL".to_string(), "C".to_string());
    Ok(command)
}

fn add_legacy_reconstruction_effects(
    settings: &PipelineSettings,
    source: &SourceInfo,
    args: &mut Vec<String>,
    target_rate_hz: u32,
) {
    match settings.dsd.legacy_dsd_to_pcm_lowpass() {
        DsdLowpassMethod::Sinc => {
            let sinc = settings.dsd.pcm_to_dsd.sinc;
            args.push("sinc".to_string());
            args.push(format!("-{:.0}", sinc.passband_hz));
            args.push("-n".to_string());
            args.push(sinc.taps.to_string());
            args.push("-t".to_string());
            args.push(format_float(sinc.transition_hz));
            if sinc.linear_phase {
                args.push("-L".to_string());
            } else {
                args.push("-M".to_string());
            }
            args.push("-b".to_string());
            args.push(format_float(sinc.kaiser_beta));
            args.push("rate".to_string());
            args.push("-I".to_string());
            args.push(target_rate_hz.to_string());
        }
        lowpass @ (DsdLowpassMethod::Auto | DsdLowpassMethod::SoxUltra) => {
            args.push("rate".to_string());
            args.push(
                mapping::sox_dsd_lowpass_rate_flag(lowpass, settings.resample_quality).to_string(),
            );
            args.push(target_rate_hz.to_string());
            if let Some(dsd_rate) = source.dsd_rate() {
                if let Some(lowpass_hz) = dsd_rate.default_pcm_lowpass_hz() {
                    if u64::from(lowpass_hz) < u64::from(target_rate_hz) / 2 {
                        args.extend([
                            "sinc".to_string(),
                            "-a".to_string(),
                            "180".to_string(),
                            format!("-{lowpass_hz}"),
                        ]);
                    }
                }
            }
        }
    }
}

fn format_float(value: f32) -> String {
    let mut text = format!("{value:.6}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(raw: &str) -> DbNano {
        raw.parse().expect("valid test dB")
    }

    #[test]
    fn album_analysis_carrier_is_headerless_little_endian_float64() {
        let mut settings = PipelineSettings::default();
        settings.target_sample_rate = RateTarget::PcmHz(96_000);
        settings
            .dsd
            .set_legacy_dsd_to_pcm_gain(
                crate::enums::DsdToPcmGainMode::Auto,
                0.15,
                None,
            )
            .expect("album auto gain settings");
        settings.dsd.set_auto_gain_scope(DsdAutoGainScope::Album);
        let source = SourceInfo {
            dsd_source_kind: None,
            format: crate::enums::AudioFormat::Dsf,
            codec: crate::enums::AudioCodec::Dsd,
            sample_rate_hz: Some(crate::enums::DsdRate::Dsd64.hz()),
            bit_depth: None,
            true_source_depth: None,
            source_representation: crate::source::SourceRepresentationKind::Dsd,
            sample_kind: Some(crate::enums::SampleKind::Dsd),
            channels: Some(2),
            duration: None,
            audio_md5: None,
        };
        let command = build_album_gain_analysis_command(
            &settings,
            &source,
            std::path::Path::new("input.dsf"),
            std::path::Path::new("carrier.f64le"),
            None,
        )
        .expect("album analysis command");

        let output_index = command
            .args
            .iter()
            .position(|arg| arg == "carrier.f64le")
            .expect("carrier output path");
        let output_contract = command.args[2..=output_index]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            output_contract,
            vec![
                "input.dsf",
                "-t",
                "raw",
                "-e",
                "floating-point",
                "-b",
                "64",
                "-L",
                "carrier.f64le",
            ],
            "{:?}",
            command.args,
        );
        assert!(
            !command.args.iter().any(|arg| arg.eq_ignore_ascii_case("caf")),
            "album carrier must not depend on a Float64 container contract: {:?}",
            command.args,
        );
        assert!(
            !command.args.iter().any(|arg| arg == "stats"),
            "production album analysis must not depend on SoX stats: {:?}",
            command.args,
        );
    }

    #[test]
    fn lossy_album_gain_rejects_rate_that_encoder_would_resample_after_gain() {
        let mut settings = PipelineSettings::default();
        settings.target_format = crate::enums::AudioFormat::Aac;
        settings.target_sample_rate = RateTarget::PcmHz(192_000);
        let source = SourceInfo {
            dsd_source_kind: None,
            format: crate::enums::AudioFormat::Dsf,
            codec: crate::enums::AudioCodec::Dsd,
            sample_rate_hz: Some(crate::enums::DsdRate::Dsd64.hz()),
            bit_depth: None,
            true_source_depth: None,
            source_representation: crate::source::SourceRepresentationKind::Dsd,
            sample_kind: Some(crate::enums::SampleKind::Dsd),
            channels: Some(2),
            duration: None,
            audio_md5: None,
        };

        let error = album_gain_target_rate_hz(&settings, &source)
            .expect_err("AAC 192 kHz would require post-gain FFmpeg resampling");
        assert!(error.to_string().contains("resample after the proved gain"), "{error}");
    }

    #[test]
    fn lossy_album_gain_accepts_direct_encoder_input_rate() {
        let mut settings = PipelineSettings::default();
        settings.target_format = crate::enums::AudioFormat::Aac;
        settings.target_sample_rate = RateTarget::PcmHz(96_000);
        let source = SourceInfo {
            dsd_source_kind: None,
            format: crate::enums::AudioFormat::Dsf,
            codec: crate::enums::AudioCodec::Dsd,
            sample_rate_hz: Some(crate::enums::DsdRate::Dsd64.hz()),
            bit_depth: None,
            true_source_depth: None,
            source_representation: crate::source::SourceRepresentationKind::Dsd,
            sample_kind: Some(crate::enums::SampleKind::Dsd),
            channels: Some(2),
            duration: None,
            audio_md5: None,
        };

        assert_eq!(album_gain_target_rate_hz(&settings, &source).unwrap(), 96_000);
    }

    fn zero_terminal() -> AlbumTerminalBound {
        AlbumTerminalBound {
            pre_gain_reconstructed_error_linear: 0.0,
            stored_sample_error_linear: Some(0.0),
            post_gain_reconstructed_error_linear: 0.0,
            domain: AlbumCeilingDomain::LosslessStoredPcm,
        }
    }

    fn finite_db(raw: &str) -> AlbumPeakMeasurement {
        let point_db = db(raw);
        let linear = 10.0_f64.powf(point_db.0 as f64 / 20_000_000_000.0);
        AlbumPeakMeasurement::Finite {
            point_db,
            signal_upper_linear: next_up_nonnegative(linear),
        }
    }

    fn gain_linear(gain: DbNano) -> f64 {
        10.0_f64.powf(gain.0 as f64 / 20_000_000_000.0)
    }

    #[test]
    fn loudest_signal_upper_drives_album_gain_without_overwriting_point_reporting() {
        let authority = resolve_album_gain(
            db("-0.150000000"),
            &[
                finite_db("-12.000000000"),
                finite_db("-3.250000000"),
                finite_db("-7.000000000"),
            ],
            zero_terminal(),
        )
        .expect("album authority");
        assert_eq!(authority.loudest_peak_dbfs, Some(db("-3.250000000")));
        assert!(authority.gain_db <= db("3.100000000"));
        assert!(authority.gain_db >= db("3.099999970"));
        assert_eq!(authority.track_count, 3);
    }

    #[test]
    fn paired_terminal_constraints_avoid_cross_track_worst_case_conservatism() {
        let high_signal = AlbumPeakMeasurement::Finite {
            point_db: db("0.000000000"),
            signal_upper_linear: 1.0,
        };
        let low_signal = AlbumPeakMeasurement::Finite {
            point_db: db("-20.000000000"),
            signal_upper_linear: 0.1,
        };
        let zero = zero_terminal();
        let noisy = AlbumTerminalBound {
            pre_gain_reconstructed_error_linear: 0.0,
            stored_sample_error_linear: Some(0.2),
            post_gain_reconstructed_error_linear: 0.2,
            domain: AlbumCeilingDomain::LosslessStoredPcm,
        };

        let paired = resolve_album_gain_constraints(
            DbNano::ZERO,
            &[(high_signal, zero), (low_signal, noisy)],
        )
        .unwrap();
        let reversed = resolve_album_gain_constraints(
            DbNano::ZERO,
            &[(low_signal, noisy), (high_signal, zero)],
        )
        .unwrap();
        // Each participant is safe at essentially unity: the high signal has
        // no terminal error, while the large terminal error belongs to the
        // much quieter signal. Only the nanodecibel realization guard remains.
        assert!(paired.gain_db >= db("-0.000000020"), "gain={}", paired.gain_db);
        assert_eq!(paired.gain_db, reversed.gain_db);

        // The compatibility wrapper deliberately applies one common terminal
        // bound to every measurement. Demonstrate why production must not use
        // that component-wise worst case for heterogeneous outputs.
        let componentwise = resolve_album_gain(
            DbNano::ZERO,
            &[high_signal, low_signal],
            noisy,
        )
        .unwrap();
        assert!(componentwise.gain_db < db("-1.900000000"));
        assert!(paired.gain_db > componentwise.gain_db);
    }

    #[test]
    fn album_gain_is_uniform_and_preserves_intertrack_level_difference() {
        let quiet = db("-18.000000000");
        let loud = db("-6.000000000");
        let authority = resolve_album_gain(
            db("-0.150000000"),
            &[finite_db("-18.000000000"), finite_db("-6.000000000")],
            zero_terminal(),
        )
        .expect("album authority");
        let quiet_after = quiet.checked_add(authority.gain_db).unwrap();
        let loud_after = loud.checked_add(authority.gain_db).unwrap();
        assert!(loud_after <= db("-0.150000000"));
        assert_eq!(loud.checked_sub(quiet), loud_after.checked_sub(quiet_after));
    }

    #[test]
    fn zero_target_uses_linear_signal_and_terminal_bounds() {
        let terminal = AlbumTerminalBound {
            pre_gain_reconstructed_error_linear: 0.0,
            stored_sample_error_linear: Some(1.0e-6),
            post_gain_reconstructed_error_linear: 4.1e-6,
            domain: AlbumCeilingDomain::LosslessStoredPcm,
        };
        let measurement = AlbumPeakMeasurement::Finite {
            point_db: db("-0.004000000"),
            signal_upper_linear: 1.0,
        };
        let authority = resolve_album_gain(db("0.000000000"), &[measurement], terminal)
            .expect("zero ceiling authority");

        let final_upper = next_up_nonnegative(
            next_up_nonnegative(gain_linear(authority.gain_db) * authority.loudest_signal_upper_linear)
                + terminal.post_gain_reconstructed_error_linear,
        );
        assert!(final_upper <= 1.0, "final upper {final_upper:.16} exceeded 0 dBFS");
        assert!(authority.gain_db < DbNano::ZERO);
        assert_eq!(authority.target_dbfs, DbNano::ZERO);
    }

    #[test]
    fn raw_point_subtraction_mutation_would_break_zero_ceiling() {
        let measurement = AlbumPeakMeasurement::Finite {
            point_db: db("-0.004000000"),
            signal_upper_linear: 1.0,
        };
        let terminal = AlbumTerminalBound {
            pre_gain_reconstructed_error_linear: 0.0,
            stored_sample_error_linear: Some(0.0),
            post_gain_reconstructed_error_linear: 0.0,
            domain: AlbumCeilingDomain::LosslessStoredPcm,
        };
        let corrected = resolve_album_gain(DbNano::ZERO, &[measurement], terminal).unwrap();
        let naive = DbNano::ZERO.checked_sub(db("-0.004000000")).unwrap();

        assert!(corrected.gain_db < DbNano::ZERO);
        assert!(gain_linear(naive) * 1.0 > 1.0);
    }

    #[test]
    fn directed_db_to_linear_interval_contains_frozen_high_precision_values() {
        // Values were frozen from a 70-digit Decimal exp(ln(10) * dB / 20)
        // calculation in qualification/verify_ceiling_contract.py.
        for (raw, reference) in [
            ("-12.000000000", 0.251_188_643_150_958_0_f64),
            ("-0.150000000", 0.982_878_873_000_032_2_f64),
            ("3.141592653", 1.435_752_670_196_839_7_f64),
            ("24.000000000", 15.848_931_924_611_133_f64),
        ] {
            let (lower, upper) = db_nano_to_linear_interval(db(raw)).unwrap();
            assert!(lower <= reference, "{raw}: {lower:.17e} > {reference:.17e}");
            assert!(upper >= reference, "{raw}: {upper:.17e} < {reference:.17e}");
            assert!(
                (upper - lower) / reference < 6.0e-13,
                "{raw}: interval too loose: [{lower:.17e}, {upper:.17e}]"
            );
        }
        assert_eq!(db_nano_to_linear_interval(DbNano::ZERO).unwrap(), (1.0, 1.0));
    }

    #[test]
    fn dbnano_conversion_is_directional_at_nanodecibel_boundaries() {
        for raw in ["-12.345678901", "-0.150000000", "0.000000000", "3.141592653"] {
            let exact = db(raw);
            let max_linear = 10.0_f64.powf(exact.0 as f64 / 20_000_000_000.0);
            let rounded = linear_gain_to_db_nano_down(next_down_positive(max_linear)).unwrap();
            assert!(rounded < exact, "{raw}: rounded={rounded}");
            assert!(db_nano_to_linear_upper(rounded).unwrap() <= max_linear);
        }
    }

    #[test]
    fn album_gain_attenuates_when_loudest_peak_exceeds_target() {
        let authority = resolve_album_gain(
            db("-1.000000000"),
            &[finite_db("-0.100000000")],
            zero_terminal(),
        )
        .expect("album authority");
        assert!(authority.gain_db <= db("-0.900000000"));
    }

    #[test]
    fn silent_tracks_do_not_override_finite_peak_and_all_silence_is_unity() {
        let mixed = resolve_album_gain(
            db("-0.150000000"),
            &[AlbumPeakMeasurement::Silence, finite_db("-5.000000000")],
            zero_terminal(),
        )
        .expect("mixed authority");
        assert!(mixed.gain_db <= db("4.850000000"));

        let silent = resolve_album_gain(
            db("-0.150000000"),
            &[AlbumPeakMeasurement::Silence, AlbumPeakMeasurement::Silence],
            zero_terminal(),
        )
        .expect("silent authority");
        assert_eq!(silent.loudest_peak_dbfs, None);
        assert_eq!(silent.gain_db, DbNano::ZERO);
    }

    #[test]
    fn silence_rejects_terminal_noise_that_alone_exceeds_ceiling() {
        let terminal = AlbumTerminalBound {
            pre_gain_reconstructed_error_linear: 0.0,
            stored_sample_error_linear: Some(1.0),
            post_gain_reconstructed_error_linear: 1.1,
            domain: AlbumCeilingDomain::LosslessStoredPcm,
        };
        assert!(resolve_album_gain(DbNano::ZERO, &[AlbumPeakMeasurement::Silence], terminal).is_err());
    }

    #[test]
    fn empty_measurement_set_is_rejected() {
        assert!(resolve_album_gain(db("-0.150000000"), &[], zero_terminal()).is_err());
    }


    #[test]
    fn pre_gain_realization_error_is_inside_the_gain_term() {
        let measurement = AlbumPeakMeasurement::Finite {
            point_db: db("0.000000000"),
            signal_upper_linear: 1.0,
        };
        let without_pre = resolve_album_gain(DbNano::ZERO, &[measurement], zero_terminal()).unwrap();
        let terminal = AlbumTerminalBound {
            pre_gain_reconstructed_error_linear: 1.0e-6,
            stored_sample_error_linear: Some(0.0),
            post_gain_reconstructed_error_linear: 0.0,
            domain: AlbumCeilingDomain::LosslessStoredPcm,
        };
        let with_pre = resolve_album_gain(DbNano::ZERO, &[measurement], terminal).unwrap();

        assert!(with_pre.gain_db < without_pre.gain_db);
        let final_upper = next_up_nonnegative(
            gain_linear(with_pre.gain_db)
                * next_up_nonnegative(
                    with_pre.loudest_signal_upper_linear
                        + terminal.pre_gain_reconstructed_error_linear,
                ),
        );
        assert!(final_upper <= 1.0, "final upper {final_upper:.16} exceeded 0 dBFS");
    }

}
