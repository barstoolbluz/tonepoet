//! Pure numerical ReplayGain policy.
//!
//! File decoding, codec-specific tags, and metadata mutation remain consumer
//! concerns.  This module operates only on finite loudness/peak measurements.

use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReplayGainOptions {
    pub reference_lufs: f64,
    /// `None` disables clipping prevention.  `Some(-1.0)` is loudgain's
    /// ordinary `-k` policy.
    pub prevention_ceiling_dbtp: Option<f64>,
}

impl Default for ReplayGainOptions {
    fn default() -> Self {
        Self {
            reference_lufs: -18.0,
            prevention_ceiling_dbtp: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReplayGainCalculation {
    pub requested_gain_db: f64,
    pub applied_gain_db: f64,
    pub reporting_peak_linear: f64,
    pub proposed_peak_linear: f64,
    pub resulting_peak_linear: f64,
    pub limited: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayGainError {
    NonFiniteLoudness,
    InvalidPeak,
    InvalidReference,
    InvalidCeiling,
    NumericalRange,
    Q78OutOfRange,
}

impl fmt::Display for ReplayGainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteLoudness => f.write_str("ReplayGain requires finite integrated loudness"),
            Self::InvalidPeak => f.write_str("ReplayGain peak must be finite and nonnegative"),
            Self::InvalidReference => f.write_str("ReplayGain reference must be finite"),
            Self::InvalidCeiling => f.write_str("ReplayGain prevention ceiling must be finite"),
            Self::NumericalRange => f.write_str("ReplayGain arithmetic exceeded finite range"),
            Self::Q78OutOfRange => f.write_str("Opus R128 Q7.8 gain is outside signed 16-bit range"),
        }
    }
}

impl Error for ReplayGainError {}

pub fn calculate_replaygain(
    integrated_lufs: f64,
    reporting_peak_linear: f64,
    options: ReplayGainOptions,
) -> Result<ReplayGainCalculation, ReplayGainError> {
    if !integrated_lufs.is_finite() {
        return Err(ReplayGainError::NonFiniteLoudness);
    }
    if !reporting_peak_linear.is_finite() || reporting_peak_linear < 0.0 {
        return Err(ReplayGainError::InvalidPeak);
    }
    if !options.reference_lufs.is_finite() {
        return Err(ReplayGainError::InvalidReference);
    }
    if options
        .prevention_ceiling_dbtp
        .is_some_and(|ceiling| !ceiling.is_finite())
    {
        return Err(ReplayGainError::InvalidCeiling);
    }

    let requested_gain_db = options.reference_lufs - integrated_lufs;
    let gain_linear = db_to_linear(requested_gain_db)?;
    let proposed_peak_linear = reporting_peak_linear * gain_linear;
    if !proposed_peak_linear.is_finite() {
        return Err(ReplayGainError::NumericalRange);
    }

    let (applied_gain_db, resulting_peak_linear, limited) =
        if let Some(ceiling_dbtp) = options.prevention_ceiling_dbtp {
            let ceiling_linear = db_to_linear(ceiling_dbtp)?;
            if proposed_peak_linear > ceiling_linear && reporting_peak_linear > 0.0 {
                let allowed_gain_db = ceiling_dbtp - linear_to_db(reporting_peak_linear)?;
                (allowed_gain_db, ceiling_linear, true)
            } else {
                (requested_gain_db, proposed_peak_linear, false)
            }
        } else {
            (requested_gain_db, proposed_peak_linear, false)
        };

    Ok(ReplayGainCalculation {
        requested_gain_db,
        applied_gain_db,
        reporting_peak_linear,
        proposed_peak_linear,
        resulting_peak_linear,
        limited,
    })
}

/// Convert a dB gain to an Opus R128 Q7.8 comment value using half-away-from-
/// zero rounding, matching loudgain 0.6.8.  The caller owns the choice of
/// ordinary (-18 LUFS) versus Opus (-23 LUFS) reference.
pub fn gain_db_to_opus_q78(gain_db: f64) -> Result<i16, ReplayGainError> {
    if !gain_db.is_finite() {
        return Err(ReplayGainError::NonFiniteLoudness);
    }
    let scaled = gain_db * 256.0;
    if !scaled.is_finite() {
        return Err(ReplayGainError::NumericalRange);
    }
    let rounded = if scaled >= 0.0 {
        (scaled + 0.5).floor()
    } else {
        (scaled - 0.5).ceil()
    };
    if rounded < f64::from(i16::MIN) || rounded > f64::from(i16::MAX) {
        return Err(ReplayGainError::Q78OutOfRange);
    }
    Ok(rounded as i16)
}

#[inline]
pub(crate) fn db_to_linear(db: f64) -> Result<f64, ReplayGainError> {
    let value = 10.0_f64.powf(db / 20.0);
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ReplayGainError::NumericalRange)
    }
}

