#![allow(missing_docs, clippy::field_reassign_with_default, clippy::cmp_owned)]

use std::path::PathBuf;
use tonepoet_pipeline::*;

fn flac_source() -> SourceInfo {

    SourceInfo {
        dsd_source_kind: None,

        format: AudioFormat::Flac,
        codec: AudioCodec::Flac,
        sample_rate_hz: Some(96_000),
        bit_depth: Some(PcmBitDepth::Int24),
        true_source_depth: Some(PcmBitDepth::Int24),
        source_representation: Default::default(),
        sample_kind: Some(SampleKind::SignedInteger),
        channels: Some(2),
        duration: None,
        audio_md5: None,
    }
}

fn legacy_dsd_settings(
    lowpass: DsdLowpassMethod,
    gain_mode: DsdToPcmGainMode,
    margin_db: f32,
    gain_db: Option<f32>,
) -> DsdSettings {
    let native = DsdSettings::default();
    serde_json::from_value(serde_json::json!({
        "noise_shaper": native.pcm_to_dsd.noise_shaper,
        "modulator_order": native.pcm_to_dsd.modulator_order,
        "trellis": native.pcm_to_dsd.trellis,
        "pcm_to_dsd_filter": native.pcm_to_dsd.filter,
        "dsd_to_pcm_lowpass": lowpass,
        "dsd_to_pcm_gain_mode": gain_mode,
        "dsd_to_pcm_auto_gain_margin_db": margin_db,
        "dsd_to_pcm_gain_db": gain_db,
        "sinc": native.pcm_to_dsd.sinc,
        "gain_compensation": native.pcm_to_dsd.gain_compensation,
    }))
    .expect("valid frozen legacy DSD wire")
}


fn request(settings: PipelineSettings) -> PlanRequest {

    PlanRequest {
        resolved_output_target: None,
        reference_programme_scope: Default::default(),
        planned_riff_non_audio_upper_bound_bytes: None,

        input_path: PathBuf::from("in.flac"),
        output_path: PathBuf::from("out.flac"),
        source: flac_source(),
        settings,
        intermediate_dir: Some(PathBuf::from("work")),
        container_ffmpeg_flags: Vec::new(),
    }
}

#[test]
fn passthrough_is_explicit_when_metadata_policy_is_copy_safe() {
    let req = request(PipelineSettings::default());
    let plan = plan_conversion(&req).unwrap();
    match plan.action {
        PlanAction::PassthroughCopy { input, output, .. } => {
            assert_eq!(input, PathBuf::from("in.flac"));
            assert_eq!(output, PathBuf::from("out.flac"));
        }
        PlanAction::Execute { .. } => panic!("expected passthrough"),
    }
}

#[test]
fn metadata_strip_blocks_passthrough_and_rewrites_without_reencoding() {
    let mut settings = PipelineSettings::default();
    settings.metadata.transfer_tags = false;
    settings.metadata.preserve_artwork = false;
    let req = request(settings);
    let plan = plan_conversion(&req).unwrap();
    let commands = plan.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].tool, ToolIdentifier::Ffmpeg);
    assert!(commands[0].args.iter().any(|arg| arg == "-map_metadata"));
    assert!(commands[0].args.iter().any(|arg| arg == "-1"));
    assert!(commands[0].args.iter().any(|arg| arg == "-c:a"));
    assert!(commands[0].args.iter().any(|arg| arg == "copy"));
}

#[test]
fn sox_flac_resample_dither_plan_is_deterministic_and_preserves_metadata_by_post_step() {
    let mut settings = PipelineSettings::default();
    settings.target_sample_rate = RateTarget::PcmHz(44_100);
    settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
    settings.dither_type = DitherType::Shibata;

    let req = request(settings);
    let first = plan_conversion(&req).unwrap();
    let second = plan_conversion(&req).unwrap();
    assert_eq!(first, second);

    let commands = first.commands();
    assert_eq!(commands[0].tool, ToolIdentifier::Sox);
    assert_eq!(commands[1].tool, ToolIdentifier::Ffmpeg);
    assert!(commands[0].args.iter().any(|arg| arg == "rate"));
    assert!(commands[0].args.iter().any(|arg| arg == "44100"));
    assert!(commands[0].args.iter().any(|arg| arg == "dither"));
    assert!(commands[0].args.iter().any(|arg| arg == "-s"));
    assert!(commands[1].args.iter().any(|arg| arg == "-map_metadata"));
}

