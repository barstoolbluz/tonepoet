#![allow(clippy::float_cmp)]

//! Phase-2 settings sentinels.
//!
//! The Phase-1 suite used the retired dual-origin DSD wire model as part
//! of its field inventory. Phase 2 deliberately removes that schema. These
//! tests retain the same propagation/inventory role against the single strict
//! directional representation instead of keeping a compatibility shadow type.

use std::collections::BTreeSet;
use std::path::PathBuf;

use tonepoet::convert::pipeline::{build_pipeline_request, PipelineRequest};
use tonepoet::convert::simple_wizard::{
    DitherType as WizardDitherType, NyquistTransition as WizardNyquistTransition,
    ReplayGainMode as WizardReplayGainMode,
};
use tonepoet::convert::{
    AacProfile as QueueAacProfile, AudioFormat as QueueAudioFormat, ConversionItem,
    ConversionOptions, FileFormat, Mp3BitrateMode, QualitySettings,
    WavPackMode as QueueWavPackMode,
};
use tonepoet_pipeline::{
    AacProfile, AacSettings, AudioFormat, BitDepthTarget, DbNano, DitherType,
    DsdFilterPreset, DsdGeneralExportLevel, DsdGeneralReconstruction, DsdLowpassMethod,
    DsdNoiseShaper, DsdReconstructionSelection, DsdReferencePolicyVersion, DsdSettings,
    DsdSourcePathway, GainCompensation, MetadataSettings, ModulatorOrder,
    Mp3Mode, Mp3Settings, NyquistTransition, OpusContentType, OpusSettings, PcmBitDepth,
    PipelineSettings, PreferredTool, RateTarget, ReplayGainMode, ReplayGainSettings,
    ResampleQuality, SampleGainPolicy, SoxResamplerSettings, SoxSincPhase,
    SoxrResamplerSettings, SsrcPdfType, SsrcProfile, SsrcSettings, TrellisSettings,
    TruePeakScanTier, TruePeakScope, VerificationSettings, WavPackMode, WavPackSettings,
    SETTINGS_FINGERPRINT_FIELD_PATHS, SETTINGS_SNAPSHOT_V2_DSD_FIELD_COUNT,
    SETTINGS_SNAPSHOT_V2_DSD_FIELD_PATHS,
};

fn db(value: &str) -> DbNano {
    value.parse().expect("valid dB fixture")
}

