use std::path::PathBuf;

use super::diagnostics::DiscDiagnostic;

/// Disc format identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscFormat {
    DvdAudio,
    Sacd,
}

impl DiscFormat {
    pub fn name(self) -> &'static str {
        match self {
            Self::DvdAudio => "DVD-Audio",
            Self::Sacd => "SACD",
        }
    }
}

/// How the audio format was determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatProvenance {
    AobProbe,
    Samg,
    IfoAttributes,
    TocHeader,
    Unknown,
}

/// Structured audio format for a presentation.
#[derive(Debug, Clone)]
pub struct AudioPresentationFormat {
    pub codec: Option<String>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub channels: Option<u8>,
    pub channel_layout: Option<String>,
    pub lossless: bool,
    pub provenance: FormatProvenance,
}

/// A single track within a presentation.
#[derive(Debug, Clone)]
pub struct DiscTrack {
    pub number: u32,
    pub title: Option<String>,
    pub duration_secs: Option<f64>,
    pub format_note: Option<String>,
}

/// Format-specific presentation identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentationId {
    DvdAudioGroup(u8),
    SacdArea(SacdAreaId),
}

/// SACD area identity within a PresentationId.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SacdAreaId {
    Stereo,
    MultiChannel,
}

/// A meaningful, user-selectable audio presentation on the disc.
#[derive(Debug, Clone)]
pub struct DiscPresentation {
    pub id: PresentationId,
    pub label: String,
    pub format: AudioPresentationFormat,
    pub tracks: Vec<DiscTrack>,
    pub total_duration_secs: f64,
}

/// A parser-discovered candidate excluded from the curated presentation list.
#[derive(Debug, Clone)]
pub struct SuppressedPresentation {
    pub id: PresentationId,
    pub reason: String,
    pub track_count: usize,
    pub duration_secs: f64,
    pub native_detail: Option<String>,
}

/// Simplified copy protection status for display.
#[derive(Debug, Clone)]
pub struct CopyProtectionSummary {
    pub description: String,
}

/// Result of probing one AOB sector for codec and format.
#[derive(Debug, Clone)]
pub struct AobProbeResult {
    pub codec: &'static str,
    pub sample_rate: u32,
    pub bit_depth: u32,
    pub channels: u8,
    pub channel_assignment_code: u8,
}

/// Unified browsable representation of an optical disc.
#[derive(Debug, Clone)]
pub struct DiscContents {
    pub format: DiscFormat,
    pub label: String,
    pub source_path: PathBuf,
    pub presentations: Vec<DiscPresentation>,
    pub suppressed: Vec<SuppressedPresentation>,
    pub copy_protection: CopyProtectionSummary,
    pub diagnostics: Vec<DiscDiagnostic>,
}