#[test]
fn brickwall_uses_ffmpeg_ssrc_final_encode_plus_original_source_metadata_transfer() {
    let mut settings = PipelineSettings::default();
    settings.target_sample_rate = RateTarget::PcmHz(44_100);
    settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
    settings.nyquist_transition = NyquistTransition::BrickWall;
    settings.dither_type = DitherType::LowShibata;

    let req = request(settings);
    let plan = plan_conversion(&req).unwrap();
    let tools: Vec<_> = plan.commands().iter().map(|cmd| cmd.tool.clone()).collect();
    // The final encode reads a tagless SSRC intermediate, so an explicit
    // original-source MetadataTransfer step is REQUIRED — the old 3-step
    // plan silently lost all tags/artwork through the SSRC path (fixed by
    // the typed metadata-effect pruner).
    assert_eq!(
        tools,
        vec![
            ToolIdentifier::Ffmpeg,
            ToolIdentifier::Ssrc,
            ToolIdentifier::Ffmpeg,
            ToolIdentifier::Ffmpeg
        ]
    );
    let transfer = &plan.commands()[3];
    assert_eq!(
        transfer.args.iter().filter(|arg| *arg == "-i").count(),
        2,
        "metadata transfer must read the encoded file plus the original source"
    );
    assert!(transfer.args.iter().any(|arg| arg == "-map_metadata"));
}

#[test]
fn ssrc_force_routes_rate_change_through_ssrc_without_brickwall_transition() {
    let mut settings = PipelineSettings::default();
    settings.target_sample_rate = RateTarget::PcmHz(44_100);
    settings.ssrc.force = true;
    let req = request(settings);
    let plan = plan_conversion(&req).unwrap();
    assert!(plan
        .commands()
        .iter()
        .any(|command| command.tool == ToolIdentifier::Ssrc));
}

#[test]
fn preferred_ffmpeg_is_honored_when_supported() {
    let mut settings = PipelineSettings::default();
    settings.force_encode = true;
    settings.preferred_tool = PreferredTool::Ffmpeg;
    let req = request(settings);
    let plan = plan_conversion(&req).unwrap();
    assert_eq!(plan.commands()[0].tool, ToolIdentifier::Ffmpeg);
}

#[test]
fn invalid_pcm_target_rejects_dsd_rate() {
    let mut settings = PipelineSettings::default();
    settings.target_sample_rate = RateTarget::Dsd(DsdRate::Dsd64);
    let req = request(settings);
    assert!(matches!(
        plan_conversion(&req),
        Err(PlanningError::InvalidSettings {
            field: "target_sample_rate",
            ..
        })
    ));
}

#[test]
fn lossy_targets_resolve_source_depth_to_the_format_default() {
    // A lossy encode makes no bit-depth promise: unmeasured-PCM and Unknown
    // sources must resolve to the working default, never fail closed (that
    // rule is reserved for PCM-lossless targets).
    for representation in [
        SourceRepresentationKind::Pcm,
        SourceRepresentationKind::Unknown,
    ] {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Mp3;
        settings.target_bit_depth = BitDepthTarget::Source;
        settings.force_encode = true;
        let mut req = request(settings);
        req.output_path = PathBuf::from("out.mp3");
        req.source.bit_depth = None;
        req.source.true_source_depth = None;
        req.source.source_representation = representation;

        let plan = plan_conversion(&req).unwrap_or_else(|err| {
            panic!("lossy target must plan for {representation:?} source: {err}")
        });
        assert!(!plan.commands().is_empty());
    }
}

#[test]
fn pcm_lossless_source_target_requires_authoritative_source_depth() {
    let mut settings = PipelineSettings::default();
    settings.target_bit_depth = BitDepthTarget::Source;
    settings.force_encode = true;
    let mut req = request(settings);
    req.source.bit_depth = None;
    req.source.true_source_depth = None;

    assert!(matches!(
        plan_conversion(&req),
        Err(PlanningError::InvalidSource {
            field: "bit_depth",
            ..
        })
    ));
}

#[test]
fn explicit_pcm_representation_does_not_promote_carrier_depth_to_source_truth() {
    let mut settings = PipelineSettings::default();
    settings.target_bit_depth = BitDepthTarget::Source;
    settings.force_encode = true;
    let mut req = request(settings);
    req.source.bit_depth = Some(PcmBitDepth::Int32);
    req.source.true_source_depth = None;
    req.source.source_representation = SourceRepresentationKind::Pcm;

    assert!(matches!(
        plan_conversion(&req),
        Err(PlanningError::InvalidSource {
            field: "bit_depth",
            ..
        })
    ));
}

#[test]
fn explicit_unknown_representation_ignores_decoded_pcm_carrier() {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Wav;
    settings.target_bit_depth = BitDepthTarget::Source;
    settings.force_encode = true;
    let mut req = request(settings);
    req.input_path = PathBuf::from("decoded-unknown.wav");
    req.output_path = PathBuf::from("out.wav");
    req.source.format = AudioFormat::Wav;
    req.source.codec = AudioCodec::PcmSigned;
    req.source.bit_depth = Some(PcmBitDepth::Int32);
    req.source.true_source_depth = None;
    req.source.source_representation = SourceRepresentationKind::Unknown;

    assert!(matches!(
        plan_conversion(&req),
        Err(PlanningError::InvalidSource {
            field: "bit_depth",
            ..
        })
    ));
}