fn rich_common_settings() -> PipelineSettings {
    let mut settings = PipelineSettings {
        target_format: AudioFormat::Flac,
        target_sample_rate: RateTarget::PcmHz(96_000),
        target_bit_depth: BitDepthTarget::Pcm(PcmBitDepth::Int24),
        resample_quality: ResampleQuality::High,
        nyquist_transition: NyquistTransition::BrickWall,
        dither_type: DitherType::Gesemann,
        dither_explicit: true,
        preferred_tool: PreferredTool::Custom("sentinel-tool".to_string()),
        force_encode: true,
        flac: tonepoet_pipeline::FlacSettings {
            compression_level: 5,
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
            insane_mode: true,
            profile: Some(SsrcProfile::Long),
            attenuation_db: Some(3.0),
            min_phase: true,
            dither_id: Some(2),
            pdf_type: Some(SsrcPdfType::Triangular),
        },
        sox_resampler: SoxResamplerSettings {
            chebyshev: true,
            bandwidth_pct: Some(97.0),
            phase: Some(25),
            allow_aliasing: true,
            sinc_taps: Some(262_144),
            sinc_attenuation_db: Some(120),
            sinc_passband_hz: Some(22_050.0),
            sinc_transition_hz: Some(500.0),
            sinc_kaiser_beta: Some(16.0),
            sinc_phase: Some(SoxSincPhase::Minimum),
        },
        soxr_resampler: SoxrResamplerSettings {
            chebyshev: true,
            cutoff: Some(0.97),
            phase: Some(25),
        },
        dsd: DsdSettings::default(),
        pcm_true_peak: Default::default(),
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
            existing_tags: tonepoet_pipeline::ReplayGainExistingTagPolicy::SkipIfComplete,
        },
    };

    settings.dsd.pcm_to_dsd.noise_shaper = DsdNoiseShaper::Crfb;
    settings.dsd.pcm_to_dsd.modulator_order = ModulatorOrder::Order7;
    settings.dsd.pcm_to_dsd.trellis = Some(TrellisSettings {
        lookahead: 17,
        nodes: 9,
        latency: Some(321),
    });
    settings.dsd.pcm_to_dsd.filter = DsdFilterPreset::Sinc;
    settings.dsd.pcm_to_dsd.sinc.oversample_factor = 16;
    settings.dsd.pcm_to_dsd.sinc.taps = 131_072;
    settings.dsd.pcm_to_dsd.sinc.passband_hz = 30_000.0;
    settings.dsd.pcm_to_dsd.sinc.transition_hz = 750.0;
    settings.dsd.pcm_to_dsd.sinc.kaiser_beta = 12.5;
    settings.dsd.pcm_to_dsd.sinc.linear_phase = false;
    settings.dsd.pcm_to_dsd.sinc.allow_aliasing = true;
    settings.dsd.pcm_to_dsd.gain_compensation = GainCompensation::Decibels(1.5);

    // While Custom is selected, qualified-Reference controls are dormant but
    // still raw settings identity. Use them to prove the strict single schema
    // propagates every stored field without making them execution claims.
    settings.dsd.from_dsd.reference_policy = DsdReferencePolicyVersion::SoxNg14801V15;
    settings.dsd.from_dsd.profile = DsdReconstructionSelection::Wideband;
    settings.dsd.from_dsd.gain = SampleGainPolicy::TruePeakNormalize {
        target_dbtp: db("-1.250000000"),
        scope: TruePeakScope::Album,
        scan: TruePeakScanTier::Standard,
    };

    settings.dsd.general_from_dsd.reconstruction = DsdGeneralReconstruction::ReferenceProtected;
    settings.dsd.general_from_dsd.lowpass = DsdLowpassMethod::Sinc;
    settings.dsd.general_from_dsd.sinc.taps = 131_072;
    settings.dsd.general_from_dsd.sinc.passband_hz = 30_000.0;
    settings.dsd.general_from_dsd.sinc.transition_hz = 750.0;
    settings.dsd.general_from_dsd.sinc.kaiser_beta = 12.5;
    settings.dsd.general_from_dsd.sinc.linear_phase = false;
    settings.dsd.general_from_dsd.sinc.allow_aliasing = true;
    settings.dsd.general_from_dsd.export_level = DsdGeneralExportLevel::NativeWithOffset {
        offset_db: db("0.500000000"),
    };
    settings.dsd.set_gain_policy(SampleGainPolicy::TruePeakNormalize {
        target_dbtp: db("-0.750000000"),
        scope: TruePeakScope::Album,
        scan: TruePeakScanTier::Standard,
    });
    settings
        .dsd
        .set_runtime_album_gain_db(Some(db("-2.000000000")));

    settings.pcm_true_peak.policy = SampleGainPolicy::TruePeakNormalize {
        target_dbtp: db("-0.500000000"),
        scope: TruePeakScope::Album,
        scan: TruePeakScanTier::Standard,
    };
    settings
        .pcm_true_peak
        .set_runtime_album_gain_db(Some(db("-1.500000000")));
    settings
}

fn flac_sentinel() -> PipelineSettings {
    let settings = rich_common_settings();
    settings.validate().expect("valid FLAC Phase-2 sentinel");
    settings
}

fn custom_fixed_sentinel() -> PipelineSettings {
    let mut settings = rich_common_settings();
    settings.target_format = AudioFormat::Custom {
        extension: "sent".to_string(),
        display_name: "Sentinel Audio".to_string(),
    };
    settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Float32);
    settings.flac.verify = false;
    settings.metadata.transfer_tags = false;
    settings.metadata.store_source_audio_md5 = false;
    settings.dsd.set_gain_policy(SampleGainPolicy::FixedGain {
        gain_db: db("1.250000000"),
    });
    // This setting is dormant while Custom is selected, but remains part of
    // raw settings identity. Exercise the non-default mode/gain slot without
    // inventing a second Reference-only gain schema.
    settings.dsd.from_dsd.gain = SampleGainPolicy::FixedGain {
        gain_db: db("1.750000000"),
    };
    settings.pcm_true_peak.policy = SampleGainPolicy::FixedGain {
        gain_db: db("2.250000000"),
    };
    settings.pcm_true_peak.set_runtime_album_gain_db(None);
    settings.validate().expect("valid custom fixed-gain sentinel");
    settings
}

