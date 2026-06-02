//! Built-in tool plugins.
//!
//! The builders here are fresh implementations against the unified planning
//! types. They use the reference files only for argument-shape semantics.

use crate::enums::{
    AacProfile, AudioFormat, DitherType, DsdFilterPreset, DsdLowpassMethod, GainCompensation,
    Mp3Mode, PcmBitDepth, ReplayGainMode, SoxSincPhase,
};
use crate::error::{PlanningError, Result};
use crate::mapping;
use crate::plan::{PlanContext, PlanOperation, PlanStep, PlannedCommand};
use crate::tools::{MetadataDisposition, ToolIdentifier, ToolPlugin, ToolSupport};

/// FFmpeg plugin.
#[derive(Debug, Default, Clone, Copy)]
pub struct FfmpegPlugin;

/// SoX plugin.
#[derive(Debug, Default, Clone, Copy)]
pub struct SoxPlugin;

/// SSRC plugin.
#[derive(Debug, Default, Clone, Copy)]
pub struct SsrcPlugin;

/// loudgain ReplayGain plugin.
#[derive(Debug, Default, Clone, Copy)]
pub struct LoudgainPlugin;

/// metaflac FLAC metadata plugin.
#[derive(Debug, Default, Clone, Copy)]
pub struct MetaflacPlugin;

/// FLAC command-line plugin for native decode verification.
#[derive(Debug, Default, Clone, Copy)]
pub struct FlacPlugin;

impl ToolPlugin for FfmpegPlugin {
    fn id(&self) -> ToolIdentifier {
        ToolIdentifier::Ffmpeg
    }

    fn supports(&self, context: &PlanContext<'_>, step: &PlanStep) -> ToolSupport {
        match &step.operation {
            PlanOperation::DecodeToPcm { .. } => ToolSupport::CANONICAL,
            PlanOperation::ResamplePcm {
                brick_wall: false, ..
            } => ToolSupport::SUPPORTED,
            PlanOperation::ResamplePcm {
                brick_wall: true, ..
            } => ToolSupport::UNSUPPORTED,
            PlanOperation::EncodePcm {
                target_format,
                apply_processing,
                ..
            } => {
                let dither = context.request.settings.dither_type;
                let dither_supported = !*apply_processing || !mapping::requires_sox_dither(dither);
                if target_format.ffmpeg_encodable() && dither_supported {
                    if *apply_processing {
                        ToolSupport::SUPPORTED
                    } else {
                        ToolSupport::PREFERRED
                    }
                } else {
                    ToolSupport::UNSUPPORTED
                }
            }
            PlanOperation::EncodeLossy { target_format, .. }
                if target_format.ffmpeg_encodable() =>
            {
                ToolSupport::CANONICAL
            }
            PlanOperation::MetadataTransfer {
                target_format,
                transfer_tags,
                preserve_artwork,
            } if ffmpeg_metadata_transfer_supported(
                target_format,
                *transfer_tags,
                *preserve_artwork,
            ) =>
            {
                ToolSupport::CANONICAL
            }
            PlanOperation::Verify { .. } => ToolSupport::FALLBACK,
            _ => ToolSupport::UNSUPPORTED,
        }
    }

    fn metadata_disposition(
        &self,
        context: &PlanContext<'_>,
        step: &PlanStep,
    ) -> MetadataDisposition {
        match &step.operation {
            PlanOperation::EncodePcm { target_format, .. }
            | PlanOperation::EncodeLossy { target_format, .. }
                if ffmpeg_encoder_writes_requested_metadata_policy(context, target_format) =>
            {
                MetadataDisposition::WritesRequestedPolicy
            }
            PlanOperation::MetadataTransfer {
                target_format,
                transfer_tags,
                preserve_artwork,
            } if ffmpeg_metadata_transfer_supported(
                target_format,
                *transfer_tags,
                *preserve_artwork,
            ) =>
            {
                MetadataDisposition::WritesRequestedPolicy
            }
            _ => MetadataDisposition::DoesNotWrite,
        }
    }

    fn build_command(&self, context: &PlanContext<'_>, step: &PlanStep) -> Result<PlannedCommand> {
        match &step.operation {
            PlanOperation::DecodeToPcm { bit_depth } => {
                build_ffmpeg_decode(context, step, *bit_depth)
            }
            PlanOperation::ResamplePcm {
                target_rate_hz,
                target_bit_depth,
                brick_wall: false,
                ..
            } => build_ffmpeg_resample(context, step, *target_rate_hz, *target_bit_depth),
            PlanOperation::EncodePcm {
                target_format,
                target_rate_hz,
                target_bit_depth,
                apply_processing,
            } => build_ffmpeg_encode_pcm(
                context,
                step,
                target_format,
                *target_rate_hz,
                *target_bit_depth,
                *apply_processing,
            ),
            PlanOperation::EncodeLossy {
                target_format,
                target_rate_hz,
                apply_processing,
            } => build_ffmpeg_encode_lossy(
                context,
                step,
                target_format,
                *target_rate_hz,
                *apply_processing,
            ),
            PlanOperation::MetadataTransfer {
                target_format,
                transfer_tags,
                preserve_artwork,
            } => build_ffmpeg_metadata_transfer(
                context,
                step,
                target_format,
                *transfer_tags,
                *preserve_artwork,
            ),
            PlanOperation::Verify { .. } => build_ffmpeg_verify(context, step),
            _ => Err(PlanningError::plugin_rejected(
                self.id(),
                format!("unsupported operation {}", step.operation.label()),
            )),
        }
    }
}

impl ToolPlugin for SoxPlugin {
    fn id(&self) -> ToolIdentifier {
        ToolIdentifier::Sox
    }