#[test]
fn legacy_unspecified_representation_keeps_single_depth_fact_authoritative() {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Wav;
    settings.target_bit_depth = BitDepthTarget::Source;
    settings.force_encode = true;
    let mut req = request(settings);
    req.input_path = PathBuf::from("legacy.wav");
    req.output_path = PathBuf::from("out.wav");
    req.source.format = AudioFormat::Wav;
    req.source.codec = AudioCodec::PcmSigned;
    req.source.bit_depth = Some(PcmBitDepth::Int24);
    req.source.true_source_depth = None;
    req.source.source_representation = SourceRepresentationKind::Unspecified;

    let topology = plan_topology(&req).expect("legacy depth remains authoritative");
    let TopologyPlan::Execute { steps, .. } = topology else {
        panic!("forced conversion must execute");
    };
    assert!(steps.iter().any(|step| matches!(
        step.operation,
        PlanOperation::EncodePcm {
            target_bit_depth: PcmBitDepth::Int24,
            ..
        }
    )));
}

#[test]
fn high_rate_pcm_is_not_misclassified_as_dsd() {
    let source = SourceInfo {
        dsd_source_kind: None,

        format: AudioFormat::Wav,
        codec: AudioCodec::PcmSigned,
        sample_rate_hz: Some(DsdRate::Dsd64.hz()),
        bit_depth: Some(PcmBitDepth::Int24),
        true_source_depth: Some(PcmBitDepth::Int24),
        source_representation: Default::default(),
        sample_kind: Some(SampleKind::SignedInteger),
        channels: Some(2),
        duration: None,
        audio_md5: None,
    };
    assert!(!source.is_dsd());
}

#[test]
fn flac_verify_uses_real_decode_test_not_metaflac_streaminfo_listing() {
    let mut settings = PipelineSettings::default();
    settings.force_encode = true;
    settings.flac.verify = true;
    let req = request(settings);
    let plan = plan_conversion(&req).unwrap();
    let verify = plan.commands().last().unwrap();
    assert_eq!(verify.tool, ToolIdentifier::Flac);
    assert!(verify.args.iter().any(|arg| arg == "-t"));
}

#[test]
fn dsd_to_pcm_uses_sox() {
    let mut settings = PipelineSettings::default();
    settings.target_sample_rate = RateTarget::PcmHz(88_200);
    settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
    let req = PlanRequest {
        resolved_output_target: None,
        reference_programme_scope: Default::default(),
        planned_riff_non_audio_upper_bound_bytes: None,

        input_path: PathBuf::from("in.dsf"),
        output_path: PathBuf::from("out.flac"),
        source: SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Dsf,
            codec: AudioCodec::Dsd,
            sample_rate_hz: Some(DsdRate::Dsd64.hz()),
            bit_depth: None,
            true_source_depth: None,
            source_representation: Default::default(),
            sample_kind: Some(SampleKind::Dsd),
            channels: Some(2),
            duration: None,
            audio_md5: None,
        },
        settings,
        intermediate_dir: Some(PathBuf::from("work")),
        container_ffmpeg_flags: Vec::new(),
    };
    let plan = plan_conversion(&req).unwrap();
    assert_eq!(plan.commands()[0].tool, ToolIdentifier::Sox);
    assert!(plan.commands()[0].args.iter().any(|arg| arg == "88200"));
}

#[test]
fn dsd_to_pcm_source_depth_uses_documented_target_default() {
    let mut settings = PipelineSettings::default();
    settings.target_sample_rate = RateTarget::PcmHz(88_200);
    settings.target_bit_depth = BitDepthTarget::Source;
    let req = PlanRequest {
        resolved_output_target: None,
        reference_programme_scope: Default::default(),
        planned_riff_non_audio_upper_bound_bytes: None,

        input_path: PathBuf::from("in.dsf"),
        output_path: PathBuf::from("out.flac"),
        source: SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Dsf,
            codec: AudioCodec::Dsd,
            sample_rate_hz: Some(DsdRate::Dsd64.hz()),
            bit_depth: None,
            true_source_depth: None,
            source_representation: Default::default(),
            sample_kind: Some(SampleKind::Dsd),
            channels: Some(2),
            duration: None,
            audio_md5: None,
        },
        settings,
        intermediate_dir: Some(PathBuf::from("work")),
        container_ffmpeg_flags: Vec::new(),
    };

    let topology = plan_topology(&req).expect("DSD Source depth resolves to target default");
    let TopologyPlan::Execute { steps, .. } = topology else {
        panic!("DSD to PCM must execute");
    };
    assert!(steps.iter().any(|step| matches!(
        step.operation,
        PlanOperation::DsdToPcm {
            target_bit_depth: PcmBitDepth::Int24,
            ..
        }
    )));
}