fn reference_sentinel() -> PipelineSettings {
    let mut settings = rich_common_settings();
    settings.dsd.from_dsd.pathway = DsdSourcePathway::Reference;
    settings.dsd.from_dsd.reference_policy = DsdReferencePolicyVersion::SoxNg14801V17;
    settings.dsd.from_dsd.profile = DsdReconstructionSelection::Wideband;
    settings.dsd.from_dsd.gain = SampleGainPolicy::TruePeakNormalize {
        target_dbtp: db("-1.250000000"),
        scope: TruePeakScope::Track,
        scan: TruePeakScanTier::Reference,
    };
    // Explicit Reference delivery is mutually exclusive with the Custom DSD
    // sample-domain gain policy. The Custom sentinels above cover those fields.
    settings.dsd.set_gain_policy(SampleGainPolicy::Off);
    settings.dsd.clear_runtime_album_gain();
    settings.validate().expect("valid Reference-pathway sentinel");
    settings
}

fn valid_sentinels() -> [PipelineSettings; 3] {
    [flac_sentinel(), custom_fixed_sentinel(), reference_sentinel()]
}

fn queue_format_for_settings(settings: &PipelineSettings) -> QueueAudioFormat {
    match &settings.target_format {
        AudioFormat::Flac => QueueAudioFormat::Flac,
        AudioFormat::Wav => QueueAudioFormat::Wav,
        AudioFormat::Aiff => QueueAudioFormat::Aiff,
        AudioFormat::WavPack => QueueAudioFormat::WavPack,
        AudioFormat::Mp3 => QueueAudioFormat::Mp3,
        AudioFormat::Aac => QueueAudioFormat::Aac,
        AudioFormat::Opus => QueueAudioFormat::Opus,
        AudioFormat::Alac => QueueAudioFormat::Alac,
        AudioFormat::Dsf => QueueAudioFormat::Dsf,
        AudioFormat::Dff => QueueAudioFormat::Dff,
        AudioFormat::Dts => QueueAudioFormat::Dts,
        AudioFormat::Ac3 => QueueAudioFormat::Ac3,
        AudioFormat::Custom { .. } => QueueAudioFormat::Flac,
    }
}

fn item_with_settings(settings: PipelineSettings) -> ConversionItem {
    let mut options = ConversionOptions::default();
    options.output_format = queue_format_for_settings(&settings);
    options.pipeline_settings = Some(settings);
    ConversionItem::new(
        PathBuf::from("/tmp/tonepoet-settings-sentinel/input.flac"),
        FileFormat::Audio(QueueAudioFormat::Flac),
        options,
    )
}

fn json_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    path.split('.').try_fold(value, |current, component| current.get(component))
}

fn canonical_field_value(settings: &PipelineSettings, path: &str) -> serde_json::Value {
    match path {
        "dsd.general_from_dsd.export_offset_db" => {
            if let DsdGeneralExportLevel::NativeWithOffset { offset_db } =
                settings.dsd.general_from_dsd.export_level
            {
                serde_json::to_value(offset_db).expect("serialize export offset")
            } else {
                serde_json::Value::Null
            }
        }
        "dsd.runtime_album_gain_db" => serde_json::to_value(
            settings.dsd.runtime_album_gain_db(),
        )
        .expect("serialize DSD runtime gain"),
        "pcm_true_peak.runtime_album_gain_db" => serde_json::to_value(
            settings.pcm_true_peak.runtime_album_gain_db(),
        )
        .expect("serialize PCM runtime gain"),
        _ => {
            let value = serde_json::to_value(settings).expect("serialize settings sentinel");
            json_path(&value, path).cloned().unwrap_or(serde_json::Value::Null)
        }
    }
}

