#![allow(missing_docs, clippy::field_reassign_with_default)]

use std::path::PathBuf;
use tonepoet_pipeline::*;

fn pcm_request() -> PlanRequest {
    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Flac;
    settings.target_sample_rate = RateTarget::PcmHz(44_100);
    settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
    settings.force_encode = true;
    // This suite isolates sample/encoder parameter binding. Metadata-policy
    // capability is covered separately, including the intentional fail-closed
    // behavior for DSF/Opus/WavPack combinations without a registered writer.
    settings.metadata.transfer_tags = false;
    settings.metadata.preserve_artwork = false;
    PlanRequest {
        input_path: PathBuf::from("in.wav"),
        output_path: PathBuf::from("out.flac"),
        source: SourceInfo {
            dsd_source_kind: None,
            format: AudioFormat::Wav,
            codec: AudioCodec::PcmSigned,
            sample_rate_hz: Some(96_000),
            bit_depth: Some(PcmBitDepth::Int32),
            true_source_depth: Some(PcmBitDepth::Int32),
            source_representation: SourceRepresentationKind::Pcm,
            sample_kind: Some(SampleKind::SignedInteger),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            audio_md5: None,
        },
        settings,
        plan_scope: PlanScope::track("parameter-test"),
        intermediate_dir: Some(PathBuf::from("work")),
        container_ffmpeg_flags: Vec::new(),
        resolved_output_target: None,
        reference_programme_scope: Default::default(),
        planned_riff_non_audio_upper_bound_bytes: None,
    }
}

fn dsd_request() -> PlanRequest {
    let mut request = pcm_request();
    request.input_path = PathBuf::from("in.dsf");
    request.source = SourceInfo {
        dsd_source_kind: None,
        format: AudioFormat::Dsf,
        codec: AudioCodec::Dsd,
        sample_rate_hz: Some(DsdRate::Dsd64.hz()),
        bit_depth: None,
        true_source_depth: None,
        source_representation: SourceRepresentationKind::Dsd,
        sample_kind: Some(SampleKind::Dsd),
        channels: Some(2),
        duration: None,
        frame_extent: None,
        audio_md5: None,
    };
    request.settings.target_sample_rate = RateTarget::PcmHz(88_200);
    request
}

fn ready(request: &PlanRequest) -> TypedConversionPlan {
    match plan_typed(request).expect("candidate search must stay within its bound") {
        PlanningOutcome::Ready(plan) => plan,
        PlanningOutcome::NeedFacts(facts) => panic!("unexpected missing facts: {facts:?}"),
        PlanningOutcome::Refused(refusal) => panic!("unexpected refusal: {refusal:?}"),
    }
}

fn resolved_matching(
    plan: &TypedConversionPlan,
    predicate: impl Fn(&ResolvedOperationParameters) -> bool,
) -> ResolvedOperationParameters {
    plan.nodes
        .iter()
        .find_map(|node| match node {
            TypedPlanNode::Operation { resolved_parameters, .. }
                if predicate(resolved_parameters) => Some(resolved_parameters.clone()),
            _ => None,
        })
        .expect("expected resolved operation parameters")
}

fn command_signature(request: &PlanRequest) -> Vec<(ToolIdentifier, Vec<String>)> {
    plan_conversion(request)
        .expect("existing command planner should lower the admitted request")
        .commands()
        .iter()
        .map(|command| (command.tool.clone(), command.args.clone()))
        .collect()
}

fn command_has_sequence(request: &PlanRequest, expected: &[&str]) -> bool {
    command_signature(request).iter().any(|(_, args)| {
        args.windows(expected.len()).any(|window| {
            window
                .iter()
                .zip(expected.iter())
                .all(|(actual, expected)| actual == expected)
        })
    })
}

fn command_has_fragment(request: &PlanRequest, fragment: &str) -> bool {
    command_signature(request)
        .iter()
        .flat_map(|(_, args)| args.iter())
        .any(|arg| arg.contains(fragment))
}