#[test]
fn lossy_source_depth_uses_documented_target_default() {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Wav;
    settings.target_bit_depth = BitDepthTarget::Source;
    settings.force_encode = true;
    let req = PlanRequest {
        resolved_output_target: None,
        reference_programme_scope: Default::default(),
        planned_riff_non_audio_upper_bound_bytes: None,

        input_path: PathBuf::from("in.mp3"),
        output_path: PathBuf::from("out.wav"),
        source: SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Mp3,
            codec: AudioCodec::Mp3,
            sample_rate_hz: Some(44_100),
            bit_depth: None,
            true_source_depth: None,
            source_representation: Default::default(),
            sample_kind: None,
            channels: Some(2),
            duration: None,
            audio_md5: None,
        },
        settings,
        intermediate_dir: Some(PathBuf::from("work")),
        container_ffmpeg_flags: Vec::new(),
    };

    let topology = plan_topology(&req).expect("lossy Source depth resolves to target default");
    let TopologyPlan::Execute { steps, .. } = topology else {
        panic!("lossy to PCM must execute");
    };
    assert!(steps.iter().any(|step| matches!(
        step.operation,
        PlanOperation::EncodePcm {
            target_bit_depth: PcmBitDepth::Int24,
            ..
        }
    )));
}

#[test]
fn lossy_source_default_ignores_decoded_integer_carrier_width() {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Wav;
    settings.target_bit_depth = BitDepthTarget::Source;
    settings.force_encode = true;
    let req = PlanRequest {
        resolved_output_target: None,
        reference_programme_scope: Default::default(),
        planned_riff_non_audio_upper_bound_bytes: None,

        input_path: PathBuf::from("decoded-lossy.wav"),
        output_path: PathBuf::from("out.wav"),
        source: SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Wav,
            codec: AudioCodec::PcmSigned,
            sample_rate_hz: Some(44_100),
            // This is the realized decoder carrier, not an original-source
            // PCM representation and therefore must not drive Source policy.
            bit_depth: Some(PcmBitDepth::Int32),
            true_source_depth: None,
            source_representation: SourceRepresentationKind::Lossy,
            sample_kind: Some(SampleKind::SignedInteger),
            channels: Some(2),
            duration: None,
            audio_md5: None,
        },
        settings,
        intermediate_dir: Some(PathBuf::from("work")),
        container_ffmpeg_flags: Vec::new(),
    };

    let topology = plan_topology(&req).expect("lossy carrier resolves to target default");
    let TopologyPlan::Execute { steps, .. } = topology else {
        panic!("forced lossy-carrier conversion must execute");
    };
    assert!(steps.iter().any(|step| matches!(
        step.operation,
        PlanOperation::EncodePcm {
            target_bit_depth: PcmBitDepth::Int24,
            ..
        }
    )));
}

#[test]
fn changed_flac_compression_blocks_passthrough() {
    let mut settings = PipelineSettings::default();
    // The default is 8; any NON-default level must force a re-encode.
    settings.flac.compression_level = 5;
    let req = request(settings);
    let plan = plan_conversion(&req).unwrap();
    assert!(matches!(plan.action, PlanAction::Execute { .. }));
}

#[test]
fn lossy_same_format_never_passes_through_without_proven_encoder_settings() {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Mp3;
    settings.mp3.mode = Mp3Mode::Cbr;
    settings.mp3.bitrate_kbps = 192;
    let req = PlanRequest {
        resolved_output_target: None,
        reference_programme_scope: Default::default(),
        planned_riff_non_audio_upper_bound_bytes: None,

        input_path: PathBuf::from("in.mp3"),
        output_path: PathBuf::from("out.mp3"),
        source: SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Mp3,
            codec: AudioCodec::Mp3,
            sample_rate_hz: Some(44_100),
            bit_depth: None,
            true_source_depth: None,
            source_representation: Default::default(),
            sample_kind: None,
            channels: Some(2),
            duration: None,
            audio_md5: None,
        },
        settings,
        intermediate_dir: Some(PathBuf::from("work")),
        container_ffmpeg_flags: Vec::new(),
    };
    let plan = plan_conversion(&req).unwrap();
    assert!(matches!(plan.action, PlanAction::Execute { .. }));
    assert_eq!(plan.commands()[0].tool, ToolIdentifier::Ffmpeg);
}

#[test]
fn replaygain_only_uses_stream_copy_then_post_processing_not_reencode() {
    let mut settings = PipelineSettings::default();
    settings.replay_gain.mode = Some(ReplayGainMode::Album);
    let req = request(settings);
    let plan = plan_conversion(&req).unwrap();
    let commands = plan.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].tool, ToolIdentifier::Ffmpeg);
    assert!(commands[0].args.iter().any(|arg| arg == "copy"));
    assert_eq!(commands[1].tool, ToolIdentifier::Loudgain);
}

#[test]
fn flac_md5_only_uses_stream_copy_then_metaflac_tagging() {
    let mut settings = PipelineSettings::default();
    settings.metadata.store_source_audio_md5 = true;
    let mut req = request(settings);
    req.source.audio_md5 = Some("0123456789abcdef0123456789abcdef".into());
    let plan = plan_conversion(&req).unwrap();
    let commands = plan.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].tool, ToolIdentifier::Ffmpeg);
    assert_eq!(commands[1].tool, ToolIdentifier::Metaflac);
    assert!(commands[1]
        .args
        .iter()
        .any(|arg| arg.starts_with("--set-tag=SOURCE_AUDIO_MD5=")));
}

#[test]
fn flac_verify_is_rejected_for_non_flac_targets() {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Mp3;
    settings.flac.verify = true;
    let req = request(settings);
    assert!(matches!(
        plan_conversion(&req),
        Err(PlanningError::InvalidSettings {
            field: "flac.verify",
            ..
        })
    ));
}

