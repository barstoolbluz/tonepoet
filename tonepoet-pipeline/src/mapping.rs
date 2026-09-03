//! Pure parameter-to-argument mappings used by tool plugins.
//!
//! These functions contain no ambient state and are deterministic.

use crate::enums::{
    AacProfile, AudioFormat, DitherType, DsdLowpassMethod, DsdNoiseShaper, ModulatorOrder, Mp3Mode,
    NyquistTransition, OpusContentType, PcmBitDepth, ResampleQuality, SoxSincPhase, SsrcPdfType,
    SsrcProfile, WavPackMode,
};
use crate::error::{PlanningError, Result};
use crate::settings::SsrcSettings;

/// Map resample quality to SoXR precision (bits).
/// Ultra uses precision=33, the maximum ffmpeg allows (~199 dB rejection).
#[must_use]
pub const fn soxr_precision(quality: ResampleQuality) -> u8 {
    match quality {
        ResampleQuality::Insane | ResampleQuality::Ultra => 33,
        ResampleQuality::VeryHigh => 28,
        ResampleQuality::High => 24,
        ResampleQuality::Medium => 20,
        ResampleQuality::Low => 16,
    }
}

/// Map resample quality to SoX rate effect flag.
#[must_use]
pub const fn sox_rate_quality_flag(quality: ResampleQuality) -> &'static str {
    match quality {
        ResampleQuality::Insane => "-u",
        ResampleQuality::Ultra => "-v",
        ResampleQuality::VeryHigh => "-h",
        ResampleQuality::High => "-m",
        ResampleQuality::Medium => "-l",
        ResampleQuality::Low => "-q",
    }
}

/// Map DSD auto presets to SoX's very-high-quality rate flag.
#[must_use]
pub const fn sox_dsd_auto_rate_flag() -> &'static str {
    "-u"
}

/// Map a DSD low-pass policy to the SoX `rate` quality flag.
/// DSD paths use `-u` (undocumented ultra: 701 taps, 210 dB rejection).
#[must_use]
pub const fn sox_dsd_lowpass_rate_flag(
    lowpass: DsdLowpassMethod,
    _quality: ResampleQuality,
) -> &'static str {
    match lowpass {
        DsdLowpassMethod::Auto | DsdLowpassMethod::SoxUltra => "-u",
        DsdLowpassMethod::Sinc => "-u",
    }
}

/// Map Nyquist transition to FFmpeg/SWResampler cutoff.
#[must_use]
pub const fn ffmpeg_cutoff(transition: NyquistTransition) -> f32 {
    match transition {
        NyquistTransition::Gentle => 0.95,
        NyquistTransition::Medium => 0.97,
        NyquistTransition::Steep | NyquistTransition::Sharp | NyquistTransition::BrickWall => 0.997,
    }
}

/// Map Nyquist transition to SoX rolloff value (fraction, for non-SoX consumers).
#[must_use]
pub const fn sox_rolloff(transition: NyquistTransition) -> Option<&'static str> {
    match transition {
        NyquistTransition::Gentle => Some("0.95"),
        NyquistTransition::Medium => Some("0.97"),
        NyquistTransition::Steep | NyquistTransition::Sharp => Some("0.997"),
        NyquistTransition::BrickWall => None,
    }
}

/// Map Nyquist transition to SoX `-b` bandwidth percentage.
/// SoX `rate` effect accepts `-b 74-99.7` as a percentage, not a fraction.
#[must_use]
pub const fn sox_bandwidth_percent(transition: NyquistTransition) -> Option<&'static str> {
    match transition {
        NyquistTransition::Gentle => Some("95"),
        NyquistTransition::Medium => Some("97"),
        NyquistTransition::Steep | NyquistTransition::Sharp => Some("99.7"),
        NyquistTransition::BrickWall => None,
    }
}

