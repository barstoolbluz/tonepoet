#![allow(clippy::float_cmp)]

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
    AacProfile, AacSettings, AudioFormat, BitDepthTarget, DitherType,
    DsdFilterPreset, DsdLowpassMethod, DsdNoiseShaper, DsdSettings, FlacSettings,
    GainCompensation, MetadataSettings, ModulatorOrder, Mp3Mode, Mp3Settings,
    NyquistTransition, OpusContentType, OpusSettings, PcmBitDepth, PipelineSettings,
    PreferredTool, RateTarget, ReplayGainMode, ReplayGainSettings, ResampleQuality,
    SETTINGS_FINGERPRINT_FIELD_PATHS, SincFilterSettings,
    SoxResamplerSettings, SoxSincPhase, SoxrResamplerSettings, SsrcProfile, SsrcSettings,
    TrellisSettings,
    VerificationSettings, WavPackMode,
    WavPackSettings,
};

/// Raw one-object sentinel with every `PipelineSettings` field set away from
/// `PipelineSettings::default()`.
///
/// This fixture intentionally fails `PipelineSettings::validate()`: current
/// invariants make a globally valid one-object all-non-default sentinel
/// impossible because `metadata.store_source_audio_md5 = true` requires both
/// `metadata.transfer_tags = true` and a FLAC target, while
/// `metadata.transfer_tags` defaults to `true` and `target_format` defaults to
/// `Flac`. The full-chain tests below therefore use the valid sentinel pair.
///
/// Sentinel values:
/// - target_format: Custom { extension: "sent", display_name: "Sentinel Audio" }
/// - target_sample_rate: PcmHz(96_000)
/// - target_bit_depth: Pcm(Float32)
/// - resample_quality: High
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