fn assert_active_change(
    before_request: &PlanRequest,
    after_request: &PlanRequest,
    predicate: impl Fn(&ResolvedOperationParameters) -> bool + Copy,
) {
    let before = ready(before_request);
    let after = ready(after_request);
    assert_ne!(
        resolved_matching(&before, predicate),
        resolved_matching(&after, predicate),
        "typed resolved parameters must change when active lowering changes",
    );
    assert_ne!(
        common_semantic_plan_fingerprint_v1(before_request, &before),
        common_semantic_plan_fingerprint_v1(after_request, &after),
        "semantic identity must change when active lowering changes",
    );
    assert_ne!(
        common_execution_plan_fingerprint_v1(before_request, &before),
        common_execution_plan_fingerprint_v1(after_request, &after),
        "execution identity must change when active lowering changes",
    );
    assert_ne!(
        command_signature(before_request),
        command_signature(after_request),
        "existing command lowering must expose the same active parameter change",
    );
}

fn assert_dormant_change(
    before_request: &PlanRequest,
    after_request: &PlanRequest,
    predicate: impl Fn(&ResolvedOperationParameters) -> bool + Copy,
) {
    let before = ready(before_request);
    let after = ready(after_request);
    assert_eq!(
        resolved_matching(&before, predicate),
        resolved_matching(&after, predicate),
        "dormant setting must be normalized out of the typed resolved payload",
    );
    assert_eq!(
        common_semantic_plan_fingerprint_v1(before_request, &before),
        common_semantic_plan_fingerprint_v1(after_request, &after),
        "dormant setting must not perturb semantic identity",
    );
    assert_eq!(
        command_signature(before_request),
        command_signature(after_request),
        "dormant setting must not perturb existing command lowering",
    );
}

#[test]
fn ssrc_active_attenuation_min_phase_and_dither_are_bound_end_to_end() {
    let mut base = pcm_request();
    base.settings.nyquist_transition = NyquistTransition::BrickWall;
    base.settings.ssrc.force = true;

    let mut attenuation = base.clone();
    attenuation.settings.ssrc.attenuation_db = Some(1.5);
    assert_active_change(&base, &attenuation, |parameters| {
        matches!(parameters, ResolvedOperationParameters::ResampleSsrc { .. })
    });
    assert!(command_has_sequence(&attenuation, &["--att", "1.5"]));

    let mut min_phase = base.clone();
    min_phase.settings.ssrc.min_phase = true;
    assert_active_change(&base, &min_phase, |parameters| {
        matches!(parameters, ResolvedOperationParameters::ResampleSsrc { .. })
    });
    assert!(command_has_sequence(&min_phase, &["--minPhase"]));

    let mut dither = base.clone();
    dither.settings.dither_type = DitherType::Tpdf;
    assert_active_change(&base, &dither, |parameters| {
        matches!(parameters, ResolvedOperationParameters::ResampleSsrc { .. })
    });
    assert!(command_has_fragment(&dither, "dither_method=triangular"));
}

#[test]
fn sox_resampler_active_controls_are_bound_end_to_end() {
    let mut base = pcm_request();
    base.settings.preferred_tool = PreferredTool::Sox;

    let mut bandwidth = base.clone();
    bandwidth.settings.sox_resampler.bandwidth_pct = Some(92.5);
    assert_active_change(&base, &bandwidth, |parameters| {
        matches!(parameters, ResolvedOperationParameters::ResampleSox { .. })
    });
    assert!(command_has_sequence(&bandwidth, &["-b", "92.5"]));

    let mut phase = base.clone();
    phase.settings.sox_resampler.phase = Some(25);
    assert_active_change(&base, &phase, |parameters| {
        matches!(parameters, ResolvedOperationParameters::ResampleSox { .. })
    });
    assert!(command_has_sequence(&phase, &["-p", "25"]));
}