/// Map dither type to SoX dither effect arguments.
#[must_use]
pub fn sox_dither_args(dither: DitherType) -> Vec<String> {
    match dither {
        DitherType::None => Vec::new(),
        DitherType::Tpdf => vec!["dither".into()],
        DitherType::SlopedTpdf => vec!["dither".into(), "-S".into()],
        DitherType::Shibata => vec!["dither".into(), "-s".into()],
        DitherType::Lipshitz => vec!["dither".into(), "-f".into(), "lipshitz".into()],
        DitherType::FWeighted => vec![
            "dither".into(),
            "-s".into(),
            "-f".into(),
            "f-weighted".into(),
        ],
        DitherType::ModifiedEWeighted => vec![
            "dither".into(),
            "-s".into(),
            "-f".into(),
            "modified-e-weighted".into(),
        ],
        DitherType::ImprovedEWeighted => vec![
            "dither".into(),
            "-s".into(),
            "-f".into(),
            "improved-e-weighted".into(),
        ],
        DitherType::Gesemann => vec!["dither".into(), "-f".into(), "gesemann".into()],
        DitherType::LowShibata => vec![
            "dither".into(),
            "-s".into(),
            "-f".into(),
            "low-shibata".into(),
        ],
        DitherType::HighShibata => vec![
            "dither".into(),
            "-s".into(),
            "-f".into(),
            "high-shibata".into(),
        ],
    }
}

/// Map dither type to SoXR dither method, or `None` if not supported.
#[must_use]
pub const fn soxr_dither_method(dither: DitherType) -> Option<&'static str> {
    match dither {
        DitherType::None => Some("none"),
        DitherType::Tpdf | DitherType::SlopedTpdf => Some("triangular"),
        DitherType::Shibata => Some("shibata"),
        DitherType::LowShibata => Some("low_shibata"),
        DitherType::HighShibata => Some("high_shibata"),
        DitherType::FWeighted => Some("f_weighted"),
        DitherType::ModifiedEWeighted => Some("modified_e_weighted"),
        DitherType::ImprovedEWeighted => Some("improved_e_weighted"),
        DitherType::Lipshitz => Some("lipshitz"),
        DitherType::Gesemann => None, // no ffmpeg equivalent
    }
}

/// SSRC-native dither/noise-shaping selection derived from a user-facing dither choice.
///
/// SSRC splits word-length reduction across two orthogonal controls:
/// `--dither N` chooses the shaper/preset ID and `--pdf N` chooses the
/// dither probability distribution. This struct keeps that pair together so
/// call sites cannot accidentally map a TPDF request to a shaper ID alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SsrcDitherSelection {
    /// Value passed to SSRC's `--dither` option.
    pub dither_id: u8,
    /// Value passed to SSRC's `--pdf` option, when the mapping needs to
    /// override SSRC's documented rectangular default.
    pub pdf_type: Option<SsrcPdfType>,
}

impl SsrcDitherSelection {
    /// Create a new SSRC dither selection with the given dither ID and optional PDF type.
    #[must_use]
    pub const fn new(dither_id: u8, pdf_type: Option<SsrcPdfType>) -> Self {
        Self {
            dither_id,
            pdf_type,
        }
    }
}

/// Explain when a user-facing dither choice cannot be represented natively by
/// SSRC and is therefore mapped to the closest SSRC-native approximation.
///
/// This is a product-facing API, not just an implementation detail: callers can
/// surface the returned note in UI, logs, or plan summaries instead of silently
/// changing the requested shaper family.
#[must_use]
pub const fn ssrc_dither_approximation_note(dither: DitherType) -> Option<&'static str> {
    match dither {
        DitherType::None | DitherType::Tpdf => None,
        DitherType::SlopedTpdf => Some(
            "SSRC does not expose sloped TPDF; using unshaped triangular PDF instead",
        ),
        DitherType::LowShibata | DitherType::Shibata | DitherType::HighShibata => Some(
            "SSRC does not expose SoX Shibata filters; using ATH Curve A shaped dither instead",
        ),
        DitherType::Lipshitz
        | DitherType::FWeighted
        | DitherType::ModifiedEWeighted
        | DitherType::ImprovedEWeighted
        | DitherType::Gesemann => Some(
            "SSRC does not expose this named shaper; using ATH Curve A, Intensity 0 with triangular PDF",
        ),
    }
}

/// True when [`ssrc_dither_selection`] necessarily approximates the requested
/// user-facing dither family.
#[must_use]
pub const fn ssrc_dither_selection_is_approximation(dither: DitherType) -> bool {
    match ssrc_dither_approximation_note(dither) {
        Some(_) => true,
        None => false,
    }
}