#[inline]
pub(crate) fn linear_to_db(linear: f64) -> Result<f64, ReplayGainError> {
    if !linear.is_finite() || linear <= 0.0 {
        return Err(ReplayGainError::InvalidPeak);
    }
    let value = 20.0 * linear.log10();
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ReplayGainError::NumericalRange)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_policy_limits_at_requested_ceiling() {
        let result = calculate_replaygain(
            -30.0,
            1.0,
            ReplayGainOptions {
                reference_lufs: -18.0,
                prevention_ceiling_dbtp: Some(-1.0),
            },
        )
        .unwrap();
        assert_eq!(result.requested_gain_db, 12.0);
        assert!((result.applied_gain_db + 1.0).abs() <= 2.0e-15);
        assert!(result.limited);
        assert!((20.0 * result.resulting_peak_linear.log10() + 1.0).abs() <= 2.0e-14);
    }

    #[test]
    fn q78_rounds_half_away_from_zero() {
        assert_eq!(gain_db_to_opus_q78(0.5 / 256.0).unwrap(), 1);
        assert_eq!(gain_db_to_opus_q78(-0.5 / 256.0).unwrap(), -1);
        assert_eq!(gain_db_to_opus_q78(1.49 / 256.0).unwrap(), 1);
        assert_eq!(gain_db_to_opus_q78(-1.49 / 256.0).unwrap(), -1);
    }

    #[test]
    fn nonfinite_tag_inputs_fail_closed() {
        assert_eq!(
            calculate_replaygain(f64::NEG_INFINITY, 0.5, ReplayGainOptions::default()),
            Err(ReplayGainError::NonFiniteLoudness)
        );
        assert_eq!(
            gain_db_to_opus_q78(f64::NAN),
            Err(ReplayGainError::NonFiniteLoudness)
        );
    }
    #[test]
    fn raw_and_reduced_gain_are_distinct_and_negative_gain_is_preserved() {
        let raw = calculate_replaygain(-30.0, 1.0, ReplayGainOptions::default()).unwrap();
        assert_eq!(raw.requested_gain_db, 12.0);
        assert_eq!(raw.applied_gain_db, 12.0);
        assert!(!raw.limited);

        let reduced = calculate_replaygain(
            -30.0,
            1.0,
            ReplayGainOptions {
                prevention_ceiling_dbtp: Some(-1.0),
                ..ReplayGainOptions::default()
            },
        )
        .unwrap();
        assert_eq!(reduced.requested_gain_db, 12.0);
        assert!(reduced.applied_gain_db < raw.applied_gain_db);
        assert!(reduced.limited);

        let negative = calculate_replaygain(-10.0, 0.5, ReplayGainOptions::default()).unwrap();
        assert_eq!(negative.requested_gain_db, -8.0);
        assert_eq!(negative.applied_gain_db, -8.0);
        assert!(!negative.limited);
    }

    #[test]
    fn equality_at_prevention_ceiling_is_not_limited() {
        let ceiling_db = -1.0;
        let ceiling_linear = db_to_linear(ceiling_db).unwrap();
        let result = calculate_replaygain(
            -18.0,
            ceiling_linear,
            ReplayGainOptions {
                prevention_ceiling_dbtp: Some(ceiling_db),
                ..ReplayGainOptions::default()
            },
        )
        .unwrap();
        assert!(!result.limited);
        assert_eq!(result.applied_gain_db, 0.0);
        assert_eq!(result.resulting_peak_linear.to_bits(), ceiling_linear.to_bits());
    }

    #[test]
    fn replaygain_validation_matrix_fails_closed() {
        assert_eq!(
            calculate_replaygain(f64::NAN, 0.5, ReplayGainOptions::default()),
            Err(ReplayGainError::NonFiniteLoudness),
        );
        for peak in [-1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                calculate_replaygain(-18.0, peak, ReplayGainOptions::default()),
                Err(ReplayGainError::InvalidPeak),
            );
        }
        assert_eq!(
            calculate_replaygain(
                -18.0,
                0.5,
                ReplayGainOptions { reference_lufs: f64::INFINITY, prevention_ceiling_dbtp: None },
            ),
            Err(ReplayGainError::InvalidReference),
        );
        assert_eq!(
            calculate_replaygain(
                -18.0,
                0.5,
                ReplayGainOptions { reference_lufs: -18.0, prevention_ceiling_dbtp: Some(f64::NAN) },
            ),
            Err(ReplayGainError::InvalidCeiling),
        );
    }

    #[test]
    fn gain_policy_does_not_round_before_the_tag_boundary() {
        let result = calculate_replaygain(-17.9949, 0.1, ReplayGainOptions::default()).unwrap();
        let expected: f64 = -18.0 - (-17.9949);
        assert_eq!(result.requested_gain_db.to_bits(), expected.to_bits());
        assert_eq!(result.applied_gain_db.to_bits(), expected.to_bits());
        assert_ne!(result.applied_gain_db, -0.01);
    }

    #[test]
    fn q78_accepts_signed_range_and_rejects_rounding_past_it() {
        assert_eq!(gain_db_to_opus_q78(f64::from(i16::MAX) / 256.0).unwrap(), i16::MAX);
        assert_eq!(gain_db_to_opus_q78(f64::from(i16::MIN) / 256.0).unwrap(), i16::MIN);
        assert_eq!(
            gain_db_to_opus_q78((f64::from(i16::MAX) + 0.5) / 256.0),
            Err(ReplayGainError::Q78OutOfRange),
        );
        assert_eq!(
            gain_db_to_opus_q78((f64::from(i16::MIN) - 0.5) / 256.0),
            Err(ReplayGainError::Q78OutOfRange),
        );
    }

}