#[test]
fn canonical_fingerprint_inventory_has_no_duplicates_or_retired_gain_paths() {
    let paths: BTreeSet<_> = SETTINGS_FINGERPRINT_FIELD_PATHS.iter().copied().collect();
    assert_eq!(paths.len(), SETTINGS_FINGERPRINT_FIELD_PATHS.len());

    for required in [
        "dsd.from_dsd.pathway",
        "dsd.general_from_dsd.reconstruction",
        "dsd.general_from_dsd.export_level",
        "dsd.general_from_dsd.gain.mode",
        "dsd.general_from_dsd.gain.target_dbtp",
        "dsd.general_from_dsd.gain.scope",
        "dsd.general_from_dsd.gain.scan",
        "pcm_true_peak.policy.mode",
        "pcm_true_peak.policy.target_dbtp",
        "pcm_true_peak.policy.scope",
        "pcm_true_peak.policy.scan",
    ] {
        assert!(paths.contains(required), "missing current identity path {required}");
    }

    for retired in [
        "dsd.schema_origin",
        "dsd.dsd_to_pcm_gain_mode",
        "dsd.dsd_to_pcm_auto_gain_margin_db",
        "dsd.dsd_to_pcm_gain_db",
        "dsd.from_dsd.automatic_gain_scope",
        "pcm_true_peak.enabled",
        "pcm_true_peak.allow_boost",
    ] {
        assert!(!paths.contains(retired), "retired settings path still participates: {retired}");
    }
}

#[test]
fn current_sentinel_set_exercises_every_fingerprint_field_away_from_default() {
    let default = PipelineSettings::default();
    let sentinels = valid_sentinels();
    for path in SETTINGS_FINGERPRINT_FIELD_PATHS {
        let default_value = canonical_field_value(&default, path);
        assert!(
            sentinels
                .iter()
                .any(|settings| canonical_field_value(settings, path) != default_value),
            "sentinel set leaves canonical settings field at default: {path} ({default_value})"
        );
    }
}

#[test]
fn conversion_options_to_conversion_item_preserves_complete_settings() {
    for expected in valid_sentinels() {
        let item = item_with_settings(expected.clone());
        let actual = item.pipeline_settings.as_ref().expect("item settings missing");
        assert_eq!(actual, &expected);
    }
}

#[test]
fn conversion_item_to_pipeline_request_preserves_complete_settings() {
    for expected in valid_sentinels() {
        let item = item_with_settings(expected.clone());
        let request = build_pipeline_request(&item).expect("pipeline request");
        assert_eq!(request.settings, expected);
    }
}

#[test]
fn prebuilt_pipeline_request_preserves_complete_settings() {
    for expected in valid_sentinels() {
        let mut item = item_with_settings(expected.clone());
        let mut request = build_pipeline_request(&item).expect("pipeline request");
        request.settings = expected.clone();
        item.pipeline_request = Some(request);
        let actual = build_pipeline_request(&item).expect("prebuilt pipeline request");
        assert_eq!(actual.settings, expected);
    }
}

#[test]
fn incompatible_metadata_and_flac_sentinel_combinations_still_fail_closed() {
    let mut md5_requires_flac = custom_fixed_sentinel();
    md5_requires_flac.metadata.transfer_tags = true;
    md5_requires_flac.metadata.store_source_audio_md5 = true;
    assert!(md5_requires_flac.validate().is_err());

    let mut md5_requires_transfer = flac_sentinel();
    md5_requires_transfer.metadata.transfer_tags = false;
    assert!(md5_requires_transfer.validate().is_err());

    let mut flac_verify_requires_flac = custom_fixed_sentinel();
    flac_verify_requires_flac.flac.verify = true;
    assert!(flac_verify_requires_flac.validate().is_err());
}

#[test]
fn normal_request_builder_rejects_legacy_only_items() {
    let item = ConversionItem::new(
        PathBuf::from("/tmp/tonepoet-settings-sentinel/legacy.flac"),
        FileFormat::Audio(QueueAudioFormat::Flac),
        ConversionOptions::default(),
    );
    assert!(build_pipeline_request(&item).is_err());
}