/// Map a user-facing dither choice to SSRC-native `--dither`/`--pdf` values.
///
/// Some user-facing dither families have no SSRC-native equivalent. Those
/// choices intentionally map to documented SSRC approximations; callers should
/// check [`ssrc_dither_approximation_note`] and surface that fact to users
/// instead of presenting the result as the requested shaper family.
#[must_use]
pub const fn ssrc_dither_selection(dither: DitherType) -> SsrcDitherSelection {
    match dither {
        DitherType::None => SsrcDitherSelection::new(99, None),
        DitherType::Tpdf | DitherType::SlopedTpdf => {
            SsrcDitherSelection::new(99, Some(SsrcPdfType::Triangular))
        }
        DitherType::LowShibata => SsrcDitherSelection::new(0, Some(SsrcPdfType::Triangular)),
        DitherType::Shibata => SsrcDitherSelection::new(2, Some(SsrcPdfType::Triangular)),
        DitherType::HighShibata => SsrcDitherSelection::new(6, Some(SsrcPdfType::Triangular)),
        DitherType::Lipshitz
        | DitherType::FWeighted
        | DitherType::ModifiedEWeighted
        | DitherType::ImprovedEWeighted
        | DitherType::Gesemann => SsrcDitherSelection::new(0, Some(SsrcPdfType::Triangular)),
    }
}

/// Map dither to the SSRC dither/noise-shaping numeric ID.
///
/// Kept for compatibility with older call sites. Prefer
/// [`ssrc_dither_selection_for_rate`] for command construction so the
/// associated PDF choice is not lost and the chosen ID is valid for the
/// destination sample rate.
#[must_use]
pub const fn ssrc_dither_id(dither: DitherType) -> u8 {
    ssrc_dither_selection(dither).dither_id
}

/// Return true when an SSRC `--dither` ID is available for the destination
/// sample rate.
///
/// The shaped ATH and legacy IDs are rate-specific. IDs `98` (Simple
/// triangular) and `99` (No shaper) are treated as sample-rate independent
/// because they do not depend on an ATH coefficient table. For unlisted rates,
/// fail closed by accepting only those two sample-rate-independent choices.
#[must_use]
pub const fn ssrc_dither_id_available_for_rate(dither_id: u8, target_rate_hz: u32) -> bool {
    if matches!(dither_id, 98 | 99) {
        return true;
    }

    match target_rate_hz {
        44_100 => matches!(dither_id, 0..=6 | 10..=16 | 90..=92),
        48_000 => matches!(dither_id, 0..=6 | 10..=16 | 90 | 91),
        88_200 | 96_000 | 192_000 => matches!(dither_id, 0..=2),
        8_000 | 11_025 | 22_050 => matches!(dither_id, 0 | 1 | 9),
        _ => false,
    }
}

/// Validate an explicit SSRC dither ID against the destination sample rate.
pub fn validate_ssrc_dither_id_for_rate(dither_id: u8, target_rate_hz: u32) -> Result<()> {
    if ssrc_dither_id_available_for_rate(dither_id, target_rate_hz) {
        Ok(())
    } else {
        Err(PlanningError::invalid_settings(
            "ssrc.dither_id",
            format!(
                "SSRC dither ID {dither_id} is not available for destination sample rate {target_rate_hz} Hz"
            ),
        ))
    }
}

/// Map a user-facing dither choice to a destination-rate-valid SSRC-native
/// `--dither`/`--pdf` pair.
///
/// Named Shibata-style choices are approximated by ATH Curve A intensities.
/// When the destination rate exposes fewer intensities, the requested intensity
/// is clamped to the strongest available ATH Curve A ID at that rate instead of
/// producing an invalid SSRC command.
pub fn ssrc_dither_selection_for_rate(
    dither: DitherType,
    target_rate_hz: u32,
) -> Result<SsrcDitherSelection> {
    let selection = match dither {
        DitherType::LowShibata => SsrcDitherSelection::new(
            ath_curve_a_id_for_rate(0, target_rate_hz)?,
            Some(SsrcPdfType::Triangular),
        ),
        DitherType::Shibata => SsrcDitherSelection::new(
            ath_curve_a_id_for_rate(2, target_rate_hz)?,
            Some(SsrcPdfType::Triangular),
        ),
        DitherType::HighShibata => SsrcDitherSelection::new(
            ath_curve_a_id_for_rate(6, target_rate_hz)?,
            Some(SsrcPdfType::Triangular),
        ),
        DitherType::Lipshitz
        | DitherType::FWeighted
        | DitherType::ModifiedEWeighted
        | DitherType::ImprovedEWeighted
        | DitherType::Gesemann => SsrcDitherSelection::new(
            ath_curve_a_id_for_rate(0, target_rate_hz)?,
            Some(SsrcPdfType::Triangular),
        ),
        DitherType::None | DitherType::Tpdf | DitherType::SlopedTpdf => ssrc_dither_selection(dither),
    };

    validate_ssrc_dither_id_for_rate(selection.dither_id, target_rate_hz)?;
    Ok(selection)
}

