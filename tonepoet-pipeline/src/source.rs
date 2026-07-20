//! Source facts supplied by the caller after probing or extraction.

use crate::dsd_reference::DsdSourceKind;
use crate::enums::{AudioCodec, AudioFormat, DsdRate, PcmBitDepth, SampleKind};
use crate::error::{PlanningError, Result};
use std::time::Duration;


/// Source-domain representation class used by Source-depth policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SourceRepresentationKind {
    /// The original stream is integer or float PCM.
    Pcm,
    /// The original stream is 1-bit DSD; no PCM width exists.
    Dsd,
    /// The original stream is a lossy codec; no PCM width exists.
    Lossy,
    /// The original representation was probed but could not be classified.
    Unknown,
    /// No explicit representation fact was serialized. This backward-compatible
    /// state permits inference from legacy format/codec/sample-kind facts.
    Unspecified,
}

impl Default for SourceRepresentationKind {
    fn default() -> Self {
        Self::Unspecified
    }
}

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
    /// PCM bit depth of the realized planner input when known. This remains
    /// carrier-first: a decoded staging WAV may be 32-bit even when the
    /// authoritative original source was 16-bit.
    pub bit_depth: Option<PcmBitDepth>,
    /// Authoritative PCM representation of the original source when known.
    /// This is distinct from `bit_depth` so dither and Source-depth policy can
    /// use source truth without suppressing required carrier conversion flags.
    #[cfg_attr(feature = "serde", serde(default))]
    pub true_source_depth: Option<PcmBitDepth>,
    /// Original source representation class. This remains separate from the
    /// realized carrier so a decoded WAV cannot make a lossy or unknown source
    /// look like authoritative PCM. Older serialized requests infer from the
    /// remaining source facts.
    #[cfg_attr(feature = "serde", serde(default))]
    pub source_representation: SourceRepresentationKind,
    /// Sample representation when known.
    pub sample_kind: Option<SampleKind>,
    /// Channel count when known.
    pub channels: Option<u16>,
    /// Duration when known.
    pub duration: Option<Duration>,
    /// Qualified DSD container/front-end facts when the source is DSD.
    #[cfg_attr(feature = "serde", serde(default))]
    pub dsd_source_kind: Option<DsdSourceKind>,
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


    /// Classify whether the original source has PCM, DSD, lossy, or unknown
    /// representation semantics. Container names alone do not prove PCM when
    /// the codec/sample-kind probe failed.
    #[must_use]
    pub fn representation_kind(&self) -> SourceRepresentationKind {
        if !matches!(
            self.source_representation,
            SourceRepresentationKind::Unspecified
        ) {
            return self.source_representation;
        }
        if self.is_dsd() {
            SourceRepresentationKind::Dsd
        } else if self.codec.is_lossy() || self.format.is_lossy() {
            SourceRepresentationKind::Lossy
        } else if matches!(
            self.codec,
            AudioCodec::Flac
                | AudioCodec::PcmSigned
                | AudioCodec::PcmUnsigned
                | AudioCodec::PcmFloat
                | AudioCodec::WavPack
                | AudioCodec::Alac
        ) || matches!(
            self.sample_kind,
            Some(SampleKind::SignedInteger | SampleKind::UnsignedInteger | SampleKind::Float)
        ) {
            SourceRepresentationKind::Pcm
        } else {
            SourceRepresentationKind::Unknown
        }
    }

    /// Return the authoritative original-source PCM depth. For ordinary files
    /// this falls back to the realized input depth; decoded carriers set the
    /// dedicated source field explicitly.
    #[must_use]
    pub fn authoritative_pcm_depth(&self) -> Option<PcmBitDepth> {
        match self.source_representation {
            // Legacy callers predate the split between realized carrier width
            // and original-source width, so their sole depth fact remains
            // authoritative when no explicit representation class was supplied.
            SourceRepresentationKind::Unspecified => self.true_source_depth.or(self.bit_depth),
            SourceRepresentationKind::Pcm => self.true_source_depth,
            SourceRepresentationKind::Dsd
            | SourceRepresentationKind::Lossy
            | SourceRepresentationKind::Unknown => None,
        }
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
        if self.is_dsd() && (self.bit_depth.is_some() || self.true_source_depth.is_some()) {
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