#[test]
fn general_dsd_policy_carries_reconstruction_export_scope_and_tier_independently() {
    let mut settings = DsdSettings::default();
    settings.general_from_dsd.reconstruction = DsdGeneralReconstruction::ReferenceProtected;
    settings.general_from_dsd.export_level = DsdGeneralExportLevel::NativeWithOffset {
        offset_db: db("0.500000000"),
    };
    settings.set_gain_policy(SampleGainPolicy::TruePeakNormalize {
        target_dbtp: db("-0.250000000"),
        scope: TruePeakScope::Album,
        scan: TruePeakScanTier::Standard,
    });

    assert_eq!(settings.true_peak_scope(), Some(TruePeakScope::Album));
    assert_eq!(settings.true_peak_scan_tier(), Some(TruePeakScanTier::Standard));
    assert!(matches!(
        settings.gain_policy(),
        SampleGainPolicy::TruePeakNormalize { .. }
    ));
    assert_eq!(
        settings.general_from_dsd.export_level,
        DsdGeneralExportLevel::NativeWithOffset {
            offset_db: db("0.500000000")
        }
    );
}

#[test]
fn strict_dsd_settings_have_one_representation_for_general_and_reference_intent() {
    let general = DsdSettings::default();
    assert_eq!(general.from_dsd.pathway, DsdSourcePathway::Custom);
    assert_eq!(general.gain_policy(), SampleGainPolicy::Off);

    let reference = DsdSettings::reference();
    assert_eq!(reference.from_dsd.pathway, DsdSourcePathway::Reference);
    assert_eq!(reference.gain_policy(), SampleGainPolicy::Off);

    assert_eq!(general.pcm_to_dsd, reference.pcm_to_dsd);
    assert_eq!(general.general_from_dsd, reference.general_from_dsd);
}

#[test]
fn directional_dsd_snapshot_inventory_is_complete_and_strict() {
    let mut paths = SETTINGS_SNAPSHOT_V2_DSD_FIELD_PATHS.to_vec();
    let original_len = paths.len();
    paths.sort_unstable();
    paths.dedup();
    assert_eq!(paths.len(), original_len, "duplicate directional DSD snapshot path");
    assert_eq!(paths.len(), SETTINGS_SNAPSHOT_V2_DSD_FIELD_COUNT);
    assert!(paths.contains(&"dsd.from_dsd.pathway"));
    assert!(paths.contains(&"dsd.from_dsd.reference_policy"));
    assert!(paths.contains(&"dsd.general_from_dsd.gain.mode"));
    assert!(!paths.contains(&"dsd.schema"));
}

#[test]
fn deprecated_projection_retains_unrelated_legacy_behavior_but_cannot_invent_new_gain_policy() {
    let mut options = ConversionOptions::default();
    options.output_format = QueueAudioFormat::Flac;
    options.quality = QualitySettings::Flac {
        compression_level: 8,
    };
    options.preserve_metadata = false;
    options.calculate_replaygain = true;
    options.replaygain_mode = Some(WizardReplayGainMode::Both);
    options.resample_quality = Some(2);
    options.nyquist_transition = Some(WizardNyquistTransition::BrickWall);
    options.dither_type = Some(WizardDitherType::Gesemann);
    options.target_sample_rate = Some(96_000);
    options.target_bit_depth = Some(24);
    options.reencode_flac = true;
    options.preferred_backend = Some(tonepoet_backend::Backend::Sox);
    options.ssrc_insane_mode = Some(true);

    let item = ConversionItem::new(
        PathBuf::from("/tmp/tonepoet-settings-sentinel/legacy-projection.flac"),
        FileFormat::Audio(QueueAudioFormat::Flac),
        options,
    );
    #[allow(deprecated)]
    let projected = tonepoet::convert::pipeline::build_pipeline_request_from_legacy_options(&item)
        .expect("explicit legacy projection")
        .settings;

    assert_eq!(projected.target_format, AudioFormat::Flac);
    assert_eq!(projected.target_sample_rate, RateTarget::PcmHz(96_000));
    assert_eq!(projected.target_bit_depth, BitDepthTarget::Pcm(PcmBitDepth::Int24));
    assert_eq!(projected.resample_quality, ResampleQuality::High);
    assert_eq!(projected.nyquist_transition, NyquistTransition::BrickWall);
    assert_eq!(projected.dither_type, DitherType::Gesemann);
    assert_eq!(projected.preferred_tool, PreferredTool::Sox);
    assert!(projected.force_encode);
    assert_eq!(projected.flac.compression_level, 8);
    assert_eq!(projected.replay_gain.mode, Some(ReplayGainMode::Both));
    assert_eq!(projected.ssrc.profile, Some(SsrcProfile::Insane));

    // The deprecated queue projection cannot express the new mutually exclusive
    // sample-domain gain schema. It must leave those policies Off rather than
    // infer Guard/Normalize from historical booleans or defaults.
    assert_eq!(projected.dsd.gain_policy(), SampleGainPolicy::Off);
    assert_eq!(projected.pcm_true_peak.policy, SampleGainPolicy::Off);
}

