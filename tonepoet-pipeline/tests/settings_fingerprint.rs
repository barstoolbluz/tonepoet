use tonepoet_pipeline::{
    settings_fingerprint, AacProfile, AacSettings, AudioFormat, BitDepthTarget, DitherType,
    DsdFilterPreset, DsdLowpassMethod, DsdNoiseShaper, DsdSettings, FlacSettings,
    GainCompensation, MetadataSettings, ModulatorOrder, Mp3Mode, Mp3Settings,
    NyquistTransition, OpusContentType, OpusSettings, PcmBitDepth, PipelineSettings,
    PreferredTool, RateTarget, ReplayGainMode, ReplayGainSettings, ResampleQuality,
    SETTINGS_FINGERPRINT_FIELD_COUNT, SETTINGS_FINGERPRINT_FIELD_PATHS, SincFilterSettings,
    SsrcProfile, SsrcSettings, TrellisSettings, VerificationSettings, WavPackMode,
    WavPackSettings,
};

/// Valid fingerprint base with named fields for compile-time drift detection.
///
/// Sentinel values:
/// - target_format: Flac
/// - target_sample_rate: PcmHz(96_000)
/// - target_bit_depth: Pcm(Float32)
/// - resample_quality: Ultra
/// - nyquist_transition: BrickWall
/// - dither_type: Gesemann
/// - preferred_tool: Custom("sentinel-tool")
/// - force_encode: true
/// - flac.compression_level: 8
/// - flac.verify: true
/// - mp3.mode: Abr
/// - mp3.bitrate_kbps: 257
/// - mp3.vbr_quality: 7
/// - aac.profile: HeAacV2
/// - aac.bitrate_kbps: 384
/// - opus.content_type: Speech
/// - opus.bitrate_kbps: 111
/// - opus.complexity: 7
/// - wavpack.mode: VeryHigh
/// - wavpack.hybrid: true
/// - wavpack.hybrid_bitrate_kbps: 256
/// - wavpack.correction_file: false
/// - ssrc.force: true
/// - ssrc.two_pass: false
/// - ssrc.insane_mode: true
/// - ssrc.profile: Some(Long)
/// - dsd.noise_shaper: Crfb
/// - dsd.modulator_order: Order7
/// - dsd.trellis.lookahead: 17
/// - dsd.trellis.nodes: 9
/// - dsd.trellis.latency: Some(321)
/// - dsd.pcm_to_dsd_filter: Sinc
/// - dsd.dsd_to_pcm_lowpass: Sinc
/// - dsd.dsd_to_pcm_gain_db: Some(-3.25)
/// - dsd.sinc.oversample_factor: 16
/// - dsd.sinc.taps: 131_072
/// - dsd.sinc.passband_hz: 30_000.0
/// - dsd.sinc.transition_hz: 750.0
/// - dsd.sinc.kaiser_beta: 12.5
/// - dsd.sinc.linear_phase: false
/// - dsd.sinc.allow_aliasing: true
/// - dsd.gain_compensation: Decibels(1.5)
/// - metadata.transfer_tags: true
/// - metadata.preserve_artwork: false
/// - metadata.store_source_audio_md5: true
/// - verification.verify_after_encode: true
/// - verification.prefer_native_flac_verify: false
/// - replay_gain.mode: Some(Both)
/// - replay_gain.prevent_clipping: false
fn flac_md5_sentinel() -> PipelineSettings {
    PipelineSettings {
        target_format: AudioFormat::Flac,
        target_sample_rate: RateTarget::PcmHz(96_000),
        target_bit_depth: BitDepthTarget::Pcm(PcmBitDepth::Float32),
        resample_quality: ResampleQuality::Ultra,
        nyquist_transition: NyquistTransition::BrickWall,
        dither_type: DitherType::Gesemann,
        preferred_tool: PreferredTool::Custom("sentinel-tool".to_string()),
        force_encode: true,
        flac: FlacSettings {
            compression_level: 8,
            verify: true,
            write_md5: false,
        },
        mp3: Mp3Settings {
            mode: Mp3Mode::Abr,
            bitrate_kbps: 257,
            vbr_quality: 7,
        },
        aac: AacSettings {
            profile: AacProfile::HeAacV2,
            bitrate_kbps: 384,
        },
        opus: OpusSettings {
            content_type: OpusContentType::Speech,
            bitrate_kbps: 111,
            complexity: 7,
        },
        wavpack: WavPackSettings {
            mode: WavPackMode::VeryHigh,
            hybrid: true,
            hybrid_bitrate_kbps: 256,
            correction_file: false,
        },
        ssrc: SsrcSettings {
            force: true,
            two_pass: false,
            insane_mode: true,
            profile: Some(SsrcProfile::Long),
        },
        dsd: DsdSettings {
            noise_shaper: DsdNoiseShaper::Crfb,
            modulator_order: ModulatorOrder::Order7,
            trellis: Some(TrellisSettings {
                lookahead: 17,
                nodes: 9,
                latency: Some(321),
            }),
            pcm_to_dsd_filter: DsdFilterPreset::Sinc,
            dsd_to_pcm_lowpass: DsdLowpassMethod::Sinc,
            dsd_to_pcm_gain_db: Some(-3.25),
            sinc: SincFilterSettings {
                oversample_factor: 16,
                taps: 131_072,
                passband_hz: 30_000.0,
                transition_hz: 750.0,
                kaiser_beta: 12.5,
                linear_phase: false,
                allow_aliasing: true,
            },
            gain_compensation: GainCompensation::Decibels(1.5),
        },
        metadata: MetadataSettings {
            transfer_tags: true,
            preserve_artwork: false,
            store_source_audio_md5: true,
        },
        verification: VerificationSettings {
            verify_after_encode: true,
            prefer_native_flac_verify: false,
        },
        replay_gain: ReplayGainSettings {
            mode: Some(ReplayGainMode::Both),
            prevent_clipping: false,
        },
    }
}