fn ath_curve_a_id_for_rate(requested_intensity: u8, target_rate_hz: u32) -> Result<u8> {
    let max_intensity = match target_rate_hz {
        44_100 | 48_000 => 6,
        88_200 | 96_000 | 192_000 => 2,
        8_000 | 11_025 | 22_050 => 1,
        _ => {
            return Err(PlanningError::invalid_settings(
                "ssrc.dither_id",
                format!(
                    "SSRC ATH noise shaping is not available for destination sample rate {target_rate_hz} Hz"
                ),
            ));
        }
    };

    Ok(requested_intensity.min(max_intensity))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssrc_tpdf_maps_to_no_shaper_with_triangular_pdf() {
        assert_eq!(
            ssrc_dither_selection(DitherType::Tpdf),
            SsrcDitherSelection::new(99, Some(SsrcPdfType::Triangular))
        );
    }

    #[test]
    fn ssrc_shibata_family_maps_to_ath_curve_a_with_triangular_pdf() {
        assert_eq!(
            ssrc_dither_selection(DitherType::LowShibata),
            SsrcDitherSelection::new(0, Some(SsrcPdfType::Triangular))
        );
        assert_eq!(
            ssrc_dither_selection(DitherType::Shibata),
            SsrcDitherSelection::new(2, Some(SsrcPdfType::Triangular))
        );
        assert_eq!(
            ssrc_dither_selection(DitherType::HighShibata),
            SsrcDitherSelection::new(6, Some(SsrcPdfType::Triangular))
        );
    }

    #[test]
    fn unsupported_named_shapers_fall_back_to_conservative_ath_shape() {
        assert_eq!(
            ssrc_dither_selection(DitherType::Lipshitz),
            SsrcDitherSelection::new(0, Some(SsrcPdfType::Triangular))
        );
        assert_eq!(
            ssrc_dither_selection(DitherType::Gesemann),
            SsrcDitherSelection::new(0, Some(SsrcPdfType::Triangular))
        );
    }

    #[test]
    fn ssrc_approximation_notes_are_explicit_for_non_native_mappings() {
        assert!(ssrc_dither_approximation_note(DitherType::None).is_none());
        assert!(ssrc_dither_approximation_note(DitherType::Tpdf).is_none());

        assert!(ssrc_dither_selection_is_approximation(DitherType::SlopedTpdf));
        assert!(ssrc_dither_selection_is_approximation(DitherType::Shibata));
        assert!(ssrc_dither_selection_is_approximation(DitherType::HighShibata));
        assert!(ssrc_dither_selection_is_approximation(DitherType::Lipshitz));
        assert!(ssrc_dither_selection_is_approximation(DitherType::FWeighted));
        assert!(ssrc_dither_selection_is_approximation(DitherType::ModifiedEWeighted));
        assert!(ssrc_dither_selection_is_approximation(DitherType::ImprovedEWeighted));
        assert!(ssrc_dither_selection_is_approximation(DitherType::Gesemann));

        let note = ssrc_dither_approximation_note(DitherType::Lipshitz).unwrap();
        assert!(note.contains("does not expose"));
        assert!(note.contains("ATH Curve A"));
    }

    #[test]
    fn ssrc_dither_id_availability_is_rate_dependent() {
        assert!(ssrc_dither_id_available_for_rate(16, 44_100));
        assert!(ssrc_dither_id_available_for_rate(16, 48_000));
        assert!(!ssrc_dither_id_available_for_rate(16, 96_000));
        assert!(!ssrc_dither_id_available_for_rate(6, 22_050));
        assert!(ssrc_dither_id_available_for_rate(9, 22_050));
        assert!(!ssrc_dither_id_available_for_rate(9, 44_100));
        assert!(ssrc_dither_id_available_for_rate(98, 176_400));
        assert!(ssrc_dither_id_available_for_rate(99, 176_400));
    }

    #[test]
    fn rate_aware_ssrc_shibata_mapping_clamps_to_available_ath_intensity() {
        assert_eq!(
            ssrc_dither_selection_for_rate(DitherType::HighShibata, 44_100).unwrap(),
            SsrcDitherSelection::new(6, Some(SsrcPdfType::Triangular))
        );
        assert_eq!(
            ssrc_dither_selection_for_rate(DitherType::HighShibata, 96_000).unwrap(),
            SsrcDitherSelection::new(2, Some(SsrcPdfType::Triangular))
        );
        assert_eq!(
            ssrc_dither_selection_for_rate(DitherType::Shibata, 22_050).unwrap(),
            SsrcDitherSelection::new(1, Some(SsrcPdfType::Triangular))
        );
    }

    #[test]
    fn rate_aware_ssrc_shaped_mapping_rejects_unlisted_rates() {
        assert!(ssrc_dither_selection_for_rate(DitherType::Shibata, 176_400).is_err());
        assert!(ssrc_dither_selection_for_rate(DitherType::Tpdf, 176_400).is_ok());
    }

    #[test]
    fn configured_lossy_encoder_rate_tables_reject_implicit_resample_rates() {
        assert_eq!(
            ffmpeg_lossy_encoder_accepts_rate_directly(&AudioFormat::Aac, 96_000),
            Some(true)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_accepts_rate_directly(&AudioFormat::Aac, 192_000),
            Some(false)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_accepts_rate_directly(&AudioFormat::Mp3, 96_000),
            Some(false)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_accepts_rate_directly(&AudioFormat::Opus, 96_000),
            Some(false)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_accepts_rate_directly(&AudioFormat::Opus, 24_000),
            Some(false)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_accepts_rate_directly(&AudioFormat::Opus, 48_000),
            Some(true)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_accepts_rate_directly(&AudioFormat::Ac3, 48_000),
            Some(true)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_accepts_rate_directly(&AudioFormat::Flac, 48_000),
            None
        );
    }

    #[test]
    fn lossy_encoder_rate_resolution_preserves_bandwidth_when_a_higher_cell_exists() {
        assert_eq!(
            ffmpeg_lossy_encoder_rate_for_request(&AudioFormat::Aac, 192_000),
            Some(96_000)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_rate_for_request(&AudioFormat::Aac, 176_400),
            Some(96_000)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_rate_for_request(&AudioFormat::Aac, 50_000),
            Some(64_000)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_rate_for_request(&AudioFormat::Mp3, 96_000),
            Some(48_000)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_rate_for_request(&AudioFormat::Mp3, 45_000),
            Some(48_000)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_rate_for_request(&AudioFormat::Dts, 44_100),
            Some(44_100)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_rate_for_request(&AudioFormat::Ac3, 44_100),
            Some(44_100)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_rate_for_request(&AudioFormat::Ac3, 22_050),
            Some(32_000)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_rate_for_request(&AudioFormat::Opus, 8_000),
            Some(48_000)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_rate_for_request(&AudioFormat::Opus, 44_100),
            Some(48_000)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_rate_for_request(&AudioFormat::Opus, 192_000),
            Some(48_000)
        );
        assert_eq!(
            ffmpeg_lossy_encoder_rate_for_request(&AudioFormat::Flac, 192_000),
            None
        );

        for format in [
            AudioFormat::Mp3,
            AudioFormat::Aac,
            AudioFormat::Opus,
            AudioFormat::Dts,
            AudioFormat::Ac3,
        ] {
            let rates = ffmpeg_lossy_encoder_direct_rates(&format).unwrap();
            assert!(rates.windows(2).all(|pair| pair[0] < pair[1]), "{format:?}: {rates:?}");
        }
    }
}

