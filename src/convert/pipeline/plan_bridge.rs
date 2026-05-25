//! Per-track bridge into `tonepoet-pipeline`.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use tonepoet_pipeline::{
    AudioCodec as PlannerCodec, AudioFormat as PlannerFormat, BitDepthTarget, MetadataDisposition,
    PcmBitDepth, PipelineSettings, PlanRequest, SampleKind, SourceInfo,
};

use super::errors::ConvertError;
use super::types::{PipelineRequest, PreparedTrack, StageRequirement, TrackSourceRef};

pub fn plan_request_for_track(
    request: &PipelineRequest,
    track: &PreparedTrack,
    realized_input: &Path,
    staged_output: &Path,
    intermediate_dir: PathBuf,
) -> Result<PlanRequest, ConvertError> {
    let source = source_info_for_realized_track(track, realized_input)?;
    source
        .validate()
        .map_err(|err| ConvertError::Backend(format!("invalid source facts for planner: {err}")))?;

    let mut settings = request.settings.clone();
    // Metadata stays in the per-track planner request. The pipeline planner already
    // consults ToolRegistry::metadata_disposition_for_step() and prunes redundant
    // metadata-transfer steps when the selected encoder writes the requested policy.
    // Track execution records the resulting effective MetadataDisposition so the
    // album post-processing gate can skip the legacy metadata stage only when every
    // successful track was already handled by the planner. ReplayGain remains
    // orchestrator-owned because album mode requires all completed tracks.
    settings.replay_gain.mode = None;
    if matches!(settings.target_bit_depth, BitDepthTarget::Source) {
        if let Some(depth) = source.bit_depth {
            settings.target_bit_depth = BitDepthTarget::Pcm(depth);
        }
    }
    settings
        .validate()
        .map_err(|err| ConvertError::Backend(format!("invalid pipeline settings: {err}")))?;

    Ok(PlanRequest {
        input_path: realized_input.to_path_buf(),
        output_path: staged_output.to_path_buf(),
        source,
        settings,
        intermediate_dir: Some(intermediate_dir),
    })
}


/// Return whether the planner request asks for any metadata effect.
#[must_use]
pub fn settings_request_metadata(settings: &PipelineSettings) -> bool {
    settings.metadata.transfer_tags
        || settings.metadata.preserve_artwork
        || settings.metadata.store_source_audio_md5
}

/// Decide whether the legacy album metadata stage is still necessary after the planner ran.
///
/// This consumes the post-planner effective [`MetadataDisposition`], not a pre-plan guess.
/// The orchestrator may skip its legacy metadata pass only for non-merged track artifacts
/// when metadata was requested and every successful track reports `WritesRequestedPolicy`.
#[must_use]
pub fn orchestrator_metadata_stage_required(
    disposition: MetadataDisposition,
    stage: StageRequirement,
    requested_metadata: bool,
) -> bool {
    matches!(stage, StageRequirement::Enabled)
        && requested_metadata
        && !disposition.writes_requested_policy()
}

pub fn source_info_for_realized_track(
    track: &PreparedTrack,
    realized_input: &Path,
) -> Result<SourceInfo, ConvertError> {
    let format = planner_format_from_path(realized_input).unwrap_or_else(|| match &track.source_ref {
        TrackSourceRef::SacdTrack { .. } => PlannerFormat::Dsf,
        _ => PlannerFormat::Flac,
    });
    let codec = codec_for_format(&format);
    let is_dsd = format.is_dsd() || codec.is_dsd();
    let bit_depth = if is_dsd {
        None
    } else {
        track.bit_depth.and_then(pcm_bit_depth_from_u32)
    };
    let sample_kind = if is_dsd {
        Some(SampleKind::Dsd)
    } else {
        bit_depth.map(|depth| match depth {
            PcmBitDepth::Float32 | PcmBitDepth::Float64 => SampleKind::Float,
            _ => SampleKind::SignedInteger,
        })
    };

    Ok(SourceInfo {
        format,
        codec,
        sample_rate_hz: (track.sample_rate > 0).then_some(track.sample_rate),
        bit_depth,
        sample_kind,
        channels: None,
        duration: None,
        audio_md5: flac_streaminfo_audio_md5(realized_input),
    })
}