#[test]
fn fingerprint_field_inventory_has_expected_size_and_no_duplicates() {
    assert_eq!(SETTINGS_FINGERPRINT_FIELD_COUNT, 51);

    let mut sorted = SETTINGS_FINGERPRINT_FIELD_PATHS.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        SETTINGS_FINGERPRINT_FIELD_PATHS.len(),
        "duplicate settings fingerprint field path"
    );
}

#[test]
fn fingerprint_is_stable_for_identical_settings() {
    let settings = flac_md5_sentinel();
    assert_eq!(settings_fingerprint(&settings), settings_fingerprint(&settings));
    assert_eq!(settings_fingerprint(&settings).to_hex().len(), 64);
}

#[test]
fn default_and_sentinel_have_different_fingerprints() {
    assert_ne!(
        settings_fingerprint(&PipelineSettings::default()),
        settings_fingerprint(&flac_md5_sentinel())
    );
}

macro_rules! assert_mutation_changes_fingerprint {
    ($covered:expr, $base:expr, $label:literal, |$settings:ident| $body:block) => {{
        $covered.push($label);
        let base = $base.clone();
        let base_fingerprint = settings_fingerprint(&base);
        let mut mutated = base.clone();
        let $settings = &mut mutated;
        $body
        assert_ne!(
            base_fingerprint,
            settings_fingerprint(&mutated),
            "field mutation did not alter settings fingerprint: {}",
            $label
        );
    }};
}