    fn supports(&self, context: &PlanContext<'_>, step: &PlanStep) -> ToolSupport {
        match &step.operation {
            PlanOperation::EncodePcm {
                target_format,
                apply_processing,
                ..
            } if target_format.sox_encodable() => {
                if *apply_processing
                    && mapping::requires_sox_dither(context.request.settings.dither_type)
                {
                    ToolSupport::CANONICAL
                } else if *apply_processing {
                    ToolSupport::PREFERRED
                } else if target_format.ffmpeg_encodable() {
                    ToolSupport::FALLBACK
                } else {
                    ToolSupport::SUPPORTED
                }
            }
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Mp3 | AudioFormat::Opus,
                ..
            } => ToolSupport::FALLBACK,
            PlanOperation::ResamplePcm {
                brick_wall: false, ..
            } => ToolSupport::PREFERRED,
            PlanOperation::PcmToDsd { .. }
            | PlanOperation::DsdToPcm { .. }
            | PlanOperation::DsdRateChange { .. } => ToolSupport::CANONICAL,
            _ => ToolSupport::UNSUPPORTED,
        }
    }

    fn build_command(&self, context: &PlanContext<'_>, step: &PlanStep) -> Result<PlannedCommand> {
        match &step.operation {
            PlanOperation::EncodePcm {
                target_format,
                target_rate_hz,
                target_bit_depth,
                apply_processing,
            } => build_sox_encode_pcm(
                context,
                step,
                target_format,
                *target_rate_hz,
                *target_bit_depth,
                *apply_processing,
            ),
            PlanOperation::EncodeLossy {
                target_format,
                target_rate_hz,
                apply_processing,
            } => build_sox_encode_lossy(
                context,
                step,
                target_format,
                *target_rate_hz,
                *apply_processing,
            ),
            PlanOperation::ResamplePcm {
                target_rate_hz,
                target_bit_depth,
                brick_wall: false,
                ..
            } => build_sox_resample(context, step, *target_rate_hz, *target_bit_depth),
            PlanOperation::PcmToDsd {
                target_format,
                target_rate,
                filter,
            } => build_sox_pcm_to_dsd(context, step, target_format, *target_rate, *filter),
            PlanOperation::DsdToPcm {
                target_format,
                target_rate_hz,
                target_bit_depth,
                lowpass,
            } => build_sox_dsd_to_pcm(
                context,
                step,
                target_format,
                *target_rate_hz,
                *target_bit_depth,
                *lowpass,
            ),
            PlanOperation::DsdRateChange {
                target_format,
                target_rate,
                lowpass,
            } => build_sox_dsd_rate_change(context, step, target_format, *target_rate, *lowpass),
            _ => Err(PlanningError::plugin_rejected(
                self.id(),
                format!("unsupported operation {}", step.operation.label()),
            )),
        }
    }
}

impl ToolPlugin for SsrcPlugin {
    fn id(&self) -> ToolIdentifier {
        ToolIdentifier::Ssrc
    }

    fn supports(&self, _context: &PlanContext<'_>, step: &PlanStep) -> ToolSupport {
        match &step.operation {
            PlanOperation::ResamplePcm {
                brick_wall: true, ..
            } => ToolSupport::CANONICAL,
            _ => ToolSupport::UNSUPPORTED,
        }
    }

    fn build_command(&self, context: &PlanContext<'_>, step: &PlanStep) -> Result<PlannedCommand> {
        if let PlanOperation::ResamplePcm {
            target_rate_hz,
            target_bit_depth,
            profile,
            brick_wall: true,
        } = &step.operation
        {
            let input = required_input_path(step)?;
            let output = required_output_path(step)?;
            let profile = (*profile).unwrap_or_else(|| {
                mapping::ssrc_profile(
                    context.request.settings.ssrc,
                    context.request.settings.resample_quality,
                )
            });
            let mut args = vec![
                "--rate".into(),
                target_rate_hz.to_string(),
                "--profile".into(),
                profile.as_arg().into(),
            ];
            args.push("--dither".into());
            args.push(mapping::ssrc_dither_id(context.request.settings.dither_type).to_string());
            if let Some(depth) = *target_bit_depth {
                args.push("--bits".into());
                args.push(depth.bits().to_string());
            }
            args.push(input);
            args.push(output);
            Ok(PlannedCommand::new(
                ToolIdentifier::Ssrc,
                args,
                step.input.clone(),
                step.output.clone(),
                context.request.source.duration,
                step.description.clone(),
            ))
        } else {
            Err(PlanningError::plugin_rejected(
                self.id(),
                format!("unsupported operation {}", step.operation.label()),
            ))
        }
    }
}

impl ToolPlugin for LoudgainPlugin {
    fn id(&self) -> ToolIdentifier {
        ToolIdentifier::Loudgain
    }

    fn supports(&self, _context: &PlanContext<'_>, step: &PlanStep) -> ToolSupport {
        match &step.operation {
            PlanOperation::ReplayGain { target_format, .. }
                if loudgain_supports_format(target_format) =>
            {
                ToolSupport::CANONICAL
            }
            _ => ToolSupport::UNSUPPORTED,
        }
    }

    fn build_command(&self, context: &PlanContext<'_>, step: &PlanStep) -> Result<PlannedCommand> {
        if let PlanOperation::ReplayGain { mode, .. } = &step.operation {
            let path = required_input_path(step)?;
            let mut args = Vec::new();
            match *mode {
                ReplayGainMode::Album => args.push("-a".into()),
                ReplayGainMode::Track => args.push("-t".into()),
                ReplayGainMode::Both => {
                    args.push("-a".into());
                    args.push("-t".into());
                }
            }
            if context.request.settings.replay_gain.prevent_clipping {
                args.push("-k".into());
            }
            args.push("-s".into());
            args.push("e".into());
            args.push(path);
            Ok(PlannedCommand::new(
                ToolIdentifier::Loudgain,
                args,
                step.input.clone(),
                step.output.clone(),
                context.request.source.duration,
                step.description.clone(),
            ))
        } else {
            Err(PlanningError::plugin_rejected(
                self.id(),
                format!("unsupported operation {}", step.operation.label()),
            ))
        }
    }
}

