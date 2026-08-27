//! Submitted-batch DSD peak-normalization primitives.
//!
//! Album scope is deliberately outside the qualified DSD Reference contract.
//! This module borrows the established reconstruction mechanics, but it does
//! not create or consume Reference attestations and never modifies the frozen
//! qualification corpus.

use crate::dsd_reference::{
    extract_single_sox_stats_peak_report, resolve_reference_profile, DbNano,
};
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

/// SoX `stats` reports `Pk lev dB` to centidecibel precision. Album mode
/// binds an explicit fixed gain from that text report, so reserve one complete
/// reporting quantum. This is a local measurement-safety rule, not part of
/// the qualified DSD Reference acceptance policy.
pub const ALBUM_SOX_STATS_REPORTING_UNCERTAINTY: DbNano = DbNano(10_000_000);

/// One deterministic post-reconstruction peak report for album aggregation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumPeakMeasurement {
    /// Finite sample peak in dBFS.
    Finite(DbNano),
    /// The analyzer reported a completely silent signal (`-inf`).
    Silence,
}

/// Resolved fixed gain shared by every DSD track in one submitted batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlbumGainAuthority {
    /// Peak target selected by the user-facing headroom control.
    pub target_dbfs: DbNano,
    /// Loudest finite measured track, or `None` when every track is silent.
    pub loudest_peak_dbfs: Option<DbNano>,
    /// Fixed gain to apply uniformly to every participating DSD track.
    pub gain_db: DbNano,
    /// Number of DSD tracks represented by this authority.
    pub track_count: usize,
}

/// Parse the strict `Pk lev dB` token from one SoX `stats` run.
pub fn parse_album_peak_measurement(
    stderr: &str,
    channels: u16,
) -> std::result::Result<AlbumPeakMeasurement, String> {
    let raw = extract_single_sox_stats_peak_report(stderr, channels)?;
    if raw == "-inf" {
        return Ok(AlbumPeakMeasurement::Silence);
    }
    raw.parse::<DbNano>()
        .map(AlbumPeakMeasurement::Finite)
        .map_err(|error| format!("album DSD peak report is invalid: {error}"))
}

/// Derive one fixed album gain from a complete measurement set.
///
/// The loudest finite track drives the result. Negative values are retained:
/// if the source already exceeds the selected target, album mode attenuates
/// the complete set instead of clipping or silently clamping. An all-silent
/// set receives exactly 0 dB because there is no finite peak to normalize.
pub fn resolve_album_gain(
    target_dbfs: DbNano,
    measurements: &[AlbumPeakMeasurement],
) -> std::result::Result<AlbumGainAuthority, String> {
    if measurements.is_empty() {
        return Err("album DSD gain requires at least one measured DSD track".to_string());
    }

    let loudest_peak_dbfs = measurements.iter().filter_map(|measurement| match measurement {
        AlbumPeakMeasurement::Finite(value) => Some(*value),
        AlbumPeakMeasurement::Silence => None,
    }).max();

    let gain_db = match loudest_peak_dbfs {
        Some(reported_peak) => {
            // The real peak may lie anywhere inside the analyzer's 0.01 dB
            // reporting bin. Treat the printed value as a lower bound and
            // raise it by one complete quantum before binding gain. That makes
            // the resulting explicit fixed gain conservative even when SoX
            // rounded the peak downward.
            let conservative_peak = reported_peak
                .checked_add(ALBUM_SOX_STATS_REPORTING_UNCERTAINTY)
                .ok_or_else(|| "album DSD peak safety reserve overflowed".to_string())?;
            target_dbfs
                .checked_sub(conservative_peak)
                .ok_or_else(|| "album DSD gain arithmetic overflowed".to_string())?
        }
        None => DbNano::ZERO,
    };

    Ok(AlbumGainAuthority {
        target_dbfs,
        loudest_peak_dbfs,
        gain_db,
        track_count: measurements.len(),
    })
}

/// Resolve the PCM rate at which album peak authority must be measured.
///
/// Measurement happens after the same DSD reconstruction/rate conversion the
/// ordinary track path would use, so the retained carrier is already at the
/// final requested PCM sample rate.
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
    match settings.target_sample_rate {
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
    }
}