#[test]
fn soxr_cutoff_and_phase_are_bound_end_to_end() {
    let mut base = pcm_request();
    base.settings.preferred_tool = PreferredTool::Ffmpeg;

    let mut cutoff = base.clone();
    cutoff.settings.soxr_resampler.cutoff = Some(0.91);
    assert_active_change(&base, &cutoff, |parameters| {
        matches!(parameters, ResolvedOperationParameters::ResampleSoxr { .. })
    });
    assert!(command_has_fragment(&cutoff, "cutoff=0.910"));

    let mut phase = base.clone();
    phase.settings.soxr_resampler.phase = Some(35);
    assert_active_change(&base, &phase, |parameters| {
        matches!(parameters, ResolvedOperationParameters::ResampleSoxr { .. })
    });
    assert!(command_has_fragment(&phase, "phase_shift=35"));
}

#[test]
fn argv_quantization_is_canonicalized_in_common_semantics() {
    let mut ssrc_a = pcm_request();
    ssrc_a.settings.nyquist_transition = NyquistTransition::BrickWall;
    ssrc_a.settings.ssrc.force = true;
    ssrc_a.settings.ssrc.attenuation_db = Some(1.46);
    let mut ssrc_b = ssrc_a.clone();
    ssrc_b.settings.ssrc.attenuation_db = Some(1.49);
    let ssrc_plan_a = ready(&ssrc_a);
    let ssrc_plan_b = ready(&ssrc_b);
    let effective_ssrc_att = |plan: &TypedConversionPlan| match resolved_matching(plan, |parameters| {
        matches!(parameters, ResolvedOperationParameters::ResampleSsrc { .. })
    }) {
        ResolvedOperationParameters::ResampleSsrc {
            effective_attenuation_db,
            ..
        } => effective_attenuation_db,
        other => panic!("unexpected SSRC payload: {other:?}"),
    };
    assert_eq!(effective_ssrc_att(&ssrc_plan_a), Some(1.5));
    assert_eq!(effective_ssrc_att(&ssrc_plan_b), Some(1.5));
    assert_eq!(command_signature(&ssrc_a), command_signature(&ssrc_b));
    assert_eq!(
        common_semantic_plan_fingerprint_v1(&ssrc_a, &ssrc_plan_a),
        common_semantic_plan_fingerprint_v1(&ssrc_b, &ssrc_plan_b),
    );
    assert_eq!(
        common_execution_plan_fingerprint_v1(&ssrc_a, &ssrc_plan_a),
        common_execution_plan_fingerprint_v1(&ssrc_b, &ssrc_plan_b),
    );

    let mut soxr_a = pcm_request();
    soxr_a.settings.preferred_tool = PreferredTool::Ffmpeg;
    soxr_a.settings.soxr_resampler.cutoff = Some(0.9126);
    let mut soxr_b = soxr_a.clone();
    soxr_b.settings.soxr_resampler.cutoff = Some(0.9134);
    let soxr_plan_a = ready(&soxr_a);
    let soxr_plan_b = ready(&soxr_b);
    let effective_soxr_cutoff = |plan: &TypedConversionPlan| match resolved_matching(plan, |parameters| {
        matches!(parameters, ResolvedOperationParameters::ResampleSoxr { .. })
    }) {
        ResolvedOperationParameters::ResampleSoxr { effective_cutoff, .. } => effective_cutoff,
        other => panic!("unexpected SoXR payload: {other:?}"),
    };
    assert_eq!(effective_soxr_cutoff(&soxr_plan_a), 0.913);
    assert_eq!(effective_soxr_cutoff(&soxr_plan_b), 0.913);
    assert_eq!(command_signature(&soxr_a), command_signature(&soxr_b));
    assert_eq!(
        common_semantic_plan_fingerprint_v1(&soxr_a, &soxr_plan_a),
        common_semantic_plan_fingerprint_v1(&soxr_b, &soxr_plan_b),
    );
    assert_eq!(
        common_execution_plan_fingerprint_v1(&soxr_a, &soxr_plan_a),
        common_execution_plan_fingerprint_v1(&soxr_b, &soxr_plan_b),
    );

    let mut sox_a = pcm_request();
    sox_a.settings.preferred_tool = PreferredTool::Sox;
    sox_a.settings.sox_resampler.sinc_passband_hz = Some(20_000.1);
    let mut sox_b = sox_a.clone();
    sox_b.settings.sox_resampler.sinc_passband_hz = Some(20_000.4);
    let sox_plan_a = ready(&sox_a);
    let sox_plan_b = ready(&sox_b);
    let effective_sox_passband = |plan: &TypedConversionPlan| match resolved_matching(plan, |parameters| {
        matches!(parameters, ResolvedOperationParameters::ResampleSox { .. })
    }) {
        ResolvedOperationParameters::ResampleSox {
            effective_sinc_passband_hz,
            ..
        } => effective_sinc_passband_hz,
        other => panic!("unexpected SoX payload: {other:?}"),
    };
    assert_eq!(effective_sox_passband(&sox_plan_a), Some(20_000.0));
    assert_eq!(effective_sox_passband(&sox_plan_b), Some(20_000.0));
    assert_eq!(command_signature(&sox_a), command_signature(&sox_b));
    assert_eq!(
        common_semantic_plan_fingerprint_v1(&sox_a, &sox_plan_a),
        common_semantic_plan_fingerprint_v1(&sox_b, &sox_plan_b),
    );
}