/// - ssrc.insane_mode: true
/// - sox_resampler.chebyshev: true
/// - sox_resampler.bandwidth_pct: Some(97.0)
/// - sox_resampler.phase: Some(25)
/// - sox_resampler.allow_aliasing: true
/// - sox_resampler.sinc_taps: Some(262144)
/// - sox_resampler.sinc_attenuation_db: Some(120)
/// - sox_resampler.sinc_passband_hz: Some(22050.0)
/// - sox_resampler.sinc_transition_hz: Some(500.0)
/// - sox_resampler.sinc_kaiser_beta: Some(16.0)
/// - sox_resampler.sinc_phase: Some(Minimum)
/// - soxr_resampler.chebyshev: true
/// - soxr_resampler.cutoff: Some(0.97)
/// - soxr_resampler.phase: Some(25)
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
/// - metadata.transfer_tags: false
/// - metadata.preserve_artwork: false
/// - metadata.store_source_audio_md5: true
/// - verification.verify_after_encode: true
/// - verification.prefer_native_flac_verify: false
/// - replay_gain.mode: Some(Both)
/// - replay_gain.prevent_clipping: false
fn raw_all_non_default_sentinel() -> PipelineSettings {
    PipelineSettings {
        target_format: AudioFormat::Custom {
            extension: "sent".to_string(),
            display_name: "Sentinel Audio".to_string(),
        },
        target_sample_rate: RateTarget::PcmHz(96_000),
        target_bit_depth: BitDepthTarget::Pcm(PcmBitDepth::Float32),
        resample_quality: ResampleQuality::High,
        nyquist_transition: NyquistTransition::BrickWall,
        dither_type: DitherType::Gesemann,
        preferred_tool: PreferredTool::Custom("sentinel-tool".to_string()),
        force_encode: true,
        flac: FlacSettings {
            compression_level: 8,
            verify: true,
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
        },
        sox_resampler: SoxResamplerSettings {
            chebyshev: true,
            bandwidth_pct: Some(97.0),
            phase: Some(25),
            allow_aliasing: true,
            sinc_taps: Some(262144),
            sinc_attenuation_db: Some(120),
            sinc_passband_hz: Some(22050.0),
            sinc_transition_hz: Some(500.0),
            sinc_kaiser_beta: Some(16.0),
            sinc_phase: Some(SoxSincPhase::Minimum),
        },
        soxr_resampler: SoxrResamplerSettings {
            chebyshev: true,
            cutoff: Some(0.97),
            phase: Some(25),
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
            transfer_tags: false,
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

/// Valid FLAC sentinel for fields whose non-default values require FLAC output.
fn flac_md5_sentinel() -> PipelineSettings {
    let mut settings = raw_all_non_default_sentinel();
    settings.target_format = AudioFormat::Flac;
    settings.metadata.transfer_tags = true;
    settings
}

/// Valid custom-target sentinel for the field values that conflict with FLAC-only
/// MD5 and native FLAC verification rules.
fn custom_format_sentinel() -> PipelineSettings {
    let mut settings = raw_all_non_default_sentinel();
    settings.flac.verify = false;
    settings.metadata.store_source_audio_md5 = false;
    settings
}

fn valid_sentinels() -> [PipelineSettings; 2] {
    [flac_md5_sentinel(), custom_format_sentinel()]
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
        AudioFormat::Custom { .. } => QueueAudioFormat::Flac,
    }
}

fn assert_settings_eq(actual: &PipelineSettings, expected: &PipelineSettings) {
    assert_eq!(&actual.target_format, &expected.target_format, "target_format");
    assert_eq!(&actual.target_sample_rate, &expected.target_sample_rate, "target_sample_rate");
    assert_eq!(&actual.target_bit_depth, &expected.target_bit_depth, "target_bit_depth");
    assert_eq!(&actual.resample_quality, &expected.resample_quality, "resample_quality");
    assert_eq!(&actual.nyquist_transition, &expected.nyquist_transition, "nyquist_transition");
    assert_eq!(&actual.dither_type, &expected.dither_type, "dither_type");
    assert_eq!(&actual.preferred_tool, &expected.preferred_tool, "preferred_tool");
    assert_eq!(&actual.force_encode, &expected.force_encode, "force_encode");
    assert_eq!(&actual.flac.compression_level, &expected.flac.compression_level, "flac.compression_level");
    assert_eq!(&actual.flac.verify, &expected.flac.verify, "flac.verify");
    assert_eq!(&actual.mp3.mode, &expected.mp3.mode, "mp3.mode");
    assert_eq!(&actual.mp3.bitrate_kbps, &expected.mp3.bitrate_kbps, "mp3.bitrate_kbps");
    assert_eq!(&actual.mp3.vbr_quality, &expected.mp3.vbr_quality, "mp3.vbr_quality");
    assert_eq!(&actual.aac.profile, &expected.aac.profile, "aac.profile");
    assert_eq!(&actual.aac.bitrate_kbps, &expected.aac.bitrate_kbps, "aac.bitrate_kbps");
    assert_eq!(&actual.opus.content_type, &expected.opus.content_type, "opus.content_type");
    assert_eq!(&actual.opus.bitrate_kbps, &expected.opus.bitrate_kbps, "opus.bitrate_kbps");
    assert_eq!(&actual.opus.complexity, &expected.opus.complexity, "opus.complexity");
    assert_eq!(&actual.wavpack.mode, &expected.wavpack.mode, "wavpack.mode");
    assert_eq!(&actual.wavpack.hybrid, &expected.wavpack.hybrid, "wavpack.hybrid");
    assert_eq!(&actual.wavpack.hybrid_bitrate_kbps, &expected.wavpack.hybrid_bitrate_kbps, "wavpack.hybrid_bitrate_kbps");
    assert_eq!(&actual.wavpack.correction_file, &expected.wavpack.correction_file, "wavpack.correction_file");
    assert_eq!(&actual.ssrc.force, &expected.ssrc.force, "ssrc.force");

    assert_eq!(&actual.ssrc.insane_mode, &expected.ssrc.insane_mode, "ssrc.insane_mode");
    assert_eq!(&actual.sox_resampler.chebyshev, &expected.sox_resampler.chebyshev, "sox_resampler.chebyshev");
    assert_eq!(&actual.sox_resampler.bandwidth_pct, &expected.sox_resampler.bandwidth_pct, "sox_resampler.bandwidth_pct");
    assert_eq!(&actual.sox_resampler.phase, &expected.sox_resampler.phase, "sox_resampler.phase");
    assert_eq!(&actual.sox_resampler.allow_aliasing, &expected.sox_resampler.allow_aliasing, "sox_resampler.allow_aliasing");
    assert_eq!(&actual.sox_resampler.sinc_taps, &expected.sox_resampler.sinc_taps, "sox_resampler.sinc_taps");
    assert_eq!(&actual.sox_resampler.sinc_attenuation_db, &expected.sox_resampler.sinc_attenuation_db, "sox_resampler.sinc_attenuation_db");
    assert_eq!(&actual.sox_resampler.sinc_passband_hz, &expected.sox_resampler.sinc_passband_hz, "sox_resampler.sinc_passband_hz");
    assert_eq!(&actual.sox_resampler.sinc_transition_hz, &expected.sox_resampler.sinc_transition_hz, "sox_resampler.sinc_transition_hz");
    assert_eq!(&actual.sox_resampler.sinc_kaiser_beta, &expected.sox_resampler.sinc_kaiser_beta, "sox_resampler.sinc_kaiser_beta");
    assert_eq!(&actual.sox_resampler.sinc_phase, &expected.sox_resampler.sinc_phase, "sox_resampler.sinc_phase");
    assert_eq!(&actual.soxr_resampler.chebyshev, &expected.soxr_resampler.chebyshev, "soxr_resampler.chebyshev");
    assert_eq!(&actual.soxr_resampler.cutoff, &expected.soxr_resampler.cutoff, "soxr_resampler.cutoff");
    assert_eq!(&actual.soxr_resampler.phase, &expected.soxr_resampler.phase, "soxr_resampler.phase");
    assert_eq!(&actual.ssrc.profile, &expected.ssrc.profile, "ssrc.profile");
    assert_eq!(&actual.dsd.noise_shaper, &expected.dsd.noise_shaper, "dsd.noise_shaper");
    assert_eq!(&actual.dsd.modulator_order, &expected.dsd.modulator_order, "dsd.modulator_order");
    assert_eq!(&actual.dsd.trellis, &expected.dsd.trellis, "dsd.trellis");
    assert_eq!(&actual.dsd.trellis.map(|trellis| trellis.lookahead), &expected.dsd.trellis.map(|trellis| trellis.lookahead), "dsd.trellis.lookahead");
    assert_eq!(&actual.dsd.trellis.map(|trellis| trellis.nodes), &expected.dsd.trellis.map(|trellis| trellis.nodes), "dsd.trellis.nodes");
    assert_eq!(&actual.dsd.trellis.and_then(|trellis| trellis.latency), &expected.dsd.trellis.and_then(|trellis| trellis.latency), "dsd.trellis.latency");
    assert_eq!(&actual.dsd.pcm_to_dsd_filter, &expected.dsd.pcm_to_dsd_filter, "dsd.pcm_to_dsd_filter");
    assert_eq!(&actual.dsd.dsd_to_pcm_lowpass, &expected.dsd.dsd_to_pcm_lowpass, "dsd.dsd_to_pcm_lowpass");
    assert_eq!(&actual.dsd.dsd_to_pcm_gain_db, &expected.dsd.dsd_to_pcm_gain_db, "dsd.dsd_to_pcm_gain_db");
    assert_eq!(&actual.dsd.sinc.oversample_factor, &expected.dsd.sinc.oversample_factor, "dsd.sinc.oversample_factor");
    assert_eq!(&actual.dsd.sinc.taps, &expected.dsd.sinc.taps, "dsd.sinc.taps");
    assert_eq!(&actual.dsd.sinc.passband_hz, &expected.dsd.sinc.passband_hz, "dsd.sinc.passband_hz");
    assert_eq!(&actual.dsd.sinc.transition_hz, &expected.dsd.sinc.transition_hz, "dsd.sinc.transition_hz");
    assert_eq!(&actual.dsd.sinc.kaiser_beta, &expected.dsd.sinc.kaiser_beta, "dsd.sinc.kaiser_beta");
    assert_eq!(&actual.dsd.sinc.linear_phase, &expected.dsd.sinc.linear_phase, "dsd.sinc.linear_phase");
    assert_eq!(&actual.dsd.sinc.allow_aliasing, &expected.dsd.sinc.allow_aliasing, "dsd.sinc.allow_aliasing");
    assert_eq!(&actual.dsd.gain_compensation, &expected.dsd.gain_compensation, "dsd.gain_compensation");
    assert_eq!(&actual.metadata.transfer_tags, &expected.metadata.transfer_tags, "metadata.transfer_tags");
    assert_eq!(&actual.metadata.preserve_artwork, &expected.metadata.preserve_artwork, "metadata.preserve_artwork");
    assert_eq!(&actual.metadata.store_source_audio_md5, &expected.metadata.store_source_audio_md5, "metadata.store_source_audio_md5");
    assert_eq!(&actual.verification.verify_after_encode, &expected.verification.verify_after_encode, "verification.verify_after_encode");
    assert_eq!(&actual.verification.prefer_native_flac_verify, &expected.verification.prefer_native_flac_verify, "verification.prefer_native_flac_verify");
    assert_eq!(&actual.replay_gain.mode, &expected.replay_gain.mode, "replay_gain.mode");
    assert_eq!(&actual.replay_gain.prevent_clipping, &expected.replay_gain.prevent_clipping, "replay_gain.prevent_clipping");
}

macro_rules! assert_covered_by_non_default {
    ($default:expr, $a:expr, $b:expr, $head:ident $(.$tail:ident)*, $label:literal) => {
        assert!(
            &$a.$head$(.$tail)* != &$default.$head$(.$tail)*
                || &$b.$head$(.$tail)* != &$default.$head$(.$tail)*,
            "sentinel pair leaves field at default in both cases: {}",
            $label
        );
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SentinelFieldInventoryRow {
    path: &'static str,
    raw_drift_covered: bool,
    valid_propagation_covered: bool,
    fingerprint_covered: bool,
    conflict_tests: &'static [&'static str],
}

const CONFLICT_MD5_REQUIRES_FLAC_OUTPUT: &str = "md5_requires_flac_output";
const CONFLICT_MD5_REQUIRES_METADATA_TRANSFER_TAGS: &str = "md5_requires_metadata_transfer_tags";
const CONFLICT_FLAC_VERIFY_REQUIRES_FLAC_OUTPUT: &str = "flac_verify_requires_flac_output";

const SENTINEL_FIELD_INVENTORY: &[SentinelFieldInventoryRow] = &[
    SentinelFieldInventoryRow { path: "target_format", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[CONFLICT_MD5_REQUIRES_FLAC_OUTPUT, CONFLICT_FLAC_VERIFY_REQUIRES_FLAC_OUTPUT] },
    SentinelFieldInventoryRow { path: "target_sample_rate", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "target_bit_depth", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "resample_quality", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "nyquist_transition", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dither_type", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "preferred_tool", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "force_encode", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "flac.compression_level", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "flac.verify", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[CONFLICT_FLAC_VERIFY_REQUIRES_FLAC_OUTPUT] },
    SentinelFieldInventoryRow { path: "mp3.mode", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "mp3.bitrate_kbps", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "mp3.vbr_quality", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "aac.profile", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "aac.bitrate_kbps", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "opus.content_type", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "opus.bitrate_kbps", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "opus.complexity", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "wavpack.mode", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "wavpack.hybrid", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "wavpack.hybrid_bitrate_kbps", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "wavpack.correction_file", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "ssrc.force", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },

    SentinelFieldInventoryRow { path: "ssrc.insane_mode", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "sox_resampler.chebyshev", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "sox_resampler.bandwidth_pct", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "sox_resampler.phase", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "sox_resampler.allow_aliasing", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "sox_resampler.sinc_taps", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "sox_resampler.sinc_attenuation_db", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "sox_resampler.sinc_passband_hz", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "sox_resampler.sinc_transition_hz", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "sox_resampler.sinc_kaiser_beta", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "sox_resampler.sinc_phase", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "soxr_resampler.chebyshev", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "soxr_resampler.cutoff", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "soxr_resampler.phase", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "ssrc.profile", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.noise_shaper", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.modulator_order", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.trellis", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.trellis.lookahead", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.trellis.nodes", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.trellis.latency", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.pcm_to_dsd_filter", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.dsd_to_pcm_lowpass", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.dsd_to_pcm_gain_db", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.sinc.oversample_factor", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.sinc.taps", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.sinc.passband_hz", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.sinc.transition_hz", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.sinc.kaiser_beta", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.sinc.linear_phase", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.sinc.allow_aliasing", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "dsd.gain_compensation", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "metadata.transfer_tags", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[CONFLICT_MD5_REQUIRES_METADATA_TRANSFER_TAGS] },
    SentinelFieldInventoryRow { path: "metadata.preserve_artwork", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "metadata.store_source_audio_md5", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[CONFLICT_MD5_REQUIRES_FLAC_OUTPUT, CONFLICT_MD5_REQUIRES_METADATA_TRANSFER_TAGS] },
    SentinelFieldInventoryRow { path: "verification.verify_after_encode", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "verification.prefer_native_flac_verify", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "replay_gain.mode", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
    SentinelFieldInventoryRow { path: "replay_gain.prevent_clipping", raw_drift_covered: true, valid_propagation_covered: true, fingerprint_covered: true, conflict_tests: &[] },
];

fn field_differs_from_default(
    path: &str,
    settings: &PipelineSettings,
    default: &PipelineSettings,
) -> bool {
    match path {
        "target_format" => settings.target_format != default.target_format,
        "target_sample_rate" => settings.target_sample_rate != default.target_sample_rate,
        "target_bit_depth" => settings.target_bit_depth != default.target_bit_depth,
        "resample_quality" => settings.resample_quality != default.resample_quality,
        "nyquist_transition" => settings.nyquist_transition != default.nyquist_transition,
        "dither_type" => settings.dither_type != default.dither_type,
        "preferred_tool" => settings.preferred_tool != default.preferred_tool,
        "force_encode" => settings.force_encode != default.force_encode,
        "flac.compression_level" => settings.flac.compression_level != default.flac.compression_level,
        "flac.verify" => settings.flac.verify != default.flac.verify,
        "mp3.mode" => settings.mp3.mode != default.mp3.mode,
        "mp3.bitrate_kbps" => settings.mp3.bitrate_kbps != default.mp3.bitrate_kbps,
        "mp3.vbr_quality" => settings.mp3.vbr_quality != default.mp3.vbr_quality,
        "aac.profile" => settings.aac.profile != default.aac.profile,
        "aac.bitrate_kbps" => settings.aac.bitrate_kbps != default.aac.bitrate_kbps,
        "opus.content_type" => settings.opus.content_type != default.opus.content_type,
        "opus.bitrate_kbps" => settings.opus.bitrate_kbps != default.opus.bitrate_kbps,
        "opus.complexity" => settings.opus.complexity != default.opus.complexity,
        "wavpack.mode" => settings.wavpack.mode != default.wavpack.mode,
        "wavpack.hybrid" => settings.wavpack.hybrid != default.wavpack.hybrid,
        "wavpack.hybrid_bitrate_kbps" => settings.wavpack.hybrid_bitrate_kbps != default.wavpack.hybrid_bitrate_kbps,
        "wavpack.correction_file" => settings.wavpack.correction_file != default.wavpack.correction_file,
        "ssrc.force" => settings.ssrc.force != default.ssrc.force,

        "ssrc.insane_mode" => settings.ssrc.insane_mode != default.ssrc.insane_mode,
        "sox_resampler.chebyshev" => settings.sox_resampler.chebyshev != default.sox_resampler.chebyshev,
        "sox_resampler.bandwidth_pct" => settings.sox_resampler.bandwidth_pct != default.sox_resampler.bandwidth_pct,
        "sox_resampler.phase" => settings.sox_resampler.phase != default.sox_resampler.phase,
        "sox_resampler.allow_aliasing" => settings.sox_resampler.allow_aliasing != default.sox_resampler.allow_aliasing,
        "sox_resampler.sinc_taps" => settings.sox_resampler.sinc_taps != default.sox_resampler.sinc_taps,
        "sox_resampler.sinc_attenuation_db" => settings.sox_resampler.sinc_attenuation_db != default.sox_resampler.sinc_attenuation_db,
        "sox_resampler.sinc_passband_hz" => settings.sox_resampler.sinc_passband_hz != default.sox_resampler.sinc_passband_hz,
        "sox_resampler.sinc_transition_hz" => settings.sox_resampler.sinc_transition_hz != default.sox_resampler.sinc_transition_hz,
        "sox_resampler.sinc_kaiser_beta" => settings.sox_resampler.sinc_kaiser_beta != default.sox_resampler.sinc_kaiser_beta,
        "sox_resampler.sinc_phase" => settings.sox_resampler.sinc_phase != default.sox_resampler.sinc_phase,
        "soxr_resampler.chebyshev" => settings.soxr_resampler.chebyshev != default.soxr_resampler.chebyshev,
        "soxr_resampler.cutoff" => settings.soxr_resampler.cutoff != default.soxr_resampler.cutoff,
        "soxr_resampler.phase" => settings.soxr_resampler.phase != default.soxr_resampler.phase,
        "ssrc.profile" => settings.ssrc.profile != default.ssrc.profile,
        "dsd.noise_shaper" => settings.dsd.noise_shaper != default.dsd.noise_shaper,
        "dsd.modulator_order" => settings.dsd.modulator_order != default.dsd.modulator_order,
        "dsd.trellis" => settings.dsd.trellis != default.dsd.trellis,
        "dsd.trellis.lookahead" => settings.dsd.trellis.map(|trellis| trellis.lookahead) != default.dsd.trellis.map(|trellis| trellis.lookahead),
        "dsd.trellis.nodes" => settings.dsd.trellis.map(|trellis| trellis.nodes) != default.dsd.trellis.map(|trellis| trellis.nodes),
        "dsd.trellis.latency" => settings.dsd.trellis.and_then(|trellis| trellis.latency) != default.dsd.trellis.and_then(|trellis| trellis.latency),
        "dsd.pcm_to_dsd_filter" => settings.dsd.pcm_to_dsd_filter != default.dsd.pcm_to_dsd_filter,
        "dsd.dsd_to_pcm_lowpass" => settings.dsd.dsd_to_pcm_lowpass != default.dsd.dsd_to_pcm_lowpass,
        "dsd.dsd_to_pcm_gain_db" => settings.dsd.dsd_to_pcm_gain_db != default.dsd.dsd_to_pcm_gain_db,
        "dsd.sinc.oversample_factor" => settings.dsd.sinc.oversample_factor != default.dsd.sinc.oversample_factor,
        "dsd.sinc.taps" => settings.dsd.sinc.taps != default.dsd.sinc.taps,
        "dsd.sinc.passband_hz" => settings.dsd.sinc.passband_hz != default.dsd.sinc.passband_hz,
        "dsd.sinc.transition_hz" => settings.dsd.sinc.transition_hz != default.dsd.sinc.transition_hz,
        "dsd.sinc.kaiser_beta" => settings.dsd.sinc.kaiser_beta != default.dsd.sinc.kaiser_beta,
        "dsd.sinc.linear_phase" => settings.dsd.sinc.linear_phase != default.dsd.sinc.linear_phase,
        "dsd.sinc.allow_aliasing" => settings.dsd.sinc.allow_aliasing != default.dsd.sinc.allow_aliasing,
        "dsd.gain_compensation" => settings.dsd.gain_compensation != default.dsd.gain_compensation,
        "metadata.transfer_tags" => settings.metadata.transfer_tags != default.metadata.transfer_tags,
        "metadata.preserve_artwork" => settings.metadata.preserve_artwork != default.metadata.preserve_artwork,
        "metadata.store_source_audio_md5" => settings.metadata.store_source_audio_md5 != default.metadata.store_source_audio_md5,
        "verification.verify_after_encode" => settings.verification.verify_after_encode != default.verification.verify_after_encode,
        "verification.prefer_native_flac_verify" => settings.verification.prefer_native_flac_verify != default.verification.prefer_native_flac_verify,
        "replay_gain.mode" => settings.replay_gain.mode != default.replay_gain.mode,
        "replay_gain.prevent_clipping" => settings.replay_gain.prevent_clipping != default.replay_gain.prevent_clipping,
        other => panic!("unknown PipelineSettings field path in sentinel inventory: {}", other),
    }
}

fn known_conflict_test(name: &str) -> bool {
    matches!(
        name,
        CONFLICT_MD5_REQUIRES_FLAC_OUTPUT
            | CONFLICT_MD5_REQUIRES_METADATA_TRANSFER_TAGS
            | CONFLICT_FLAC_VERIFY_REQUIRES_FLAC_OUTPUT
    )
}

#[test]
fn sentinel_suite_inventory_matches_fingerprint_field_list() {
    let mut inventory: Vec<&str> = SENTINEL_FIELD_INVENTORY.iter().map(|row| row.path).collect();
    let mut fingerprint = SETTINGS_FINGERPRINT_FIELD_PATHS.to_vec();
    inventory.sort_unstable();
    fingerprint.sort_unstable();

    let mut deduped = inventory.clone();
    deduped.dedup();
    assert_eq!(deduped.len(), inventory.len(), "duplicate sentinel inventory path");
    assert_eq!(inventory, fingerprint, "sentinel inventory drifted from fingerprint field list");
    assert_eq!(SENTINEL_FIELD_INVENTORY.len(), tonepoet_pipeline::SETTINGS_FINGERPRINT_FIELD_COUNT);
}

#[test]
fn sentinel_suite_inventory_classification_is_mechanically_checked() {
    let default = PipelineSettings::default();
    let raw = raw_all_non_default_sentinel();
    let valid = valid_sentinels();

    for row in SENTINEL_FIELD_INVENTORY {
        assert!(row.fingerprint_covered, "{} is not marked fingerprint-covered", row.path);
        assert_eq!(
            field_differs_from_default(row.path, &raw, &default),
            row.raw_drift_covered,
            "raw drift classification mismatch for {}",
            row.path
        );

        let valid_covered = valid
            .iter()
            .any(|settings| field_differs_from_default(row.path, settings, &default));
        assert_eq!(
            valid_covered,
            row.valid_propagation_covered,
            "valid propagation classification mismatch for {}",
            row.path
        );

        for conflict_test in row.conflict_tests {
            assert!(known_conflict_test(conflict_test), "unknown conflict test {} for {}", conflict_test, row.path);
        }
        if !row.valid_propagation_covered {
            assert!(
                !row.conflict_tests.is_empty(),
                "{} lacks valid propagation coverage without a named conflict test",
                row.path
            );
        }
    }
}

#[test]
fn raw_single_sentinel_sets_every_field_away_from_default() {
    let default = PipelineSettings::default();
    let raw = raw_all_non_default_sentinel();

    assert_covered_by_non_default!(default, raw, raw, target_format, "target_format");
    assert_covered_by_non_default!(default, raw, raw, target_sample_rate, "target_sample_rate");
    assert_covered_by_non_default!(default, raw, raw, target_bit_depth, "target_bit_depth");
    assert_covered_by_non_default!(default, raw, raw, resample_quality, "resample_quality");
    assert_covered_by_non_default!(default, raw, raw, nyquist_transition, "nyquist_transition");
    assert_covered_by_non_default!(default, raw, raw, dither_type, "dither_type");
    assert_covered_by_non_default!(default, raw, raw, preferred_tool, "preferred_tool");
    assert_covered_by_non_default!(default, raw, raw, force_encode, "force_encode");
    assert_covered_by_non_default!(default, raw, raw, flac.compression_level, "flac.compression_level");
    assert_covered_by_non_default!(default, raw, raw, flac.verify, "flac.verify");
    assert_covered_by_non_default!(default, raw, raw, mp3.mode, "mp3.mode");
    assert_covered_by_non_default!(default, raw, raw, mp3.bitrate_kbps, "mp3.bitrate_kbps");
    assert_covered_by_non_default!(default, raw, raw, mp3.vbr_quality, "mp3.vbr_quality");
    assert_covered_by_non_default!(default, raw, raw, aac.profile, "aac.profile");
    assert_covered_by_non_default!(default, raw, raw, aac.bitrate_kbps, "aac.bitrate_kbps");
    assert_covered_by_non_default!(default, raw, raw, opus.content_type, "opus.content_type");
    assert_covered_by_non_default!(default, raw, raw, opus.bitrate_kbps, "opus.bitrate_kbps");
    assert_covered_by_non_default!(default, raw, raw, opus.complexity, "opus.complexity");
    assert_covered_by_non_default!(default, raw, raw, wavpack.mode, "wavpack.mode");
    assert_covered_by_non_default!(default, raw, raw, wavpack.hybrid, "wavpack.hybrid");
    assert_covered_by_non_default!(default, raw, raw, wavpack.hybrid_bitrate_kbps, "wavpack.hybrid_bitrate_kbps");
    assert_covered_by_non_default!(default, raw, raw, wavpack.correction_file, "wavpack.correction_file");
    assert_covered_by_non_default!(default, raw, raw, ssrc.force, "ssrc.force");

    assert_covered_by_non_default!(default, raw, raw, ssrc.insane_mode, "ssrc.insane_mode");
    assert_covered_by_non_default!(default, raw, raw, sox_resampler.chebyshev, "sox_resampler.chebyshev");
    assert_covered_by_non_default!(default, raw, raw, sox_resampler.bandwidth_pct, "sox_resampler.bandwidth_pct");
    assert_covered_by_non_default!(default, raw, raw, sox_resampler.phase, "sox_resampler.phase");
    assert_covered_by_non_default!(default, raw, raw, sox_resampler.allow_aliasing, "sox_resampler.allow_aliasing");
    assert_covered_by_non_default!(default, raw, raw, sox_resampler.sinc_taps, "sox_resampler.sinc_taps");
    assert_covered_by_non_default!(default, raw, raw, sox_resampler.sinc_attenuation_db, "sox_resampler.sinc_attenuation_db");
    assert_covered_by_non_default!(default, raw, raw, sox_resampler.sinc_passband_hz, "sox_resampler.sinc_passband_hz");
    assert_covered_by_non_default!(default, raw, raw, sox_resampler.sinc_transition_hz, "sox_resampler.sinc_transition_hz");
    assert_covered_by_non_default!(default, raw, raw, sox_resampler.sinc_kaiser_beta, "sox_resampler.sinc_kaiser_beta");
    assert_covered_by_non_default!(default, raw, raw, sox_resampler.sinc_phase, "sox_resampler.sinc_phase");
    assert_covered_by_non_default!(default, raw, raw, soxr_resampler.chebyshev, "soxr_resampler.chebyshev");
    assert_covered_by_non_default!(default, raw, raw, soxr_resampler.cutoff, "soxr_resampler.cutoff");
    assert_covered_by_non_default!(default, raw, raw, soxr_resampler.phase, "soxr_resampler.phase");
    assert_covered_by_non_default!(default, raw, raw, ssrc.profile, "ssrc.profile");
    assert_covered_by_non_default!(default, raw, raw, dsd.noise_shaper, "dsd.noise_shaper");
    assert_covered_by_non_default!(default, raw, raw, dsd.modulator_order, "dsd.modulator_order");
    assert_covered_by_non_default!(default, raw, raw, dsd.trellis, "dsd.trellis");
    assert_covered_by_non_default!(default, raw, raw, dsd.pcm_to_dsd_filter, "dsd.pcm_to_dsd_filter");
    assert_covered_by_non_default!(default, raw, raw, dsd.dsd_to_pcm_lowpass, "dsd.dsd_to_pcm_lowpass");
    assert_covered_by_non_default!(default, raw, raw, dsd.dsd_to_pcm_gain_db, "dsd.dsd_to_pcm_gain_db");
    assert_covered_by_non_default!(default, raw, raw, dsd.sinc.oversample_factor, "dsd.sinc.oversample_factor");
    assert_covered_by_non_default!(default, raw, raw, dsd.sinc.taps, "dsd.sinc.taps");
    assert_covered_by_non_default!(default, raw, raw, dsd.sinc.passband_hz, "dsd.sinc.passband_hz");
    assert_covered_by_non_default!(default, raw, raw, dsd.sinc.transition_hz, "dsd.sinc.transition_hz");
    assert_covered_by_non_default!(default, raw, raw, dsd.sinc.kaiser_beta, "dsd.sinc.kaiser_beta");
    assert_covered_by_non_default!(default, raw, raw, dsd.sinc.linear_phase, "dsd.sinc.linear_phase");
    assert_covered_by_non_default!(default, raw, raw, dsd.sinc.allow_aliasing, "dsd.sinc.allow_aliasing");
    assert_covered_by_non_default!(default, raw, raw, dsd.gain_compensation, "dsd.gain_compensation");
    assert_covered_by_non_default!(default, raw, raw, metadata.transfer_tags, "metadata.transfer_tags");
    assert_covered_by_non_default!(default, raw, raw, metadata.preserve_artwork, "metadata.preserve_artwork");
    assert_covered_by_non_default!(default, raw, raw, metadata.store_source_audio_md5, "metadata.store_source_audio_md5");
    assert_covered_by_non_default!(default, raw, raw, verification.verify_after_encode, "verification.verify_after_encode");
    assert_covered_by_non_default!(default, raw, raw, verification.prefer_native_flac_verify, "verification.prefer_native_flac_verify");
    assert_covered_by_non_default!(default, raw, raw, replay_gain.mode, "replay_gain.mode");
    assert_covered_by_non_default!(default, raw, raw, replay_gain.prevent_clipping, "replay_gain.prevent_clipping");

    assert!(raw.validate().is_err());
}

#[test]
fn amended_contract_valid_sentinel_set_covers_every_pipeline_settings_field() {
    let default = PipelineSettings::default();
    let flac = flac_md5_sentinel();
    let custom = custom_format_sentinel();

    flac.validate().unwrap();
    custom.validate().unwrap();

    assert_covered_by_non_default!(default, flac, custom, target_format, "target_format");
    assert_covered_by_non_default!(default, flac, custom, target_sample_rate, "target_sample_rate");
    assert_covered_by_non_default!(default, flac, custom, target_bit_depth, "target_bit_depth");
    assert_covered_by_non_default!(default, flac, custom, resample_quality, "resample_quality");
    assert_covered_by_non_default!(default, flac, custom, nyquist_transition, "nyquist_transition");
    assert_covered_by_non_default!(default, flac, custom, dither_type, "dither_type");
    assert_covered_by_non_default!(default, flac, custom, preferred_tool, "preferred_tool");
    assert_covered_by_non_default!(default, flac, custom, force_encode, "force_encode");
    assert_covered_by_non_default!(default, flac, custom, flac.compression_level, "flac.compression_level");
    assert_covered_by_non_default!(default, flac, custom, flac.verify, "flac.verify");
    assert_covered_by_non_default!(default, flac, custom, mp3.mode, "mp3.mode");
    assert_covered_by_non_default!(default, flac, custom, mp3.bitrate_kbps, "mp3.bitrate_kbps");
    assert_covered_by_non_default!(default, flac, custom, mp3.vbr_quality, "mp3.vbr_quality");
    assert_covered_by_non_default!(default, flac, custom, aac.profile, "aac.profile");
    assert_covered_by_non_default!(default, flac, custom, aac.bitrate_kbps, "aac.bitrate_kbps");
    assert_covered_by_non_default!(default, flac, custom, opus.content_type, "opus.content_type");
    assert_covered_by_non_default!(default, flac, custom, opus.bitrate_kbps, "opus.bitrate_kbps");
    assert_covered_by_non_default!(default, flac, custom, opus.complexity, "opus.complexity");
    assert_covered_by_non_default!(default, flac, custom, wavpack.mode, "wavpack.mode");
    assert_covered_by_non_default!(default, flac, custom, wavpack.hybrid, "wavpack.hybrid");
    assert_covered_by_non_default!(default, flac, custom, wavpack.hybrid_bitrate_kbps, "wavpack.hybrid_bitrate_kbps");
    assert_covered_by_non_default!(default, flac, custom, wavpack.correction_file, "wavpack.correction_file");
    assert_covered_by_non_default!(default, flac, custom, ssrc.force, "ssrc.force");

    assert_covered_by_non_default!(default, flac, custom, ssrc.insane_mode, "ssrc.insane_mode");
    assert_covered_by_non_default!(default, flac, custom, sox_resampler.chebyshev, "sox_resampler.chebyshev");
    assert_covered_by_non_default!(default, flac, custom, sox_resampler.bandwidth_pct, "sox_resampler.bandwidth_pct");
    assert_covered_by_non_default!(default, flac, custom, sox_resampler.phase, "sox_resampler.phase");
    assert_covered_by_non_default!(default, flac, custom, sox_resampler.allow_aliasing, "sox_resampler.allow_aliasing");
    assert_covered_by_non_default!(default, flac, custom, sox_resampler.sinc_taps, "sox_resampler.sinc_taps");
    assert_covered_by_non_default!(default, flac, custom, sox_resampler.sinc_attenuation_db, "sox_resampler.sinc_attenuation_db");
    assert_covered_by_non_default!(default, flac, custom, sox_resampler.sinc_passband_hz, "sox_resampler.sinc_passband_hz");
    assert_covered_by_non_default!(default, flac, custom, sox_resampler.sinc_transition_hz, "sox_resampler.sinc_transition_hz");
    assert_covered_by_non_default!(default, flac, custom, sox_resampler.sinc_kaiser_beta, "sox_resampler.sinc_kaiser_beta");
    assert_covered_by_non_default!(default, flac, custom, sox_resampler.sinc_phase, "sox_resampler.sinc_phase");
    assert_covered_by_non_default!(default, flac, custom, soxr_resampler.chebyshev, "soxr_resampler.chebyshev");
    assert_covered_by_non_default!(default, flac, custom, soxr_resampler.cutoff, "soxr_resampler.cutoff");
    assert_covered_by_non_default!(default, flac, custom, soxr_resampler.phase, "soxr_resampler.phase");
    assert_covered_by_non_default!(default, flac, custom, ssrc.profile, "ssrc.profile");
    assert_covered_by_non_default!(default, flac, custom, dsd.noise_shaper, "dsd.noise_shaper");
    assert_covered_by_non_default!(default, flac, custom, dsd.modulator_order, "dsd.modulator_order");
    assert_covered_by_non_default!(default, flac, custom, dsd.trellis, "dsd.trellis");
    assert_covered_by_non_default!(default, flac, custom, dsd.pcm_to_dsd_filter, "dsd.pcm_to_dsd_filter");
    assert_covered_by_non_default!(default, flac, custom, dsd.dsd_to_pcm_lowpass, "dsd.dsd_to_pcm_lowpass");
    assert_covered_by_non_default!(default, flac, custom, dsd.dsd_to_pcm_gain_db, "dsd.dsd_to_pcm_gain_db");
    assert_covered_by_non_default!(default, flac, custom, dsd.sinc.oversample_factor, "dsd.sinc.oversample_factor");
    assert_covered_by_non_default!(default, flac, custom, dsd.sinc.taps, "dsd.sinc.taps");
    assert_covered_by_non_default!(default, flac, custom, dsd.sinc.passband_hz, "dsd.sinc.passband_hz");
    assert_covered_by_non_default!(default, flac, custom, dsd.sinc.transition_hz, "dsd.sinc.transition_hz");
    assert_covered_by_non_default!(default, flac, custom, dsd.sinc.kaiser_beta, "dsd.sinc.kaiser_beta");
    assert_covered_by_non_default!(default, flac, custom, dsd.sinc.linear_phase, "dsd.sinc.linear_phase");
    assert_covered_by_non_default!(default, flac, custom, dsd.sinc.allow_aliasing, "dsd.sinc.allow_aliasing");
    assert_covered_by_non_default!(default, flac, custom, dsd.gain_compensation, "dsd.gain_compensation");
    assert_covered_by_non_default!(default, flac, custom, metadata.transfer_tags, "metadata.transfer_tags");
    assert_covered_by_non_default!(default, flac, custom, metadata.preserve_artwork, "metadata.preserve_artwork");
    assert_covered_by_non_default!(default, flac, custom, metadata.store_source_audio_md5, "metadata.store_source_audio_md5");
    assert_covered_by_non_default!(default, flac, custom, verification.verify_after_encode, "verification.verify_after_encode");
    assert_covered_by_non_default!(default, flac, custom, verification.prefer_native_flac_verify, "verification.prefer_native_flac_verify");
    assert_covered_by_non_default!(default, flac, custom, replay_gain.mode, "replay_gain.mode");
    assert_covered_by_non_default!(default, flac, custom, replay_gain.prevent_clipping, "replay_gain.prevent_clipping");
}

#[test]
fn single_valid_all_non_default_sentinel_conflict_is_executably_documented() {
    let mut md5_requires_flac_output = custom_format_sentinel();
    md5_requires_flac_output.metadata.transfer_tags = true;
    md5_requires_flac_output.metadata.store_source_audio_md5 = true;
    assert!(
        md5_requires_flac_output.validate().is_err(),
        "{} conflict was not enforced",
        CONFLICT_MD5_REQUIRES_FLAC_OUTPUT
    );

    let mut md5_requires_metadata_transfer_tags = flac_md5_sentinel();
    md5_requires_metadata_transfer_tags.metadata.transfer_tags = false;
    assert!(
        md5_requires_metadata_transfer_tags.validate().is_err(),
        "{} conflict was not enforced",
        CONFLICT_MD5_REQUIRES_METADATA_TRANSFER_TAGS
    );

    let mut flac_verify_requires_flac_output = custom_format_sentinel();
    flac_verify_requires_flac_output.flac.verify = true;
    assert!(
        flac_verify_requires_flac_output.validate().is_err(),
        "{} conflict was not enforced",
        CONFLICT_FLAC_VERIFY_REQUIRES_FLAC_OUTPUT
    );
}

#[test]
fn conversion_options_to_conversion_item_preserves_settings_field_by_field() {
    for expected in valid_sentinels() {
        let item = item_with_settings(expected.clone());
        let actual = item.pipeline_settings.as_ref().expect("item settings missing");
        assert_settings_eq(actual, &expected);
    }
}

#[test]
fn conversion_item_to_pipeline_request_preserves_settings_field_by_field() {
    for expected in valid_sentinels() {
        let item = item_with_settings(expected.clone());
        let request = build_pipeline_request(&item).unwrap();
        assert_settings_eq(&request.settings, &expected);
    }
}

#[test]
fn prebuilt_pipeline_request_preserves_settings_field_by_field() {
    for expected in valid_sentinels() {
        let mut item = item_with_settings(expected.clone());
        let mut request = build_pipeline_request(&item).unwrap();
        request.settings = expected.clone();
        item.pipeline_request = Some(request);

        let actual = build_pipeline_request(&item).unwrap();
        assert_settings_eq(&actual.settings, &expected);
    }
}

// The PipelineRequest -> PlanRequest edge is tested in the production module
// that owns the real per-track `convert_tracks(...)` path. The installer adds
// a `#[cfg(test)]` capture around that path's actual `PlanRequest` literal and
// appends a field-by-field runtime test there. `tests/settings_static_audit.rs`
// remains as a separate syntactic guard.

#[test]
fn normal_request_builder_rejects_legacy_only_items() {
    let item = ConversionItem::new(
        PathBuf::from("/tmp/tonepoet-settings-sentinel/legacy.flac"),
        FileFormat::Audio(QueueAudioFormat::Flac),
        ConversionOptions::default(),
    );
    assert!(build_pipeline_request(&item).is_err());
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum LegacyProjectionStatus {
    Translated,
    Derived,
    Defaulted,
    Unrepresentable,
}

const LEGACY_FIELD_INVENTORY: &[(&str, LegacyProjectionStatus)] = &[
    ("target_format", LegacyProjectionStatus::Translated),
    ("target_sample_rate", LegacyProjectionStatus::Translated),
    ("target_bit_depth", LegacyProjectionStatus::Translated),
    ("resample_quality", LegacyProjectionStatus::Translated),
    ("nyquist_transition", LegacyProjectionStatus::Translated),
    ("dither_type", LegacyProjectionStatus::Translated),
    ("preferred_tool", LegacyProjectionStatus::Translated),
    ("force_encode", LegacyProjectionStatus::Translated),
    ("flac.compression_level", LegacyProjectionStatus::Translated),
    ("flac.verify", LegacyProjectionStatus::Derived),
    ("mp3.mode", LegacyProjectionStatus::Translated),
    ("mp3.bitrate_kbps", LegacyProjectionStatus::Translated),
    ("mp3.vbr_quality", LegacyProjectionStatus::Translated),
    ("aac.profile", LegacyProjectionStatus::Translated),
    ("aac.bitrate_kbps", LegacyProjectionStatus::Translated),
    ("opus.content_type", LegacyProjectionStatus::Defaulted),
    ("opus.bitrate_kbps", LegacyProjectionStatus::Translated),
    ("opus.complexity", LegacyProjectionStatus::Translated),
    ("wavpack.mode", LegacyProjectionStatus::Translated),
    ("wavpack.hybrid", LegacyProjectionStatus::Defaulted),
    ("wavpack.hybrid_bitrate_kbps", LegacyProjectionStatus::Defaulted),
    ("wavpack.correction_file", LegacyProjectionStatus::Defaulted),
    ("ssrc.force", LegacyProjectionStatus::Derived),

    ("ssrc.insane_mode", LegacyProjectionStatus::Translated),
    ("sox_resampler.chebyshev", LegacyProjectionStatus::Defaulted),
    ("sox_resampler.bandwidth_pct", LegacyProjectionStatus::Defaulted),
    ("sox_resampler.phase", LegacyProjectionStatus::Defaulted),
    ("sox_resampler.allow_aliasing", LegacyProjectionStatus::Defaulted),
    ("sox_resampler.sinc_taps", LegacyProjectionStatus::Defaulted),
    ("sox_resampler.sinc_attenuation_db", LegacyProjectionStatus::Defaulted),
    ("sox_resampler.sinc_passband_hz", LegacyProjectionStatus::Defaulted),
    ("sox_resampler.sinc_transition_hz", LegacyProjectionStatus::Defaulted),
    ("sox_resampler.sinc_kaiser_beta", LegacyProjectionStatus::Defaulted),
    ("sox_resampler.sinc_phase", LegacyProjectionStatus::Defaulted),
    ("soxr_resampler.chebyshev", LegacyProjectionStatus::Defaulted),
    ("soxr_resampler.cutoff", LegacyProjectionStatus::Defaulted),
    ("soxr_resampler.phase", LegacyProjectionStatus::Defaulted),
    ("ssrc.profile", LegacyProjectionStatus::Derived),
    ("dsd.noise_shaper", LegacyProjectionStatus::Unrepresentable),
    ("dsd.modulator_order", LegacyProjectionStatus::Unrepresentable),
    ("dsd.trellis", LegacyProjectionStatus::Unrepresentable),
    ("dsd.trellis.lookahead", LegacyProjectionStatus::Unrepresentable),
    ("dsd.trellis.nodes", LegacyProjectionStatus::Unrepresentable),
    ("dsd.trellis.latency", LegacyProjectionStatus::Unrepresentable),
    ("dsd.pcm_to_dsd_filter", LegacyProjectionStatus::Unrepresentable),
    ("dsd.dsd_to_pcm_lowpass", LegacyProjectionStatus::Unrepresentable),
    ("dsd.dsd_to_pcm_gain_db", LegacyProjectionStatus::Unrepresentable),
    ("dsd.sinc.oversample_factor", LegacyProjectionStatus::Unrepresentable),
    ("dsd.sinc.taps", LegacyProjectionStatus::Unrepresentable),
    ("dsd.sinc.passband_hz", LegacyProjectionStatus::Unrepresentable),
    ("dsd.sinc.transition_hz", LegacyProjectionStatus::Unrepresentable),
    ("dsd.sinc.kaiser_beta", LegacyProjectionStatus::Unrepresentable),
    ("dsd.sinc.linear_phase", LegacyProjectionStatus::Unrepresentable),
    ("dsd.sinc.allow_aliasing", LegacyProjectionStatus::Unrepresentable),
    ("dsd.gain_compensation", LegacyProjectionStatus::Unrepresentable),
    ("metadata.transfer_tags", LegacyProjectionStatus::Translated),
    ("metadata.preserve_artwork", LegacyProjectionStatus::Translated),
    ("metadata.store_source_audio_md5", LegacyProjectionStatus::Derived),
    ("verification.verify_after_encode", LegacyProjectionStatus::Derived),
    ("verification.prefer_native_flac_verify", LegacyProjectionStatus::Defaulted),
    ("replay_gain.mode", LegacyProjectionStatus::Translated),
    ("replay_gain.prevent_clipping", LegacyProjectionStatus::Defaulted),
];

#[test]
fn legacy_projection_inventory_lists_every_pipeline_settings_field() {
    let mut inventory: Vec<&str> = LEGACY_FIELD_INVENTORY.iter().map(|(path, _)| *path).collect();
    let mut fingerprint = SETTINGS_FINGERPRINT_FIELD_PATHS.to_vec();
    inventory.sort_unstable();
    fingerprint.sort_unstable();
    assert_eq!(inventory, fingerprint);
}

fn legacy_item(options: ConversionOptions) -> ConversionItem {
    ConversionItem::new(
        PathBuf::from("/tmp/tonepoet-settings-sentinel/legacy.flac"),
        FileFormat::Audio(QueueAudioFormat::Flac),
        options,
    )
}

fn rich_legacy_flac_options() -> ConversionOptions {
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
    options
}

#[allow(deprecated)]
fn legacy_projection(item: &ConversionItem) -> PipelineRequest {
    tonepoet::convert::pipeline::build_pipeline_request_from_legacy_options(item).unwrap()
}

fn legacy_options_for_quality(
    output_format: QueueAudioFormat,
    quality: QualitySettings,
) -> ConversionOptions {
    let mut options = rich_legacy_flac_options();
    options.output_format = output_format;
    options.quality = quality;
    options
}

macro_rules! assert_legacy_value {
    ($covered:ident, $status:expr, $path:literal, $actual:expr, $expected:expr) => {{
        $covered.push(($path, $status));
        assert_eq!(&$actual, &$expected, "legacy projection mismatch for {}", $path);
    }};
}

macro_rules! assert_legacy_unrepresentable {
    ($covered:ident, $path:literal, $actual:expr, $default:expr, $sentinel:expr) => {{
        $covered.push(($path, LegacyProjectionStatus::Unrepresentable));
        assert_eq!(&$actual, &$default, "legacy projection unexpectedly translated {}", $path);
        assert_ne!(&$actual, &$sentinel, "sentinel did not exercise unrepresentable {}", $path);
    }};
}

fn assert_legacy_coverage(covered: &[(&'static str, LegacyProjectionStatus)]) {
    let mut actual = covered.to_vec();
    let mut expected = LEGACY_FIELD_INVENTORY.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected, "legacy behavior assertions drifted from inventory");
}

#[test]
fn explicit_legacy_projection_has_behavioral_assertion_for_every_field() {
    let flac = legacy_projection(&legacy_item(rich_legacy_flac_options())).settings;
    let mp3 = legacy_projection(&legacy_item(legacy_options_for_quality(
        QueueAudioFormat::Mp3,
        QualitySettings::Mp3 {
            bitrate_mode: Mp3BitrateMode::Cbr { bitrate: 192 },
            quality: 0,
        },
    )))
    .settings;
    let aac = legacy_projection(&legacy_item(legacy_options_for_quality(
        QueueAudioFormat::Aac,
        QualitySettings::Aac {
            bitrate: 96,
            profile: QueueAacProfile::HeV2,
        },
    )))
    .settings;
    let opus = legacy_projection(&legacy_item(legacy_options_for_quality(
        QueueAudioFormat::Opus,
        QualitySettings::Opus {
            bitrate: 160,
            complexity: 5,
        },
    )))
    .settings;
    let wavpack = legacy_projection(&legacy_item(legacy_options_for_quality(
        QueueAudioFormat::WavPack,
        QualitySettings::WavPack {
            compression_mode: QueueWavPackMode::VeryHigh,
            hybrid_mode: true,
            correction_file: true,
        },
    )))
    .settings;

    let default = PipelineSettings::default();
    let sentinel = flac_md5_sentinel();
    let mut covered = Vec::new();

    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "target_format", flac.target_format, AudioFormat::Flac);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "target_sample_rate", flac.target_sample_rate, RateTarget::PcmHz(96_000));
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "target_bit_depth", flac.target_bit_depth, BitDepthTarget::Pcm(PcmBitDepth::Int24));
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "resample_quality", flac.resample_quality, ResampleQuality::High);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "nyquist_transition", flac.nyquist_transition, NyquistTransition::BrickWall);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "dither_type", flac.dither_type, DitherType::Gesemann);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "preferred_tool", flac.preferred_tool, PreferredTool::Sox);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "force_encode", flac.force_encode, true);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "flac.compression_level", flac.flac.compression_level, 8);
    assert_legacy_value!(covered, LegacyProjectionStatus::Derived, "flac.verify", flac.flac.verify, false);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "mp3.mode", mp3.mp3.mode, Mp3Mode::Cbr);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "mp3.bitrate_kbps", mp3.mp3.bitrate_kbps, 192);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "mp3.vbr_quality", mp3.mp3.vbr_quality, Mp3Settings::default().vbr_quality);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "aac.profile", aac.aac.profile, AacProfile::HeAacV2);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "aac.bitrate_kbps", aac.aac.bitrate_kbps, 96);
    assert_legacy_value!(covered, LegacyProjectionStatus::Defaulted, "opus.content_type", opus.opus.content_type, OpusContentType::Auto);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "opus.bitrate_kbps", opus.opus.bitrate_kbps, 160);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "opus.complexity", opus.opus.complexity, 5);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "wavpack.mode", wavpack.wavpack.mode, WavPackMode::VeryHigh);
    assert_legacy_unrepresentable!(covered, "wavpack.hybrid", wavpack.wavpack.hybrid, default.wavpack.hybrid, sentinel.wavpack.hybrid);
    assert_legacy_unrepresentable!(covered, "wavpack.hybrid_bitrate_kbps", wavpack.wavpack.hybrid_bitrate_kbps, default.wavpack.hybrid_bitrate_kbps, sentinel.wavpack.hybrid_bitrate_kbps);
    assert_legacy_unrepresentable!(covered, "wavpack.correction_file", wavpack.wavpack.correction_file, default.wavpack.correction_file, sentinel.wavpack.correction_file);
    assert_legacy_value!(covered, LegacyProjectionStatus::Derived, "ssrc.force", flac.ssrc.force, true);

    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "ssrc.insane_mode", flac.ssrc.insane_mode, true);
    assert_legacy_unrepresentable!(covered, "sox_resampler.chebyshev", flac.sox_resampler.chebyshev, default.sox_resampler.chebyshev, sentinel.sox_resampler.chebyshev);
    assert_legacy_unrepresentable!(covered, "sox_resampler.bandwidth_pct", flac.sox_resampler.bandwidth_pct, default.sox_resampler.bandwidth_pct, sentinel.sox_resampler.bandwidth_pct);
    assert_legacy_unrepresentable!(covered, "sox_resampler.phase", flac.sox_resampler.phase, default.sox_resampler.phase, sentinel.sox_resampler.phase);
    assert_legacy_unrepresentable!(covered, "sox_resampler.allow_aliasing", flac.sox_resampler.allow_aliasing, default.sox_resampler.allow_aliasing, sentinel.sox_resampler.allow_aliasing);
    assert_legacy_unrepresentable!(covered, "sox_resampler.sinc_taps", flac.sox_resampler.sinc_taps, default.sox_resampler.sinc_taps, sentinel.sox_resampler.sinc_taps);
    assert_legacy_unrepresentable!(covered, "sox_resampler.sinc_attenuation_db", flac.sox_resampler.sinc_attenuation_db, default.sox_resampler.sinc_attenuation_db, sentinel.sox_resampler.sinc_attenuation_db);
    assert_legacy_unrepresentable!(covered, "sox_resampler.sinc_passband_hz", flac.sox_resampler.sinc_passband_hz, default.sox_resampler.sinc_passband_hz, sentinel.sox_resampler.sinc_passband_hz);
    assert_legacy_unrepresentable!(covered, "sox_resampler.sinc_transition_hz", flac.sox_resampler.sinc_transition_hz, default.sox_resampler.sinc_transition_hz, sentinel.sox_resampler.sinc_transition_hz);
    assert_legacy_unrepresentable!(covered, "sox_resampler.sinc_kaiser_beta", flac.sox_resampler.sinc_kaiser_beta, default.sox_resampler.sinc_kaiser_beta, sentinel.sox_resampler.sinc_kaiser_beta);
    assert_legacy_unrepresentable!(covered, "sox_resampler.sinc_phase", flac.sox_resampler.sinc_phase, default.sox_resampler.sinc_phase, sentinel.sox_resampler.sinc_phase);
    assert_legacy_unrepresentable!(covered, "soxr_resampler.chebyshev", flac.soxr_resampler.chebyshev, default.soxr_resampler.chebyshev, sentinel.soxr_resampler.chebyshev);
    assert_legacy_unrepresentable!(covered, "soxr_resampler.cutoff", flac.soxr_resampler.cutoff, default.soxr_resampler.cutoff, sentinel.soxr_resampler.cutoff);
    assert_legacy_unrepresentable!(covered, "soxr_resampler.phase", flac.soxr_resampler.phase, default.soxr_resampler.phase, sentinel.soxr_resampler.phase);
    assert_legacy_value!(covered, LegacyProjectionStatus::Derived, "ssrc.profile", flac.ssrc.profile, Some(SsrcProfile::Insane));
    assert_legacy_unrepresentable!(covered, "dsd.noise_shaper", flac.dsd.noise_shaper, default.dsd.noise_shaper, sentinel.dsd.noise_shaper);
    assert_legacy_unrepresentable!(covered, "dsd.modulator_order", flac.dsd.modulator_order, default.dsd.modulator_order, sentinel.dsd.modulator_order);
    assert_legacy_unrepresentable!(covered, "dsd.trellis", flac.dsd.trellis, default.dsd.trellis, sentinel.dsd.trellis);
    assert_legacy_unrepresentable!(covered, "dsd.trellis.lookahead", flac.dsd.trellis.map(|trellis| trellis.lookahead), default.dsd.trellis.map(|trellis| trellis.lookahead), sentinel.dsd.trellis.map(|trellis| trellis.lookahead));
    assert_legacy_unrepresentable!(covered, "dsd.trellis.nodes", flac.dsd.trellis.map(|trellis| trellis.nodes), default.dsd.trellis.map(|trellis| trellis.nodes), sentinel.dsd.trellis.map(|trellis| trellis.nodes));
    assert_legacy_unrepresentable!(covered, "dsd.trellis.latency", flac.dsd.trellis.and_then(|trellis| trellis.latency), default.dsd.trellis.and_then(|trellis| trellis.latency), sentinel.dsd.trellis.and_then(|trellis| trellis.latency));
    assert_legacy_unrepresentable!(covered, "dsd.pcm_to_dsd_filter", flac.dsd.pcm_to_dsd_filter, default.dsd.pcm_to_dsd_filter, sentinel.dsd.pcm_to_dsd_filter);
    assert_legacy_unrepresentable!(covered, "dsd.dsd_to_pcm_lowpass", flac.dsd.dsd_to_pcm_lowpass, default.dsd.dsd_to_pcm_lowpass, sentinel.dsd.dsd_to_pcm_lowpass);
    assert_legacy_unrepresentable!(covered, "dsd.dsd_to_pcm_gain_db", flac.dsd.dsd_to_pcm_gain_db, default.dsd.dsd_to_pcm_gain_db, sentinel.dsd.dsd_to_pcm_gain_db);
    assert_legacy_unrepresentable!(covered, "dsd.sinc.oversample_factor", flac.dsd.sinc.oversample_factor, default.dsd.sinc.oversample_factor, sentinel.dsd.sinc.oversample_factor);
    assert_legacy_unrepresentable!(covered, "dsd.sinc.taps", flac.dsd.sinc.taps, default.dsd.sinc.taps, sentinel.dsd.sinc.taps);
    assert_legacy_unrepresentable!(covered, "dsd.sinc.passband_hz", flac.dsd.sinc.passband_hz, default.dsd.sinc.passband_hz, sentinel.dsd.sinc.passband_hz);
    assert_legacy_unrepresentable!(covered, "dsd.sinc.transition_hz", flac.dsd.sinc.transition_hz, default.dsd.sinc.transition_hz, sentinel.dsd.sinc.transition_hz);
    assert_legacy_unrepresentable!(covered, "dsd.sinc.kaiser_beta", flac.dsd.sinc.kaiser_beta, default.dsd.sinc.kaiser_beta, sentinel.dsd.sinc.kaiser_beta);
    assert_legacy_unrepresentable!(covered, "dsd.sinc.linear_phase", flac.dsd.sinc.linear_phase, default.dsd.sinc.linear_phase, sentinel.dsd.sinc.linear_phase);
    assert_legacy_unrepresentable!(covered, "dsd.sinc.allow_aliasing", flac.dsd.sinc.allow_aliasing, default.dsd.sinc.allow_aliasing, sentinel.dsd.sinc.allow_aliasing);
    assert_legacy_unrepresentable!(covered, "dsd.gain_compensation", flac.dsd.gain_compensation, default.dsd.gain_compensation, sentinel.dsd.gain_compensation);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "metadata.transfer_tags", flac.metadata.transfer_tags, false);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "metadata.preserve_artwork", flac.metadata.preserve_artwork, false);
    assert_legacy_value!(covered, LegacyProjectionStatus::Derived, "metadata.store_source_audio_md5", flac.metadata.store_source_audio_md5, false);
    assert_legacy_value!(covered, LegacyProjectionStatus::Derived, "verification.verify_after_encode", flac.verification.verify_after_encode, false);
    assert_legacy_value!(covered, LegacyProjectionStatus::Defaulted, "verification.prefer_native_flac_verify", flac.verification.prefer_native_flac_verify, true);
    assert_legacy_value!(covered, LegacyProjectionStatus::Translated, "replay_gain.mode", flac.replay_gain.mode, Some(ReplayGainMode::Both));
    assert_legacy_value!(covered, LegacyProjectionStatus::Defaulted, "replay_gain.prevent_clipping", flac.replay_gain.prevent_clipping, true);

    assert_legacy_coverage(&covered);
}
