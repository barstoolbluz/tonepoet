//! Pure loudgain v0.6.8 numerical/presentation compatibility.
//!
//! This module does not decode files or write tags.  It intentionally preserves
//! the old operation graph where a symbolic simplification could alter a
//! displayed decimal at a rounding boundary.

use crate::replaygain::{db_to_linear, linear_to_db, ReplayGainError};

pub const ORDINARY_REFERENCE_LUFS: f64 = -18.0;
pub const OPUS_REFERENCE_LUFS: f64 = -23.0;
pub const CLIP_PREVENTION_CEILING_DBTP: f64 = -1.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudgain068Measurement {
    pub loudness_lufs: f64,
    pub range_lu: f64,
    pub reporting_peak_linear: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudgain068Options {
    pub reference_lufs: f64,
    pub prevent_clipping: bool,
    pub clipping_ceiling_dbtp: f64,
}

impl Default for Loudgain068Options {
    fn default() -> Self {
        Self {
            reference_lufs: ORDINARY_REFERENCE_LUFS,
            prevent_clipping: false,
            clipping_ceiling_dbtp: CLIP_PREVENTION_CEILING_DBTP,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Loudgain068Row {
    pub loudness_lufs: f64,
    pub range_lu: f64,
    pub true_peak_linear: f64,
    pub true_peak_dbtp: f64,
    pub reference_lufs: f64,
    pub will_clip: bool,
    pub clip_prevent: bool,
    pub gain_db: f64,
    pub new_peak_linear: f64,
    pub new_peak_dbtp: f64,
}

impl Loudgain068Row {
    /// Exact eleven-column `-O` row, including caller-owned label.
    #[must_use]
    pub fn format_tsv(&self, label: &str) -> String {
        format!(
            "{label}\t{:.2} LUFS\t{:.2} dB\t{:.6}\t{:.2} dBTP\t{:.2} LUFS\t{}\t{}\t{:.2} dB\t{:.6}\t{:.2} dBTP",
            self.loudness_lufs,
            self.range_lu,
            self.true_peak_linear,
            self.true_peak_dbtp,
            self.reference_lufs,
            if self.will_clip { "Y" } else { "N" },
            if self.clip_prevent { "Y" } else { "N" },
            self.gain_db,
            self.new_peak_linear,
            self.new_peak_dbtp,
        )
    }

    #[must_use]
    pub fn replaygain_gain_tag(&self) -> String {
        format!("{:.2} dB", self.gain_db)
    }

    #[must_use]
    pub fn replaygain_peak_tag(&self) -> String {
        format!("{:.6}", self.true_peak_linear)
    }
}

/// Build one compatibility row. `album_would_clip` is the *unprevented* album
/// overload fact used by loudgain when it reports an individual track row in
/// album mode.  It does not alter this row's gain reduction.
pub fn calculate_row(
    measurement: Loudgain068Measurement,
    options: Loudgain068Options,
    album_would_clip: bool,
) -> Result<Loudgain068Row, ReplayGainError> {
    if !measurement.loudness_lufs.is_finite() || !measurement.range_lu.is_finite() {
        return Err(ReplayGainError::NonFiniteLoudness);
    }
    if !measurement.reporting_peak_linear.is_finite() || measurement.reporting_peak_linear < 0.0 {
        return Err(ReplayGainError::InvalidPeak);
    }
    if !options.reference_lufs.is_finite() {
        return Err(ReplayGainError::InvalidReference);
    }
    if !options.clipping_ceiling_dbtp.is_finite() {
        return Err(ReplayGainError::InvalidCeiling);
    }

    let true_peak_dbtp = if measurement.reporting_peak_linear == 0.0 {
        f64::NEG_INFINITY
    } else {
        linear_to_db(measurement.reporting_peak_linear)?
    };
    let mut gain_db = options.reference_lufs - measurement.loudness_lufs;
    let raw_gain_linear = db_to_linear(gain_db)?;
    let proposed_peak = measurement.reporting_peak_linear * raw_gain_linear;
    if !proposed_peak.is_finite() {
        return Err(ReplayGainError::NumericalRange);
    }
    let ceiling_linear = db_to_linear(options.clipping_ceiling_dbtp)?;
    let own_would_clip = proposed_peak > ceiling_linear;
    let mut new_peak_linear = proposed_peak;
    let mut clip_prevent = false;

    // Preserve loudgain's ratio/log adjustment instead of replacing it with the
    // algebraically simpler ceiling_dbtp - peak_dbtp expression.
    if options.prevent_clipping && own_would_clip {
        new_peak_linear = ceiling_linear;
        let ratio = proposed_peak / new_peak_linear;
        gain_db -= 20.0 * ratio.log10();
        clip_prevent = true;
    }
    let new_peak_dbtp = if new_peak_linear == 0.0 {
        f64::NEG_INFINITY
    } else {
        linear_to_db(new_peak_linear)?
    };
    let will_clip = if options.prevent_clipping {
        false
    } else {
        own_would_clip || album_would_clip
    };

    Ok(Loudgain068Row {
        loudness_lufs: measurement.loudness_lufs,
        range_lu: measurement.range_lu,
        true_peak_linear: measurement.reporting_peak_linear,
        true_peak_dbtp,
        reference_lufs: options.reference_lufs,
        will_clip,
        clip_prevent,
        gain_db,
        new_peak_linear,
        new_peak_dbtp,
    })
}

#[must_use]
pub fn would_clip(
    measurement: Loudgain068Measurement,
    reference_lufs: f64,
    ceiling_dbtp: f64,
) -> Result<bool, ReplayGainError> {
    if !measurement.loudness_lufs.is_finite() {
        return Err(ReplayGainError::NonFiniteLoudness);
    }
    if !measurement.reporting_peak_linear.is_finite() || measurement.reporting_peak_linear < 0.0 {
        return Err(ReplayGainError::InvalidPeak);
    }
    let gain = reference_lufs - measurement.loudness_lufs;
    let proposed = measurement.reporting_peak_linear * db_to_linear(gain)?;
    Ok(proposed > db_to_linear(ceiling_dbtp)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_matches_reference_column_contract() {
        let row = Loudgain068Row {
            loudness_lufs: -5.16,
            range_lu: 5.65,
            true_peak_linear: 1.057608,
            true_peak_dbtp: 0.4864,
            reference_lufs: -18.0,
            will_clip: false,
            clip_prevent: false,
            gain_db: -12.84,
            new_peak_linear: 0.241255,
            new_peak_dbtp: -12.3501,
        };
        assert_eq!(
            row.format_tsv("track-001.wav"),
            "track-001.wav\t-5.16 LUFS\t5.65 dB\t1.057608\t0.49 dBTP\t-18.00 LUFS\tN\tN\t-12.84 dB\t0.241255\t-12.35 dBTP"
        );
    }

    #[test]
    fn album_overload_sets_track_flag_only_without_prevention() {
        let measurement = Loudgain068Measurement {
            loudness_lufs: -18.0,
            range_lu: 0.0,
            reporting_peak_linear: 0.5,
        };
        let row = calculate_row(measurement, Loudgain068Options::default(), true).unwrap();
        assert!(row.will_clip);
        assert!(!row.clip_prevent);

        let row = calculate_row(
            measurement,
            Loudgain068Options {
                prevent_clipping: true,
                ..Loudgain068Options::default()
            },
            true,
        )
        .unwrap();
        assert!(!row.will_clip);
        assert!(!row.clip_prevent);
    }

    #[test]
    fn prevention_caps_peak_and_reduces_gain() {
        let measurement = Loudgain068Measurement {
            loudness_lufs: -30.0,
            range_lu: 0.0,
            reporting_peak_linear: 1.0,
        };
        let row = calculate_row(
            measurement,
            Loudgain068Options {
                prevent_clipping: true,
                ..Loudgain068Options::default()
            },
            false,
        )
        .unwrap();
        assert!(row.clip_prevent);
        assert!(!row.will_clip);
        assert!((row.gain_db + 1.0).abs() <= 2.0e-14);
        assert!((row.new_peak_dbtp + 1.0).abs() <= 2.0e-14);
    }
    fn formatting_row(gain_db: f64, peak: f64, range_lu: f64) -> Loudgain068Row {
        Loudgain068Row {
            loudness_lufs: -0.0,
            range_lu,
            true_peak_linear: peak,
            true_peak_dbtp: -0.0,
            reference_lufs: -18.0,
            will_clip: false,
            clip_prevent: false,
            gain_db,
            new_peak_linear: peak,
            new_peak_dbtp: -0.0,
        }
    }

    #[test]
    fn formatting_edges_preserve_negative_zero_halfway_neighbors_and_decimal_carries() {
        let exact_half = formatting_row(-0.0, 0.1234564, 1.125);
        assert_eq!(exact_half.replaygain_gain_tag(), "-0.00 dB");
        assert_eq!(exact_half.replaygain_peak_tag(), "0.123456");
        assert!(exact_half.format_tsv("x").contains("\t-0.00 LUFS\t1.12 dB\t"));

        let below = formatting_row(0.0, 0.5, f64::from_bits(1.125_f64.to_bits() - 1));
        let above = formatting_row(0.0, 0.5, f64::from_bits(1.125_f64.to_bits() + 1));
        assert!(below.format_tsv("x").contains("\t1.12 dB\t"));
        assert!(above.format_tsv("x").contains("\t1.13 dB\t"));

        let carry = formatting_row(9.999, 0.9999996, 9.999);
        assert_eq!(carry.replaygain_gain_tag(), "10.00 dB");
        assert_eq!(carry.replaygain_peak_tag(), "1.000000");
        assert!(carry.format_tsv("x").contains("\t10.00 dB\t"));
    }

    #[test]
    fn very_small_and_zero_peaks_have_stable_six_decimal_presentation() {
        assert_eq!(formatting_row(0.0, 0.0000004, 0.0).replaygain_peak_tag(), "0.000000");

        let zero = calculate_row(
            Loudgain068Measurement {
                loudness_lufs: -18.0,
                range_lu: 0.0,
                reporting_peak_linear: 0.0,
            },
            Loudgain068Options::default(),
            false,
        )
        .unwrap();
        assert_eq!(zero.replaygain_peak_tag(), "0.000000");
        assert!(zero.true_peak_dbtp.is_infinite() && zero.true_peak_dbtp.is_sign_negative());
        assert!(zero.format_tsv("silence").contains("-inf dBTP"));
    }

    #[test]
    fn clipping_flags_cover_track_only_album_only_and_exact_ceiling_cases() {
        let ceiling = db_to_linear(CLIP_PREVENTION_CEILING_DBTP).unwrap();
        let exact = Loudgain068Measurement {
            loudness_lufs: ORDINARY_REFERENCE_LUFS,
            range_lu: 0.0,
            reporting_peak_linear: ceiling,
        };
        let row = calculate_row(exact, Loudgain068Options::default(), false).unwrap();
        assert!(!row.will_clip);
        assert!(!row.clip_prevent);

        let track_over = Loudgain068Measurement {
            loudness_lufs: ORDINARY_REFERENCE_LUFS,
            range_lu: 0.0,
            reporting_peak_linear: ceiling * 1.01,
        };
        assert!(calculate_row(track_over, Loudgain068Options::default(), false).unwrap().will_clip);

        let quiet_track = Loudgain068Measurement {
            loudness_lufs: ORDINARY_REFERENCE_LUFS,
            range_lu: 0.0,
            reporting_peak_linear: ceiling * 0.5,
        };
        let album_only = calculate_row(quiet_track, Loudgain068Options::default(), true).unwrap();
        assert!(album_only.will_clip);
        assert!(!album_only.clip_prevent);
    }

    #[test]
    fn presentation_rounds_only_when_strings_are_built() {
        let measurement = Loudgain068Measurement {
            loudness_lufs: -17.9949,
            range_lu: 1.2349,
            reporting_peak_linear: 0.12345649,
        };
        let row = calculate_row(measurement, Loudgain068Options::default(), false).unwrap();
        let exact_gain = ORDINARY_REFERENCE_LUFS - measurement.loudness_lufs;
        assert_eq!(row.gain_db.to_bits(), exact_gain.to_bits());
        assert_eq!(row.replaygain_gain_tag(), "-0.01 dB");
        assert_eq!(row.replaygain_peak_tag(), "0.123456");
        assert!(row.format_tsv("x").contains("\t1.23 dB\t"));
    }

}