impl ToolPlugin for MetaflacPlugin {
    fn id(&self) -> ToolIdentifier {
        ToolIdentifier::Metaflac
    }

    fn supports(&self, _context: &PlanContext<'_>, step: &PlanStep) -> ToolSupport {
        match &step.operation {
            PlanOperation::StoreSourceAudioMd5 { target_format }
                if target_format == &AudioFormat::Flac =>
            {
                ToolSupport::CANONICAL
            }
            _ => ToolSupport::UNSUPPORTED,
        }
    }

    fn build_command(&self, context: &PlanContext<'_>, step: &PlanStep) -> Result<PlannedCommand> {
        match &step.operation {
            PlanOperation::StoreSourceAudioMd5 { target_format }
                if target_format == &AudioFormat::Flac =>
            {
                let path = required_output_path(step).or_else(|_| required_input_path(step))?;
                let md5 = context.request.source.audio_md5.as_ref().ok_or_else(|| {
                    PlanningError::invalid_source(
                        "audio_md5",
                        "metadata.store_source_audio_md5 requires SourceInfo::audio_md5",
                    )
                })?;
                Ok(PlannedCommand::new(
                    ToolIdentifier::Metaflac,
                    vec![format!("--set-tag=SOURCE_AUDIO_MD5={md5}"), path],
                    step.input.clone(),
                    step.output.clone(),
                    context.request.source.duration,
                    "Store source audio MD5 as a FLAC Vorbis comment",
                ))
            }
            _ => Err(PlanningError::plugin_rejected(
                self.id(),
                format!("unsupported operation {}", step.operation.label()),
            )),
        }
    }
}

impl ToolPlugin for FlacPlugin {
    fn id(&self) -> ToolIdentifier {
        ToolIdentifier::Flac
    }

    fn supports(&self, context: &PlanContext<'_>, step: &PlanStep) -> ToolSupport {
        match &step.operation {
            PlanOperation::Verify { target_format }
                if target_format == &AudioFormat::Flac
                    && context
                        .request
                        .settings
                        .verification
                        .prefer_native_flac_verify =>
            {
                ToolSupport::PREFERRED
            }
            PlanOperation::Verify { target_format } if target_format == &AudioFormat::Flac => {
                ToolSupport::FALLBACK
            }
            _ => ToolSupport::UNSUPPORTED,
        }
    }

    fn build_command(&self, context: &PlanContext<'_>, step: &PlanStep) -> Result<PlannedCommand> {
        if let PlanOperation::Verify { target_format } = &step.operation {
            if target_format != &AudioFormat::Flac {
                return Err(PlanningError::plugin_rejected(
                    self.id(),
                    format!("unsupported operation {}", step.operation.label()),
                ));
            }
            let input = required_input_path(step)?;
            Ok(PlannedCommand::new(
                ToolIdentifier::Flac,
                vec!["-t".into(), "-s".into(), input],
                step.input.clone(),
                step.output.clone(),
                context.request.source.duration,
                step.description.clone(),
            ))
        } else {
            Err(PlanningError::plugin_rejected(
                self.id(),
                format!("unsupported operation {}", step.operation.label()),
            ))
        }
    }
}

fn build_ffmpeg_decode(
    context: &PlanContext<'_>,
    step: &PlanStep,
    bit_depth: PcmBitDepth,
) -> Result<PlannedCommand> {
    let input = required_input_path(step)?;
    let output = required_output_path(step)?;
    let codec = mapping::ffmpeg_pcm_codec(bit_depth, &AudioFormat::Wav)?;
    let args = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-nostdin".into(),
        "-i".into(),
        input,
        "-map".into(),
        "0:a:0".into(),
        "-vn".into(),
        "-c:a".into(),
        codec.into(),
        "-f".into(),
        "wav".into(),
        output,
    ];
    Ok(PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    ))
}

fn build_ffmpeg_resample(
    context: &PlanContext<'_>,
    step: &PlanStep,
    target_rate_hz: u32,
    target_depth: Option<PcmBitDepth>,
) -> Result<PlannedCommand> {
    let input = required_input_path(step)?;
    let output = required_output_path(step)?;
    let depth = target_depth.unwrap_or(PcmBitDepth::Float64);
    let codec = mapping::ffmpeg_pcm_codec(depth, &AudioFormat::Wav)?;
    let filter = ffmpeg_audio_filter(context, Some(target_rate_hz), Some(depth))?;
    let args = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-nostdin".into(),
        "-i".into(),
        input,
        "-map".into(),
        "0:a:0".into(),
        "-vn".into(),
        "-af".into(),
        filter,
        "-c:a".into(),
        codec.into(),
        output,
    ];
    Ok(PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    ))
}