#[derive(Debug)]
struct CustomEncodePlugin;

impl ToolPlugin for CustomEncodePlugin {
    fn id(&self) -> ToolIdentifier {
        ToolIdentifier::Custom("customenc".into())
    }

    fn supports(&self, _context: &PlanContext<'_>, step: &PlanStep) -> ToolSupport {
        match &step.operation {
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Custom { .. },
                ..
            } => ToolSupport::CANONICAL,
            PlanOperation::ReplayGain {
                target_format: AudioFormat::Custom { .. },
                ..
            } => ToolSupport::CANONICAL,
            _ => ToolSupport::UNSUPPORTED,
        }
    }

    fn metadata_disposition(
        &self,
        _context: &PlanContext<'_>,
        step: &PlanStep,
    ) -> MetadataDisposition {
        match &step.operation {
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Custom { .. },
                ..
            } => MetadataDisposition::WritesRequestedPolicy,
            _ => MetadataDisposition::DoesNotWrite,
        }
    }

    // The pruner ignores the legacy disposition; a typed effect is required
    // for the MetadataTransfer step to prune. Truthful here: the custom
    // encode's input IS the original request input.
    fn metadata_effect(
        &self,
        _context: &PlanContext<'_>,
        step: &PlanStep,
    ) -> MetadataPlanEffect {
        match &step.operation {
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Custom { .. },
                ..
            } => MetadataPlanEffect {
                source_tags_transferred_from_original_source: true,
                artwork_transferred_from_original_source: true,
                ..MetadataPlanEffect::none()
            },
            _ => MetadataPlanEffect::none(),
        }
    }

    fn build_command(&self, context: &PlanContext<'_>, step: &PlanStep) -> Result<PlannedCommand> {
        match &step.operation {
            PlanOperation::EncodePcm { .. } => {
                let input = step.input.as_path().unwrap().to_string_lossy().into_owned();
                let output = step
                    .output
                    .as_path()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                Ok(PlannedCommand::new(
                    self.id(),
                    vec!["--input".into(), input, "--output".into(), output],
                    step.input.clone(),
                    step.output.clone(),
                    context.request.source.duration,
                    step.description.clone(),
                ))
            }
            PlanOperation::ReplayGain { .. } => {
                let input = step.input.as_path().unwrap().to_string_lossy().into_owned();
                Ok(PlannedCommand::new(
                    self.id(),
                    vec!["--replaygain".into(), input],
                    step.input.clone(),
                    step.output.clone(),
                    context.request.source.duration,
                    step.description.clone(),
                ))
            }
            _ => panic!("unexpected custom operation"),
        }
    }
}

#[test]
fn custom_target_is_routed_through_registry_not_rejected_by_topology() {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Custom {
        extension: "cust".into(),
        display_name: "Custom".into(),
    };
    let req = request(settings);
    let mut registry = ToolRegistry::empty();
    registry.register(Box::new(CustomEncodePlugin)).unwrap();
    let plan = plan_conversion_with_registry(&req, &registry).unwrap();
    assert_eq!(
        plan.commands()[0].tool,
        ToolIdentifier::Custom("customenc".into())
    );
}

#[test]
fn custom_target_can_supply_custom_replaygain_plugin() {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Custom {
        extension: "cust".into(),
        display_name: "Custom".into(),
    };
    settings.replay_gain.mode = Some(ReplayGainMode::Track);
    let req = request(settings);
    let mut registry = ToolRegistry::empty();
    registry.register(Box::new(CustomEncodePlugin)).unwrap();
    let plan = plan_conversion_with_registry(&req, &registry).unwrap();
    let tools: Vec<_> = plan
        .commands()
        .iter()
        .map(|command| command.tool.clone())
        .collect();
    assert_eq!(
        tools,
        vec![
            ToolIdentifier::Custom("customenc".into()),
            ToolIdentifier::Custom("customenc".into())
        ]
    );
}

#[test]
fn dsf_metadata_requires_a_registered_metadata_plugin() {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Dsf;
    settings.target_sample_rate = RateTarget::Dsd(DsdRate::Dsd64);
    let req = request(settings);
    assert!(matches!(
        plan_conversion(&req),
        Err(PlanningError::NoPluginForOperation { .. })
    ));
}

#[test]
fn flac_verify_only_uses_stream_copy_then_native_verify() {
    let mut settings = PipelineSettings::default();
    settings.flac.verify = true;
    let req = request(settings);
    let plan = plan_conversion(&req).unwrap();
    let commands = plan.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].tool, ToolIdentifier::Ffmpeg);
    assert!(commands[0].args.iter().any(|arg| arg == "copy"));
    assert_eq!(commands[1].tool, ToolIdentifier::Flac);
}