/// Resolve SSRC profile from explicit profile, insane mode, and quality.
#[must_use]
pub const fn ssrc_profile(settings: SsrcSettings, quality: ResampleQuality) -> SsrcProfile {
    if settings.insane_mode {
        return SsrcProfile::Insane;
    }
    if let Some(profile) = settings.profile {
        return profile;
    }
    match quality {
        ResampleQuality::Insane => SsrcProfile::Insane,
        ResampleQuality::Ultra => SsrcProfile::High,
        ResampleQuality::VeryHigh => SsrcProfile::Long,
        ResampleQuality::High => SsrcProfile::Standard,
        ResampleQuality::Medium => SsrcProfile::Short,
        ResampleQuality::Low => SsrcProfile::Fast,
    }
}

/// FFmpeg PCM codec for the target format and bit depth.
pub fn ffmpeg_pcm_codec(depth: PcmBitDepth, format: &AudioFormat) -> Result<&'static str> {
    let big_endian = matches!(format, AudioFormat::Aiff);
    match depth {
        PcmBitDepth::Int8 => Ok("pcm_u8"),
        PcmBitDepth::Int16 if big_endian => Ok("pcm_s16be"),
        PcmBitDepth::Int16 => Ok("pcm_s16le"),
        PcmBitDepth::Int24 if big_endian => Ok("pcm_s24be"),
        PcmBitDepth::Int24 => Ok("pcm_s24le"),
        PcmBitDepth::Int32 if big_endian => Ok("pcm_s32be"),
        PcmBitDepth::Int32 => Ok("pcm_s32le"),
        PcmBitDepth::Float32 if supports_float(format) && big_endian => Ok("pcm_f32be"),
        PcmBitDepth::Float32 if supports_float(format) => Ok("pcm_f32le"),
        PcmBitDepth::Float64 if supports_float(format) && big_endian => Ok("pcm_f64be"),
        PcmBitDepth::Float64 if supports_float(format) => Ok("pcm_f64le"),
        PcmBitDepth::Float32 | PcmBitDepth::Float64 => Err(PlanningError::invalid_settings(
            "target_bit_depth",
            format!(
                "{} does not support floating-point PCM output",
                format.display_name()
            ),
        )),
    }
}

