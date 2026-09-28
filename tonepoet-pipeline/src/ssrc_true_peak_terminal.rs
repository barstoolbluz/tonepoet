//! Qualification authority for SSRC-owned true-peak terminal realization.
//!
//! The protected Binary64 SSRC pass establishes the Float64 observation
//! carrier.  The certified terminal replays the *same protected source-rate
//! Float64 ingress* through the same qualified SSRC executable, with the same
//! resampling controls, and expresses the resolved true-peak scalar as a
//! diagonal `--mixChannels` matrix.  SSRC therefore owns the final resample,
//! gain multiplication and final PCM sample realization in one execution.
//! Integer cells also own the user-selected dither/noise shaping; floating-point
//! cells are commissioned only with dither/PDF inactive, matching direct SSRC.
//!
//! Production admission is intentionally fail-closed.  This source tree ships
//! an empty generated registry plus qualification/commissioning tooling.  A
//! cell becomes usable only after execution evidence produced on the operator's
//! machine is reviewed and promoted into the registry.

use crate::{PcmBitDepth, SsrcPdfType, SsrcProfile};

/// Versioned contract for replaying the protected source-rate Float64 ingress
/// through SSRC as the final true-peak terminal.
pub const SSRC_TRUE_PEAK_REPLAY_TERMINAL_V1: &str =
    "tonepoet:ssrc-true-peak-replay-terminal/v1";
/// Versioned authority for the exact diagonal `--mixChannels` gain matrix.
pub const SSRC_MIXCHANNELS_GAIN_V1: &str =
    "tonepoet:ssrc-mixchannels-diagonal-gain/v1";
/// Fixed-point scale used to store terminal error in billionths of a target LSB.
pub const SSRC_TERMINAL_ERROR_LSB_SCALE: u64 = 1_000_000_000;
/// Deterministic SSRC dither seed used by the certified terminal contract.
///
/// Fixing the seed makes qualification and production realization exactly
/// replayable.  The seed is part of the versioned terminal contract; changing
/// it requires new execution evidence and a new contract revision.
pub const SSRC_TRUE_PEAK_DITHER_SEED_V1: u32 = 1;

/// Exact physical cell qualified for terminal replay. Runtime gain is not a
/// registry key: qualification samples representative gain points to derive the
/// per-cell error bound, but those sample extrema are not a product-policy
/// admission range. Channel count *is* a key because it changes the diagonal
/// matrix and the physical SSRC execution cell.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SsrcTruePeakTerminalScope {
    /// SSRC quality profile used by both observation and terminal replay.
    pub profile: SsrcProfile,
    /// Source sample rate of the protected replay ingress.
    pub source_rate_hz: u32,
    /// Destination sample rate realized by SSRC.
    pub target_rate_hz: u32,
    /// Interleaved channel count covered by the gain matrix and evidence.
    pub channels: u16,
    /// Final PCM depth realized by SSRC.
    pub target_bit_depth: PcmBitDepth,
    /// Exact SSRC native dither/noise-shaper ID, when active.
    pub dither_id: Option<u8>,
    /// Exact SSRC probability-density selection, when active.
    pub pdf_type: Option<SsrcPdfType>,
    /// Canonical attenuation already owned by the selected protected SSRC
    /// resampler (the commissioned Binary64 grid currently uses `0.0`).
    pub base_attenuation_db: Option<String>,
    /// Whether SSRC minimum-phase resampling is active.
    pub min_phase: bool,
    /// Runtime architecture on which the physical cell was qualified.
    pub architecture: String,
}

/// Depth-aware stored-sample error authority for a commissioned terminal cell.
///
/// Integer PCM is naturally expressed in target-LSB units. Floating-point PCM
/// has no fixed LSB across the normalized range, so its bound is stored as the
/// exact bits of a conservative absolute linear-full-scale `f64` upper bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SsrcTruePeakStoredSampleErrorBound {
    /// Billionths of one target integer LSB.
    TargetLsbNano(u64),
    /// Exact `f64::to_bits()` representation of an absolute linear-FS bound.
    AbsoluteLinearFsBits(u64),
}