fn build_ffmpeg_encode_pcm(
    context: &PlanContext<'_>,
    step: &PlanStep,
    target_format: &AudioFormat,
    target_rate_hz: Option<u32>,
    target_depth: PcmBitDepth,
    apply_processing: bool,
) -> Result<PlannedCommand> {
    // WavPack hybrid mode requires the native wavpack CLI (ffmpeg can't do hybrid).
    if matches!(target_format, AudioFormat::WavPack)
        && context.request.settings.wavpack.hybrid
    {
        return build_wavpack_hybrid_encode(context, step);
    }
    if !target_format.ffmpeg_encodable() {
        return Err(PlanningError::unsupported_format(
            target_format.clone(),
            "FFmpeg plugin does not encode this target format",
        ));
    }
    let input = required_input_path(step)?;
    let output = required_output_path(step)?;
    let mut args = ffmpeg_base_input_args(&input);
    add_ffmpeg_metadata_args(context, &mut args, target_format);
    if apply_processing {
        add_ffmpeg_audio_filter_args(context, &mut args, target_rate_hz, Some(target_depth))?;
    }
    add_ffmpeg_pcm_encoder_args(context, &mut args, target_format, target_depth)?;
    add_ffmpeg_container_flags(context, &mut args);
    args.push(output);
    Ok(PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    ))
}

fn build_ffmpeg_encode_lossy(
    context: &PlanContext<'_>,
    step: &PlanStep,
    target_format: &AudioFormat,
    target_rate_hz: Option<u32>,
    apply_processing: bool,
) -> Result<PlannedCommand> {
    let input = required_input_path(step)?;
    let output = required_output_path(step)?;
    let mut args = ffmpeg_base_input_args(&input);
    add_ffmpeg_metadata_args(context, &mut args, target_format);
    if apply_processing {
        if let Some(rate) = target_rate_hz {
            args.push("-ar".into());
            args.push(rate.to_string());
        }
    }
    match target_format {
        AudioFormat::Mp3 => {
            args.push("-c:a".into());
            args.push("libmp3lame".into());
            match context.request.settings.mp3.mode {
                Mp3Mode::Cbr => {
                    args.push("-b:a".into());
                    args.push(format!("{}k", context.request.settings.mp3.bitrate_kbps));
                }
                Mp3Mode::Abr => {
                    args.push("-abr".into());
                    args.push("1".into());
                    args.push("-b:a".into());
                    args.push(format!("{}k", context.request.settings.mp3.bitrate_kbps));
                }
                Mp3Mode::Vbr => {
                    args.push("-q:a".into());
                    args.push(context.request.settings.mp3.vbr_quality.to_string());
                }
            }
        }
        AudioFormat::Aac => {
            args.push("-c:a".into());
            args.push("libfdk_aac".into());
            args.push("-profile:a".into());
            args.push(mapping::ffmpeg_aac_profile(context.request.settings.aac.profile).into());
            args.push("-b:a".into());
            args.push(format!("{}k", context.request.settings.aac.bitrate_kbps));
        }
        AudioFormat::Opus => {
            args.push("-c:a".into());
            args.push("libopus".into());
            args.push("-application".into());
            args.push(mapping::opus_application(context.request.settings.opus.content_type).into());
            args.push("-b:a".into());
            args.push(format!("{}k", context.request.settings.opus.bitrate_kbps));
            args.push("-compression_level".into());
            args.push(context.request.settings.opus.complexity.to_string());
        }
        AudioFormat::Dts => {
            args.push("-c:a".into());
            args.push("dca".into());
            args.push("-strict".into());
            args.push("-2".into());
            args.push("-b:a".into());
            args.push("768k".into());
        }
        AudioFormat::Ac3 => {
            args.push("-c:a".into());
            args.push("ac3".into());
            args.push("-b:a".into());
            args.push("448k".into());
        }
        _ => {
            return Err(PlanningError::unsupported_format(
                target_format.clone(),
                "FFmpeg lossy encoder does not support this target format",
            ));
        }
    }
    add_ffmpeg_container_flags(context, &mut args);
    args.push(output);
    Ok(PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    ))
}

fn metadata_rewritable_by_ffmpeg(format: &AudioFormat) -> bool {
    format.ffmpeg_encodable()
}

fn ffmpeg_metadata_transfer_supported(
    format: &AudioFormat,
    transfer_tags: bool,
    preserve_artwork: bool,
) -> bool {
    metadata_rewritable_by_ffmpeg(format)
        && (!transfer_tags || format_supports_tags(format))
        && (!preserve_artwork || format_supports_artwork(format))
}

fn ffmpeg_encoder_writes_requested_metadata_policy(
    context: &PlanContext<'_>,
    format: &AudioFormat,
) -> bool {
    metadata_rewritable_by_ffmpeg(format)
        && (!context.request.settings.metadata.transfer_tags || format_supports_tags(format))
        && (!context.request.settings.metadata.preserve_artwork || format_supports_artwork(format))
}

fn format_supports_tags(format: &AudioFormat) -> bool {
    matches!(
        format,
        AudioFormat::Flac
            | AudioFormat::Wav
            | AudioFormat::Aiff
            | AudioFormat::WavPack
            | AudioFormat::Mp3
            | AudioFormat::Aac
            | AudioFormat::Opus
            | AudioFormat::Alac
    )
}

fn format_supports_artwork(format: &AudioFormat) -> bool {
    matches!(
        format,
        AudioFormat::Flac
            | AudioFormat::Mp3
            | AudioFormat::Aac
            | AudioFormat::Opus
            | AudioFormat::Alac
    )
}

fn loudgain_supports_format(format: &AudioFormat) -> bool {
    matches!(
        format,
        AudioFormat::Flac
            | AudioFormat::Mp3
            | AudioFormat::Aac
            | AudioFormat::Opus
            | AudioFormat::Alac
            | AudioFormat::WavPack
    )
}