#[test]
fn dsd_sinc_transition_width_shapes_sox_command() {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Dsf;
    settings.target_sample_rate = RateTarget::Dsd(DsdRate::Dsd128);
    settings.metadata.transfer_tags = false;
    settings.metadata.preserve_artwork = false;
    settings.dsd.pcm_to_dsd.filter = DsdFilterPreset::Sinc;
    settings.dsd.pcm_to_dsd.sinc.transition_hz = 750.0;
    let req = request(settings);
    let plan = plan_conversion(&req).unwrap();
    let args = &plan.commands()[0].args;
    let pos = args
        .iter()
        .position(|arg| arg == "-t")
        .expect("sinc transition flag");
    assert_eq!(args[pos + 1], "750");
}

#[test]
fn dsd_lowpass_paths_all_use_sox_ultra_rate_flag() {
    let mut auto = PipelineSettings::default();
    auto.target_sample_rate = RateTarget::PcmHz(88_200);
    auto.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
    auto.resample_quality = ResampleQuality::Low;
    auto.dsd = legacy_dsd_settings(DsdLowpassMethod::Auto, DsdToPcmGainMode::Disabled, 0.15, None);

    let source = SourceInfo {
        dsd_source_kind: None,

        format: AudioFormat::Dsf,
        codec: AudioCodec::Dsd,
        sample_rate_hz: Some(DsdRate::Dsd64.hz()),
        bit_depth: None,
        true_source_depth: None,
        source_representation: Default::default(),
        sample_kind: Some(SampleKind::Dsd),
        channels: Some(2),
        duration: None,
        audio_md5: None,
    };

    let req_auto = PlanRequest {
        resolved_output_target: None,
        reference_programme_scope: Default::default(),
        planned_riff_non_audio_upper_bound_bytes: None,

        input_path: PathBuf::from("in.dsf"),
        output_path: PathBuf::from("out.flac"),
        source: source.clone(),
        settings: auto,
        intermediate_dir: Some(PathBuf::from("work")),
        container_ffmpeg_flags: Vec::new(),
    };

    let mut ultra = req_auto.clone();
    ultra.settings.dsd = legacy_dsd_settings(DsdLowpassMethod::SoxUltra, DsdToPcmGainMode::Disabled, 0.15, None);

    let auto_plan = plan_conversion(&req_auto).unwrap();
    let ultra_plan = plan_conversion(&ultra).unwrap();
    let auto_args = &auto_plan.commands()[0].args;
    let ultra_args = &ultra_plan.commands()[0].args;
    // resample_quality no longer affects DSD rate conversion: every DSD
    // lowpass path deliberately uses sox's -u (701 taps / 210.7 dB).
    assert!(auto_args.iter().any(|arg| arg == "-u"));
    assert!(!auto_args.iter().any(|arg| arg == "-q"));
    assert!(ultra_args.iter().any(|arg| arg == "-u"));
}

#[test]
fn dsd_source_rejects_pcm_bit_depth_fact() {
    let source = SourceInfo {
        dsd_source_kind: None,

        format: AudioFormat::Dsf,
        codec: AudioCodec::Dsd,
        sample_rate_hz: Some(DsdRate::Dsd64.hz()),
        bit_depth: Some(PcmBitDepth::Int24),
        true_source_depth: Some(PcmBitDepth::Int24),
        source_representation: Default::default(),
        sample_kind: Some(SampleKind::Dsd),
        channels: Some(2),
        duration: None,
        audio_md5: None,
    };
    assert!(matches!(
        source.validate(),
        Err(PlanningError::InvalidSource {
            field: "bit_depth",
            ..
        })
    ));
}

#[test]
fn execute_plan_lists_deterministic_cleanup_paths() {
    let mut settings = PipelineSettings::default();
    settings.target_sample_rate = RateTarget::PcmHz(44_100);
    let req = request(settings);
    let plan = plan_conversion(&req).unwrap();
    assert!(!plan.cleanup_paths().is_empty());
    assert!(plan
        .cleanup_paths()
        .iter()
        .all(|path| path != &PathBuf::from("out.flac")));
}

#[test]
fn identical_input_output_paths_are_rejected() {
    let mut req = request(PipelineSettings::default());
    req.output_path = req.input_path.clone();
    assert!(matches!(
        plan_conversion(&req),
        Err(PlanningError::InvalidSettings {
            field: "output_path",
            ..
        })
    ));
}

#[test]
fn invalid_custom_extension_is_rejected() {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Custom {
        extension: ".bad".into(),
        display_name: "Bad".into(),
    };
    let req = request(settings);
    assert!(matches!(
        plan_conversion(&req),
        Err(PlanningError::InvalidSettings {
            field: "target_format.extension",
            ..
        })
    ));
}

#[test]
fn passthrough_plan_includes_atomic_work_path_and_cleanup() {
    let req = request(PipelineSettings::default());
    let plan = plan_conversion(&req).unwrap();
    match plan.action {
        PlanAction::PassthroughCopy {
            work_path,
            cleanup_paths,
            finalization,
            ..
        } => {
            assert_eq!(cleanup_paths, vec![work_path.clone()]);
            assert!(
                matches!(finalization, Finalization::AtomicRename { from, to } if from == work_path && to == PathBuf::from("out.flac"))
            );
        }
        PlanAction::Execute { .. } => panic!("expected passthrough"),
    }
}