/// True when a format can safely contain floating-point PCM in tonepoet workflows.
#[must_use]
pub fn supports_float(format: &AudioFormat) -> bool {
    matches!(format, AudioFormat::Wav | AudioFormat::Aiff | AudioFormat::WavPack)
}

/// FFmpeg sample format for a PCM bit depth.
#[must_use]
pub const fn ffmpeg_sample_fmt(depth: PcmBitDepth) -> &'static str {
    match depth {
        PcmBitDepth::Int8 => "u8",
        PcmBitDepth::Int16 => "s16",
        PcmBitDepth::Int24 | PcmBitDepth::Int32 => "s32",
        PcmBitDepth::Float32 => "flt",
        PcmBitDepth::Float64 => "dbl",
    }
}

/// FFmpeg AAC profile string.
#[must_use]
pub const fn ffmpeg_aac_profile(profile: AacProfile) -> &'static str {
    match profile {
        AacProfile::LcAac => "aac_low",
        AacProfile::HeAac => "aac_he",
        AacProfile::HeAacV2 => "aac_he_v2",
    }
}

const MP3_DIRECT_SAMPLE_RATES_HZ: &[u32] = &[
    8_000, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000,
];
const AAC_DIRECT_SAMPLE_RATES_HZ: &[u32] = &[
    8_000, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 64_000, 88_200,
    96_000,
];
const OPUS_DIRECT_SAMPLE_RATES_HZ: &[u32] = &[48_000];
const DTS_DIRECT_SAMPLE_RATES_HZ: &[u32] = &[
    8_000, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000,
];
const AC3_DIRECT_SAMPLE_RATES_HZ: &[u32] = &[32_000, 44_100, 48_000];

/// PCM rates Tonepoet may present directly to its configured FFmpeg encoder
/// without a rate conversion remaining inside the encoder boundary.
///
/// This is the single authority for both admission and ordinary-path rate
/// resolution. For MP3, AAC, DTS and AC-3 these are output sample rates. Opus
/// is different: its coded stream runs at 48 kHz, so lower libopus input-rate
/// modes are bandwidth limits rather than alternate output rates. Tonepoet
/// therefore exposes only 48 kHz as a direct Opus encoder-boundary rate and
/// performs any required conversion before encoding.
///
/// The encoder names are fixed by `plugins.rs`: `libmp3lame`, `libfdk_aac`,
/// `libopus`, `dca`, and `ac3`. The slices are strictly ascending.
#[must_use]
pub fn ffmpeg_lossy_encoder_direct_rates(format: &AudioFormat) -> Option<&'static [u32]> {
    match format {
        AudioFormat::Mp3 => Some(MP3_DIRECT_SAMPLE_RATES_HZ),
        AudioFormat::Aac => Some(AAC_DIRECT_SAMPLE_RATES_HZ),
        AudioFormat::Opus => Some(OPUS_DIRECT_SAMPLE_RATES_HZ),
        AudioFormat::Dts => Some(DTS_DIRECT_SAMPLE_RATES_HZ),
        AudioFormat::Ac3 => Some(AC3_DIRECT_SAMPLE_RATES_HZ),
        _ => None,
    }
}