/// Build the single expensive decode used by album-scoped DSD normalization.
///
/// The command writes a 64-bit floating-point CAF carrier while the final
/// `stats` effect measures the exact same post-reconstruction samples. CAF is
/// used instead of RIFF/WAV so long multichannel tracks are not constrained by
/// the 4 GiB RIFF size ceiling. No normalization gain or output dither is
/// applied in this pass; the submitted-batch barrier binds one fixed gain only
/// after every participating track has reported its peak.
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
        "caf".to_string(),
        "-e".to_string(),
        "floating-point".to_string(),
        "-b".to_string(),
        "64".to_string(),
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
    args.push("stats".to_string());

    let mut command = PlannedCommand::new(
        ToolIdentifier::Sox,
        args,
        InputSource::Path(input.to_path_buf()),
        OutputSink::Path(output.to_path_buf()),
        duration,
        "Decode DSD once and measure submitted-batch album peak",
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
    fn loudest_track_drives_album_gain() {
        let authority = resolve_album_gain(
            db("-0.150000000"),
            &[
                AlbumPeakMeasurement::Finite(db("-12.000000000")),
                AlbumPeakMeasurement::Finite(db("-3.250000000")),
                AlbumPeakMeasurement::Finite(db("-7.000000000")),
            ],
        )
        .expect("album authority");
        assert_eq!(authority.loudest_peak_dbfs, Some(db("-3.250000000")));
        assert_eq!(authority.gain_db, db("3.090000000"));
        assert_eq!(authority.track_count, 3);
    }

    #[test]
    fn album_gain_is_uniform_and_preserves_intertrack_level_difference() {
        let quiet = db("-18.000000000");
        let loud = db("-6.000000000");
        let authority = resolve_album_gain(
            db("-0.150000000"),
            &[
                AlbumPeakMeasurement::Finite(quiet),
                AlbumPeakMeasurement::Finite(loud),
            ],
        )
        .expect("album authority");
        let quiet_after = quiet.checked_add(authority.gain_db).unwrap();
        let loud_after = loud.checked_add(authority.gain_db).unwrap();
        assert_eq!(loud_after, db("-0.160000000"));
        assert_eq!(loud.checked_sub(quiet), loud_after.checked_sub(quiet_after));
    }

    #[test]
    fn analyzer_reporting_reserve_prevents_headroom_overshoot() {
        let reported_peak = db("-0.154000000");
        let target = db("-0.150000000");
        let authority = resolve_album_gain(
            target,
            &[AlbumPeakMeasurement::Finite(reported_peak)],
        )
        .expect("album authority");

        // If SoX rounded a true -0.149 dBFS peak down into this printed bin,
        // using the report verbatim would apply +0.004 dB and exceed target.
        // The one-quantum reserve instead attenuates by 0.006 dB.
        assert_eq!(authority.gain_db, db("-0.006000000"));
        let conservative_peak = reported_peak
            .checked_add(ALBUM_SOX_STATS_REPORTING_UNCERTAINTY)
            .unwrap();
        assert!(
            conservative_peak
                .checked_add(authority.gain_db)
                .unwrap()
                <= target
        );
    }

    #[test]
    fn album_gain_attenuates_when_loudest_peak_exceeds_target() {
        let authority = resolve_album_gain(
            db("-1.000000000"),
            &[AlbumPeakMeasurement::Finite(db("-0.100000000"))],
        )
        .expect("album authority");
        assert_eq!(authority.gain_db, db("-0.910000000"));
    }

    #[test]
    fn silent_tracks_do_not_override_finite_peak_and_all_silence_is_unity() {
        let mixed = resolve_album_gain(
            db("-0.150000000"),
            &[
                AlbumPeakMeasurement::Silence,
                AlbumPeakMeasurement::Finite(db("-5.000000000")),
            ],
        )
        .expect("mixed authority");
        assert_eq!(mixed.gain_db, db("4.840000000"));

        let silent = resolve_album_gain(
            db("-0.150000000"),
            &[AlbumPeakMeasurement::Silence, AlbumPeakMeasurement::Silence],
        )
        .expect("silent authority");
        assert_eq!(silent.loudest_peak_dbfs, None);
        assert_eq!(silent.gain_db, DbNano::ZERO);
    }

    #[test]
    fn empty_measurement_set_is_rejected() {
        assert!(resolve_album_gain(db("-0.150000000"), &[]).is_err());
    }

    #[test]
    fn parses_multichannel_sox_stats_overall_peak_and_silence() {
        assert_eq!(
            parse_album_peak_measurement("Pk lev dB     -2.000000 -3.0 -2.0\n", 2).unwrap(),
            AlbumPeakMeasurement::Finite(db("-2.000000")),
        );
        assert_eq!(
            parse_album_peak_measurement("Pk lev dB      -inf -inf -inf\n", 2).unwrap(),
            AlbumPeakMeasurement::Silence,
        );
    }
}
