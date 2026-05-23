//! Source facts supplied by the caller after probing or extraction.

use crate::enums::{AudioCodec, AudioFormat, DsdRate, PcmBitDepth, SampleKind};
use crate::error::{PlanningError, Result};
use std::time::Duration;

/// Read-only audio facts required for deterministic planning.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SourceInfo {
    /// Detected container or source format.
    pub format: AudioFormat,
    /// Detected primary audio codec.
    pub codec: AudioCodec,
    /// Sample rate in Hz when known.
    pub sample_rate_hz: Option<u32>,
    /// PCM bit depth when known.
    pub bit_depth: Option<PcmBitDepth>,
    /// Sample representation when known.
    pub sample_kind: Option<SampleKind>,
    /// Channel count when known.
    pub channels: Option<u16>,
    /// Duration when known.
    pub duration: Option<Duration>,
    /// Optional source-audio MD5 supplied by the caller when MD5 tagging is requested.
    pub audio_md5: Option<String>,
}

impl SourceInfo {
    /// True when explicit source facts identify the stream as DSD.
    ///
    /// A DSD-rate sample rate by itself is not enough: high-rate PCM exists, and
    /// the planner must not route PCM through DSD paths by coincidence.
    #[must_use]
    pub fn is_dsd(&self) -> bool {
        self.format.is_dsd()
            || self.codec.is_dsd()
            || matches!(self.sample_kind, Some(SampleKind::Dsd))
    }

    /// Resolve the source DSD rate from explicit DSD facts plus sample rate.
    #[must_use]
    pub fn dsd_rate(&self) -> Option<DsdRate> {
        if self.is_dsd() {
            self.sample_rate_hz.and_then(DsdRate::from_hz)
        } else {
            None
        }
    }

    /// Validate source facts that the planner cannot infer safely.
    pub fn validate(&self) -> Result<()> {
        if let Some(hz) = self.sample_rate_hz {
            if hz == 0 {
                return Err(PlanningError::invalid_source(
                    "sample_rate_hz",
                    "sample rate must be greater than zero",
                ));
            }
        }
        if let Some(channels) = self.channels {
            if channels == 0 {
                return Err(PlanningError::invalid_source(
                    "channels",
                    "channel count must be greater than zero",
                ));
            }
        }
        if self.is_dsd() && self.sample_rate_hz.is_some() && self.dsd_rate().is_none() {
            return Err(PlanningError::invalid_source(
                "sample_rate_hz",
                "explicit DSD source facts require a known DSD sample-rate multiple",
            ));
        }
        if matches!(
            self.sample_kind,
            Some(SampleKind::SignedInteger | SampleKind::UnsignedInteger | SampleKind::Float)
        ) && (self.format.is_dsd() || self.codec.is_dsd())
        {
            return Err(PlanningError::invalid_source(
                "sample_kind",
                "PCM sample kind conflicts with DSD format or codec facts",
            ));
        }
        if self.is_dsd() && self.bit_depth.is_some() {
            return Err(PlanningError::invalid_source(
                "bit_depth",
                "DSD sources must not report a PCM bit depth",
            ));
        }
        if let Some(md5) = &self.audio_md5 {
            let valid =
                md5.len() == 32 && md5.as_bytes().iter().all(|byte| byte.is_ascii_hexdigit());
            if !valid {
                return Err(PlanningError::invalid_source(
                    "audio_md5",
                    "audio MD5 must be 32 hexadecimal characters",
                ));
            }
        }
        Ok(())
    }
}