fn build_ffmpeg_metadata_transfer(
    context: &PlanContext<'_>,
    step: &PlanStep,
    target_format: &AudioFormat,
    transfer_tags: bool,
    preserve_artwork: bool,
) -> Result<PlannedCommand> {
    if !ffmpeg_metadata_transfer_supported(target_format, transfer_tags, preserve_artwork) {
        return Err(PlanningError::unsupported_format(
            target_format.clone(),
            "FFmpeg metadata rewrite does not support the requested tag/artwork policy for this target format",
        ));
    }
    let encoded_input = required_input_path(step)?;
    let output = required_output_path(step)?;
    let needs_source_metadata_input = transfer_tags || preserve_artwork;
    let mut args = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-nostdin".into(),
        "-i".into(),
        encoded_input,
    ];
    if needs_source_metadata_input {
        args.push("-i".into());
        args.push(context.request.input_path.to_string_lossy().into_owned());
    }
    args.push("-map".into());
    args.push("0:a:0".into());
    args.push("-c:a".into());
    args.push("copy".into());
    if transfer_tags {
        args.push("-map_metadata".into());
        args.push("1".into());
    } else {
        args.push("-map_metadata".into());
        args.push("-1".into());
    }
    if preserve_artwork {
        args.push("-map".into());
        args.push("1:v?".into());
        args.push("-c:v".into());
        args.push("copy".into());
    } else {
        args.push("-vn".into());
    }
    args.push(output);
    Ok(PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    ))
}

fn build_ffmpeg_verify(context: &PlanContext<'_>, step: &PlanStep) -> Result<PlannedCommand> {
    let input = required_input_path(step)?;
    let args = vec![
        "-v".into(),
        "error".into(),
        "-i".into(),
        input,
        "-f".into(),
        "null".into(),
        "-".into(),
    ];
    Ok(PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    ))
}

fn build_sox_encode_pcm(
    context: &PlanContext<'_>,
    step: &PlanStep,
    target_format: &AudioFormat,
    target_rate_hz: Option<u32>,
    target_depth: PcmBitDepth,
    apply_processing: bool,
) -> Result<PlannedCommand> {
    let input = required_input_path(step)?;
    let output = required_output_path(step)?;
    let mut args = vec!["-S".into(), input];
    add_sox_output_format_args(context, &mut args, target_format, target_depth);
    args.push(output);
    if apply_processing {
        add_sox_pcm_effects(context, &mut args, target_rate_hz, Some(target_depth));
    }
    Ok(PlannedCommand::new(
        ToolIdentifier::Sox,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    ))
}

fn build_sox_encode_lossy(
    context: &PlanContext<'_>,
    step: &PlanStep,
    target_format: &AudioFormat,
    target_rate_hz: Option<u32>,
    apply_processing: bool,
) -> Result<PlannedCommand> {
    let input = required_input_path(step)?;
    let output = required_output_path(step)?;
    let mut args = vec!["-S".into(), input];
    match target_format {
        AudioFormat::Mp3 => {
            args.push("-C".into());
            args.push(mapping::sox_mp3_compression(
                context.request.settings.mp3.mode,
                context.request.settings.mp3.bitrate_kbps,
                context.request.settings.mp3.vbr_quality,
            ));
        }
        AudioFormat::Opus => {
            args.push("-C".into());
            args.push(context.request.settings.opus.bitrate_kbps.to_string());
        }
        _ => {
            return Err(PlanningError::unsupported_format(
                target_format.clone(),
                "SoX lossy encoder supports only MP3 and Opus in this planner",
            ));
        }
    }
    args.push(output);
    if apply_processing {
        add_sox_pcm_effects(context, &mut args, target_rate_hz, None);
    }
    Ok(PlannedCommand::new(
        ToolIdentifier::Sox,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    ))
}

fn build_sox_resample(
    context: &PlanContext<'_>,
    step: &PlanStep,
    target_rate_hz: u32,
    target_depth: Option<PcmBitDepth>,
) -> Result<PlannedCommand> {
    let input = required_input_path(step)?;
    let output = required_output_path(step)?;
    let mut args = vec!["-S".into(), input];
    if let Some(depth) = target_depth {
        add_sox_bit_depth_args(&mut args, depth);
    }
    args.push(output);
    add_sox_pcm_effects(context, &mut args, Some(target_rate_hz), target_depth);
    Ok(PlannedCommand::new(
        ToolIdentifier::Sox,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    ))
}

fn build_sox_pcm_to_dsd(
    context: &PlanContext<'_>,
    step: &PlanStep,
    target_format: &AudioFormat,
    target_rate: crate::enums::DsdRate,
    filter: DsdFilterPreset,
) -> Result<PlannedCommand> {
    if !target_format.is_dsd() {
        return Err(PlanningError::unsupported_format(
            target_format.clone(),
            "PCM to DSD requires a DSF or DFF target",
        ));
    }
    let input = required_input_path(step)?;
    let output = required_output_path(step)?;
    let mut args = vec!["-S".into(), input, output];
    add_sox_pcm_to_dsd_effects(context, &mut args, target_rate, filter);
    Ok(PlannedCommand::new(
        ToolIdentifier::Sox,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    ))
}

fn build_sox_dsd_to_pcm(
    context: &PlanContext<'_>,
    step: &PlanStep,
    target_format: &AudioFormat,
    target_rate_hz: u32,
    target_depth: PcmBitDepth,
    lowpass: DsdLowpassMethod,
) -> Result<PlannedCommand> {
    if !target_format.is_pcm_lossless() {
        return Err(PlanningError::unsupported_format(
            target_format.clone(),
            "DSD to PCM SoX path requires a PCM-capable lossless target",
        ));
    }
    let input = required_input_path(step)?;
    let output = required_output_path(step)?;
    let mut args = vec!["-S".into(), input];
    add_sox_output_format_args(context, &mut args, target_format, target_depth);
    args.push(output);
    add_sox_dsd_to_pcm_effects(context, &mut args, target_rate_hz, target_depth, lowpass);
    Ok(PlannedCommand::new(
        ToolIdentifier::Sox,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    ))
}