#[test]
fn non_finite_legacy_dsd_gain_is_rejected_at_wire_boundary() {
    let encoded = r#"{
        "noise_shaper":"Clans",
        "modulator_order":"Order8",
        "trellis":null,
        "pcm_to_dsd_filter":"Auto",
        "dsd_to_pcm_lowpass":"Auto",
        "dsd_to_pcm_gain_mode":"Manual",
        "dsd_to_pcm_auto_gain_margin_db":0.15,
        "dsd_to_pcm_gain_db":1e400,
        "sinc":{
            "oversample_factor":8,
            "taps":65536,
            "passband_hz":20000.0,
            "transition_hz":1000.0,
            "kaiser_beta":14.0,
            "linear_phase":true,
            "allow_aliasing":false
        },
        "gain_compensation":"Auto"
    }"#;
    assert!(serde_json::from_str::<DsdSettings>(encoded).is_err());
}

#[test]
fn sox_selected_encode_gets_metadata_transfer_step() {
    let mut settings = PipelineSettings::default();
    settings.force_encode = true;
    settings.preferred_tool = PreferredTool::Sox;
    let req = request(settings);
    let plan = plan_conversion(&req).unwrap();
    let tools: Vec<_> = plan.commands().iter().map(|cmd| cmd.tool.clone()).collect();
    assert_eq!(tools, vec![ToolIdentifier::Sox, ToolIdentifier::Ffmpeg]);
    assert!(plan.commands()[1]
        .args
        .iter()
        .any(|arg| arg == "-map_metadata"));
}

#[test]
fn flac_int32_forced_sox_routes_encode_through_ffmpeg_experimental() {
    let mut settings = PipelineSettings::default();
    settings.force_encode = true;
    settings.target_format = AudioFormat::Flac;
    settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
    settings.preferred_tool = PreferredTool::Sox;

    let plan = plan_conversion(&request(settings))
        .expect("true 32-bit FLAC must remain plannable when Sox is preferred");
    let encode = plan
        .commands()
        .iter()
        .find(|command| command.description.contains("32-bit FLAC"))
        .expect("planner must expose the selected true-32-bit FLAC route");

    assert_eq!(encode.tool, ToolIdentifier::Ffmpeg);
    assert!(
        encode
            .args
            .windows(2)
            .any(|args| args[0] == "-strict" && args[1] == "experimental"),
        "FFmpeg true-32-bit FLAC route must opt into the experimental encoder: {:?}",
        encode.args
    );
    assert!(
        plan.commands()
            .iter()
            .all(|command| command.tool != ToolIdentifier::Sox),
        "Sox must never encode a true 32-bit FLAC target"
    );
}

#[test]
fn wav_artwork_preservation_needs_a_metadata_plugin() {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Wav;
    let req = request(settings);
    assert!(matches!(
        plan_conversion(&req),
        Err(PlanningError::NoPluginForOperation { .. })
    ));
}

#[test]
fn wav_replaygain_needs_a_replaygain_plugin() {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Wav;
    settings.metadata.preserve_artwork = false;
    settings.replay_gain.mode = Some(ReplayGainMode::Track);
    let req = request(settings);
    assert!(matches!(
        plan_conversion(&req),
        Err(PlanningError::NoPluginForOperation { .. })
    ));
}

#[test]
fn generated_final_work_path_cannot_equal_input_path() {
    let mut req = request(PipelineSettings::default());
    req.input_path = PathBuf::from("work/.out.tonepoet-final.flac");
    assert!(matches!(
        plan_conversion(&req),
        Err(PlanningError::InvalidSettings {
            field: "intermediate_dir/output_path",
            ..
        })
    ));
}

#[test]
fn metadata_pruning_updates_later_verify_input() {
    let mut settings = PipelineSettings::default();
    settings.force_encode = true;
    settings.flac.verify = true;
    let req = request(settings);
    let plan = plan_conversion(&req).unwrap();
    let commands = plan.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].tool, ToolIdentifier::Ffmpeg);
    assert_eq!(commands[1].tool, ToolIdentifier::Flac);
    assert_eq!(commands[1].input.as_path(), commands[0].output.as_path());
}

#[test]
fn dsd_source_rejects_bit_depth_even_without_sample_kind() {
    let source = SourceInfo {
        dsd_source_kind: None,

        format: AudioFormat::Dsf,
        codec: AudioCodec::Dsd,
        sample_rate_hz: Some(DsdRate::Dsd64.hz()),
        bit_depth: Some(PcmBitDepth::Int24),
        true_source_depth: Some(PcmBitDepth::Int24),
        source_representation: Default::default(),
        sample_kind: None,
        channels: Some(2),
        duration: None,
        audio_md5: None,
    };
    assert!(matches!(
        source.validate(),
        Err(PlanningError::InvalidSource {
            field: "bit_depth",
            ..
        })
    ));
}