impl SsrcTruePeakStoredSampleErrorBound {
    fn linear_upper(self, depth: PcmBitDepth) -> Result<f64, String> {
        let bound = match (self, depth) {
            (
                Self::TargetLsbNano(nano),
                PcmBitDepth::Int8
                | PcmBitDepth::Int16
                | PcmBitDepth::Int24
                | PcmBitDepth::Int32,
            ) => {
                if nano == 0 {
                    return Err("SSRC terminal evidence has a zero integer stored-sample error bound".to_owned());
                }
                let lsb = 2.0_f64.powi(-((depth.bits() - 1) as i32));
                (nano as f64 / SSRC_TERMINAL_ERROR_LSB_SCALE as f64) * lsb
            }
            (Self::AbsoluteLinearFsBits(bits), PcmBitDepth::Float32 | PcmBitDepth::Float64) => {
                f64::from_bits(bits)
            }
            (Self::TargetLsbNano(_), PcmBitDepth::Float32 | PcmBitDepth::Float64) => {
                return Err("SSRC floating-point terminal evidence cannot use a target-LSB error bound".to_owned());
            }
            (
                Self::AbsoluteLinearFsBits(_),
                PcmBitDepth::Int8
                | PcmBitDepth::Int16
                | PcmBitDepth::Int24
                | PcmBitDepth::Int32,
            ) => {
                return Err("SSRC integer terminal evidence cannot use an absolute floating-point error bound".to_owned());
            }
        };
        if !bound.is_finite() || bound <= 0.0 {
            return Err("SSRC terminal evidence has an invalid stored-sample error bound".to_owned());
        }
        Ok(bound)
    }
}

/// Reviewed production authority for one exact SSRC true-peak terminal cell.
///
/// The binding couples execution evidence, executable/source/build identity,
/// physical scope, representative gain characterization, and the stored-sample
/// error bound charged by the hard-ceiling solver and rechecked on every track.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SelectedSsrcTruePeakTerminalBinding {
    /// Versioned terminal replay contract.
    pub contract_id: String,
    /// Versioned gain-rendering authority.
    pub gain_model_id: String,
    /// Human-readable execution evidence identity.
    pub evidence_id: String,
    /// SHA-256 of the qualification report promoted into this record.
    pub qualification_report_sha256: String,
    /// SHA-256 of the exact SSRC executable characterized by the report.
    pub expected_executable_sha256: String,
    /// Pinned SSRC source revision characterized by the report.
    pub source_revision: String,
    /// Reproducible build/closure identity bound to the executable.
    pub build_identity: String,
    /// Exact physical terminal cell covered by the evidence.
    pub scope: SsrcTruePeakTerminalScope,
    /// Lowest representative gain executed during qualification. Evidence
    /// metadata only; this is not a runtime feature-availability limit.
    pub minimum_gain_db_nano: i64,
    /// Highest representative gain executed during qualification. Evidence
    /// metadata only; this is not a runtime feature-availability limit.
    pub maximum_gain_db_nano: i64,
    /// Conservative stored-sample realization error for this physical cell.
    pub stored_sample_error_bound: SsrcTruePeakStoredSampleErrorBound,
}

impl SelectedSsrcTruePeakTerminalBinding {
    /// Conservative stored-sample error bound in absolute linear full scale.
    pub fn stored_sample_error_linear_upper(&self) -> Result<f64, String> {
        self.stored_sample_error_bound
            .linear_upper(self.scope.target_bit_depth)
    }