#[test]
fn strict_settings_round_trip_preserves_typed_gain_and_rejects_unknown_fields() {
    let settings = flac_sentinel();
    let bytes = serde_json::to_vec(&settings).expect("serialize strict settings");
    let restored: PipelineSettings =
        serde_json::from_slice(&bytes).expect("round trip strict settings");

    // Runtime album authority is deliberately not persisted.
    let mut expected = settings;
    expected.dsd.clear_runtime_album_gain();
    expected.pcm_true_peak.clear_runtime_album_gain();
    assert_eq!(restored, expected);

    let mut value = serde_json::to_value(&expected).expect("serialize strict settings value");
    value
        .get_mut("dsd")
        .and_then(serde_json::Value::as_object_mut)
        .expect("DSD object")
        .insert("schema_origin".to_string(), serde_json::json!("legacy_v1"));
    assert!(serde_json::from_value::<PipelineSettings>(value).is_err());
}

#[test]
fn obsolete_ambiguous_gain_forms_are_rejected_not_migrated() {
    let current = serde_json::to_value(PipelineSettings::default()).expect("serialize defaults");

    for (key, value) in [
        ("dsd_to_pcm_gain_mode", serde_json::json!("auto")),
        ("dsd_to_pcm_auto_gain_margin_db", serde_json::json!(0.15)),
        ("dsd_to_pcm_gain_db", serde_json::json!(3.0)),
    ] {
        let mut candidate = current.clone();
        candidate
            .get_mut("dsd")
            .and_then(serde_json::Value::as_object_mut)
            .expect("DSD object")
            .insert(key.to_string(), value);
        assert!(
            serde_json::from_value::<PipelineSettings>(candidate).is_err(),
            "obsolete DSD field {key} must be rejected"
        );
    }

    let mut candidate = current;
    candidate
        .get_mut("pcm_true_peak")
        .and_then(serde_json::Value::as_object_mut)
        .expect("PCM gain object")
        .insert("allow_boost".to_string(), serde_json::json!(true));
    assert!(serde_json::from_value::<PipelineSettings>(candidate).is_err());
}

// Keep representative old quality adapters in this integration test so their
// imports remain compiled alongside the settings bridge. They are independent
// of the removed DSD schema.
#[test]
fn deprecated_quality_projection_still_handles_non_dsd_codec_variants() {
    let cases = [
        (
            QueueAudioFormat::Mp3,
            QualitySettings::Mp3 {
                bitrate_mode: Mp3BitrateMode::Cbr { bitrate: 192 },
                quality: 0,
            },
        ),
        (
            QueueAudioFormat::Aac,
            QualitySettings::Aac {
                bitrate: 96,
                profile: QueueAacProfile::HeV2,
            },
        ),
        (
            QueueAudioFormat::WavPack,
            QualitySettings::WavPack {
                compression_mode: QueueWavPackMode::VeryHigh,
                hybrid_mode: true,
                correction_file: false,
            },
        ),
    ];

    for (format, quality) in cases {
        let mut options = ConversionOptions::default();
        options.output_format = format;
        options.quality = quality;
        let item = ConversionItem::new(
            PathBuf::from("/tmp/tonepoet-settings-sentinel/legacy-codec.flac"),
            FileFormat::Audio(QueueAudioFormat::Flac),
            options,
        );
        #[allow(deprecated)]
        let request: PipelineRequest =
            tonepoet::convert::pipeline::build_pipeline_request_from_legacy_options(&item)
                .expect("legacy codec projection");
        assert_eq!(request.settings.dsd.gain_policy(), SampleGainPolicy::Off);
        assert_eq!(request.settings.pcm_true_peak.policy, SampleGainPolicy::Off);
    }
}