#[test]
fn resampler_payload_retains_requested_and_effective_values() {
    let mut ssrc = pcm_request();
    ssrc.settings.nyquist_transition = NyquistTransition::BrickWall;
    ssrc.settings.ssrc.force = true;
    ssrc.settings.ssrc.insane_mode = true;
    ssrc.settings.ssrc.profile = Some(SsrcProfile::Fast);
    ssrc.settings.ssrc.attenuation_db = Some(2.0);
    let ssrc_plan = ready(&ssrc);
    match resolved_matching(&ssrc_plan, |parameters| {
        matches!(parameters, ResolvedOperationParameters::ResampleSsrc { .. })
    }) {
        ResolvedOperationParameters::ResampleSsrc { requested, effective_profile, .. } => {
            assert!(requested.force);
            assert!(requested.insane_mode);
            assert_eq!(requested.profile, Some(SsrcProfile::Fast));
            assert_eq!(requested.attenuation_db, Some(2.0));
            assert_eq!(effective_profile, SsrcProfile::Insane);
        }
        other => panic!("unexpected SSRC payload: {other:?}"),
    }

    let mut soxr = pcm_request();
    soxr.settings.preferred_tool = PreferredTool::Ffmpeg;
    soxr.settings.soxr_resampler.cutoff = Some(0.91);
    soxr.settings.soxr_resampler.phase = Some(35);
    let soxr_plan = ready(&soxr);
    match resolved_matching(&soxr_plan, |parameters| {
        matches!(parameters, ResolvedOperationParameters::ResampleSoxr { .. })
    }) {
        ResolvedOperationParameters::ResampleSoxr {
            requested,
            effective_cutoff,
            effective_phase,
            ..
        } => {
            assert_eq!(requested.cutoff, Some(0.91));
            assert_eq!(requested.phase, Some(35));
            assert_eq!(effective_cutoff, 0.91);
            assert_eq!(effective_phase, Some(35));
        }
        other => panic!("unexpected SoXR payload: {other:?}"),
    }
}

#[test]
fn dsd_fixed_ultra_rate_mapping_normalizes_dormant_resample_quality() {
    let base = dsd_request();
    let mut changed = base.clone();
    changed.settings.resample_quality = ResampleQuality::VeryHigh;
    assert_dormant_change(&base, &changed, |parameters| {
        matches!(parameters, ResolvedOperationParameters::DsdToPcm { .. })
    });

    let mut rate_change = dsd_request();
    rate_change.output_path = PathBuf::from("out.dsf");
    rate_change.settings.target_format = AudioFormat::Dsf;
    rate_change.settings.target_sample_rate = RateTarget::Dsd(DsdRate::Dsd128);
    rate_change.settings.target_bit_depth = BitDepthTarget::Source;
    let mut changed_rate = rate_change.clone();
    changed_rate.settings.resample_quality = ResampleQuality::VeryHigh;
    assert_dormant_change(&rate_change, &changed_rate, |parameters| {
        matches!(parameters, ResolvedOperationParameters::DsdRateChange { .. })
    });
}