    /// Validate that a runtime true-peak gain can be represented by the exact
    /// production SSRC gain matrix. Qualification gain points characterize the
    /// cell error bound; their min/max do not impose a hidden normalization cap.
    pub fn validate_gain_db_nano(&self, gain_db_nano: i64) -> Result<(), String> {
        ssrc_true_peak_gain_matrix(crate::DbNano(gain_db_nano), self.scope.channels)
            .map(|_| ())
            .map_err(|error| {
                format!(
                    "SSRC terminal gain {:.9} dB cannot be represented by the certified gain matrix: {error}",
                    gain_db_nano as f64 / 1_000_000_000.0,
                )
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommissionedSsrcTruePeakTerminalRecord {
    pub profile: SsrcProfile,
    pub source_rate_hz: u32,
    pub target_rate_hz: u32,
    pub channels: u16,
    pub target_bit_depth: PcmBitDepth,
    pub dither_id: Option<u8>,
    pub pdf_type: Option<SsrcPdfType>,
    pub base_attenuation_db: Option<&'static str>,
    pub min_phase: bool,
    pub architecture: &'static str,
    pub evidence_id: &'static str,
    pub qualification_report_sha256: &'static str,
    pub expected_executable_sha256: &'static str,
    pub source_revision: &'static str,
    pub build_identity: &'static str,
    pub minimum_gain_db_nano: i64,
    pub maximum_gain_db_nano: i64,
    pub stored_sample_error_bound: SsrcTruePeakStoredSampleErrorBound,
}

mod commissioned {
    include!("ssrc_true_peak_terminal_commissioned.rs");
}

fn binding_from_record(
    record: &CommissionedSsrcTruePeakTerminalRecord,
) -> SelectedSsrcTruePeakTerminalBinding {
    SelectedSsrcTruePeakTerminalBinding {
        contract_id: SSRC_TRUE_PEAK_REPLAY_TERMINAL_V1.to_owned(),
        gain_model_id: SSRC_MIXCHANNELS_GAIN_V1.to_owned(),
        evidence_id: record.evidence_id.to_owned(),
        qualification_report_sha256: record.qualification_report_sha256.to_owned(),
        expected_executable_sha256: record.expected_executable_sha256.to_owned(),
        source_revision: record.source_revision.to_owned(),
        build_identity: record.build_identity.to_owned(),
        scope: SsrcTruePeakTerminalScope {
            profile: record.profile,
            source_rate_hz: record.source_rate_hz,
            target_rate_hz: record.target_rate_hz,
            channels: record.channels,
            target_bit_depth: record.target_bit_depth,
            dither_id: record.dither_id,
            pdf_type: record.pdf_type,
            base_attenuation_db: record.base_attenuation_db.map(str::to_owned),
            min_phase: record.min_phase,
            architecture: record.architecture.to_owned(),
        },
        minimum_gain_db_nano: record.minimum_gain_db_nano,
        maximum_gain_db_nano: record.maximum_gain_db_nano,
        stored_sample_error_bound: record.stored_sample_error_bound,
    }
}

/// Return the commissioned production binding for one exact physical scope.
#[must_use]
pub fn production_binding_for_scope(
    scope: &SsrcTruePeakTerminalScope,
) -> Option<SelectedSsrcTruePeakTerminalBinding> {
    let mut matches = commissioned::COMMISSIONED_SSRC_TRUE_PEAK_TERMINALS
        .iter()
        .filter(|record| {
            record.profile == scope.profile
                && record.source_rate_hz == scope.source_rate_hz
                && record.target_rate_hz == scope.target_rate_hz
                && record.channels == scope.channels
                && record.target_bit_depth == scope.target_bit_depth
                && record.dither_id == scope.dither_id
                && record.pdf_type == scope.pdf_type
                && record.base_attenuation_db == scope.base_attenuation_db.as_deref()
                && record.min_phase == scope.min_phase
                && record.architecture == scope.architecture
        });
    let record = matches.next()?;
    // A generated registry must contain at most one authority record for an
    // exact physical scope.  Reject duplicate scope authority rather than
    // letting source order silently choose which evidence wins.
    if matches.next().is_some() {
        return None;
    }
    Some(binding_from_record(record))
}

/// Revalidate a selected binding against the exact runtime scope and pins.
pub fn validate_selected_binding(
    selected: &SelectedSsrcTruePeakTerminalBinding,
    expected: &SsrcTruePeakTerminalScope,
) -> Result<(), String> {
    if selected.contract_id != SSRC_TRUE_PEAK_REPLAY_TERMINAL_V1 {
        return Err(format!(
            "SSRC terminal contract {} is not {}",
            selected.contract_id, SSRC_TRUE_PEAK_REPLAY_TERMINAL_V1,
        ));
    }
    if selected.gain_model_id != SSRC_MIXCHANNELS_GAIN_V1 {
        return Err(format!(
            "SSRC terminal gain model {} is not {}",
            selected.gain_model_id, SSRC_MIXCHANNELS_GAIN_V1,
        ));
    }
    let expected_evidence_id = format!("sha256:{}", selected.qualification_report_sha256);
    if selected.evidence_id != expected_evidence_id {
        return Err(format!(
            "SSRC terminal evidence identity {} does not bind qualification report {}",
            selected.evidence_id, selected.qualification_report_sha256,
        ));
    }
    if selected.build_identity.trim().is_empty() {
        return Err("SSRC terminal evidence has an empty build identity".to_owned());
    }
    if &selected.scope != expected {
        return Err(format!(
            "SSRC terminal commissioned scope {:?} does not match runtime scope {:?}",
            selected.scope, expected,
        ));
    }
    if selected.scope.architecture != std::env::consts::ARCH {
        return Err(format!(
            "SSRC terminal evidence targets architecture {}, runtime is {}",
            selected.scope.architecture,
            std::env::consts::ARCH,
        ));
    }
    if selected.scope.source_rate_hz == 0
        || selected.scope.target_rate_hz == 0
        || selected.scope.channels == 0
    {
        return Err("SSRC terminal evidence has invalid audio geometry".to_owned());
    }
    if selected.source_revision != crate::ssrc_binary64::PINNED_SSRC_SOURCE_REV {
        return Err(format!(
            "SSRC terminal evidence source revision {} is not pinned revision {}",
            selected.source_revision,
            crate::ssrc_binary64::PINNED_SSRC_SOURCE_REV,
        ));
    }
    if selected.minimum_gain_db_nano > selected.maximum_gain_db_nano {
        return Err("SSRC terminal evidence has an inverted qualification gain corpus".to_owned());
    }
    if selected.scope.target_bit_depth.is_float()
        && (selected.scope.dither_id.is_some() || selected.scope.pdf_type.is_some())
    {
        return Err("SSRC floating-point terminal evidence must have dither and PDF disabled".to_owned());
    }
    if selected.scope.dither_id.is_none() && selected.scope.pdf_type.is_some() {
        return Err("SSRC terminal evidence cannot activate a PDF without dither".to_owned());
    }
    for (label, digest) in [
        ("executable", selected.expected_executable_sha256.as_str()),
        ("report", selected.qualification_report_sha256.as_str()),
    ] {
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("SSRC terminal evidence has an invalid {label} SHA-256"));
        }
    }
    selected.stored_sample_error_linear_upper()?;
    Ok(())
}

/// Render the exact diagonal matrix used by the certified terminal.
///
/// The scalar comes from the same directed-lower dB conversion that the
/// hard-ceiling solver already uses for Tonepoet's external Float64 scalar.
/// Rust's finite-f64 display is shortest-round-trip, so the decimal consumed by
/// SSRC reconstructs this exact binary64 value rather than a coarser dB value.
pub fn ssrc_true_peak_gain_matrix(
    gain_db: crate::DbNano,
    channels: u16,
) -> Result<String, String> {
    if channels == 0 {
        return Err("SSRC true-peak gain matrix requires at least one channel".to_owned());
    }
    if channels > 64 {
        return Err(format!(
            "SSRC true-peak gain matrix refuses implausible channel count {channels}"
        ));
    }
    let gain = crate::dsd_album_gain::conservative_linear_gain_lower(gain_db)?;
    if !gain.is_finite() || gain < 0.0 {
        return Err("SSRC true-peak directed gain is not a finite nonnegative scalar".to_owned());
    }
    let gain = gain.to_string();
    let channels = usize::from(channels);
    let mut matrix = String::new();
    for row in 0..channels {
        if row != 0 {
            matrix.push(';');
        }
        for column in 0..channels {
            if column != 0 {
                matrix.push(',');
            }
            if row == column {
                matrix.push_str(&gain);
            } else {
                matrix.push('0');
            }
        }
    }
    Ok(matrix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DbNano;

    #[test]
    fn diagonal_gain_matrix_uses_directed_scalar() {
        let gain_db = "-6.000000000".parse::<DbNano>().unwrap();
        let scalar = crate::dsd_album_gain::conservative_linear_gain_lower(gain_db).unwrap();
        let matrix = ssrc_true_peak_gain_matrix(gain_db, 2).unwrap();
        assert_eq!(matrix, format!("{scalar},0;0,{scalar}"));
    }

    #[test]
    fn zero_db_matrix_is_identity() {
        assert_eq!(ssrc_true_peak_gain_matrix(DbNano::ZERO, 1).unwrap(), "1");
        assert_eq!(
            ssrc_true_peak_gain_matrix(DbNano::ZERO, 3).unwrap(),
            "1,0,0;0,1,0;0,0,1"
        );
    }

    #[test]
    fn matrix_rejects_zero_channels() {
        assert!(ssrc_true_peak_gain_matrix(DbNano::ZERO, 0).is_err());
    }

    #[test]
    fn gain_validation_is_not_capped_by_qualification_sample_extrema() {
        let gain: DbNano = "29.900000000".parse().unwrap();
        let matrix = ssrc_true_peak_gain_matrix(gain, 2).unwrap();
        assert!(!matrix.is_empty());
    }

    #[test]
    fn unrepresentable_gain_still_fails_closed() {
        assert!(ssrc_true_peak_gain_matrix(DbNano(i64::MAX), 2).is_err());
    }
}