#[test]
fn ssrc_force_without_rate_change_is_rejected() {
    let mut settings = PipelineSettings::default();
    settings.ssrc.force = true;
    let req = request(settings);
    assert!(matches!(
        plan_conversion(&req),
        Err(PlanningError::InvalidSettings {
            field: "ssrc.force",
            ..
        })
    ));
}

fn dsd_request_for(format: AudioFormat, depth: PcmBitDepth, extension: &str) -> PlanRequest {

    let mut settings = PipelineSettings::default();
    settings.target_format = format;
    settings.target_sample_rate = RateTarget::PcmHz(88_200);
    settings.target_bit_depth = BitDepthTarget::Pcm(depth);
    settings.force_encode = true;
    PlanRequest {
        resolved_output_target: None,
        reference_programme_scope: Default::default(),
        planned_riff_non_audio_upper_bound_bytes: None,

        input_path: PathBuf::from("in.dsf"),
        output_path: PathBuf::from(format!("out.{extension}")),
        source: SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Dsf,
            codec: AudioCodec::Dsd,
            sample_rate_hz: Some(DsdRate::Dsd64.hz()),
            bit_depth: None,
            true_source_depth: None,
            source_representation: Default::default(),
            sample_kind: Some(SampleKind::Dsd),
            channels: Some(2),
            duration: None,
            audio_md5: None,
        },
        settings,
        intermediate_dir: Some(PathBuf::from("work")),
        container_ffmpeg_flags: Vec::new(),
    }
}

#[test]
fn source_resolved_int32_alac_is_rejected_through_public_planner() {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Alac;
    settings.target_bit_depth = BitDepthTarget::Source;
    settings.force_encode = true;
    let req = PlanRequest {
        resolved_output_target: None,
        reference_programme_scope: Default::default(),
        planned_riff_non_audio_upper_bound_bytes: None,

        input_path: PathBuf::from("in.wav"),
        output_path: PathBuf::from("out.m4a"),
        source: SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Wav,
            codec: AudioCodec::PcmSigned,
            sample_rate_hz: Some(96_000),
            bit_depth: Some(PcmBitDepth::Int32),
            true_source_depth: Some(PcmBitDepth::Int32),
            source_representation: Default::default(),
            sample_kind: Some(SampleKind::SignedInteger),
            channels: Some(2),
            duration: None,
            audio_md5: None,
        },
        settings,
        intermediate_dir: Some(PathBuf::from("work")),
        container_ffmpeg_flags: Vec::new(),
    };
    let err = plan_conversion(&req).expect_err("ALAC Source over Int32 must fail closed");
    match err {
        PlanningError::InvalidSettings { field, reason } => {
            assert_eq!(field, "target_bit_depth");
            assert!(reason.contains("ALAC 32-bit"), "{reason}");
        }
        other => panic!("unexpected planning error: {other}"),
    }
}

#[test]
fn dsd_to_flac_int32_routes_through_wav_then_ffmpeg_experimental() {
    let plan = plan_conversion(&dsd_request_for(
        AudioFormat::Flac,
        PcmBitDepth::Int32,
        "flac",
    ))
    .expect("DSD to true 32-bit FLAC should be plannable");
    let commands = plan.commands();
    assert!(commands.len() >= 2, "expected DSD intermediate plus final encode");
    assert_eq!(commands[0].tool, ToolIdentifier::Sox);
    assert_eq!(commands[1].tool, ToolIdentifier::Ffmpeg);
    assert!(commands[1]
        .args
        .windows(2)
        .any(|args| args[0] == "-strict" && args[1] == "experimental"));
    assert!(commands[1].description.contains("32-bit FLAC"));
    assert!(commands[0]
        .output
        .as_path()
        .and_then(|path| path.extension())
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wav")));
}

#[test]
fn dsd_to_aiff_float32_routes_through_wav_then_ffmpeg() {
    let mut request = dsd_request_for(AudioFormat::Aiff, PcmBitDepth::Float32, "aiff");
    // AIFF has no ffmpeg metadata-transfer support; the production bridge
    // downgrades the metadata policy before planning, so mirror that here.
    request.settings.metadata.transfer_tags = false;
    request.settings.metadata.preserve_artwork = false;
    let plan = plan_conversion(&request).expect("DSD to float AIFF should be plannable");
    let commands = plan.commands();
    assert!(commands.len() >= 2, "expected DSD intermediate plus final encode");
    assert_eq!(commands[0].tool, ToolIdentifier::Sox);
    assert_eq!(commands[1].tool, ToolIdentifier::Ffmpeg);
    assert!(commands[1].args.iter().any(|arg| arg == "pcm_f32be"));
}

#[test]
fn dsd_to_wavpack_float32_is_rejected_through_public_planner() {
    let err = plan_conversion(&dsd_request_for(
        AudioFormat::WavPack,
        PcmBitDepth::Float32,
        "wv",
    ))
    .expect_err("unsupported WavPack float must fail before command construction");
    match err {
        PlanningError::InvalidSettings { field, reason } => {
            assert_eq!(field, "target_bit_depth");
            assert!(reason.contains("floating-point WavPack"), "{reason}");
        }
        other => panic!("unexpected planning error: {other}"),
    }
}