#[test]
fn directional_dsd_sinc_parameters_are_bound_end_to_end() {
    let mut base = dsd_request();
    base.settings.dsd.general_from_dsd.lowpass = DsdLowpassMethod::Sinc;

    let mut taps = base.clone();
    taps.settings.dsd.general_from_dsd.sinc.taps *= 2;
    assert_active_change(&base, &taps, |parameters| {
        matches!(parameters, ResolvedOperationParameters::DsdToPcm { .. })
    });

    let mut passband = base.clone();
    passband.settings.dsd.general_from_dsd.sinc.passband_hz = 24_000.0;
    assert_active_change(&base, &passband, |parameters| {
        matches!(parameters, ResolvedOperationParameters::DsdToPcm { .. })
    });
}

#[test]
fn dsd_sinc_quantization_matches_retained_argv_and_aliasing_refuses() {
    let mut first = dsd_request();
    first.settings.dsd.general_from_dsd.lowpass = DsdLowpassMethod::Sinc;
    first.settings.dsd.general_from_dsd.sinc.passband_hz = 24_000.1;
    first.settings.dsd.general_from_dsd.sinc.transition_hz = 500.0001;
    first.settings.dsd.general_from_dsd.sinc.kaiser_beta = 16.0001;
    let mut second = first.clone();
    second.settings.dsd.general_from_dsd.sinc.passband_hz = 24_000.4;
    second.settings.dsd.general_from_dsd.sinc.transition_hz = 500.0004;
    second.settings.dsd.general_from_dsd.sinc.kaiser_beta = 16.0004;

    let first_plan = ready(&first);
    let second_plan = ready(&second);
    let effective_sinc = |plan: &TypedConversionPlan| match resolved_matching(plan, |parameters| {
        matches!(parameters, ResolvedOperationParameters::DsdToPcm { .. })
    }) {
        ResolvedOperationParameters::DsdToPcm { effective_sinc, .. } => effective_sinc,
        other => panic!("unexpected DSD-to-PCM payload: {other:?}"),
    };
    let effective_first = effective_sinc(&first_plan).expect("sinc must be active");
    let effective_second = effective_sinc(&second_plan).expect("sinc must be active");
    assert_eq!(effective_first, effective_second);
    assert_eq!(effective_first.passband_hz, 24_000.0);
    assert_eq!(effective_first.transition_hz, 500.0);
    assert_eq!(effective_first.kaiser_beta, 16.0);
    assert_eq!(command_signature(&first), command_signature(&second));
    assert_eq!(
        common_semantic_plan_fingerprint_v1(&first, &first_plan),
        common_semantic_plan_fingerprint_v1(&second, &second_plan),
    );

    let mut aliasing = first;
    aliasing.settings.dsd.general_from_dsd.sinc.allow_aliasing = true;
    match plan_typed(&aliasing).expect("alias refusal is a semantic result") {
        PlanningOutcome::Refused(refusal) => {
            assert_eq!(refusal.code, "dsd_to_pcm_sinc_aliasing_unsupported");
        }
        other => panic!("unsupported DSD-to-PCM aliasing must refuse, got {other:?}"),
    }
    match plan_conversion(&aliasing) {
        Err(PlanningError::InvalidSettings { field, .. }) => {
            assert_eq!(field, "dsd.general_from_dsd.sinc.allow_aliasing");
        }
        other => panic!("retained lowerer must explicitly refuse DSD-to-PCM aliasing, got {other:?}"),
    }
}