#[test]
fn every_conversion_affecting_field_changes_the_fingerprint() {
    let base = flac_md5_sentinel();
    let mut covered = Vec::new();

    assert_mutation_changes_fingerprint!(covered, base, "target_format", |settings| {
        settings.target_format = AudioFormat::Custom {
            extension: "fp".to_string(),
            display_name: "Fingerprint Target".to_string(),
        };
    });
    assert_mutation_changes_fingerprint!(covered, base, "target_sample_rate", |settings| {
        settings.target_sample_rate = RateTarget::PcmHz(88_200);
    });
    assert_mutation_changes_fingerprint!(covered, base, "target_bit_depth", |settings| {
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
    });
    assert_mutation_changes_fingerprint!(covered, base, "resample_quality", |settings| {
        settings.resample_quality = ResampleQuality::VeryHigh;
    });
    assert_mutation_changes_fingerprint!(covered, base, "nyquist_transition", |settings| {
        settings.nyquist_transition = NyquistTransition::Steep;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dither_type", |settings| {
        settings.dither_type = DitherType::HighShibata;
    });
    assert_mutation_changes_fingerprint!(covered, base, "preferred_tool", |settings| {
        settings.preferred_tool = PreferredTool::Ssrc;
    });
    assert_mutation_changes_fingerprint!(covered, base, "force_encode", |settings| {
        settings.force_encode = false;
    });
    assert_mutation_changes_fingerprint!(covered, base, "flac.compression_level", |settings| {
        settings.flac.compression_level = 7;
    });
    assert_mutation_changes_fingerprint!(covered, base, "flac.verify", |settings| {
        settings.flac.verify = false;
    });
    assert_mutation_changes_fingerprint!(covered, base, "flac.write_md5", |settings| {
        settings.flac.write_md5 = true;
    });
    assert_mutation_changes_fingerprint!(covered, base, "mp3.mode", |settings| {
        settings.mp3.mode = Mp3Mode::Cbr;
    });
    assert_mutation_changes_fingerprint!(covered, base, "mp3.bitrate_kbps", |settings| {
        settings.mp3.bitrate_kbps = 258;
    });
    assert_mutation_changes_fingerprint!(covered, base, "mp3.vbr_quality", |settings| {
        settings.mp3.vbr_quality = 6;
    });
    assert_mutation_changes_fingerprint!(covered, base, "aac.profile", |settings| {
        settings.aac.profile = AacProfile::HeAac;
    });
    assert_mutation_changes_fingerprint!(covered, base, "aac.bitrate_kbps", |settings| {
        settings.aac.bitrate_kbps = 383;
    });
    assert_mutation_changes_fingerprint!(covered, base, "opus.content_type", |settings| {
        settings.opus.content_type = OpusContentType::Music;
    });
    assert_mutation_changes_fingerprint!(covered, base, "opus.bitrate_kbps", |settings| {
        settings.opus.bitrate_kbps = 112;
    });
    assert_mutation_changes_fingerprint!(covered, base, "opus.complexity", |settings| {
        settings.opus.complexity = 8;
    });
    assert_mutation_changes_fingerprint!(covered, base, "wavpack.mode", |settings| {
        settings.wavpack.mode = WavPackMode::High;
    });
    assert_mutation_changes_fingerprint!(covered, base, "wavpack.hybrid", |settings| {
        settings.wavpack.hybrid = false;
    });
    assert_mutation_changes_fingerprint!(
        covered,
        base,
        "wavpack.hybrid_bitrate_kbps",
        |settings| {
            settings.wavpack.hybrid_bitrate_kbps = 512;
        }
    );
    assert_mutation_changes_fingerprint!(covered, base, "wavpack.correction_file", |settings| {
        settings.wavpack.correction_file = true;
    });
    assert_mutation_changes_fingerprint!(covered, base, "ssrc.force", |settings| {
        settings.ssrc.force = false;
    });
    assert_mutation_changes_fingerprint!(covered, base, "ssrc.two_pass", |settings| {
        settings.ssrc.two_pass = true;
    });
    assert_mutation_changes_fingerprint!(covered, base, "ssrc.insane_mode", |settings| {
        settings.ssrc.insane_mode = false;
    });
    assert_mutation_changes_fingerprint!(covered, base, "ssrc.profile", |settings| {
        settings.ssrc.profile = Some(SsrcProfile::High);
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.noise_shaper", |settings| {
        settings.dsd.noise_shaper = DsdNoiseShaper::Sdm;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.modulator_order", |settings| {
        settings.dsd.modulator_order = ModulatorOrder::Order6;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.trellis", |settings| {
        settings.dsd.trellis = None;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.trellis.lookahead", |settings| {
        settings.dsd.trellis.as_mut().unwrap().lookahead = 18;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.trellis.nodes", |settings| {
        settings.dsd.trellis.as_mut().unwrap().nodes = 10;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.trellis.latency", |settings| {
        settings.dsd.trellis.as_mut().unwrap().latency = Some(322);
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.pcm_to_dsd_filter", |settings| {
        settings.dsd.pcm_to_dsd_filter = DsdFilterPreset::Auto;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.dsd_to_pcm_lowpass", |settings| {
        settings.dsd.dsd_to_pcm_lowpass = DsdLowpassMethod::SoxUltra;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.dsd_to_pcm_gain_db", |settings| {
        settings.dsd.dsd_to_pcm_gain_db = Some(-2.75);
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.sinc.oversample_factor", |settings| {
        settings.dsd.sinc.oversample_factor = 32;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.sinc.taps", |settings| {
        settings.dsd.sinc.taps = 65_536;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.sinc.passband_hz", |settings| {
        settings.dsd.sinc.passband_hz = 31_000.0;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.sinc.transition_hz", |settings| {
        settings.dsd.sinc.transition_hz = 800.0;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.sinc.kaiser_beta", |settings| {
        settings.dsd.sinc.kaiser_beta = 13.0;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.sinc.linear_phase", |settings| {
        settings.dsd.sinc.linear_phase = true;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.sinc.allow_aliasing", |settings| {
        settings.dsd.sinc.allow_aliasing = false;
    });
    assert_mutation_changes_fingerprint!(covered, base, "dsd.gain_compensation", |settings| {
        settings.dsd.gain_compensation = GainCompensation::Linear(2.0);
    });
    assert_mutation_changes_fingerprint!(covered, base, "metadata.transfer_tags", |settings| {
        settings.metadata.transfer_tags = false;
    });
    assert_mutation_changes_fingerprint!(covered, base, "metadata.preserve_artwork", |settings| {
        settings.metadata.preserve_artwork = true;
    });
    assert_mutation_changes_fingerprint!(covered, base, "metadata.store_source_audio_md5", |settings| {
        settings.metadata.store_source_audio_md5 = false;
    });
    assert_mutation_changes_fingerprint!(covered, base, "verification.verify_after_encode", |settings| {
        settings.verification.verify_after_encode = false;
    });
    assert_mutation_changes_fingerprint!(
        covered,
        base,
        "verification.prefer_native_flac_verify",
        |settings| {
            settings.verification.prefer_native_flac_verify = true;
        }
    );
    assert_mutation_changes_fingerprint!(covered, base, "replay_gain.mode", |settings| {
        settings.replay_gain.mode = Some(ReplayGainMode::Album);
    });
    assert_mutation_changes_fingerprint!(covered, base, "replay_gain.prevent_clipping", |settings| {
        settings.replay_gain.prevent_clipping = true;
    });

    covered.sort_unstable();
    let mut expected = SETTINGS_FINGERPRINT_FIELD_PATHS.to_vec();
    expected.sort_unstable();
    assert_eq!(covered, expected, "fingerprint mutation inventory drifted");
}

#[cfg(feature = "serde")]
#[test]
fn serde_recursive_field_count_matches_checked_inventory_for_known_shapes() {
    let default = serde_json::to_value(PipelineSettings::default()).unwrap();
    let sentinel = serde_json::to_value(flac_md5_sentinel()).unwrap();

    assert_eq!(recursive_object_key_count(&default), 56);
    assert_eq!(recursive_object_key_count(&sentinel), 63);
}

#[cfg(feature = "serde")]
fn recursive_object_key_count(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(map) => {
            map.len() + map.values().map(recursive_object_key_count).sum::<usize>()
        }
        serde_json::Value::Array(values) => values.iter().map(recursive_object_key_count).sum(),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => 0,
    }
}