fn build_sox_dsd_rate_change(
    context: &PlanContext<'_>,
    step: &PlanStep,
    target_format: &AudioFormat,
    target_rate: crate::enums::DsdRate,
    lowpass: DsdLowpassMethod,
) -> Result<PlannedCommand> {
    if !target_format.is_dsd() {
        return Err(PlanningError::unsupported_format(
            target_format.clone(),
            "DSD rate change requires a DSF or DFF target",
        ));
    }
    let input = required_input_path(step)?;
    let output = required_output_path(step)?;
    let mut args = vec!["-S".into(), input, output];
    add_sox_dsd_rate_change_effects(context, &mut args, target_rate, lowpass);
    Ok(PlannedCommand::new(
        ToolIdentifier::Sox,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    ))
}

fn ffmpeg_base_input_args(input: &str) -> Vec<String> {
    vec![
        "-y".into(),
        "-hide_banner".into(),
        "-nostdin".into(),
        "-i".into(),
        input.into(),
        "-map".into(),
        "0:a:0".into(),
    ]
}

/// Insert extra ffmpeg flags for the selected container (e.g., `-rf64 auto`).
/// No-op when no container override is active.
fn add_ffmpeg_container_flags(context: &PlanContext<'_>, args: &mut Vec<String>) {
    for flag in &context.request.container_ffmpeg_flags {
        args.push(flag.clone());
    }
}

fn add_ffmpeg_metadata_args(
    context: &PlanContext<'_>,
    args: &mut Vec<String>,
    target_format: &AudioFormat,
) {
    if context.request.settings.metadata.transfer_tags && format_supports_tags(target_format) {
        args.push("-map_metadata".into());
        args.push("0".into());
    } else {
        args.push("-map_metadata".into());
        args.push("-1".into());
    }
    if context.request.settings.metadata.preserve_artwork && format_supports_artwork(target_format)
    {
        args.push("-map".into());
        args.push("0:v?".into());
        args.push("-c:v".into());
        args.push("copy".into());
    } else {
        args.push("-vn".into());
    }
}

fn add_ffmpeg_audio_filter_args(
    context: &PlanContext<'_>,
    args: &mut Vec<String>,
    target_rate_hz: Option<u32>,
    target_depth: Option<PcmBitDepth>,
) -> Result<()> {
    let filter = ffmpeg_audio_filter(context, target_rate_hz, target_depth)?;
    if !filter.is_empty() {
        args.push("-af".into());
        args.push(filter);
    }
    Ok(())
}

fn ffmpeg_audio_filter(
    context: &PlanContext<'_>,
    target_rate_hz: Option<u32>,
    target_depth: Option<PcmBitDepth>,
) -> Result<String> {
    let settings = &context.request.settings;
    let mut opts = vec!["resampler=soxr".to_string()];
    if let Some(rate) = target_rate_hz {
        opts.push(format!("out_sample_rate={rate}"));
    }
    opts.push(format!(
        "precision={}",
        mapping::soxr_precision(settings.resample_quality)
    ));
    let cutoff = settings.soxr_resampler.cutoff
        .unwrap_or_else(|| mapping::ffmpeg_cutoff(settings.nyquist_transition));
    opts.push(format!("cutoff={:.3}", cutoff));
    if settings.soxr_resampler.chebyshev {
        opts.push("cheby=1".to_string());
    }
    if let Some(phase) = settings.soxr_resampler.phase {
        opts.push(format!("phase_shift={}", phase));
    }
    if settings.dither_type != DitherType::None {
        let method = mapping::soxr_dither_method(settings.dither_type).ok_or_else(|| {
            PlanningError::invalid_settings(
                "dither_type",
                "selected dither is not supported by FFmpeg/SoXR; select SoX or Auto",
            )
        })?;
        opts.push(format!("dither_method={method}"));
    }
    if let Some(depth) = target_depth {
        opts.push(format!(
            "out_sample_fmt={}",
            mapping::ffmpeg_sample_fmt(depth)
        ));
    }
    if target_rate_hz.is_none()
        && settings.dither_type == DitherType::None
        && target_depth.is_none()
    {
        Ok(String::new())
    } else {
        Ok(format!("aresample={}", opts.join(":")))
    }
}

fn add_ffmpeg_pcm_encoder_args(
    context: &PlanContext<'_>,
    args: &mut Vec<String>,
    target_format: &AudioFormat,
    target_depth: PcmBitDepth,
) -> Result<()> {
    match target_format {
        AudioFormat::Flac => {
            args.push("-c:a".into());
            args.push("flac".into());
            args.push("-compression_level".into());
            args.push(context.request.settings.flac.compression_level.to_string());
            if !context.request.settings.flac.write_md5 {
                args.push("-flags".into());
                args.push("-md5".into());
            }
        }
        AudioFormat::Wav | AudioFormat::Aiff => {
            args.push("-c:a".into());
            args.push(mapping::ffmpeg_pcm_codec(target_depth, target_format)?.into());
        }
        AudioFormat::WavPack => {
            args.push("-c:a".into());
            args.push("wavpack".into());
            args.push("-compression_level".into());
            args.push(
                mapping::wavpack_compression_level(context.request.settings.wavpack.mode)
                    .to_string(),
            );
        }
        AudioFormat::Alac => {
            args.push("-c:a".into());
            args.push("alac".into());
        }
        other => {
            return Err(PlanningError::unsupported_format(
                other.clone(),
                "FFmpeg PCM encoder path does not support this format",
            ));
        }
    }
    Ok(())
}