#[test]
fn pcm_to_dsd_modulator_and_sinc_parameters_are_bound_end_to_end() {
    let mut base = pcm_request();
    base.output_path = PathBuf::from("out.dsf");
    base.settings.target_format = AudioFormat::Dsf;
    base.settings.target_sample_rate = RateTarget::Dsd(DsdRate::Dsd64);
    base.settings.target_bit_depth = BitDepthTarget::Source;
    base.settings.dsd.pcm_to_dsd.filter = DsdFilterPreset::Sinc;

    let mut taps = base.clone();
    taps.settings.dsd.pcm_to_dsd.sinc.taps *= 2;
    assert_active_change(&base, &taps, |parameters| {
        matches!(parameters, ResolvedOperationParameters::PcmToDsd { .. })
    });

    let mut gain = base.clone();
    gain.settings.dsd.pcm_to_dsd.gain_compensation = GainCompensation::Linear(1.5);
    assert_active_change(&base, &gain, |parameters| {
        matches!(parameters, ResolvedOperationParameters::PcmToDsd { .. })
    });
    assert!(command_has_sequence(&gain, &["vol", "1.5"]));

    let mut modulator = base.clone();
    modulator.settings.dsd.pcm_to_dsd.noise_shaper = DsdNoiseShaper::Sdm;
    modulator.settings.dsd.pcm_to_dsd.modulator_order = ModulatorOrder::Order6;
    assert_active_change(&base, &modulator, |parameters| {
        matches!(parameters, ResolvedOperationParameters::PcmToDsd { .. })
    });

    let mut trellis = base.clone();
    trellis.settings.dsd.pcm_to_dsd.trellis = Some(TrellisSettings {
        lookahead: 4,
        nodes: 8,
        latency: Some(16),
    });
    assert_active_change(&base, &trellis, |parameters| {
        matches!(parameters, ResolvedOperationParameters::PcmToDsd { .. })
    });
    assert!(command_has_sequence(
        &trellis,
        &["-t", "4", "-n", "8", "-l", "16"],
    ));
}

#[test]
fn pcm_to_dsd_gain_compensation_is_canonicalized_to_argv_precision() {
    let mut linear_a = pcm_request();
    linear_a.output_path = PathBuf::from("out.dsf");
    linear_a.settings.target_format = AudioFormat::Dsf;
    linear_a.settings.target_sample_rate = RateTarget::Dsd(DsdRate::Dsd64);
    linear_a.settings.target_bit_depth = BitDepthTarget::Source;
    linear_a.settings.dsd.pcm_to_dsd.filter = DsdFilterPreset::Sinc;
    linear_a.settings.dsd.pcm_to_dsd.gain_compensation = GainCompensation::Linear(1.5001);
    let mut linear_b = linear_a.clone();
    linear_b.settings.dsd.pcm_to_dsd.gain_compensation = GainCompensation::Linear(1.5004);

    let linear_plan_a = ready(&linear_a);
    let linear_plan_b = ready(&linear_b);
    let effective_gain = |plan: &TypedConversionPlan| match resolved_matching(plan, |parameters| {
        matches!(parameters, ResolvedOperationParameters::PcmToDsd { .. })
    }) {
        ResolvedOperationParameters::PcmToDsd {
            effective_gain_compensation,
            ..
        } => effective_gain_compensation,
        other => panic!("unexpected PCM-to-DSD payload: {other:?}"),
    };
    assert_eq!(
        effective_gain(&linear_plan_a),
        GainCompensation::Linear(1.5)
    );
    assert_eq!(
        effective_gain(&linear_plan_b),
        GainCompensation::Linear(1.5)
    );
    assert_eq!(command_signature(&linear_a), command_signature(&linear_b));
    assert_eq!(
        common_semantic_plan_fingerprint_v1(&linear_a, &linear_plan_a),
        common_semantic_plan_fingerprint_v1(&linear_b, &linear_plan_b),
    );

    let mut db_a = linear_a.clone();
    db_a.settings.dsd.pcm_to_dsd.gain_compensation = GainCompensation::Decibels(1.231);
    let mut db_b = db_a.clone();
    db_b.settings.dsd.pcm_to_dsd.gain_compensation = GainCompensation::Decibels(1.234);
    let db_plan_a = ready(&db_a);
    let db_plan_b = ready(&db_b);
    assert_eq!(
        effective_gain(&db_plan_a),
        GainCompensation::Decibels(1.23)
    );
    assert_eq!(
        effective_gain(&db_plan_b),
        GainCompensation::Decibels(1.23)
    );
    assert_eq!(command_signature(&db_a), command_signature(&db_b));
    assert_eq!(
        common_semantic_plan_fingerprint_v1(&db_a, &db_plan_a),
        common_semantic_plan_fingerprint_v1(&db_b, &db_plan_b),
    );
}

