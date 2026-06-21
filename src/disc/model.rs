use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::diagnostics::DiscDiagnostic;

/// Disc format identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscFormat {
    DvdAudio,
    Sacd,
    DvdVideo,
}

impl DiscFormat {
    pub fn name(self) -> &'static str {
        match self {
            Self::DvdAudio => "DVD-Audio",
            Self::Sacd => "SACD",
            Self::DvdVideo => "DVD-Video",
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
    pub performer: Option<String>,
    pub duration_secs: Option<f64>,
    pub format_note: Option<String>,
}

/// Format-specific presentation identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PresentationId {
    DvdAudioGroup(u8),
    SacdArea(SacdAreaId),
    /// DVD-Video title/audio-stream identity. VTS is included deliberately:
    /// a bare title number is ambiguous on real discs with multiple VTS sets,
    /// and the stream index is needed to convert non-default audio tracks.
    DvdVideoTitle {
        vts_number: u8,
        title_number: u8,
        audio_stream_index: u8,
    },
}

impl PresentationId {
    /// Construct a DVD-Video presentation identity from authored VTS/title and
    /// zero-based audio stream index values.
    pub fn dvd_video(vts_number: u8, title_number: u8, audio_stream_index: u8) -> Self {
        Self::DvdVideoTitle {
            vts_number,
            title_number,
            audio_stream_index,
        }
    }

    /// Return the authored DVD-Video identity tuple when this is a DVD-Video
    /// presentation. The audio stream index remains zero-based because that is
    /// the value carried into `SourceOptions` and the demux/materializer path.
    pub fn dvd_video_parts(&self) -> Option<(u8, u8, u8)> {
        match self {
            Self::DvdVideoTitle {
                vts_number,
                title_number,
                audio_stream_index,
            } => Some((*vts_number, *title_number, *audio_stream_index)),
            _ => None,
        }
    }

    /// Stable, short user-facing label. DVD-Video stream numbers are displayed
    /// one-based, while persisted/source-option state keeps the zero-based
    /// stream index required by the demux layer.
    pub fn display_label(&self) -> String {
        match self {
            Self::DvdAudioGroup(n) => format!("DVD-Audio group {n}"),
            Self::SacdArea(SacdAreaId::Stereo) => "SACD stereo area".to_string(),
            Self::SacdArea(SacdAreaId::MultiChannel) => "SACD multichannel area".to_string(),
            Self::DvdVideoTitle {
                vts_number,
                title_number,
                audio_stream_index,
            } => format!(
                "DVD-Video VTS {vts_number} title {title_number} audio stream {}",
                dvd_video_audio_stream_display_number(*audio_stream_index)
            ),
        }
    }

    /// Compact label used in dense CLI/table output.
    pub fn compact_label(&self) -> String {
        match self {
            Self::DvdAudioGroup(n) => format!("Group {n}"),
            Self::SacdArea(SacdAreaId::Stereo) => "Stereo".to_string(),
            Self::SacdArea(SacdAreaId::MultiChannel) => "Multichannel".to_string(),
            Self::DvdVideoTitle {
                vts_number,
                title_number,
                audio_stream_index,
            } => format!(
                "VTS {vts_number} Title {title_number} Stream {}",
                dvd_video_audio_stream_display_number(*audio_stream_index)
            ),
        }
    }
}

/// Convert the persisted/materializer zero-based DVD-Video audio stream index to
/// the one-based stream number shown to users.
pub fn dvd_video_audio_stream_display_number(audio_stream_index: u8) -> u16 {
    u16::from(audio_stream_index) + 1
}

/// SACD area identity within a PresentationId.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    pub album_title: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
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
    pub channel_label: String,
    /// Source layout label when this probe detected an authored stereo
    /// presentation derived from a multichannel carrier. `channels` remains the
    /// carrier channel count from the actual probed stream.
    pub stereo_downmix_source_label: Option<String>,
    pub mlp_num_substreams: Option<u32>,
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
    pub album_title: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
}

#[cfg(test)]
mod presentation_id_tests {
    use super::*;

    #[test]
    fn dvd_video_presentation_id_serializes_full_identity() {
        let id = PresentationId::dvd_video(2, 7, 3);
        let value = serde_json::to_value(id).expect("serialize DVD-Video presentation id");
        assert_eq!(value["kind"], "dvd_video_title");
        assert_eq!(value["value"]["vts_number"], 2);
        assert_eq!(value["value"]["title_number"], 7);
        assert_eq!(value["value"]["audio_stream_index"], 3);

        let round_trip: PresentationId =
            serde_json::from_value(value).expect("deserialize DVD-Video presentation id");
        assert_eq!(round_trip, id);
    }

    #[test]
    fn dvd_video_labels_display_one_based_stream_numbers() {
        let id = PresentationId::dvd_video(1, 2, 0);
        assert_eq!(id.display_label(), "DVD-Video VTS 1 title 2 audio stream 1");
        assert_eq!(id.compact_label(), "VTS 1 Title 2 Stream 1");
        assert_eq!(id.dvd_video_parts(), Some((1, 2, 0)));
    }
}