/// Build a PlannedCommand for native `wavpack` CLI in hybrid mode.
/// Produces a lossy .wv file (+ optional .wvc correction sidecar).
fn build_wavpack_hybrid_encode(
    context: &PlanContext<'_>,
    step: &PlanStep,
) -> Result<PlannedCommand> {
    let input = required_input_path(step)?;
    let output = required_output_path(step)?;
    let settings = &context.request.settings.wavpack;

    let mut args: Vec<String> = Vec::new();

    // Compression mode flag
    let mode_flag = mapping::wavpack_mode_flag(settings.mode);
    if !mode_flag.is_empty() {
        args.push(mode_flag.into());
    }

    // Hybrid bitrate
    args.push("-b".into());
    args.push(settings.hybrid_bitrate_kbps.to_string());

    // Correction file
    if settings.correction_file {
        args.push("-c".into());
    }

    // Input and output
    args.push(input);
    args.push("-o".into());
    args.push(output);

    Ok(PlannedCommand::new(
        ToolIdentifier::Custom("wavpack".to_string()),
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    ))
}

fn add_sox_output_format_args(
    context: &PlanContext<'_>,
    args: &mut Vec<String>,
    target_format: &AudioFormat,
    target_depth: PcmBitDepth,
) {
    add_sox_bit_depth_args(args, target_depth);
    match target_format {
        AudioFormat::Flac => {
            args.push("-C".into());
            args.push(context.request.settings.flac.compression_level.to_string());
        }
        AudioFormat::WavPack => {
            args.push("-C".into());
            let level = mapping::wavpack_compression_level(context.request.settings.wavpack.mode);
            args.push(level.to_string());
        }
        _ => {}
    }
}

fn add_sox_bit_depth_args(args: &mut Vec<String>, depth: PcmBitDepth) {
    args.push("-b".into());
    args.push(match depth {
        PcmBitDepth::Float32 => "32".into(),
        PcmBitDepth::Float64 => "64".into(),
        _ => depth.bits().to_string(),
    });
    if depth.is_float() {
        args.push("-e".into());
        args.push("float".into());
    }
}

fn add_sox_pcm_effects(
    context: &PlanContext<'_>,
    args: &mut Vec<String>,
    target_rate_hz: Option<u32>,
    target_depth: Option<PcmBitDepth>,
) {
    if let Some(rate) = target_rate_hz {
        let sox_rs = &context.request.settings.sox_resampler;

        // Sinc FIR pre-filter (before rate effect). Active when any numeric sinc
        // field is set — phase alone is not enough to trigger the effect.
        let sinc_active = sox_rs.sinc_taps.is_some()
            || sox_rs.sinc_attenuation_db.is_some()
            || sox_rs.sinc_passband_hz.is_some()
            || sox_rs.sinc_transition_hz.is_some()
            || sox_rs.sinc_kaiser_beta.is_some();
        if sinc_active {
            args.push("sinc".into());
            if let Some(pb) = sox_rs.sinc_passband_hz {
                args.push(format!("-{:.0}", pb)); // negative = lowpass
            }
            if let Some(taps) = sox_rs.sinc_taps {
                args.push("-n".into());
                args.push(taps.to_string());
            }
            if let Some(att) = sox_rs.sinc_attenuation_db {
                args.push("-a".into());
                args.push(att.to_string());
            }
            if let Some(tr) = sox_rs.sinc_transition_hz {
                args.push("-t".into());
                args.push(format!("{}", tr));
            }
            if let Some(beta) = sox_rs.sinc_kaiser_beta {
                args.push("-b".into());
                args.push(format!("{}", beta));
            }
            match sox_rs.sinc_phase {
                Some(SoxSincPhase::Linear) => args.push("-L".into()),
                Some(SoxSincPhase::Minimum) => args.push("-M".into()),
                Some(SoxSincPhase::Intermediate) => args.push("-I".into()),
                None => {} // sox defaults to linear
            }
        }

        args.push("rate".into());
        args.push(mapping::sox_rate_quality_flag(context.request.settings.resample_quality).into());
        // Chebyshev (-s) and bandwidth (-b) are mutually exclusive.
        if sox_rs.chebyshev {
            args.push("-s".into());
        } else if let Some(bw) = sox_rs.bandwidth_pct {
            args.push("-b".into());
            args.push(format!("{}", bw));
        } else if let Some(bandwidth_pct) = mapping::sox_bandwidth_percent(context.request.settings.nyquist_transition) {
            args.push("-b".into());
            args.push(bandwidth_pct.into());
        }
        if let Some(phase) = sox_rs.phase {
            args.push("-p".into());
            args.push(phase.to_string());
        }
        if sox_rs.allow_aliasing {
            args.push("-a".into());
        }
        args.push(rate.to_string());
    }
    let depth_allows_dither = match target_depth {
        Some(depth) => !depth.is_float(),
        None => true,
    };
    let should_dither =
        context.request.settings.dither_type != DitherType::None && depth_allows_dither;
    if should_dither {
        args.extend(mapping::sox_dither_args(
            context.request.settings.dither_type,
        ));
    }
}

fn add_sox_pcm_to_dsd_effects(
    context: &PlanContext<'_>,
    args: &mut Vec<String>,
    target_rate: crate::enums::DsdRate,
    filter: DsdFilterPreset,
) {
    match filter {
        DsdFilterPreset::Auto => {
            args.push("rate".into());
            args.push(mapping::sox_dsd_auto_rate_flag().into());
            args.push(target_rate.hz().to_string());
        }
        DsdFilterPreset::Sinc => {
            let sinc = context.request.settings.dsd.sinc;
            args.push("upsample".into());
            args.push(sinc.oversample_factor.to_string());
            args.push("sinc".into());
            args.push(format!("-{:.0}", sinc.passband_hz));
            args.push("-n".into());
            args.push(sinc.taps.to_string());
            args.push("-t".into());
            args.push(format_float(sinc.transition_hz));
            if sinc.linear_phase {
                args.push("-L".into());
            } else {
                args.push("-M".into());
            }
            args.push("-b".into());
            args.push(format_float(sinc.kaiser_beta));
            if sinc.allow_aliasing {
                args.push("-a".into());
            }
            match context.request.settings.dsd.gain_compensation {
                GainCompensation::Auto => {
                    args.push("vol".into());
                    args.push(sinc.oversample_factor.to_string());
                }
                GainCompensation::Linear(value) => {
                    args.push("vol".into());
                    args.push(format_float(value));
                }
                GainCompensation::Decibels(value) => {
                    args.push("gain".into());
                    args.push(format!("{value:+.2}"));
                }
                GainCompensation::Disabled => {}
            }
            args.push("rate".into());
            args.push("-I".into());
            args.push(target_rate.hz().to_string());
        }
    }
    add_sox_sdm_args(context, args);
}