pub fn planner_format_from_main(format: crate::convert::AudioFormat) -> PlannerFormat {
    match format {
        crate::convert::AudioFormat::Flac => PlannerFormat::Flac,
        crate::convert::AudioFormat::Wav => PlannerFormat::Wav,
        crate::convert::AudioFormat::Aiff => PlannerFormat::Aiff,
        crate::convert::AudioFormat::WavPack => PlannerFormat::WavPack,
        crate::convert::AudioFormat::Mp3 => PlannerFormat::Mp3,
        crate::convert::AudioFormat::Aac => PlannerFormat::Aac,
        crate::convert::AudioFormat::Opus => PlannerFormat::Opus,
        crate::convert::AudioFormat::Alac => PlannerFormat::Alac,
        crate::convert::AudioFormat::Dsf => PlannerFormat::Dsf,
        crate::convert::AudioFormat::Dff => PlannerFormat::Dff,
    }
}

pub fn planner_format_from_path(path: &Path) -> Option<PlannerFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "flac" => Some(PlannerFormat::Flac),
        "wav" | "wave" => Some(PlannerFormat::Wav),
        "aiff" | "aif" | "aifc" => Some(PlannerFormat::Aiff),
        "wv" => Some(PlannerFormat::WavPack),
        "mp3" => Some(PlannerFormat::Mp3),
        "aac" | "m4a" | "mp4" => Some(PlannerFormat::Aac),
        "opus" => Some(PlannerFormat::Opus),
        "alac" => Some(PlannerFormat::Alac),
        "dsf" => Some(PlannerFormat::Dsf),
        "dff" => Some(PlannerFormat::Dff),
        _ => None,
    }
}

fn codec_for_format(format: &PlannerFormat) -> PlannerCodec {
    match format {
        PlannerFormat::Flac => PlannerCodec::Flac,
        PlannerFormat::Wav | PlannerFormat::Aiff => PlannerCodec::PcmSigned,
        PlannerFormat::WavPack => PlannerCodec::WavPack,
        PlannerFormat::Mp3 => PlannerCodec::Mp3,
        PlannerFormat::Aac => PlannerCodec::Aac,
        PlannerFormat::Opus => PlannerCodec::Opus,
        PlannerFormat::Alac => PlannerCodec::Alac,
        PlannerFormat::Dsf | PlannerFormat::Dff => PlannerCodec::Dsd,
        PlannerFormat::Custom { display_name, .. } => PlannerCodec::Custom(display_name.clone()),
    }
}

fn pcm_bit_depth_from_u32(bits: u32) -> Option<PcmBitDepth> {
    match bits {
        8 => Some(PcmBitDepth::Int8),
        16 => Some(PcmBitDepth::Int16),
        24 => Some(PcmBitDepth::Int24),
        32 => Some(PcmBitDepth::Int32),
        _ => None,
    }
}

fn flac_streaminfo_audio_md5(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic).ok()?;
    if &magic != b"fLaC" {
        return None;
    }

    loop {
        let mut header = [0_u8; 4];
        file.read_exact(&mut header).ok()?;
        let is_last = header[0] & 0x80 != 0;
        let block_type = header[0] & 0x7f;
        let len = ((header[1] as u32) << 16) | ((header[2] as u32) << 8) | header[3] as u32;

        if block_type == 0 {
            if len < 34 {
                return None;
            }
            let mut streaminfo = vec![0_u8; len as usize];
            file.read_exact(&mut streaminfo).ok()?;
            let md5 = &streaminfo[18..34];
            if md5.iter().all(|byte| *byte == 0) {
                return None;
            }
            return Some(md5.iter().map(|byte| format!("{byte:02x}")).collect());
        }

        file.seek(SeekFrom::Current(len as i64)).ok()?;
        if is_last {
            return None;
        }
    }
}