fn encoder_request(format: AudioFormat, output: &str) -> PlanRequest {
    let mut request = pcm_request();
    request.output_path = PathBuf::from(output);
    request.settings.target_format = format;
    request.settings.target_sample_rate = RateTarget::Source;
    request.settings.target_bit_depth = BitDepthTarget::Source;
    request
}

#[test]
fn mp3_aac_and_opus_encoder_parameters_are_bound_end_to_end() {
    let mut mp3 = encoder_request(AudioFormat::Mp3, "out.mp3");
    mp3.settings.mp3.mode = Mp3Mode::Cbr;
    let mut mp3_changed = mp3.clone();
    mp3_changed.settings.mp3.bitrate_kbps = 192;
    assert_active_change(&mp3, &mp3_changed, |parameters| {
        matches!(parameters, ResolvedOperationParameters::EncodeMp3 { .. })
    });
    assert!(command_has_sequence(&mp3_changed, &["-b:a", "192k"]));

    let aac = encoder_request(AudioFormat::Aac, "out.m4a");
    let mut aac_changed = aac.clone();
    aac_changed.settings.aac.profile = AacProfile::HeAac;
    aac_changed.settings.aac.bitrate_kbps = 192;
    assert_active_change(&aac, &aac_changed, |parameters| {
        matches!(parameters, ResolvedOperationParameters::EncodeAac { .. })
    });
    assert!(command_has_sequence(&aac_changed, &["-b:a", "192k"]));

    let opus = encoder_request(AudioFormat::Opus, "out.opus");
    let mut opus_changed = opus.clone();
    opus_changed.settings.opus.content_type = OpusContentType::Music;
    opus_changed.settings.opus.bitrate_kbps = 160;
    opus_changed.settings.opus.complexity = 8;
    assert_active_change(&opus, &opus_changed, |parameters| {
        matches!(parameters, ResolvedOperationParameters::EncodeOpus { .. })
    });
    assert!(command_has_sequence(&opus_changed, &["-compression_level", "8"]));
}

#[test]
fn flac_and_wavpack_output_affecting_parameters_are_bound_end_to_end() {
    let flac = encoder_request(AudioFormat::Flac, "out.flac");
    let mut flac_changed = flac.clone();
    flac_changed.settings.flac.compression_level = 3;
    assert_active_change(&flac, &flac_changed, |parameters| {
        matches!(parameters, ResolvedOperationParameters::EncodeFlac { .. })
    });

    let wavpack = encoder_request(AudioFormat::WavPack, "out.wv");
    let mut wavpack_changed = wavpack.clone();
    wavpack_changed.settings.wavpack.mode = WavPackMode::Fast;
    assert_active_change(&wavpack, &wavpack_changed, |parameters| {
        matches!(parameters, ResolvedOperationParameters::EncodeWavPack { .. })
    });
}

#[test]
fn dormant_mp3_bitrate_does_not_change_vbr_semantic_identity_or_lowering() {
    let mut base = encoder_request(AudioFormat::Mp3, "out.mp3");
    base.settings.mp3.mode = Mp3Mode::Vbr;
    let mut changed = base.clone();
    changed.settings.mp3.bitrate_kbps = 128;
    let base_plan = ready(&base);
    let changed_plan = ready(&changed);
    assert_eq!(
        resolved_matching(&base_plan, |parameters| {
            matches!(parameters, ResolvedOperationParameters::EncodeMp3 { .. })
        }),
        resolved_matching(&changed_plan, |parameters| {
            matches!(parameters, ResolvedOperationParameters::EncodeMp3 { .. })
        }),
        "dormant MP3 bitrate must be normalized out of the typed payload in VBR mode",
    );
    assert_eq!(
        common_semantic_plan_fingerprint_v1(&base, &base_plan),
        common_semantic_plan_fingerprint_v1(&changed, &changed_plan),
    );
    assert_eq!(command_signature(&base), command_signature(&changed));
}