fn add_sox_dsd_to_pcm_effects(
    context: &PlanContext<'_>,
    args: &mut Vec<String>,
    target_rate_hz: u32,
    target_depth: PcmBitDepth,
    lowpass: DsdLowpassMethod,
) {
    match lowpass {
        DsdLowpassMethod::Sinc => {
            let sinc = context.request.settings.dsd.sinc;
            args.push("sinc".into());
            args.push(format!("-{:.0}", sinc.passband_hz));
            args.push("-n".into());
            args.push(sinc.taps.to_string());
            args.push("-t".into());
            args.push(format_float(sinc.transition_hz));
            if sinc.linear_phase {
                args.push("-L".into());
            } else {
                args.push("-M".into());
            }
            args.push("-b".into());
            args.push(format_float(sinc.kaiser_beta));
            args.push("rate".into());
            args.push("-I".into());
            args.push(target_rate_hz.to_string());
        }
        DsdLowpassMethod::Auto | DsdLowpassMethod::SoxUltra => {
            args.push("rate".into());
            args.push(
                mapping::sox_dsd_lowpass_rate_flag(
                    lowpass,
                    context.request.settings.resample_quality,
                )
                .into(),
            );
            args.push(target_rate_hz.to_string());
            // Strip residual DSD shaped noise AFTER rate conversion so the
            // sinc filter operates at the output PCM rate (e.g., 88.2 kHz)
            // instead of the DSD input rate (2.8 MHz). Benchmarked at <1%
            // overhead vs. hours when placed before rate.
            if let Some(source_hz) = context.request.source.sample_rate_hz {
                if let Some(dsd_rate) = crate::enums::DsdRate::from_hz(source_hz) {
                    if let Some(lowpass_hz) = dsd_rate.default_pcm_lowpass_hz() {
                        args.push("sinc".into());
                        args.push("-a".into());
                        args.push("180".into());
                        args.push(format!("-{lowpass_hz}"));
                    }
                }
            }
        }
    }
    if let Some(gain_db) = context.request.settings.dsd.dsd_to_pcm_gain_db {
        args.push("gain".into());
        args.push(format!("{gain_db:+.2}"));
    }
    if target_depth == PcmBitDepth::Int16
        && context.request.settings.dither_type != DitherType::None
    {
        args.extend(mapping::sox_dither_args(
            context.request.settings.dither_type,
        ));
    }
}

fn add_sox_dsd_rate_change_effects(
    context: &PlanContext<'_>,
    args: &mut Vec<String>,
    target_rate: crate::enums::DsdRate,
    lowpass: DsdLowpassMethod,
) {
    match lowpass {
        DsdLowpassMethod::Sinc => {
            let sinc = context.request.settings.dsd.sinc;
            args.push("sinc".into());
            args.push(format!("-{:.0}", sinc.passband_hz));
            args.push("-n".into());
            args.push(sinc.taps.to_string());
            args.push("-t".into());
            args.push(format_float(sinc.transition_hz));
            if sinc.linear_phase {
                args.push("-L".into());
            } else {
                args.push("-M".into());
            }
            args.push("-b".into());
            args.push(format_float(sinc.kaiser_beta));
        }
        DsdLowpassMethod::Auto | DsdLowpassMethod::SoxUltra => {}
    }
    args.push("rate".into());
    args.push(
        mapping::sox_dsd_lowpass_rate_flag(lowpass, context.request.settings.resample_quality)
            .into(),
    );
    args.push(target_rate.hz().to_string());
    add_sox_sdm_args(context, args);
}

fn add_sox_sdm_args(context: &PlanContext<'_>, args: &mut Vec<String>) {
    let dsd = context.request.settings.dsd;
    args.push("sdm".into());
    args.push("-f".into());
    args.push(mapping::dsd_shaper_name(
        dsd.noise_shaper,
        dsd.modulator_order,
    ));
    if let Some(trellis) = dsd.trellis {
        args.push("-t".into());
        args.push(trellis.lookahead.to_string());
        args.push("-n".into());
        args.push(trellis.nodes.to_string());
        if let Some(latency) = trellis.latency {
            args.push("-l".into());
            args.push(latency.to_string());
        }
    }
}

fn required_input_path(step: &PlanStep) -> Result<String> {
    step.input.as_path().map(path_to_string).ok_or_else(|| {
        PlanningError::invalid_settings("input", "selected plugin requires a path input")
    })
}

fn required_output_path(step: &PlanStep) -> Result<String> {
    step.output.as_path().map(path_to_string).ok_or_else(|| {
        PlanningError::invalid_settings("output", "selected plugin requires a path output")
    })
}

fn path_to_string(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

fn format_float(value: f32) -> String {
    let mut rendered = format!("{value:.3}");
    while rendered.contains('.') && rendered.ends_with('0') {
        rendered.pop();
    }
    if rendered.ends_with('.') {
        rendered.pop();
    }
    rendered
}