/// Whether `sample_rate_hz` is a rate-stable direct boundary rate for
/// Tonepoet's configured FFmpeg lossy encoder.
///
/// This is intentionally an encoder-boundary authority, not a general codec
/// input-rate claim. FFmpeg/libopus may accept other input rates by resampling
/// before or inside the encoder, but those are not direct rate-stable paths and
/// therefore cannot be used after a proved album NormalizePeak gain.
#[must_use]
pub fn ffmpeg_lossy_encoder_accepts_rate_directly(
    format: &AudioFormat,
    sample_rate_hz: u32,
) -> Option<bool> {
    ffmpeg_lossy_encoder_direct_rates(format)
        .map(|rates| rates.binary_search(&sample_rate_hz).is_ok())
}

/// Resolve the ordinary lossy encoder-boundary rate for `requested_hz`.
///
/// Exact supported requests remain exact. Requests below or between supported
/// rates resolve upward to the smallest rate that can preserve their requested
/// bandwidth. Requests above the format maximum resolve downward to that
/// maximum. Since Opus exposes only 48 kHz in the authority table, every Opus
/// request resolves to 48 kHz.
///
/// Returns `None` only for targets without a built-in lossy rate authority.
#[must_use]
pub fn ffmpeg_lossy_encoder_rate_for_request(
    format: &AudioFormat,
    requested_hz: u32,
) -> Option<u32> {
    let rates = ffmpeg_lossy_encoder_direct_rates(format)?;
    match rates.binary_search(&requested_hz) {
        Ok(index) => Some(rates[index]),
        Err(index) if index < rates.len() => Some(rates[index]),
        Err(_) => rates.last().copied(),
    }
}

/// FFmpeg/libopus application string.
#[must_use]
pub const fn opus_application(content: OpusContentType) -> &'static str {
    match content {
        OpusContentType::Auto | OpusContentType::Music => "audio",
        OpusContentType::Speech => "voip",
    }
}

/// SoX MP3 `-C` value.
#[must_use]
pub fn sox_mp3_compression(mode: Mp3Mode, bitrate_kbps: u32, vbr_quality: u8) -> String {
    match mode {
        Mp3Mode::Cbr => bitrate_kbps.to_string(),
        Mp3Mode::Abr => format!("~{bitrate_kbps}"),
        Mp3Mode::Vbr => format!("-{vbr_quality}"),
    }
}

/// WavPack compression argument for FFmpeg.
#[must_use]
pub const fn wavpack_compression_level(mode: WavPackMode) -> u8 {
    match mode {
        WavPackMode::Fast => 0,
        WavPackMode::Normal => 1,
        WavPackMode::High => 2,
        WavPackMode::VeryHigh => 3,
    }
}

/// WavPack CLI mode flag for native `wavpack` command.
#[must_use]
pub const fn wavpack_mode_flag(mode: WavPackMode) -> &'static str {
    match mode {
        WavPackMode::Fast => "-f",
        WavPackMode::Normal => "",
        WavPackMode::High => "-h",
        WavPackMode::VeryHigh => "-hh",
    }
}

/// Sox sinc phase flag.
#[must_use]
pub const fn sox_sinc_phase_flag(phase: SoxSincPhase) -> &'static str {
    match phase {
        SoxSincPhase::Linear => "-L",
        SoxSincPhase::Minimum => "-M",
        SoxSincPhase::Intermediate => "-I",
    }
}

/// SoX-DSD shaper string such as `clans-8`.
#[must_use]
pub fn dsd_shaper_name(shaper: DsdNoiseShaper, order: ModulatorOrder) -> String {
    let prefix = match shaper {
        DsdNoiseShaper::Clans => "clans",
        DsdNoiseShaper::Sdm => "sdm",
        DsdNoiseShaper::Crfb => "crfb",
    };
    format!("{prefix}-{}", order.value())
}

/// Whether the given tool should avoid FFmpeg's SoXR dither and route dither to SoX instead.
#[must_use]
pub const fn requires_sox_dither(dither: DitherType) -> bool {
    matches!(
        dither,
        DitherType::Lipshitz | DitherType::Gesemann | DitherType::SlopedTpdf
    )
}
